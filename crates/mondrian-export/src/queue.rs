//! 后台渲染队列

use crate::preset::{
    AudioCodecConfig, Container, ExportConfig, ExportInput, TimelineExportInput, VideoCodecConfig,
};
use crate::validator::{
    probe_media_summary, validate_export_output, ExpectedVideoConstraints,
    ExportValidationExpectations,
};
use chrono::{DateTime, Utc};
use mondrian_core::types::{AssetId, JobId, TimeCode};
use mondrian_media::audio::{
    AudioBuffer, AudioMixer, AudioSourceCache, AudioTrackConfig, AudioTrackData,
};
use mondrian_media::decode_video_frame_at_time_rgba_scaled;
use mondrian_renderer::{
    build_timeline_render_plan, composite_timeline_elements_into, TimelineAdjustmentLayer,
    TimelineCompositeElement, TimelineCompositeOptions, TimelineCompositeScratch,
    TimelineMediaLayer, TimelineRenderPlanElement,
};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    Pending,
    Rendering { frame: u64, total_frames: u64 },
    Encoding,
    Completed,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct RenderJob {
    pub id: JobId,
    pub config: ExportConfig,
    pub status: JobStatus,
    pub progress: f32,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl RenderJob {
    pub fn new(config: ExportConfig) -> Self {
        Self {
            id: JobId::new(),
            config,
            status: JobStatus::Pending,
            progress: 0.0,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
        }
    }
}

enum JobExecutionResult {
    Completed,
    Failed(String),
    Cancelled,
}

trait ExportExecutor: Send + Sync + 'static {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &AtomicBool,
        report: &mut dyn FnMut(JobStatus, f32),
    ) -> JobExecutionResult;
}

#[derive(Default)]
pub struct FfmpegExportExecutor;

impl ExportExecutor for FfmpegExportExecutor {
    fn execute(
        &self,
        job: &RenderJob,
        cancel: &AtomicBool,
        report: &mut dyn FnMut(JobStatus, f32),
    ) -> JobExecutionResult {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        if let Some(parent) = job.config.output_path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return JobExecutionResult::Failed(format!(
                    "无法创建导出目录 {}: {}",
                    parent.display(),
                    err
                ));
            }
        }

        match &job.config.input {
            ExportInput::File { input_path, in_point, out_point } => execute_file_export(
                job,
                input_path,
                in_point.as_deref(),
                out_point.as_deref(),
                cancel,
                report,
            ),
            ExportInput::Timeline(timeline) => {
                execute_timeline_export(job, timeline, cancel, report)
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TimelineRenderRange {
    start_frame: i64,
    total_frames: u64,
    fps_num: i64,
    fps_den: i64,
}

enum TimelineAudioInput {
    PcmFile {
        path: PathBuf,
        sample_rate: u32,
        channels: u8,
    },
    Silent {
        sample_rate: u32,
        channels: u8,
    },
    Disabled,
}

#[derive(Clone)]
struct DecodedVideoLayer {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

fn execute_file_export(
    job: &RenderJob,
    input_path: &Path,
    in_point: Option<&str>,
    out_point: Option<&str>,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    if !input_path.exists() {
        return JobExecutionResult::Failed(format!("导出输入不存在：{}", input_path.display()));
    }

    let source_summary = probe_media_summary(input_path).ok();
    report(JobStatus::Encoding, 0.02);

    let duration_ms = probe_duration_ms(input_path, in_point, out_point).unwrap_or(0);

    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-progress")
        .arg("pipe:2")
        .arg("-nostats")
        .arg("-loglevel")
        .arg("error");

    if let Some(in_point) = in_point {
        cmd.arg("-ss").arg(in_point);
    }
    cmd.arg("-i").arg(input_path);
    if let Some(out_point) = out_point {
        cmd.arg("-to").arg(out_point);
    }

    if let Some(filter) = build_video_filter(&job.config) {
        cmd.arg("-vf").arg(filter);
    }

    apply_video_codec_args(&mut cmd, &job.config.preset.video);
    apply_audio_codec_args(&mut cmd, &job.config.preset.audio);
    cmd.arg("-f")
        .arg(container_format(&job.config.preset.container))
        .arg(&job.config.output_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return JobExecutionResult::Failed(format!("无法启动 ffmpeg: {}", err));
        }
    };

    match monitor_ffmpeg_child(child, duration_ms, cancel, report) {
        JobExecutionResult::Completed => {
            let expectations = ExportValidationExpectations {
                require_video_stream: true,
                require_audio_stream: source_summary.map(|s| s.has_audio).unwrap_or(false),
                expected_video: job.config.preset.resolution.as_ref().map(|resolution| {
                    ExpectedVideoConstraints {
                        width: Some(normalize_output_dimension(resolution.width)),
                        height: Some(normalize_output_dimension(resolution.height)),
                        fps_num: None,
                        fps_den: None,
                    }
                }),
                expected_duration_secs: if duration_ms > 0 {
                    Some(duration_ms as f64 / 1000.0)
                } else {
                    None
                },
            };
            match validate_export_output(job.config.output_path.as_path(), &expectations) {
                Ok(()) => JobExecutionResult::Completed,
                Err(err) => JobExecutionResult::Failed(format!("导出结果校验失败: {err}")),
            }
        }
        other => other,
    }
}

fn execute_timeline_export(
    job: &RenderJob,
    timeline: &TimelineExportInput,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    let mut temp_audio_path_to_cleanup: Option<PathBuf> = None;
    let result = (|| {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        for (asset_id, path) in &timeline.asset_paths {
            if !path.exists() {
                return JobExecutionResult::Failed(format!(
                    "时间线素材离线：asset={} path={}",
                    asset_id,
                    path.display()
                ));
            }
        }

        let range = compute_timeline_render_range(timeline);
        if range.total_frames == 0 {
            return JobExecutionResult::Failed("时间线导出范围为空".to_string());
        }

        let audio_input = prepare_timeline_audio_input(job, timeline, range, cancel, report);
        let audio_input = match audio_input {
            Ok(input) => input,
            Err(outcome) => return outcome,
        };
        if let TimelineAudioInput::PcmFile { path, .. } = &audio_input {
            temp_audio_path_to_cleanup = Some(path.clone());
        }

        let (width, height) = timeline_output_resolution(job, timeline);
        let validation_expectations = ExportValidationExpectations {
            require_video_stream: true,
            require_audio_stream: !matches!(&audio_input, TimelineAudioInput::Disabled),
            expected_video: Some(ExpectedVideoConstraints {
                width: Some(width),
                height: Some(height),
                fps_num: Some(range.fps_num),
                fps_den: Some(range.fps_den),
            }),
            expected_duration_secs: Some(
                range.total_frames as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64,
            ),
        };
        let mut cmd = Command::new("ffmpeg");
        cmd.arg("-y")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("rawvideo")
            .arg("-pix_fmt")
            .arg("rgba")
            .arg("-s")
            .arg(format!("{width}x{height}"))
            .arg("-r")
            .arg(format!("{}/{}", range.fps_num, range.fps_den))
            .arg("-i")
            .arg("pipe:0");

        match &audio_input {
            TimelineAudioInput::PcmFile { path, sample_rate, channels } => {
                cmd.arg("-f")
                    .arg("f32le")
                    .arg("-ar")
                    .arg(sample_rate.to_string())
                    .arg("-ac")
                    .arg(channels.to_string())
                    .arg("-i")
                    .arg(path)
                    .arg("-map")
                    .arg("0:v:0")
                    .arg("-map")
                    .arg("1:a:0")
                    .arg("-shortest");
            }
            TimelineAudioInput::Silent { sample_rate, channels } => {
                let channel_layout = if *channels <= 1 { "mono" } else { "stereo" };
                cmd.arg("-f")
                    .arg("lavfi")
                    .arg("-i")
                    .arg(format!(
                        "anullsrc=channel_layout={channel_layout}:sample_rate={sample_rate}"
                    ))
                    .arg("-map")
                    .arg("0:v:0")
                    .arg("-map")
                    .arg("1:a:0")
                    .arg("-shortest");
            }
            TimelineAudioInput::Disabled => {
                cmd.arg("-an");
            }
        }

        apply_video_codec_args(&mut cmd, &job.config.preset.video);
        if !matches!(&audio_input, TimelineAudioInput::Disabled) {
            apply_audio_codec_args(&mut cmd, &job.config.preset.audio);
        }
        cmd.arg("-f")
            .arg(container_format(&job.config.preset.container))
            .arg(&job.config.output_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => {
                return JobExecutionResult::Failed(format!("无法启动 ffmpeg: {}", err));
            }
        };

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return JobExecutionResult::Failed("ffmpeg stdin 管道不可用".to_string());
        };

        match write_timeline_frames(stdin, timeline, range, width, height, cancel, report) {
            JobExecutionResult::Completed => {}
            JobExecutionResult::Cancelled => {
                let _ = child.kill();
                let _ = child.wait();
                return JobExecutionResult::Cancelled;
            }
            JobExecutionResult::Failed(reason) => {
                let _ = child.kill();
                let _ = child.wait();
                return JobExecutionResult::Failed(reason);
            }
        }

        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return JobExecutionResult::Cancelled;
        }

        report(JobStatus::Encoding, 0.98);
        match child.wait_with_output() {
            Ok(output) if output.status.success() => {
                match validate_export_output(
                    job.config.output_path.as_path(),
                    &validation_expectations,
                ) {
                    Ok(()) => JobExecutionResult::Completed,
                    Err(err) => JobExecutionResult::Failed(format!("导出结果校验失败: {err}")),
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let reason = stderr
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .map(|line| line.trim().to_string())
                    .unwrap_or_else(|| format!("ffmpeg 退出码：{}", output.status));
                JobExecutionResult::Failed(format!("时间线编码失败：{reason}"))
            }
            Err(err) => JobExecutionResult::Failed(format!("等待 ffmpeg 结束失败: {}", err)),
        }
    })();

    if let Some(path) = temp_audio_path_to_cleanup {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn prepare_timeline_audio_input(
    job: &RenderJob,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> Result<TimelineAudioInput, JobExecutionResult> {
    if matches!(job.config.preset.container, Container::Gif) {
        return Ok(TimelineAudioInput::Disabled);
    }

    let sample_rate = timeline.sequence.settings.audio_sample_rate.max(8_000);
    let channels = timeline.sequence.settings.audio_channels.clamp(1, 2);
    if !timeline_has_audio_content(timeline, range) {
        return Ok(TimelineAudioInput::Silent { sample_rate, channels });
    }

    let temp_path = std::env::temp_dir().join(format!(
        "mondrian-export-audio-{}-{}.f32",
        job.id,
        Utc::now().timestamp_millis()
    ));

    match render_timeline_audio_to_pcm_f32(
        temp_path.as_path(),
        timeline,
        range,
        sample_rate,
        channels,
        cancel,
        report,
    ) {
        JobExecutionResult::Completed => {
            Ok(TimelineAudioInput::PcmFile { path: temp_path, sample_rate, channels })
        }
        JobExecutionResult::Cancelled => Err(JobExecutionResult::Cancelled),
        JobExecutionResult::Failed(reason) => Err(JobExecutionResult::Failed(reason)),
    }
}

fn timeline_has_audio_content(timeline: &TimelineExportInput, range: TimelineRenderRange) -> bool {
    let seq = &timeline.sequence;
    let has_solo = seq.audio_tracks.iter().any(|t| t.is_solo && !t.is_muted);
    let start = range.start_frame;
    let end_exclusive = start.saturating_add(range.total_frames as i64);

    for track in &seq.audio_tracks {
        if track.is_muted || (has_solo && !track.is_solo) {
            continue;
        }
        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }
            if !timeline.asset_paths.contains_key(&clip.asset_id) {
                continue;
            }

            let clip_start = clip.position.frame;
            let clip_end = clip.end_position().frame;
            if clip_end > start && clip_start < end_exclusive {
                return true;
            }
        }
    }
    false
}

fn render_timeline_audio_to_pcm_f32(
    output_path: &Path,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    sample_rate: u32,
    channels: u8,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    let file = match std::fs::File::create(output_path) {
        Ok(file) => file,
        Err(err) => {
            return JobExecutionResult::Failed(format!(
                "创建临时音频文件失败 {}: {}",
                output_path.display(),
                err
            ));
        }
    };
    let mut writer = BufWriter::new(file);
    let cache = AudioSourceCache::new(sample_rate, channels);
    let mixer = AudioMixer::new(sample_rate, channels);

    let total_samples = timeline_total_audio_samples(range, sample_rate);
    if total_samples == 0 {
        return JobExecutionResult::Completed;
    }

    let chunk_frames_target = (sample_rate as usize / 5).clamp(1024, 16_384);
    let timeline_start_secs =
        range.start_frame.max(0) as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64;

    let mut rendered_samples = 0usize;
    let mut sample_bytes = Vec::<u8>::with_capacity(chunk_frames_target * channels as usize * 4);

    while rendered_samples < total_samples {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        let remaining = total_samples - rendered_samples;
        let chunk_frames = remaining.min(chunk_frames_target).max(1);
        let chunk_start_secs = timeline_start_secs + rendered_samples as f64 / sample_rate as f64;
        let chunk = match render_timeline_audio_chunk(
            timeline,
            &cache,
            &mixer,
            chunk_start_secs,
            chunk_frames,
            sample_rate,
            channels,
        ) {
            Ok(buffer) => buffer,
            Err(err) => return JobExecutionResult::Failed(err),
        };

        sample_bytes.clear();
        sample_bytes.reserve(chunk.samples.len() * 4);
        for sample in &chunk.samples {
            sample_bytes.extend_from_slice(&sample.to_le_bytes());
        }
        if let Err(err) = writer.write_all(&sample_bytes) {
            return JobExecutionResult::Failed(format!("写入临时音频文件失败: {}", err));
        }

        rendered_samples += chunk_frames;
        let ratio = rendered_samples as f32 / total_samples as f32;
        let progress = (0.02 + 0.14 * ratio).clamp(0.02, 0.16);
        report(JobStatus::Encoding, progress);
    }

    if let Err(err) = writer.flush() {
        return JobExecutionResult::Failed(format!("刷新临时音频文件失败: {}", err));
    }
    JobExecutionResult::Completed
}

fn timeline_total_audio_samples(range: TimelineRenderRange, sample_rate: u32) -> usize {
    if range.total_frames == 0 || sample_rate == 0 {
        return 0;
    }
    let seconds = range.total_frames as f64 * range.fps_den as f64 / range.fps_num.max(1) as f64;
    (seconds * sample_rate as f64).round().max(0.0) as usize
}

fn render_timeline_audio_chunk(
    timeline: &TimelineExportInput,
    cache: &AudioSourceCache,
    mixer: &AudioMixer,
    window_start_secs: f64,
    chunk_frames: usize,
    sample_rate: u32,
    channels: u8,
) -> Result<AudioBuffer, String> {
    let seq = &timeline.sequence;
    let chunk_duration_secs = chunk_frames as f64 / sample_rate.max(1) as f64;
    let window_end_secs = window_start_secs + chunk_duration_secs;
    let has_solo = seq.audio_tracks.iter().any(|t| t.is_solo && !t.is_muted);
    let mut tracks = Vec::<AudioTrackData>::new();

    for track in &seq.audio_tracks {
        if track.is_muted || (has_solo && !track.is_solo) {
            continue;
        }

        for clip in &track.clips {
            if clip.is_disabled {
                continue;
            }

            let Some(path) = timeline.asset_paths.get(&clip.asset_id) else {
                continue;
            };
            let clip_start_secs = clip.position.to_secs();
            let clip_end_secs = clip.end_position().to_secs();
            let overlap_start = window_start_secs.max(clip_start_secs);
            let overlap_end = window_end_secs.min(clip_end_secs);
            if overlap_end <= overlap_start {
                continue;
            }

            let decoded = cache.get_or_decode(path.as_path()).map_err(|err| {
                format!(
                    "解码音频失败 asset={} path={} err={}",
                    clip.asset_id,
                    path.display(),
                    err
                )
            })?;

            let overlap_tc = TimeCode::from_secs(overlap_start, seq.settings.frame_rate);
            let source_start_secs = clip.timeline_to_source_time(overlap_tc).to_secs().max(0.0);
            let source_start_frame = (source_start_secs * sample_rate as f64).floor() as usize;
            let segment_frames =
                ((overlap_end - overlap_start) * sample_rate as f64).ceil().max(1.0) as usize;
            let segment = decoded.slice_frames(source_start_frame, segment_frames);
            if segment.samples.is_empty() {
                continue;
            }

            let place_offset = ((overlap_start - window_start_secs) * sample_rate as f64)
                .round()
                .max(0.0) as usize;
            let mut placed = AudioBuffer::silent(sample_rate, channels, chunk_frames);
            let max_place_frames = chunk_frames.saturating_sub(place_offset);
            let copy_frames = segment.frame_count().min(max_place_frames);

            let dst_channels = channels as usize;
            let src_channels = segment.channels as usize;
            for frame in 0..copy_frames {
                let dst_base = (place_offset + frame) * dst_channels;
                let src_base = frame * src_channels;
                for ch in 0..dst_channels {
                    let src_ch = ch.min(src_channels.saturating_sub(1));
                    let sample = segment.samples.get(src_base + src_ch).copied().unwrap_or(0.0);
                    placed.samples[dst_base + ch] = sample;
                }
            }

            tracks.push(AudioTrackData {
                buffer: placed,
                config: AudioTrackConfig {
                    volume: 1.0,
                    pan: 0.0,
                    is_muted: false,
                    is_solo: false,
                },
            });
        }
    }

    if tracks.is_empty() {
        return Ok(AudioBuffer::silent(sample_rate, channels, chunk_frames));
    }

    let mut mixed = mixer.mix(&tracks);
    let mixed_frames = mixed.frame_count();
    if mixed_frames < chunk_frames {
        mixed.samples.resize(chunk_frames * channels as usize, 0.0);
    } else if mixed_frames > chunk_frames {
        mixed.samples.truncate(chunk_frames.saturating_mul(channels as usize));
    }
    Ok(mixed)
}

fn write_timeline_frames(
    stdin: ChildStdin,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    let mut writer = BufWriter::new(stdin);
    write_timeline_frames_to_writer(&mut writer, timeline, range, width, height, cancel, report)
}

fn write_timeline_frames_to_writer<W: Write>(
    writer: &mut W,
    timeline: &TimelineExportInput,
    range: TimelineRenderRange,
    width: u32,
    height: u32,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    let total = range.total_frames.max(1);
    let mut canvas = vec![0u8; width as usize * height as usize * 4];

    for index in 0..total {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        let timeline_frame = range.start_frame + index as i64;
        match render_timeline_frame_into(timeline, timeline_frame, width, height, &mut canvas) {
            Ok(()) => {}
            Err(err) => {
                return JobExecutionResult::Failed(format!(
                    "渲染时间线帧失败（frame={}）: {}",
                    timeline_frame, err
                ));
            }
        }

        if let Err(err) = writer.write_all(&canvas) {
            return JobExecutionResult::Failed(format!("写入编码管道失败: {}", err));
        }

        let rendered = index + 1;
        let ratio = rendered as f32 / total as f32;
        let progress = (0.18 + 0.72 * ratio).clamp(0.18, 0.92);
        report(
            JobStatus::Rendering { frame: rendered, total_frames: total },
            progress,
        );
    }

    if let Err(err) = writer.flush() {
        return JobExecutionResult::Failed(format!("刷新编码管道失败: {}", err));
    }

    JobExecutionResult::Completed
}

fn render_timeline_frame_into(
    timeline: &TimelineExportInput,
    timeline_frame: i64,
    width: u32,
    height: u32,
    canvas: &mut Vec<u8>,
) -> Result<(), String> {
    let required_len = width as usize * height as usize * 4;
    if canvas.len() != required_len {
        canvas.resize(required_len, 0);
    }

    let render_plan = build_timeline_render_plan(&timeline.sequence, timeline_frame.max(0));
    if render_plan.is_empty() {
        clear_canvas_black_opaque(canvas);
        return Ok(());
    }

    let mut decode_cache = (render_plan.len() > 1).then(|| {
        HashMap::<(AssetId, i64), Arc<DecodedVideoLayer>>::with_capacity(render_plan.len())
    });
    let mut decoded_media =
        std::iter::repeat_with(|| None).take(render_plan.len()).collect::<Vec<_>>();

    for (index, element) in render_plan.iter().enumerate() {
        let TimelineRenderPlanElement::Media(media) = element else {
            continue;
        };
        let Some(path) = timeline.asset_paths.get(&media.asset_id) else {
            continue;
        };
        let decoded = if let Some(cache) = decode_cache.as_mut() {
            if let Some(hit) = cache.get(&(media.asset_id, media.source_frame)) {
                Arc::clone(hit)
            } else {
                let decoded = decode_video_layer_scaled(
                    media.asset_id,
                    path.as_path(),
                    media.source_secs,
                    width,
                    height,
                )?;
                cache.insert((media.asset_id, media.source_frame), Arc::clone(&decoded));
                decoded
            }
        } else {
            decode_video_layer_scaled(
                media.asset_id,
                path.as_path(),
                media.source_secs,
                width,
                height,
            )?
        };
        decoded_media[index] = Some(decoded);
    }

    let mut composite_elements = Vec::with_capacity(render_plan.len());
    for (index, element) in render_plan.iter().enumerate() {
        match element {
            TimelineRenderPlanElement::Adjustment(adjustment) => {
                composite_elements.push(TimelineCompositeElement::Adjustment(
                    TimelineAdjustmentLayer {
                        effect_graph: adjustment.effect_graph.clone(),
                        opacity: adjustment.opacity,
                        blend_mode: Some(adjustment.blend_mode),
                        frame_seed: adjustment.frame_seed,
                    },
                ));
            }
            TimelineRenderPlanElement::Media(media) => {
                let Some(decoded) = decoded_media[index].as_ref() else {
                    continue;
                };
                composite_elements.push(TimelineCompositeElement::Media(TimelineMediaLayer {
                    rgba: &decoded.data,
                    width: decoded.width,
                    height: decoded.height,
                    opacity: media.opacity,
                    blend_mode: media.blend_mode,
                    transform: media.transform,
                    effect_graph: media.effect_graph.clone(),
                    frame_seed: media.frame_seed,
                }));
            }
        }
    }

    let mut scratch = TimelineCompositeScratch::default();
    composite_timeline_elements_into(
        canvas,
        width,
        height,
        &composite_elements,
        TimelineCompositeOptions::default(),
        &mut scratch,
    );
    Ok(())
}

fn decode_video_layer_scaled(
    asset_id: AssetId,
    path: &Path,
    source_secs: f64,
    width: u32,
    height: u32,
) -> Result<Arc<DecodedVideoLayer>, String> {
    let decoded =
        decode_video_frame_at_time_rgba_scaled(path, source_secs, Some(width), Some(height))
            .map_err(|err| format!("asset={} path={} err={}", asset_id, path.display(), err))?;
    Ok(Arc::new(DecodedVideoLayer {
        width: decoded.width,
        height: decoded.height,
        data: decoded.data,
    }))
}

fn compute_timeline_render_range(timeline: &TimelineExportInput) -> TimelineRenderRange {
    let sequence = &timeline.sequence;
    let start = timeline.in_point_frame.unwrap_or(0).max(0);
    let sequence_end_exclusive = sequence.total_duration().frame.max(1);
    let max_end_exclusive = sequence_end_exclusive.max(start.saturating_add(1));

    let requested_end_exclusive = timeline
        .out_point_frame
        .map(|frame| frame.saturating_add(1))
        .unwrap_or(sequence_end_exclusive);
    let end_exclusive = requested_end_exclusive.max(start.saturating_add(1)).min(max_end_exclusive);
    let total_frames = end_exclusive.saturating_sub(start) as u64;

    let fps_num = sequence.settings.frame_rate.num.max(1);
    let fps_den = sequence.settings.frame_rate.den.max(1);
    TimelineRenderRange { start_frame: start, total_frames, fps_num, fps_den }
}

fn timeline_output_resolution(job: &RenderJob, timeline: &TimelineExportInput) -> (u32, u32) {
    if let Some(resolution) = &job.config.preset.resolution {
        return (
            normalize_output_dimension(resolution.width),
            normalize_output_dimension(resolution.height),
        );
    }

    (
        normalize_output_dimension(timeline.sequence.settings.resolution.width),
        normalize_output_dimension(timeline.sequence.settings.resolution.height),
    )
}

fn normalize_output_dimension(value: u32) -> u32 {
    let mut dim = value.max(1);
    if dim > 1 && dim % 2 == 1 {
        dim = dim.saturating_sub(1);
    }
    dim.max(1)
}

fn clear_canvas_black_opaque(canvas: &mut [u8]) {
    canvas.fill(0);
    force_canvas_alpha_opaque(canvas);
}

fn force_canvas_alpha_opaque(canvas: &mut [u8]) {
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

/// 异步后台渲染队列
pub struct RenderQueue {
    jobs: Arc<Mutex<VecDeque<RenderJob>>>,
    wake: Arc<Condvar>,
    shutdown: Arc<AtomicBool>,
    cancel_flags: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
    executor: Arc<dyn ExportExecutor>,
}

impl RenderQueue {
    pub fn new() -> Arc<Self> {
        Self::new_with_executor(Arc::new(FfmpegExportExecutor))
    }

    fn new_with_executor(executor: Arc<dyn ExportExecutor>) -> Arc<Self> {
        let queue = Arc::new(Self::with_executor(executor));
        queue.spawn_worker();
        queue
    }

    fn with_executor(executor: Arc<dyn ExportExecutor>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(VecDeque::new())),
            wake: Arc::new(Condvar::new()),
            shutdown: Arc::new(AtomicBool::new(false)),
            cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            executor,
        }
    }

    fn spawn_worker(&self) {
        let jobs = Arc::clone(&self.jobs);
        let wake = Arc::clone(&self.wake);
        let shutdown = Arc::clone(&self.shutdown);
        let cancel_flags = Arc::clone(&self.cancel_flags);
        let executor = Arc::clone(&self.executor);

        std::thread::Builder::new()
            .name("mondrian-export-worker".to_string())
            .spawn(move || {
                while let Some((job, cancel_flag)) =
                    take_next_pending_job(&jobs, &wake, &shutdown, &cancel_flags)
                {
                    let mut report = |status: JobStatus, progress: f32| {
                        update_job_status(&jobs, job.id, status, progress);
                    };
                    let outcome = executor.execute(&job, cancel_flag.as_ref(), &mut report);

                    match outcome {
                        JobExecutionResult::Completed => {
                            update_job_terminal_state(&jobs, job.id, JobStatus::Completed, 1.0);
                        }
                        JobExecutionResult::Cancelled => {
                            update_job_terminal_state(&jobs, job.id, JobStatus::Cancelled, 0.0);
                        }
                        JobExecutionResult::Failed(reason) => {
                            update_job_terminal_state(
                                &jobs,
                                job.id,
                                JobStatus::Failed(reason),
                                0.0,
                            );
                        }
                    }

                    cancel_flags.lock().remove(&job.id);
                }
            })
            .expect("failed to spawn export worker thread");
    }

    pub fn enqueue(&self, job: RenderJob) -> JobId {
        let id = job.id;
        self.jobs.lock().push_back(job);
        self.wake.notify_one();
        id
    }

    pub fn list_jobs(&self) -> Vec<RenderJob> {
        self.jobs.lock().iter().cloned().collect()
    }

    pub fn cancel(&self, id: JobId) {
        let mut should_wake = false;
        {
            let mut queue = self.jobs.lock();
            if let Some(job) = queue.iter_mut().find(|j| j.id == id) {
                match job.status {
                    JobStatus::Pending => {
                        job.status = JobStatus::Cancelled;
                        job.progress = 0.0;
                        job.completed_at = Some(Utc::now());
                        should_wake = true;
                    }
                    JobStatus::Rendering { .. } | JobStatus::Encoding => {
                        if let Some(flag) = self.cancel_flags.lock().get(&id).cloned() {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
                    JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled => {}
                }
            }
        }
        if should_wake {
            self.wake.notify_all();
        }
    }

    pub fn clear_completed(&self) {
        let mut queue = self.jobs.lock();
        queue.retain(|j| !matches!(j.status, JobStatus::Completed | JobStatus::Cancelled));
    }
}

impl Drop for RenderQueue {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }
}

impl Default for RenderQueue {
    fn default() -> Self {
        let queue = Self::with_executor(Arc::new(FfmpegExportExecutor));
        queue.spawn_worker();
        queue
    }
}

fn take_next_pending_job(
    jobs: &Mutex<VecDeque<RenderJob>>,
    wake: &Condvar,
    shutdown: &AtomicBool,
    cancel_flags: &Mutex<HashMap<JobId, Arc<AtomicBool>>>,
) -> Option<(RenderJob, Arc<AtomicBool>)> {
    let mut queue = jobs.lock();
    loop {
        if shutdown.load(Ordering::Relaxed) {
            return None;
        }

        if let Some(index) = queue.iter().position(|job| matches!(job.status, JobStatus::Pending)) {
            let job = queue.get_mut(index).expect("pending job index should always be valid");
            job.status = JobStatus::Rendering { frame: 0, total_frames: 1000 };
            job.progress = 0.0;
            job.started_at = Some(Utc::now());

            let snapshot = job.clone();
            let cancel_flag = Arc::new(AtomicBool::new(false));
            cancel_flags.lock().insert(snapshot.id, Arc::clone(&cancel_flag));
            return Some((snapshot, cancel_flag));
        }

        wake.wait(&mut queue);
    }
}

fn update_job_status(
    jobs: &Mutex<VecDeque<RenderJob>>,
    job_id: JobId,
    status: JobStatus,
    progress: f32,
) {
    let mut queue = jobs.lock();
    if let Some(job) = queue.iter_mut().find(|job| job.id == job_id) {
        if matches!(job.status, JobStatus::Cancelled) && !matches!(status, JobStatus::Cancelled) {
            return;
        }
        if is_terminal(&job.status) {
            return;
        }
        job.status = status;
        job.progress = progress.clamp(0.0, 1.0);
    }
}

fn update_job_terminal_state(
    jobs: &Mutex<VecDeque<RenderJob>>,
    job_id: JobId,
    status: JobStatus,
    progress: f32,
) {
    let mut queue = jobs.lock();
    if let Some(job) = queue.iter_mut().find(|job| job.id == job_id) {
        if is_terminal(&job.status) {
            return;
        }
        job.status = status;
        job.progress = progress.clamp(0.0, 1.0);
        job.completed_at = Some(Utc::now());
    }
}

fn is_terminal(status: &JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled
    )
}

fn monitor_ffmpeg_child(
    mut child: Child,
    duration_ms: u64,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let _ = child.kill();
            return JobExecutionResult::Failed("ffmpeg stderr 管道不可用".to_string());
        }
    };

    let (progress_tx, progress_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let _ = progress_tx.send(line);
                }
                Err(_) => break,
            }
        }
    });

    let mut last_ratio = 0.0_f64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return JobExecutionResult::Cancelled;
        }

        match progress_rx.recv_timeout(Duration::from_millis(120)) {
            Ok(line) => {
                if let Some(out_time_us) = parse_progress_time_us(&line) {
                    if duration_ms > 0 {
                        let ratio =
                            (out_time_us as f64 / (duration_ms as f64 * 1000.0)).clamp(0.0, 1.0);
                        if ratio > last_ratio + 0.001 {
                            last_ratio = ratio;
                            let frame = (ratio * 1000.0).round().clamp(0.0, 1000.0) as u64;
                            let progress = (0.05 + 0.9 * ratio).clamp(0.0, 0.98) as f32;
                            report(JobStatus::Rendering { frame, total_frames: 1000 }, progress);
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    report(JobStatus::Encoding, 0.99);
                    return JobExecutionResult::Completed;
                }
                return JobExecutionResult::Failed(format!("ffmpeg 退出码：{}", status));
            }
            Ok(None) => {}
            Err(err) => {
                return JobExecutionResult::Failed(format!("检查 ffmpeg 进程状态失败: {}", err));
            }
        }
    }
}

fn build_video_filter(config: &ExportConfig) -> Option<String> {
    let mut filters = Vec::<String>::new();

    if matches!(config.preset.video, VideoCodecConfig::Gif { .. }) {
        filters.push("fps=15".to_string());
    }

    if let Some(resolution) = &config.preset.resolution {
        filters.push(format!(
            "scale={}:{}:force_original_aspect_ratio=decrease",
            resolution.width, resolution.height
        ));
        filters.push(format!(
            "pad={}:{}:(ow-iw)/2:(oh-ih)/2",
            resolution.width, resolution.height
        ));
    }

    if filters.is_empty() {
        None
    } else {
        Some(filters.join(","))
    }
}

fn apply_video_codec_args(cmd: &mut Command, codec: &VideoCodecConfig) {
    match codec {
        VideoCodecConfig::H264 { crf, bitrate_kbps } => {
            cmd.arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("medium")
                .arg("-crf")
                .arg(crf.to_string());
            if let Some(bitrate) = bitrate_kbps {
                cmd.arg("-b:v").arg(format!("{}k", bitrate));
            }
        }
        VideoCodecConfig::H265 { crf, bitrate_kbps } => {
            cmd.arg("-c:v")
                .arg("libx265")
                .arg("-preset")
                .arg("medium")
                .arg("-crf")
                .arg(crf.to_string());
            if let Some(bitrate) = bitrate_kbps {
                cmd.arg("-b:v").arg(format!("{}k", bitrate));
            }
        }
        VideoCodecConfig::Av1 { crf } => {
            cmd.arg("-c:v")
                .arg("libaom-av1")
                .arg("-crf")
                .arg(crf.to_string())
                .arg("-b:v")
                .arg("0");
        }
        VideoCodecConfig::ProRes { variant } => {
            cmd.arg("-c:v")
                .arg("prores_ks")
                .arg("-profile:v")
                .arg(prores_profile_variant(variant));
        }
        VideoCodecConfig::Gif { .. } => {
            cmd.arg("-c:v").arg("gif");
        }
    }
}

fn apply_audio_codec_args(cmd: &mut Command, codec: &AudioCodecConfig) {
    match codec {
        AudioCodecConfig::Aac { bitrate_kbps } => {
            cmd.arg("-c:a").arg("aac").arg("-b:a").arg(format!("{}k", bitrate_kbps));
        }
        AudioCodecConfig::Pcm { bit_depth } => {
            let pcm = match bit_depth {
                24 => "pcm_s24le",
                32 => "pcm_s32le",
                _ => "pcm_s16le",
            };
            cmd.arg("-c:a").arg(pcm);
        }
        AudioCodecConfig::Mp3 { bitrate_kbps } => {
            cmd.arg("-c:a").arg("libmp3lame").arg("-b:a").arg(format!("{}k", bitrate_kbps));
        }
    }
}

fn prores_profile_variant(variant: &str) -> &'static str {
    match variant.to_ascii_lowercase().as_str() {
        "proxy" => "0",
        "lt" => "1",
        "standard" => "2",
        "hq" => "3",
        "4444" => "4",
        "4444xq" => "5",
        _ => "3",
    }
}

fn container_format(container: &Container) -> &'static str {
    match container {
        Container::Mp4 => "mp4",
        Container::Mov => "mov",
        Container::Mkv => "matroska",
        Container::Gif => "gif",
        Container::Mxf => "mxf",
        Container::Webm => "webm",
    }
}

fn parse_progress_time_us(line: &str) -> Option<u64> {
    if let Some(raw) = line.strip_prefix("out_time_ms=") {
        // ffmpeg progress 的 out_time_ms 字段单位为 microseconds
        return raw.trim().parse::<u64>().ok();
    }
    if let Some(raw) = line.strip_prefix("out_time=") {
        let millis = parse_time_spec_millis(raw.trim())?;
        return Some(millis.saturating_mul(1000));
    }
    None
}

fn probe_duration_ms(path: &Path, in_point: Option<&str>, out_point: Option<&str>) -> Option<u64> {
    let output = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=nokey=1:noprint_wrappers=1")
        .arg(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8(output.stdout).ok()?;
    let total_ms = (raw.trim().parse::<f64>().ok()? * 1000.0).max(0.0) as u64;

    let in_ms = in_point.and_then(parse_time_spec_millis).unwrap_or(0);
    let out_ms = out_point.and_then(parse_time_spec_millis);

    match out_ms {
        Some(out_ms) if out_ms > in_ms => Some(out_ms - in_ms),
        Some(out_ms) => Some(out_ms),
        None if total_ms > in_ms => Some(total_ms - in_ms),
        None => Some(total_ms),
    }
}

fn parse_time_spec_millis(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }

    if let Ok(secs) = raw.parse::<f64>() {
        if secs.is_sign_negative() {
            return None;
        }
        return Some((secs * 1000.0).round() as u64);
    }

    let parts: Vec<&str> = raw.split(':').collect();
    match parts.as_slice() {
        [ss] => {
            let secs = ss.parse::<f64>().ok()?;
            if secs.is_sign_negative() {
                return None;
            }
            Some((secs * 1000.0).round() as u64)
        }
        [mm, ss] => {
            let mins = mm.parse::<u64>().ok()?;
            let secs = ss.parse::<f64>().ok()?;
            Some(mins.saturating_mul(60_000) + (secs * 1000.0).round() as u64)
        }
        [hh, mm, ss] => {
            let hours = hh.parse::<u64>().ok()?;
            let mins = mm.parse::<u64>().ok()?;
            let secs = ss.parse::<f64>().ok()?;
            Some(
                hours
                    .saturating_mul(3_600_000)
                    .saturating_add(mins.saturating_mul(60_000))
                    .saturating_add((secs * 1000.0).round() as u64),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::{AssetId, BlendMode, TimeCode};
    use mondrian_effects::{get_or_compile_scheduled_effect_graph, EffectRenderPlan};
    use mondrian_timeline::clip::Clip;
    use mondrian_timeline::sequence::Sequence;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    struct FakeExecutor {
        calls: Arc<AtomicUsize>,
        delay_ms: u64,
    }

    impl ExportExecutor for FakeExecutor {
        fn execute(
            &self,
            _job: &RenderJob,
            cancel: &AtomicBool,
            report: &mut dyn FnMut(JobStatus, f32),
        ) -> JobExecutionResult {
            self.calls.fetch_add(1, Ordering::Relaxed);
            report(JobStatus::Encoding, 0.2);

            let step = 20u64;
            let mut elapsed = 0u64;
            while elapsed < self.delay_ms {
                if cancel.load(Ordering::Relaxed) {
                    return JobExecutionResult::Cancelled;
                }
                std::thread::sleep(Duration::from_millis(step));
                elapsed += step;
            }

            report(
                JobStatus::Rendering { frame: 1000, total_frames: 1000 },
                0.95,
            );
            JobExecutionResult::Completed
        }
    }

    fn dummy_config(output_name: &str) -> ExportConfig {
        ExportConfig {
            preset: crate::preset::ExportPreset::youtube_1080p(),
            input: ExportInput::File {
                input_path: PathBuf::from("dummy-input.mp4"),
                in_point: None,
                out_point: None,
            },
            output_path: PathBuf::from(output_name),
        }
    }

    fn wait_until(timeout_ms: u64, mut predicate: impl FnMut() -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < timeout_ms as u128 {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn queue_executes_jobs_and_marks_completed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = RenderQueue::new_with_executor(Arc::new(FakeExecutor {
            calls: Arc::clone(&calls),
            delay_ms: 100,
        }));

        let job_id = queue.enqueue(RenderJob::new(dummy_config("out-a.mp4")));

        let done = wait_until(2_000, || {
            queue
                .list_jobs()
                .iter()
                .find(|job| job.id == job_id)
                .map(|job| matches!(job.status, JobStatus::Completed))
                .unwrap_or(false)
        });

        assert!(done, "job should complete within timeout");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cancelling_pending_job_skips_execution() {
        let calls = Arc::new(AtomicUsize::new(0));
        let queue = RenderQueue::new_with_executor(Arc::new(FakeExecutor {
            calls: Arc::clone(&calls),
            delay_ms: 300,
        }));

        let first_id = queue.enqueue(RenderJob::new(dummy_config("out-first.mp4")));
        let second_id = queue.enqueue(RenderJob::new(dummy_config("out-second.mp4")));
        queue.cancel(second_id);

        let done = wait_until(3_000, || {
            let jobs = queue.list_jobs();
            let first_done = jobs
                .iter()
                .find(|job| job.id == first_id)
                .map(|job| matches!(job.status, JobStatus::Completed))
                .unwrap_or(false);
            let second_cancelled = jobs
                .iter()
                .find(|job| job.id == second_id)
                .map(|job| matches!(job.status, JobStatus::Cancelled))
                .unwrap_or(false);
            first_done && second_cancelled
        });

        assert!(done, "first should complete and second should cancel");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn timeline_render_range_respects_marked_in_out() {
        let mut seq = Sequence::new("range-test");
        let tb = seq.time_base();
        let clip = Clip::new(AssetId::new(), TimeCode::new(0, tb), TimeCode::new(200, tb));
        seq.video_tracks[0].add_clip(clip).expect("add clip");

        let timeline = TimelineExportInput {
            sequence: seq,
            asset_paths: HashMap::new(),
            in_point_frame: Some(40),
            out_point_frame: Some(99),
        };

        let range = compute_timeline_render_range(&timeline);
        assert_eq!(range.start_frame, 40);
        assert_eq!(range.total_frames, 60);
    }

    #[test]
    fn timeline_has_audio_content_detects_overlap() {
        let mut seq = Sequence::new("audio-range-test");
        let tb = seq.time_base();
        let asset_id = AssetId::new();
        let clip = Clip::new(asset_id, TimeCode::new(25, tb), TimeCode::new(20, tb));
        seq.audio_tracks[0].add_clip(clip).expect("add audio clip");

        let mut asset_paths = HashMap::new();
        asset_paths.insert(asset_id, PathBuf::from("dummy-audio.wav"));
        let timeline = TimelineExportInput {
            sequence: seq,
            asset_paths,
            in_point_frame: Some(30),
            out_point_frame: Some(40),
        };

        let range = compute_timeline_render_range(&timeline);
        assert!(timeline_has_audio_content(&timeline, range));
    }

    #[test]
    fn timeline_total_audio_samples_matches_frame_duration() {
        let range = TimelineRenderRange {
            start_frame: 0,
            total_frames: 50,
            fps_num: 25,
            fps_den: 1,
        };
        assert_eq!(timeline_total_audio_samples(range, 48_000), 96_000);
    }

    #[test]
    fn render_timeline_frame_into_clears_canvas_when_no_layers() {
        let seq = Sequence::new("empty");
        let timeline = TimelineExportInput {
            sequence: seq,
            asset_paths: HashMap::new(),
            in_point_frame: Some(0),
            out_point_frame: Some(10),
        };

        let mut canvas = vec![77u8; 4 * 2 * 4];
        render_timeline_frame_into(&timeline, 0, 4, 2, &mut canvas).expect("render should pass");

        for px in canvas.chunks_exact(4) {
            assert_eq!(px, &[0, 0, 0, 255]);
        }
    }

    #[test]
    fn shared_compositor_applies_media_effects_for_export() {
        let mut scratch = mondrian_renderer::TimelineCompositeScratch::default();
        let output = mondrian_renderer::composite_timeline_elements(
            1,
            1,
            &[mondrian_renderer::TimelineCompositeElement::Media(
                mondrian_renderer::TimelineMediaLayer {
                    rgba: &[120, 80, 40, 255],
                    width: 1,
                    height: 1,
                    opacity: 1.0,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                        ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                            exposure: 0.0,
                            contrast: 1.0,
                            saturation: 0.0,
                        }],
                    })
                    .expect("compile media effect graph"),
                    frame_seed: 0,
                },
            )],
            mondrian_renderer::TimelineCompositeOptions::default(),
            &mut scratch,
        );

        assert_eq!(output[0], output[1]);
        assert_eq!(output[1], output[2]);
        assert_eq!(output[3], 255);
    }

    #[test]
    fn shared_compositor_respects_adjustment_order_for_export() {
        let mut scratch = mondrian_renderer::TimelineCompositeScratch::default();
        let output = mondrian_renderer::composite_timeline_elements(
            2,
            1,
            &[
                mondrian_renderer::TimelineCompositeElement::Media(
                    mondrian_renderer::TimelineMediaLayer {
                        rgba: &[255, 0, 0, 255, 255, 0, 0, 255],
                        width: 2,
                        height: 1,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                        effect_graph: get_or_compile_scheduled_effect_graph(
                            &EffectRenderPlan::default(),
                        )
                        .expect("compile identity graph"),
                        frame_seed: 0,
                    },
                ),
                mondrian_renderer::TimelineCompositeElement::Adjustment(
                    mondrian_renderer::TimelineAdjustmentLayer {
                        effect_graph: get_or_compile_scheduled_effect_graph(&EffectRenderPlan {
                            ops: vec![mondrian_effects::EffectRenderOp::ColorAdjust {
                                exposure: 0.0,
                                contrast: 1.0,
                                saturation: 0.0,
                            }],
                        })
                        .expect("compile adjustment graph"),
                        opacity: 1.0,
                        blend_mode: Some(BlendMode::Normal),
                        frame_seed: 0,
                    },
                ),
                mondrian_renderer::TimelineCompositeElement::Media(
                    mondrian_renderer::TimelineMediaLayer {
                        rgba: &[0, 0, 0, 0, 0, 255, 0, 255],
                        width: 2,
                        height: 1,
                        opacity: 1.0,
                        blend_mode: BlendMode::Normal,
                        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                        effect_graph: get_or_compile_scheduled_effect_graph(
                            &EffectRenderPlan::default(),
                        )
                        .expect("compile identity graph"),
                        frame_seed: 0,
                    },
                ),
            ],
            mondrian_renderer::TimelineCompositeOptions::default(),
            &mut scratch,
        );

        assert_eq!(&output[0..4], &[54, 54, 54, 255]);
        assert_eq!(&output[4..8], &[0, 255, 0, 255]);
    }
}

#[cfg(test)]
#[path = "queue_perf_tests.rs"]
mod perf_tests;
