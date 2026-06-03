//! Export helpers: filter building, codec args, validation, probing.
use super::*;

pub(crate) fn update_job_terminal_state(
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

pub(crate) fn is_terminal(status: &JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled
    )
}

pub(crate) fn monitor_ffmpeg_child(
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

pub(crate) fn build_video_filter(config: &ExportConfig) -> Option<String> {
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

pub(crate) fn apply_video_codec_args(cmd: &mut Command, codec: &VideoCodecConfig) {
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

pub(crate) fn apply_color_tag_args(cmd: &mut Command, color_space: ColorSpace) {
    let tags = color_space.ffmpeg_tags();
    cmd.arg("-color_primaries")
        .arg(tags.color_primaries)
        .arg("-color_trc")
        .arg(tags.color_trc)
        .arg("-colorspace")
        .arg(tags.colorspace);
}

/// 写入 HDR10 元数据（母版显示色彩体积 + 内容光级别）。
///
/// 当序列设置中 `preserve_hdr_metadata` 为 true 且输出为 HDR 色彩空间时调用。
pub(crate) fn apply_hdr_metadata_args(cmd: &mut Command, settings: &SequenceSettings) {
    let cm = &settings.color_management;
    let mastering = cm
        .hdr_mastering_display
        .as_deref()
        .unwrap_or("G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1)");
    let cll = cm.hdr_max_cll.as_deref().unwrap_or("1000,400");
    cmd.arg("-x265-params").arg(format!("master-display={mastering}"));
    cmd.arg("-x265-params").arg(format!("max-cll={cll}"));
}

pub(crate) fn apply_sequence_video_format_args(cmd: &mut Command, settings: &SequenceSettings) {
    let pix_fmt = match settings.color_management.export_bit_depth {
        ExportBitDepth::Eight => "yuv420p",
        ExportBitDepth::Ten => "yuv420p10le",
        ExportBitDepth::SixteenFloat => "yuv444p10le",
    };
    let range = match settings.color_management.video_range {
        VideoRange::Full => "pc",
        VideoRange::Legal => "tv",
    };
    cmd.arg("-pix_fmt").arg(pix_fmt).arg("-color_range").arg(range);
}

pub(crate) fn apply_audio_codec_args(cmd: &mut Command, codec: &AudioCodecConfig) {
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

pub(crate) fn validate_timeline_export_color_compatibility(
    config: &ExportConfig,
    timeline: &TimelineExportInput,
) -> Result<(), String> {
    let settings = &timeline.sequence.settings;
    let output = settings.color_management.output_color_space;
    let bit_depth = settings.color_management.export_bit_depth;
    let preserve_hdr = settings.color_management.preserve_hdr_metadata;

    if output.is_hdr() && bit_depth == ExportBitDepth::Eight {
        return Err("HDR 输出不能使用 8-bit 导出位深".to_string());
    }
    if preserve_hdr && !output.is_hdr() {
        return Err("只有 HDR 输出色彩空间可以保留 HDR metadata".to_string());
    }
    if preserve_hdr && bit_depth == ExportBitDepth::Eight {
        return Err("保留 HDR metadata 需要 10-bit 或更高位深".to_string());
    }

    match (&config.preset.container, &config.preset.video) {
        (Container::Gif, _) | (_, VideoCodecConfig::Gif { .. }) => {
            if output.is_hdr() || preserve_hdr || bit_depth != ExportBitDepth::Eight {
                return Err("GIF 导出仅支持 8-bit SDR 输出".to_string());
            }
        }
        (Container::Webm, VideoCodecConfig::H264 { .. } | VideoCodecConfig::H265 { .. }) => {
            return Err("WebM 容器不支持 H.264/H.265 视频编码".to_string());
        }
        (Container::Mp4, VideoCodecConfig::ProRes { .. }) => {
            return Err("ProRes 应使用 MOV/MXF 等专业容器导出".to_string());
        }
        (_, VideoCodecConfig::H264 { .. }) if output.is_hdr() || preserve_hdr => {
            return Err("HDR 输出建议使用 H.265、AV1 或 ProRes，当前 H.264 配置已拒绝".to_string());
        }
        (_, VideoCodecConfig::H264 { .. }) if bit_depth == ExportBitDepth::SixteenFloat => {
            return Err("H.264 不支持 16-bit float 导出位深".to_string());
        }
        (_, VideoCodecConfig::H265 { .. } | VideoCodecConfig::Av1 { .. })
            if bit_depth == ExportBitDepth::SixteenFloat =>
        {
            return Err("H.265/AV1 应使用 8-bit 或 10-bit YUV 导出位深".to_string());
        }
        _ => {}
    }

    Ok(())
}

pub(crate) fn prores_profile_variant(variant: &str) -> &'static str {
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

pub(crate) fn container_format(container: &Container) -> &'static str {
    match container {
        Container::Mp4 => "mp4",
        Container::Mov => "mov",
        Container::Mkv => "matroska",
        Container::Gif => "gif",
        Container::Mxf => "mxf",
        Container::Webm => "webm",
    }
}

pub(crate) fn parse_progress_time_us(line: &str) -> Option<u64> {
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

pub(crate) fn probe_duration_ms(
    path: &Path,
    in_point: Option<&str>,
    out_point: Option<&str>,
) -> Option<u64> {
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

pub(crate) fn parse_time_spec_millis(raw: &str) -> Option<u64> {
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
