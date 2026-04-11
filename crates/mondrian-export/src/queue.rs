//! 后台渲染队列

use crate::preset::{
    AudioCodecConfig, Container, ExportConfig, ExportInput, TimelineExportInput, VideoCodecConfig,
};
use chrono::{DateTime, Utc};
use mondrian_core::types::{JobId, TimeCode};
use mondrian_media::decode_video_frame_at_time_rgba_scaled;
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
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

    monitor_ffmpeg_child(child, duration_ms, cancel, report)
}

fn execute_timeline_export(
    job: &RenderJob,
    timeline: &TimelineExportInput,
    cancel: &AtomicBool,
    report: &mut dyn FnMut(JobStatus, f32),
) -> JobExecutionResult {
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

    let (width, height) = timeline_output_resolution(job, timeline);
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

    let include_audio = !matches!(job.config.preset.container, Container::Gif);
    if include_audio {
        let sample_rate = timeline.sequence.settings.audio_sample_rate.max(8_000);
        cmd.arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg(format!(
                "anullsrc=channel_layout=stereo:sample_rate={sample_rate}"
            ))
            .arg("-map")
            .arg("0:v:0")
            .arg("-map")
            .arg("1:a:0")
            .arg("-shortest");
    } else {
        cmd.arg("-an");
    }

    apply_video_codec_args(&mut cmd, &job.config.preset.video);
    if include_audio {
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
        Ok(output) if output.status.success() => JobExecutionResult::Completed,
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
    let total = range.total_frames.max(1);

    for index in 0..total {
        if cancel.load(Ordering::Relaxed) {
            return JobExecutionResult::Cancelled;
        }

        let timeline_frame = range.start_frame + index as i64;
        let rgba = match render_timeline_frame(timeline, timeline_frame, width, height) {
            Ok(frame) => frame,
            Err(err) => {
                return JobExecutionResult::Failed(format!(
                    "渲染时间线帧失败（frame={}）: {}",
                    timeline_frame, err
                ));
            }
        };

        if let Err(err) = writer.write_all(&rgba) {
            return JobExecutionResult::Failed(format!("写入编码管道失败: {}", err));
        }

        let rendered = index + 1;
        let ratio = rendered as f32 / total as f32;
        let progress = (0.03 + 0.9 * ratio).clamp(0.03, 0.95);
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

fn render_timeline_frame(
    timeline: &TimelineExportInput,
    timeline_frame: i64,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut canvas = vec![0u8; width as usize * height as usize * 4];
    initialize_canvas_alpha_opaque(&mut canvas);

    let timecode = TimeCode::new(timeline_frame.max(0), timeline.sequence.time_base());
    let active_clips = timeline.sequence.active_clips_at(timecode);

    for active in active_clips {
        let Some(path) = timeline.asset_paths.get(&active.clip.asset_id) else {
            continue;
        };
        let opacity = active.opacity.clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }

        let decoded = decode_video_frame_at_time_rgba_scaled(
            path.as_path(),
            active.source_time.to_secs().max(0.0),
            Some(width),
            Some(height),
        )
        .map_err(|err| {
            format!(
                "asset={} path={} err={}",
                active.clip.asset_id,
                path.display(),
                err
            )
        })?;

        blend_rgba_layer_centered(
            &mut canvas,
            width,
            height,
            &decoded.data,
            decoded.width,
            decoded.height,
            opacity,
        );
    }

    Ok(canvas)
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

fn initialize_canvas_alpha_opaque(canvas: &mut [u8]) {
    for px in canvas.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

fn blend_rgba_layer_centered(
    dst_rgba: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src_rgba: &[u8],
    src_w: u32,
    src_h: u32,
    opacity: f32,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }

    let copy_w = dst_w.min(src_w) as usize;
    let copy_h = dst_h.min(src_h) as usize;
    if copy_w == 0 || copy_h == 0 {
        return;
    }

    let dst_x = ((dst_w as i64 - copy_w as i64) / 2).max(0) as usize;
    let dst_y = ((dst_h as i64 - copy_h as i64) / 2).max(0) as usize;
    let src_x = ((src_w as i64 - copy_w as i64) / 2).max(0) as usize;
    let src_y = ((src_h as i64 - copy_h as i64) / 2).max(0) as usize;

    let dst_stride = dst_w as usize * 4;
    let src_stride = src_w as usize * 4;
    let opacity_scale = opacity.clamp(0.0, 1.0);
    if opacity_scale <= 0.0 {
        return;
    }

    for row in 0..copy_h {
        let dst_row_offset = (dst_y + row) * dst_stride + dst_x * 4;
        let src_row_offset = (src_y + row) * src_stride + src_x * 4;
        let dst_row = &mut dst_rgba[dst_row_offset..dst_row_offset + copy_w * 4];
        let src_row = &src_rgba[src_row_offset..src_row_offset + copy_w * 4];

        for (dst_px, src_px) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
            let src_alpha = src_px[3] as f32 / 255.0;
            let alpha = (src_alpha * opacity_scale * 255.0).round().clamp(0.0, 255.0) as u32;
            if alpha == 0 {
                continue;
            }
            if alpha >= 255 {
                dst_px[0] = src_px[0];
                dst_px[1] = src_px[1];
                dst_px[2] = src_px[2];
                dst_px[3] = 255;
                continue;
            }

            let inv_alpha = 255 - alpha;
            dst_px[0] = blend_channel_u8(src_px[0] as u32, dst_px[0] as u32, alpha, inv_alpha);
            dst_px[1] = blend_channel_u8(src_px[1] as u32, dst_px[1] as u32, alpha, inv_alpha);
            dst_px[2] = blend_channel_u8(src_px[2] as u32, dst_px[2] as u32, alpha, inv_alpha);
            dst_px[3] = 255;
        }
    }
}

#[inline]
fn blend_channel_u8(src: u32, dst: u32, alpha: u32, inv_alpha: u32) -> u8 {
    let value = src * alpha + dst * inv_alpha + 127;
    ((value + (value >> 8)) >> 8) as u8
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
    use mondrian_core::types::{AssetId, TimeCode};
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
    fn blend_rgba_layer_centered_places_layer_in_canvas_center() {
        let mut dst = vec![0u8; 4 * 4 * 4];
        initialize_canvas_alpha_opaque(&mut dst);
        let src = vec![
            10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
        ];

        blend_rgba_layer_centered(&mut dst, 4, 4, &src, 2, 2, 1.0);

        let pixel_at = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * 4 + x) * 4;
            [dst[i], dst[i + 1], dst[i + 2], dst[i + 3]]
        };

        assert_eq!(pixel_at(1, 1), [10, 20, 30, 255]);
        assert_eq!(pixel_at(2, 1), [10, 20, 30, 255]);
        assert_eq!(pixel_at(1, 2), [10, 20, 30, 255]);
        assert_eq!(pixel_at(2, 2), [10, 20, 30, 255]);
        assert_eq!(pixel_at(0, 0), [0, 0, 0, 255]);
    }
}
