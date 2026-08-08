//! Export helpers: codec arguments, transactional process monitoring, and validation.
use super::*;

pub(crate) fn apply_video_codec_args(cmd: &mut Command, codec: &VideoCodecConfig) {
    match codec {
        VideoCodecConfig::H264 { profile, rate_control } => {
            cmd.arg("-c:v")
                .arg("libx264")
                .arg("-preset")
                .arg("medium")
                .arg("-profile:v")
                .arg(match profile {
                    crate::preset::H264Profile::High => "high",
                });
            apply_video_rate_control_args(cmd, *rate_control);
        }
        VideoCodecConfig::Hevc { profile, rate_control } => {
            cmd.arg("-c:v")
                .arg("libx265")
                .arg("-preset")
                .arg("medium")
                .arg("-profile:v")
                .arg(match profile {
                    crate::preset::HevcProfile::Main => "main",
                    crate::preset::HevcProfile::Main10 => "main10",
                });
            apply_video_rate_control_args(cmd, *rate_control);
        }
        VideoCodecConfig::Av1 { profile, rate_control } => {
            cmd.arg("-c:v")
                .arg("libaom-av1")
                .arg("-profile:v")
                .arg(match profile {
                    crate::preset::Av1Profile::Main => "0",
                })
                .arg("-b:v")
                .arg("0");
            apply_video_rate_control_args(cmd, *rate_control);
        }
        VideoCodecConfig::ProRes { profile } => {
            cmd.arg("-c:v")
                .arg("prores_ks")
                .arg("-profile:v")
                .arg(prores_profile_variant(*profile));
        }
        VideoCodecConfig::Gif { .. } => {
            cmd.arg("-c:v").arg("gif");
        }
    }
}

fn apply_video_rate_control_args(cmd: &mut Command, rate_control: crate::preset::VideoRateControl) {
    cmd.arg("-crf").arg(rate_control.crf.to_string());
    if let (Some(max_bitrate), Some(buffer_size)) = (
        rate_control.max_bitrate_kbps,
        rate_control.buffer_size_kbits,
    ) {
        cmd.arg("-maxrate")
            .arg(format!("{max_bitrate}k"))
            .arg("-bufsize")
            .arg(format!("{buffer_size}k"));
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
    fn resolve(_settings: &SequenceSettings, delivery: &ResolvedExportDeliveryContract) -> Self {
        let color_space = delivery.color_target.color_space;
        if delivery.chroma_sampling == crate::preset::ExportChromaSampling::Rgb {
            return Self {
                pixel_format: delivery.pixel_format,
                codec_range: None,
                scale_range: None,
                yuv_matrix: None,
                color_space,
            };
        }
        let (codec_range, scale_range) = match delivery.video_range {
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
            pixel_format: delivery.pixel_format,
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
            static_hdr_metadata: crate::validator::ExpectedStaticHdrMetadata::Absent,
        }
    }
}

/// Resolve the exact encoded color tags, range, pixel representation, and
/// static-HDR metadata expected from one admitted delivery.
///
/// Execution and acceptance Adapters must consume this shared expectation
/// instead of reconstructing FFmpeg color-tag policy.
pub fn expected_export_video_signal(
    settings: &SequenceSettings,
    delivery: &ResolvedExportDeliveryContract,
) -> Result<crate::validator::ExpectedVideoSignalConstraints, String> {
    let mut constraints =
        ExportVideoSignalContract::resolve(settings, delivery).validation_constraints();
    if settings.delivery.static_hdr_metadata_policy.writes_authored_metadata() {
        let mastering_display = settings
            .delivery
            .hdr_mastering_display
            .as_ref()
            .ok_or_else(|| "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string())?
            .clone();
        mastering_display
            .validate()
            .map_err(|error| format!("SMPTE ST 2086 母版显示元数据无效: {error}"))?;
        let content_light = settings.delivery.hdr_content_light.ok_or_else(|| {
            "写入静态 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据".to_string()
        })?;
        content_light
            .validate()
            .map_err(|error| format!("MaxCLL/MaxFALL 内容光级别元数据无效: {error}"))?;
        constraints.static_hdr_metadata = crate::validator::ExpectedStaticHdrMetadata::Exact(
            crate::validator::ExpectedStaticHdrMetadataConstraints {
                mastering_display,
                content_light,
            },
        );
    }
    Ok(constraints)
}

pub(crate) fn apply_export_video_signal_args(
    cmd: &mut Command,
    settings: &SequenceSettings,
    delivery: &ResolvedExportDeliveryContract,
) {
    let contract = ExportVideoSignalContract::resolve(settings, delivery);
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

/// Emit encoder-native color signaling for the H.264/H.265 VUI.
///
/// FFmpeg 8 no longer maps the container-level `-color_primaries` and
/// `-color_trc` options into the x264/x265 bitstream, which would otherwise
/// ship delivery files without primaries or transfer tags. The encoder-native
/// `colorprim`/`transfer`/`colormatrix` keys are the contract that reaches
/// the bitstream on every supported FFmpeg; the container-level flags remain
/// alongside for non-VUI encoders and for the MP4 `colr` box.
pub(crate) fn apply_encoder_signal_params(
    cmd: &mut Command,
    codec: &VideoCodecConfig,
    settings: &SequenceSettings,
    delivery: &ResolvedExportDeliveryContract,
) -> Result<(), String> {
    let contract = ExportVideoSignalContract::resolve(settings, delivery);
    let Some(tags) = contract.color_space.ffmpeg_tags() else {
        return Ok(());
    };
    let Some(matrix) = contract.yuv_matrix else {
        return Ok(());
    };
    let vui = format!(
        "colorprim={}:transfer={}:colormatrix={}",
        tags.color_primaries,
        tags.color_trc,
        matrix.tag_name()
    );
    match codec {
        VideoCodecConfig::H264 { .. } => {
            cmd.arg("-x264-params").arg(vui);
        }
        VideoCodecConfig::Hevc { .. } => {
            let mut params = vui;
            if settings.delivery.static_hdr_metadata_policy.writes_authored_metadata() {
                params.push(':');
                params.push_str(&h265_hdr_metadata_params(settings)?);
            }
            cmd.arg("-x265-params").arg(params);
        }
        VideoCodecConfig::Av1 { .. }
        | VideoCodecConfig::ProRes { .. }
        | VideoCodecConfig::Gif { .. } => {}
    }
    Ok(())
}

fn h265_hdr_metadata_params(settings: &SequenceSettings) -> Result<String, String> {
    let delivery = &settings.delivery;
    let mastering_metadata = delivery
        .hdr_mastering_display
        .as_ref()
        .ok_or_else(|| "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据".to_string())?;
    mastering_metadata
        .validate()
        .map_err(|error| format!("SMPTE ST 2086 母版显示元数据无效: {error}"))?;
    let mastering = mastering_metadata
        .to_x265_master_display()
        .ok_or_else(|| "SMPTE ST 2086 母版显示元数据不完整".to_string())?;
    let content_light = delivery
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
        AudioCodecConfig::Disabled => {}
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

pub(crate) fn resolve_timeline_export_delivery(
    config: &ExportConfig,
    timeline: &TimelineExportSnapshot,
) -> Result<ResolvedExportDeliveryContract, String> {
    let delivery = crate::delivery::resolve_export_delivery(
        &config.preset,
        &timeline.sequence.settings,
        &timeline.color_environment,
    )
    .map_err(|error| error.to_string())?;
    let source_issues = export_media_diagnostic_set(timeline)?.issue_summary;
    validate_timeline_dynamic_hdr_delivery(timeline, source_issues)?;
    Ok(delivery)
}

pub(crate) fn validate_timeline_dynamic_hdr_delivery(
    timeline: &TimelineExportSnapshot,
    source_issues: VideoColorDiagnosticIssueAggregate,
) -> Result<(), String> {
    let write_static_hdr = timeline
        .sequence
        .settings
        .delivery
        .static_hdr_metadata_policy
        .writes_authored_metadata();
    if write_static_hdr
        && (source_issues.diagnostics_with_dynamic_hdr10_plus > 0
            || source_issues.diagnostics_with_dolby_vision_config > 0)
    {
        return Err(format!(
            "当前 HDR metadata 后端只写入项目级 ST 2086/MaxCLL/MaxFALL；引用素材包含 HDR10+ 动态 metadata（{} 个）或 Dolby Vision 配置（{} 个），渲染后不能安全透传，请使用经过验证的动态 HDR 重新制作流程",
            source_issues.diagnostics_with_dynamic_hdr10_plus,
            source_issues.diagnostics_with_dolby_vision_config
        ));
    }
    Ok(())
}

pub(crate) const fn prores_profile_variant(profile: crate::preset::ProResProfile) -> &'static str {
    match profile {
        crate::preset::ProResProfile::Proxy => "0",
        crate::preset::ProResProfile::Lt => "1",
        crate::preset::ProResProfile::Standard => "2",
        crate::preset::ProResProfile::Hq => "3",
        crate::preset::ProResProfile::FourFourFourFour => "4",
        crate::preset::ProResProfile::FourFourFourFourXq => "5",
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
