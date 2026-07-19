//! Export helpers: codec arguments, transactional process monitoring, and validation.
use super::*;
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, ExitStatus};
use std::time::Duration;

const FFMPEG_ERROR_TAIL_CAPACITY: usize = 64 * 1024;

pub(crate) struct FfmpegExit {
    pub(crate) status: ExitStatus,
    pub(crate) stderr_tail: String,
}

pub(crate) fn wait_for_ffmpeg_child(
    mut child: Child,
    cancellation: &ExecutionCancellationToken,
) -> Result<FfmpegExit, JobExecutionResult> {
    let Some(mut stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(JobExecutionResult::Failed(
            "ffmpeg stderr pipe is unavailable".to_owned(),
        ));
    };
    let stderr_reader = match std::thread::Builder::new()
        .name("mondrian-export-ffmpeg-stderr".to_owned())
        .spawn(move || {
            let mut tail = VecDeque::with_capacity(FFMPEG_ERROR_TAIL_CAPACITY);
            let mut buffer = [0_u8; 4_096];
            loop {
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        for byte in &buffer[..count] {
                            if tail.len() == FFMPEG_ERROR_TAIL_CAPACITY {
                                tail.pop_front();
                            }
                            tail.push_back(*byte);
                        }
                    }
                    Err(_) => break,
                }
            }
            String::from_utf8_lossy(&tail.into_iter().collect::<Vec<_>>()).into_owned()
        }) {
        Ok(handle) => handle,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(JobExecutionResult::Failed(format!(
                "failed to start bounded ffmpeg diagnostic reader: {error}"
            )));
        }
    };

    loop {
        if cancellation.is_canceled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stderr_reader.join();
            return Err(JobExecutionResult::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let stderr_tail = stderr_reader.join().unwrap_or_default();
                return Ok(FfmpegExit { status, stderr_tail });
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_reader.join();
                return Err(JobExecutionResult::Failed(format!(
                    "failed to observe ffmpeg process: {error}"
                )));
            }
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportYuvMatrix {
    Bt709,
    Fcc,
    Bt470Bg,
    Smpte170M,
    Smpte240M,
    Bt2020NonConstant,
}

impl ExportYuvMatrix {
    const fn scale_name(self) -> &'static str {
        match self {
            Self::Bt709 => "bt709",
            Self::Fcc => "fcc",
            Self::Bt470Bg => "bt470bg",
            Self::Smpte170M => "smpte170m",
            Self::Smpte240M => "smpte240m",
            Self::Bt2020NonConstant => "bt2020",
        }
    }

    const fn tag_name(self) -> &'static str {
        match self {
            Self::Bt709 => "bt709",
            Self::Fcc => "fcc",
            Self::Bt470Bg => "bt470bg",
            Self::Smpte170M => "smpte170m",
            Self::Smpte240M => "smpte240m",
            Self::Bt2020NonConstant => "bt2020nc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExportVideoSignalContract {
    pixel_format: &'static str,
    codec_range: Option<&'static str>,
    scale_range: Option<&'static str>,
    yuv_matrix: Option<ExportYuvMatrix>,
    color_space: ColorSpace,
}

impl ExportVideoSignalContract {
    fn resolve(
        settings: &SequenceSettings,
        codec: &VideoCodecConfig,
        alpha_mode: ExportAlphaMode,
    ) -> Self {
        let color_space = settings.color_management.output_color_space;
        if matches!(codec, VideoCodecConfig::Gif { .. }) {
            return Self {
                pixel_format: "rgb8",
                codec_range: None,
                scale_range: None,
                yuv_matrix: None,
                color_space,
            };
        }
        let pixel_format = match codec {
            VideoCodecConfig::ProRes { variant }
                if prores_variant_is_4444(variant) && alpha_mode == ExportAlphaMode::Preserve =>
            {
                "yuva444p12le"
            }
            VideoCodecConfig::ProRes { variant } if prores_variant_is_4444(variant) => {
                "yuv444p12le"
            }
            VideoCodecConfig::ProRes { .. } => "yuv422p10le",
            _ => match settings.color_management.delivery_bit_depth {
                DeliveryBitDepth::Eight => "yuv420p",
                DeliveryBitDepth::Ten => "yuv420p10le",
                DeliveryBitDepth::Twelve => "yuv420p12le",
            },
        };
        let (codec_range, scale_range) = match settings.color_management.video_range {
            VideoRange::Full => ("pc", "full"),
            VideoRange::Legal => ("tv", "limited"),
        };
        let encoding = color_space.encoding();
        let yuv_matrix = match encoding.matrix {
            mondrian_core::ColorMatrixCoefficients::Bt2020NonConstant => {
                ExportYuvMatrix::Bt2020NonConstant
            }
            mondrian_core::ColorMatrixCoefficients::Bt470Bg => ExportYuvMatrix::Bt470Bg,
            mondrian_core::ColorMatrixCoefficients::Smpte170M => ExportYuvMatrix::Smpte170M,
            mondrian_core::ColorMatrixCoefficients::Fcc => ExportYuvMatrix::Fcc,
            mondrian_core::ColorMatrixCoefficients::Smpte240M => ExportYuvMatrix::Smpte240M,
            mondrian_core::ColorMatrixCoefficients::Unspecified
                if encoding.primaries == mondrian_core::ColorPrimaries::Bt2020 =>
            {
                ExportYuvMatrix::Bt2020NonConstant
            }
            mondrian_core::ColorMatrixCoefficients::Bt709
            | mondrian_core::ColorMatrixCoefficients::Rgb
            | mondrian_core::ColorMatrixCoefficients::Unspecified => ExportYuvMatrix::Bt709,
        };
        Self {
            pixel_format,
            codec_range: Some(codec_range),
            scale_range: Some(scale_range),
            yuv_matrix: Some(yuv_matrix),
            color_space,
        }
    }

    fn validation_constraints(self) -> crate::validator::ExpectedVideoSignalConstraints {
        let tags = self.color_space.ffmpeg_tags();
        crate::validator::ExpectedVideoSignalConstraints {
            pixel_format: Some(self.pixel_format.to_owned()),
            color_range: self.codec_range.map(str::to_owned),
            color_primaries: tags.map(|tags| tags.color_primaries.to_owned()),
            color_transfer: tags.map(|tags| tags.color_trc.to_owned()),
            color_matrix: self.yuv_matrix.map(|matrix| matrix.tag_name().to_owned()),
            require_color_tags_absent: tags.is_none(),
            static_hdr_metadata: None,
        }
    }
}

pub(crate) fn expected_export_video_signal(
    settings: &SequenceSettings,
    codec: &VideoCodecConfig,
    alpha_mode: ExportAlphaMode,
) -> Result<crate::validator::ExpectedVideoSignalConstraints, String> {
    let mut constraints =
        ExportVideoSignalContract::resolve(settings, codec, alpha_mode).validation_constraints();
    if settings.color_management.static_hdr_metadata_policy.writes_authored_metadata() {
        let mastering_display = settings
            .color_management
            .hdr_mastering_display
            .as_ref()
            .ok_or_else(|| "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string())?
            .clone();
        mastering_display
            .validate()
            .map_err(|error| format!("SMPTE ST 2086 母版显示元数据无效: {error}"))?;
        let content_light = settings.color_management.hdr_content_light.ok_or_else(|| {
            "写入静态 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据".to_string()
        })?;
        content_light
            .validate()
            .map_err(|error| format!("MaxCLL/MaxFALL 内容光级别元数据无效: {error}"))?;
        constraints.static_hdr_metadata =
            Some(crate::validator::ExpectedStaticHdrMetadataConstraints {
                mastering_display,
                content_light,
            });
    }
    Ok(constraints)
}

pub(crate) fn apply_export_video_signal_args(
    cmd: &mut Command,
    settings: &SequenceSettings,
    codec: &VideoCodecConfig,
    alpha_mode: ExportAlphaMode,
) {
    let contract = ExportVideoSignalContract::resolve(settings, codec, alpha_mode);
    if let (Some(range), Some(matrix)) = (contract.scale_range, contract.yuv_matrix) {
        cmd.arg("-vf").arg(format!(
            "scale=iw:ih:in_range=full:out_range={range}:out_color_matrix={}",
            matrix.scale_name()
        ));
    }
    cmd.arg("-pix_fmt").arg(contract.pixel_format);
    if let Some(range) = contract.codec_range {
        cmd.arg("-color_range").arg(range);
    }
    if let (Some(tags), Some(matrix)) = (contract.color_space.ffmpeg_tags(), contract.yuv_matrix) {
        cmd.arg("-color_primaries")
            .arg(tags.color_primaries)
            .arg("-color_trc")
            .arg(tags.color_trc)
            .arg("-colorspace")
            .arg(matrix.tag_name());
    }
}

/// Write authored static HDR metadata through the libx265 encoder contract.
///
/// Validation must reject other encoders before this boundary is reached.
pub(crate) fn apply_h265_hdr_metadata_args(
    cmd: &mut Command,
    settings: &SequenceSettings,
) -> Result<(), String> {
    cmd.arg("-x265-params").arg(h265_hdr_metadata_params(settings)?);
    Ok(())
}

fn h265_hdr_metadata_params(settings: &SequenceSettings) -> Result<String, String> {
    let cm = &settings.color_management;
    let mastering_metadata = cm
        .hdr_mastering_display
        .as_ref()
        .ok_or_else(|| "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string())?;
    mastering_metadata
        .validate()
        .map_err(|error| format!("SMPTE ST 2086 母版显示元数据无效: {error}"))?;
    let mastering = mastering_metadata
        .to_x265_master_display()
        .ok_or_else(|| "SMPTE ST 2086 母版显示元数据不完整".to_string())?;
    let content_light = cm
        .hdr_content_light
        .ok_or_else(|| "写入静态 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据".to_string())?;
    content_light
        .validate()
        .map_err(|error| format!("MaxCLL/MaxFALL 内容光级别元数据无效: {error}"))?;
    let cll = content_light.to_x265_max_cll();
    Ok(format!("master-display={mastering}:max-cll={cll}"))
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
    timeline: &TimelineExportSnapshot,
) -> Result<(), String> {
    let settings = &timeline.sequence.settings;
    let output = settings.color_management.output_color_space;
    let output_encoding = output.encoding();
    let bit_depth = settings.color_management.delivery_bit_depth;
    let write_static_hdr =
        settings.color_management.static_hdr_metadata_policy.writes_authored_metadata();

    if config.preset.alpha_mode == ExportAlphaMode::Preserve
        && !matches!(
            (&config.preset.container, &config.preset.video),
            (Container::Mov, VideoCodecConfig::ProRes { variant })
                if prores_variant_is_4444(variant)
        )
    {
        return Err(
            "保留 Alpha 当前仅支持 MOV + ProRes 4444/4444 XQ；请选择专用 RGB+Alpha 交付预设"
                .to_string(),
        );
    }

    if output_encoding.is_scene_log() {
        if bit_depth == DeliveryBitDepth::Eight {
            return Err("Camera log 输出需要 10-bit 或更高位深".to_string());
        }
        match (&config.preset.container, &config.preset.video) {
            (Container::Mov | Container::Mxf, VideoCodecConfig::ProRes { .. }) => {}
            _ => {
                return Err("Camera log 输出仅支持 MOV/MXF + ProRes 专业中间格式".to_string());
            }
        }
    }

    if output.is_hdr() && bit_depth == DeliveryBitDepth::Eight {
        return Err("HDR 输出不能使用 8-bit 导出位深".to_string());
    }
    if write_static_hdr && !output.is_hdr() {
        return Err("只有 HDR 输出色彩空间可以写入静态 HDR metadata".to_string());
    }
    if write_static_hdr && bit_depth == DeliveryBitDepth::Eight {
        return Err("写入静态 HDR metadata 需要 10-bit 或更高位深".to_string());
    }
    if write_static_hdr {
        let mastering =
            settings.color_management.hdr_mastering_display.as_ref().ok_or_else(|| {
                "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string()
            })?;
        mastering
            .validate()
            .map_err(|error| format!("HDR mastering metadata 无效: {error}"))?;
        let content_light = settings.color_management.hdr_content_light.ok_or_else(|| {
            "写入静态 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据".to_string()
        })?;
        content_light
            .validate()
            .map_err(|error| format!("HDR content-light metadata 无效: {error}"))?;

        let engine = if settings.color_management.inherit {
            &timeline.project_color_management.engine
        } else {
            &settings.color_management.engine
        };
        if let mondrian_core::ColorEngine::MondrianStandard { package } = engine {
            let target = mondrian_core::mondrian_standard_output_target_contract_for_package(
                *package, output,
            )?;
            if content_light.max_content_light_level > target.nominal_peak_nits {
                return Err(format!(
                    "Mondrian Standard {:?} View 峰值为 {} nit，但 MaxCLL 声明 {} nit",
                    output, target.nominal_peak_nits, content_light.max_content_light_level
                ));
            }
        }

        let source_issues = export_asset_issue_summary(timeline);
        if source_issues.diagnostics_with_dynamic_hdr10_plus > 0
            || source_issues.diagnostics_with_dolby_vision_config > 0
        {
            return Err(format!(
                "当前 HDR metadata 后端只写入项目级 ST 2086/MaxCLL/MaxFALL；引用素材包含 HDR10+ 动态 metadata（{} 个）或 Dolby Vision 配置（{} 个），渲染后不能安全透传，请使用经过验证的动态 HDR 重新制作流程",
                source_issues.diagnostics_with_dynamic_hdr10_plus,
                source_issues.diagnostics_with_dolby_vision_config
            ));
        }
    }
    if write_static_hdr && !matches!(&config.preset.video, VideoCodecConfig::H265 { .. }) {
        return Err(
            "HDR metadata 写入当前仅由 H.265/libx265 编码后端支持；AV1/ProRes 尚无已验证的 metadata backend"
                .to_string(),
        );
    }

    match &config.preset.video {
        VideoCodecConfig::ProRes { variant }
            if prores_variant_is_4444(variant) && bit_depth != DeliveryBitDepth::Twelve =>
        {
            return Err("ProRes 4444/4444 XQ 交付必须声明 12-bit 位深".to_string());
        }
        VideoCodecConfig::ProRes { variant }
            if !prores_variant_is_4444(variant) && bit_depth != DeliveryBitDepth::Ten =>
        {
            return Err("ProRes Proxy/LT/Standard/HQ 交付必须声明 10-bit 位深".to_string());
        }
        VideoCodecConfig::H264 { .. }
        | VideoCodecConfig::H265 { .. }
        | VideoCodecConfig::Av1 { .. }
            if bit_depth == DeliveryBitDepth::Twelve =>
        {
            return Err("H.264/H.265/AV1 当前仅支持 8-bit 或 10-bit 交付".to_string());
        }
        VideoCodecConfig::Gif { .. } => {}
        _ => {}
    }

    match (&config.preset.container, &config.preset.video) {
        (Container::Gif, _) | (_, VideoCodecConfig::Gif { .. }) => {
            if output.is_hdr() || write_static_hdr || bit_depth != DeliveryBitDepth::Eight {
                return Err("GIF 导出仅支持 8-bit SDR 输出".to_string());
            }
            if output != ColorSpace::Srgb {
                return Err("GIF 不携带可靠色彩标签，仅允许显式 sRGB 输出".to_string());
            }
        }
        (Container::Webm, VideoCodecConfig::H264 { .. } | VideoCodecConfig::H265 { .. }) => {
            return Err("WebM 容器不支持 H.264/H.265 视频编码".to_string());
        }
        (Container::Mp4, VideoCodecConfig::ProRes { .. }) => {
            return Err("ProRes 应使用 MOV/MXF 等专业容器导出".to_string());
        }
        (_, VideoCodecConfig::H264 { .. }) if output.is_hdr() || write_static_hdr => {
            return Err("HDR 输出建议使用 H.265、AV1 或 ProRes，当前 H.264 配置已拒绝".to_string());
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

fn prores_variant_is_4444(variant: &str) -> bool {
    matches!(variant.to_ascii_lowercase().as_str(), "4444" | "4444xq")
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

#[cfg(test)]
mod hdr_metadata_tests {
    use super::*;
    use mondrian_core::{VideoContentLightMetadata, VideoMasteringDisplayMetadata};

    #[test]
    fn h265_hdr_metadata_is_one_atomic_encoder_parameter() {
        let mut settings = SequenceSettings::default();
        settings.color_management.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        settings.color_management.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let mut command = Command::new("ffmpeg");

        apply_h265_hdr_metadata_args(&mut command, &settings).expect("valid static HDR metadata");

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-x265-params");
        assert!(args[1].starts_with("master-display="));
        assert!(args[1].contains(":max-cll="));
    }
}
