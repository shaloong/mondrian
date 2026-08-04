//! Pure export-delivery resolution and admission.
//!
//! This Module is the single policy boundary between Sequence-owned creative
//! output intent and preset-owned encoded representation. UI frontends may
//! call it for early feedback; the queue calls it again against the immutable
//! Timeline Export Snapshot before admitting work.

use crate::preset::{
    AudioCodecConfig, Av1Profile, Container, ExportAlphaMode, ExportChromaSampling,
    ExportColorTarget, ExportPreset, HevcProfile, ProResProfile, Resolution, VideoCodecConfig,
    VideoRateControl,
};
use mondrian_core::{
    AudioChannelLayout, ColorEngine, ColorSpace, OutputTransformIntent, ProjectColorEnvironment,
};
use mondrian_timeline::sequence::{
    DeliveryBitDepth, SequenceSettings, StaticHdrMetadataPolicy, VideoRange,
};

/// Stable category for one rejected delivery contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportDeliveryIssueCode {
    /// Output dimensions cannot be represented by the selected signal format.
    InvalidResolution,
    /// Rate-control values are outside the verified encoder domain.
    InvalidRateControl,
    /// The selected container cannot carry the selected essence.
    UnsupportedContainerCodec,
    /// The selected audio codec cannot be carried by the container.
    UnsupportedContainerAudio,
    /// Codec profile, sample depth, and chroma choices disagree.
    IncompatibleVideoSignal,
    /// Alpha was requested from a codec or container that cannot preserve it.
    UnsupportedAlpha,
    /// The color output cannot be represented safely by this delivery.
    IncompatibleColorOutput,
    /// Authored HDR metadata cannot be lowered by this encoder contract.
    UnsupportedHdrMetadata,
    /// One codec-specific authoring value is invalid.
    InvalidCodecParameter,
    /// One audio-specific authoring value is invalid.
    InvalidAudioParameter,
    /// The exact Sequence layout cannot be lowered by the selected encoder.
    UnsupportedAudioLayout,
}

/// Structured delivery-admission failure shared by product UI and execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportDeliveryError {
    /// Stable machine-readable issue category.
    pub code: ExportDeliveryIssueCode,
    /// Specific user-facing explanation.
    pub detail: String,
}

impl ExportDeliveryError {
    fn new(code: ExportDeliveryIssueCode, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
}

impl std::fmt::Display for ExportDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ExportDeliveryError {}

/// Fully explicit encoded representation produced by export admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExportColorTarget {
    /// Exact encoded color identity written by the output boundary.
    pub color_space: ColorSpace,
    /// Whether the output boundary performs a rendering/tone-mapping View.
    pub tone_map: bool,
    /// Exact Project-engine transform intent consumed by the renderer.
    pub output_transform: OutputTransformIntent,
}

/// Fully explicit export target admitted before rendering begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedExportDeliveryContract {
    /// Exact encoded raster size; execution must not normalize it.
    pub resolution: Resolution,
    /// Exact encoded sample depth.
    pub bit_depth: DeliveryBitDepth,
    /// Exact encoded range.
    pub video_range: VideoRange,
    /// Exact encoded chroma representation.
    pub chroma_sampling: ExportChromaSampling,
    /// Exact FFmpeg output pixel format.
    pub pixel_format: &'static str,
    /// Exact creative color target, independent from signal representation.
    pub color_target: ResolvedExportColorTarget,
}

/// Resolve and validate one preset against complete Sequence output intent.
///
/// No authoring state is mutated and no implicit codec fallback is permitted.
pub fn resolve_export_delivery(
    preset: &ExportPreset,
    settings: &SequenceSettings,
    color_environment: &ProjectColorEnvironment,
) -> Result<ResolvedExportDeliveryContract, ExportDeliveryError> {
    settings.validate_with_color_environment(color_environment).map_err(|error| {
        ExportDeliveryError::new(
            ExportDeliveryIssueCode::IncompatibleColorOutput,
            format!("Sequence Program Output 无效: {error}"),
        )
    })?;
    validate_rate_control(&preset.video)?;
    validate_audio_parameters(&preset.audio)?;
    validate_audio_layout(&preset.audio, settings.audio_channel_layout)?;
    validate_container(&preset.container, &preset.video, &preset.audio)?;

    let bit_depth = preset.video_signal.bit_depth.resolve(settings.delivery.bit_depth);
    let video_range = preset.video_signal.range.resolve(settings.delivery.video_range);
    let chroma_sampling = preset.video_signal.chroma_sampling;
    let resolution = preset.resolution.unwrap_or(Resolution {
        width: settings.resolution.width,
        height: settings.resolution.height,
    });

    validate_alpha(preset)?;
    let pixel_format =
        resolve_pixel_format(&preset.video, preset.alpha_mode, bit_depth, chroma_sampling)?;
    validate_dimensions(resolution, chroma_sampling)?;
    let color_target = resolve_export_color_target(preset, settings, color_environment)?;
    validate_color_output(
        preset,
        settings,
        color_environment,
        &color_target,
        bit_depth,
        video_range,
    )?;

    Ok(ResolvedExportDeliveryContract {
        resolution,
        bit_depth,
        video_range,
        chroma_sampling,
        pixel_format,
        color_target,
    })
}

fn resolve_export_color_target(
    preset: &ExportPreset,
    settings: &SequenceSettings,
    color_environment: &ProjectColorEnvironment,
) -> Result<ResolvedExportColorTarget, ExportDeliveryError> {
    match preset.color_target {
        ExportColorTarget::FollowSequence => {
            let context = settings.root_program_color_context(color_environment);
            let color_space = context.output_color_space.color().ok_or_else(|| {
                ExportDeliveryError::new(
                    ExportDeliveryIssueCode::IncompatibleColorOutput,
                    "Sequence Program Output 必须是可编码色彩空间",
                )
            })?;
            Ok(ResolvedExportColorTarget {
                color_space,
                tone_map: context.output_tone_map,
                output_transform: context.output_transform,
            })
        }
        ExportColorTarget::Colorimetric(color_space) => {
            validate_explicit_export_color_space(color_space)?;
            Ok(ResolvedExportColorTarget {
                color_space,
                tone_map: false,
                output_transform: OutputTransformIntent::Colorimetric,
            })
        }
        ExportColorTarget::RenderingView(color_space) => {
            if !color_space.is_display_referred() {
                return Err(ExportDeliveryError::new(
                    ExportDeliveryIssueCode::IncompatibleColorOutput,
                    "Rendering View 导出目标必须是显示或交付色彩空间；Log 中间格式应使用 Colorimetric",
                ));
            }
            let output_transform = match color_environment.engine() {
                ColorEngine::MondrianStandard { package } => {
                    OutputTransformIntent::mondrian_standard_package(*package)
                }
                ColorEngine::Aces { preset } => OutputTransformIntent::aces_preset(*preset),
                ColorEngine::CustomOcio { .. } => {
                    OutputTransformIntent::CustomOcio { output_color_space: color_space }
                }
            };
            let resolved_view = output_transform
                .resolve_display_view(color_space, color_environment.engine())
                .map_err(|detail| {
                    ExportDeliveryError::new(
                        ExportDeliveryIssueCode::IncompatibleColorOutput,
                        detail.to_string(),
                    )
                })?;
            if resolved_view.is_none() {
                return Err(ExportDeliveryError::new(
                    ExportDeliveryIssueCode::IncompatibleColorOutput,
                    format!("Project 色彩引擎没有可用于 {color_space:?} 的 Rendering View"),
                ));
            }
            Ok(ResolvedExportColorTarget { color_space, tone_map: true, output_transform })
        }
    }
}

fn validate_explicit_export_color_space(
    color_space: ColorSpace,
) -> Result<(), ExportDeliveryError> {
    if color_space.is_display_referred() || color_space.encoding().is_scene_log() {
        return Ok(());
    }
    Err(ExportDeliveryError::new(
        ExportDeliveryIssueCode::IncompatibleColorOutput,
        "显式导出目标必须是显示/交付色彩空间或受支持的 Camera Log 编码",
    ))
}

fn validate_rate_control(codec: &VideoCodecConfig) -> Result<(), ExportDeliveryError> {
    let (name, max_crf, rate_control) = match codec {
        VideoCodecConfig::H264 { rate_control, .. } => ("H.264", 51, rate_control),
        VideoCodecConfig::Hevc { rate_control, .. } => ("HEVC", 51, rate_control),
        VideoCodecConfig::Av1 { rate_control, .. } => ("AV1", 63, rate_control),
        VideoCodecConfig::ProRes { .. } | VideoCodecConfig::Gif { .. } => return Ok(()),
    };
    validate_rate_control_values(name, max_crf, *rate_control)
}

fn validate_rate_control_values(
    codec: &str,
    max_crf: u8,
    rate_control: VideoRateControl,
) -> Result<(), ExportDeliveryError> {
    if rate_control.crf > max_crf {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::InvalidRateControl,
            format!(
                "{codec} CRF 必须在 0..={max_crf}，当前为 {}",
                rate_control.crf
            ),
        ));
    }
    match (
        rate_control.max_bitrate_kbps,
        rate_control.buffer_size_kbits,
    ) {
        (None, None) => Ok(()),
        (Some(max_bitrate), Some(buffer_size)) if max_bitrate > 0 && buffer_size > 0 => Ok(()),
        _ => Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::InvalidRateControl,
            format!("{codec} 受限质量模式必须同时提供非零 max bitrate 与 VBV buffer"),
        )),
    }
}

fn validate_audio_parameters(audio: &AudioCodecConfig) -> Result<(), ExportDeliveryError> {
    let valid = match audio {
        AudioCodecConfig::Disabled => true,
        AudioCodecConfig::Aac { bitrate_kbps } | AudioCodecConfig::Mp3 { bitrate_kbps } => {
            *bitrate_kbps > 0
        }
        AudioCodecConfig::Pcm { bit_depth } => matches!(bit_depth, 16 | 24 | 32),
    };
    if valid {
        Ok(())
    } else {
        Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::InvalidAudioParameter,
            "音频编码参数不在已验证范围内",
        ))
    }
}

/// Canonical FFmpeg name for one explicitly supported encoded audio layout.
///
/// Custom speaker sets and Discrete buses remain representable inside the DSP
/// core, but export must not guess an encoded channel order from their extent.
pub(crate) const fn ffmpeg_audio_channel_layout(
    layout: AudioChannelLayout,
) -> Option<&'static str> {
    match layout {
        AudioChannelLayout::Mono => Some("mono"),
        AudioChannelLayout::Stereo => Some("stereo"),
        AudioChannelLayout::Surround51Side => Some("5.1(side)"),
        AudioChannelLayout::Surround51Back => Some("5.1"),
        AudioChannelLayout::Surround71 => Some("7.1"),
        AudioChannelLayout::Speakers(_) | AudioChannelLayout::Discrete(_) => None,
    }
}

fn validate_audio_layout(
    audio: &AudioCodecConfig,
    layout: AudioChannelLayout,
) -> Result<(), ExportDeliveryError> {
    let supported = match audio {
        AudioCodecConfig::Disabled => true,
        AudioCodecConfig::Mp3 { .. } => {
            matches!(
                layout,
                AudioChannelLayout::Mono | AudioChannelLayout::Stereo
            )
        }
        AudioCodecConfig::Aac { .. } | AudioCodecConfig::Pcm { .. } => {
            ffmpeg_audio_channel_layout(layout).is_some()
        }
    };
    if supported {
        Ok(())
    } else {
        Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::UnsupportedAudioLayout,
            format!(
                "音频编码 {:?} 无法显式承载 Sequence 布局 {layout}；请选择可证明的布局或编码",
                audio
            ),
        ))
    }
}

fn validate_container(
    container: &Container,
    video: &VideoCodecConfig,
    audio: &AudioCodecConfig,
) -> Result<(), ExportDeliveryError> {
    let video_supported = match container {
        Container::Mp4 => matches!(
            video,
            VideoCodecConfig::H264 { .. }
                | VideoCodecConfig::Hevc { .. }
                | VideoCodecConfig::Av1 { .. }
        ),
        Container::Mov => matches!(
            video,
            VideoCodecConfig::H264 { .. }
                | VideoCodecConfig::Hevc { .. }
                | VideoCodecConfig::ProRes { .. }
        ),
        Container::Mkv => matches!(
            video,
            VideoCodecConfig::H264 { .. }
                | VideoCodecConfig::Hevc { .. }
                | VideoCodecConfig::Av1 { .. }
        ),
        Container::Mxf => matches!(video, VideoCodecConfig::ProRes { .. }),
        Container::Webm => matches!(video, VideoCodecConfig::Av1 { .. }),
        Container::Gif => matches!(video, VideoCodecConfig::Gif { .. }),
    };
    if !video_supported {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::UnsupportedContainerCodec,
            "所选容器不支持该视频编码合同",
        ));
    }

    let audio_supported = match container {
        Container::Mp4 => matches!(
            audio,
            AudioCodecConfig::Disabled
                | AudioCodecConfig::Aac { .. }
                | AudioCodecConfig::Mp3 { .. }
        ),
        Container::Mov | Container::Mkv => true,
        Container::Mxf => {
            matches!(
                audio,
                AudioCodecConfig::Disabled | AudioCodecConfig::Pcm { .. }
            )
        }
        Container::Webm | Container::Gif => matches!(audio, AudioCodecConfig::Disabled),
    };
    if !audio_supported {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::UnsupportedContainerAudio,
            "所选容器不支持该音频编码合同",
        ));
    }
    Ok(())
}

fn resolve_pixel_format(
    codec: &VideoCodecConfig,
    alpha_mode: ExportAlphaMode,
    bit_depth: DeliveryBitDepth,
    chroma: ExportChromaSampling,
) -> Result<&'static str, ExportDeliveryError> {
    let pixel_format = match codec {
        VideoCodecConfig::H264 { profile: crate::preset::H264Profile::High, .. }
            if bit_depth == DeliveryBitDepth::Eight && chroma == ExportChromaSampling::Yuv420 =>
        {
            "yuv420p"
        }
        VideoCodecConfig::Hevc { profile: HevcProfile::Main, .. }
            if bit_depth == DeliveryBitDepth::Eight && chroma == ExportChromaSampling::Yuv420 =>
        {
            "yuv420p"
        }
        VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. }
            if bit_depth == DeliveryBitDepth::Ten && chroma == ExportChromaSampling::Yuv420 =>
        {
            "yuv420p10le"
        }
        VideoCodecConfig::Av1 { profile: Av1Profile::Main, .. }
            if bit_depth == DeliveryBitDepth::Eight && chroma == ExportChromaSampling::Yuv420 =>
        {
            "yuv420p"
        }
        VideoCodecConfig::Av1 { profile: Av1Profile::Main, .. }
            if bit_depth == DeliveryBitDepth::Ten && chroma == ExportChromaSampling::Yuv420 =>
        {
            "yuv420p10le"
        }
        VideoCodecConfig::ProRes {
            profile:
                ProResProfile::Proxy | ProResProfile::Lt | ProResProfile::Standard | ProResProfile::Hq,
        } if bit_depth == DeliveryBitDepth::Ten
            && chroma == ExportChromaSampling::Yuv422
            && alpha_mode == ExportAlphaMode::FlattenBlack =>
        {
            "yuv422p10le"
        }
        VideoCodecConfig::ProRes { profile }
            if profile.is_4444()
                && bit_depth == DeliveryBitDepth::Twelve
                && chroma == ExportChromaSampling::Yuv444 =>
        {
            if alpha_mode == ExportAlphaMode::Preserve {
                "yuva444p12le"
            } else {
                "yuv444p12le"
            }
        }
        VideoCodecConfig::Gif { colors, .. }
            if (2..=256).contains(colors)
                && bit_depth == DeliveryBitDepth::Eight
                && chroma == ExportChromaSampling::Rgb
                && alpha_mode == ExportAlphaMode::FlattenBlack =>
        {
            "rgb8"
        }
        VideoCodecConfig::Gif { colors, .. } if !(2..=256).contains(colors) => {
            return Err(ExportDeliveryError::new(
                ExportDeliveryIssueCode::InvalidCodecParameter,
                "GIF 调色板颜色数必须在 2..=256",
            ));
        }
        _ => {
            return Err(ExportDeliveryError::new(
                ExportDeliveryIssueCode::IncompatibleVideoSignal,
                "codec profile、位深、色度采样与 Alpha 设置不构成受支持的编码合同",
            ));
        }
    };
    Ok(pixel_format)
}

fn validate_dimensions(
    resolution: Resolution,
    chroma: ExportChromaSampling,
) -> Result<(), ExportDeliveryError> {
    if !(SequenceSettings::MIN_WIDTH..=SequenceSettings::MAX_WIDTH).contains(&resolution.width)
        || !(SequenceSettings::MIN_HEIGHT..=SequenceSettings::MAX_HEIGHT)
            .contains(&resolution.height)
    {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::InvalidResolution,
            format!(
                "导出尺寸不受支持: {}x{}",
                resolution.width, resolution.height
            ),
        ));
    }
    let requires_even_width = matches!(
        chroma,
        ExportChromaSampling::Yuv420 | ExportChromaSampling::Yuv422
    );
    let requires_even_height = chroma == ExportChromaSampling::Yuv420;
    if requires_even_width && !resolution.width.is_multiple_of(2) {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::InvalidResolution,
            "4:2:0/4:2:2 导出宽度必须为偶数；系统不会静默裁切",
        ));
    }
    if requires_even_height && !resolution.height.is_multiple_of(2) {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::InvalidResolution,
            "4:2:0 导出高度必须为偶数；系统不会静默裁切",
        ));
    }
    Ok(())
}

fn validate_alpha(preset: &ExportPreset) -> Result<(), ExportDeliveryError> {
    if preset.alpha_mode == ExportAlphaMode::FlattenBlack {
        return Ok(());
    }
    if matches!(
        (&preset.container, &preset.video),
        (Container::Mov, VideoCodecConfig::ProRes { profile }) if profile.is_4444()
    ) {
        return Ok(());
    }
    Err(ExportDeliveryError::new(
        ExportDeliveryIssueCode::UnsupportedAlpha,
        "保留 Alpha 当前仅支持 MOV + ProRes 4444/4444 XQ",
    ))
}

fn validate_color_output(
    preset: &ExportPreset,
    settings: &SequenceSettings,
    color_environment: &ProjectColorEnvironment,
    color_target: &ResolvedExportColorTarget,
    bit_depth: DeliveryBitDepth,
    video_range: VideoRange,
) -> Result<(), ExportDeliveryError> {
    let output = color_target.color_space;
    let write_static_hdr = matches!(
        settings.delivery.static_hdr_metadata_policy,
        StaticHdrMetadataPolicy::WriteAuthored
    );

    if output.encoding().is_scene_log() {
        if bit_depth == DeliveryBitDepth::Eight {
            return Err(ExportDeliveryError::new(
                ExportDeliveryIssueCode::IncompatibleColorOutput,
                "Camera log 输出需要 10-bit 或更高位深",
            ));
        }
        if !matches!(
            (&preset.container, &preset.video),
            (
                Container::Mov | Container::Mxf,
                VideoCodecConfig::ProRes { .. }
            )
        ) {
            return Err(ExportDeliveryError::new(
                ExportDeliveryIssueCode::IncompatibleColorOutput,
                "Camera log 输出仅支持 MOV/MXF + ProRes 专业中间格式",
            ));
        }
    }

    if output.is_hdr() && bit_depth == DeliveryBitDepth::Eight {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::IncompatibleColorOutput,
            "PQ/HLG HDR 输出不能使用 8-bit 编码",
        ));
    }
    if matches!(preset.video, VideoCodecConfig::H264 { .. }) && output.is_hdr() {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::IncompatibleColorOutput,
            "当前 H.264 High 8-bit 合同仅支持 SDR；HDR 请使用 HEVC Main10",
        ));
    }
    if matches!(preset.video, VideoCodecConfig::Gif { .. })
        && (output != ColorSpace::Srgb
            || bit_depth != DeliveryBitDepth::Eight
            || video_range != VideoRange::Full)
    {
        return Err(ExportDeliveryError::new(
            ExportDeliveryIssueCode::IncompatibleColorOutput,
            "GIF 不携带可靠视频色彩标签，仅允许显式 sRGB / 8-bit / Full 输出",
        ));
    }

    if write_static_hdr {
        if !output.is_hdr() {
            return Err(ExportDeliveryError::new(
                ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                "只有 HDR 输出色彩空间可以写入静态 HDR metadata",
            ));
        }
        if !matches!(
            preset.video,
            VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. }
        ) {
            return Err(ExportDeliveryError::new(
                ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                "静态 HDR metadata 当前仅由 HEVC Main10/libx265 后端写入",
            ));
        }
        let mastering = settings.delivery.hdr_mastering_display.as_ref().ok_or_else(|| {
            ExportDeliveryError::new(
                ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                "写入静态 HDR metadata 需要 SMPTE ST 2086 母版显示元数据",
            )
        })?;
        mastering.validate().map_err(|error| {
            ExportDeliveryError::new(
                ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                format!("SMPTE ST 2086 母版显示元数据无效: {error}"),
            )
        })?;
        let content_light = settings.delivery.hdr_content_light.ok_or_else(|| {
            ExportDeliveryError::new(
                ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                "写入静态 HDR metadata 需要 MaxCLL/MaxFALL 内容光级别元数据",
            )
        })?;
        content_light.validate().map_err(|error| {
            ExportDeliveryError::new(
                ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                format!("MaxCLL/MaxFALL 内容光级别元数据无效: {error}"),
            )
        })?;

        let engine = color_environment.engine();
        if let ColorEngine::MondrianStandard { package } = engine {
            let target = mondrian_core::mondrian_standard_output_target_contract_for_package(
                *package, output,
            )
            .map_err(|detail| {
                ExportDeliveryError::new(ExportDeliveryIssueCode::IncompatibleColorOutput, detail)
            })?;
            if content_light.max_content_light_level > target.nominal_peak_nits {
                return Err(ExportDeliveryError::new(
                    ExportDeliveryIssueCode::UnsupportedHdrMetadata,
                    format!(
                        "Mondrian Standard {:?} View 峰值为 {} nit，但 MaxCLL 声明 {} nit",
                        output, target.nominal_peak_nits, content_light.max_content_light_level
                    ),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{ExportParameter, ExportVideoSignal, H264Profile, VideoRateControl};

    #[test]
    fn h264_sdr_preset_resolves_without_using_ten_bit_sequence_default() {
        let settings = SequenceSettings::default();
        let contract = resolve_export_delivery(
            &ExportPreset::h264_aac_sdr_1080p(),
            &settings,
            &ProjectColorEnvironment::default(),
        )
        .expect("explicit preset signal should resolve");

        assert_eq!(contract.bit_depth, DeliveryBitDepth::Eight);
        assert_eq!(contract.video_range, VideoRange::Legal);
        assert_eq!(contract.pixel_format, "yuv420p");
    }

    #[test]
    fn hevc_main10_preset_rejects_accidental_eight_bit_override() {
        let mut preset = ExportPreset::hevc_main10_aac();
        preset.video_signal.bit_depth = ExportParameter::Explicit(DeliveryBitDepth::Eight);

        let error = resolve_export_delivery(
            &preset,
            &SequenceSettings::default(),
            &ProjectColorEnvironment::default(),
        )
        .expect_err("Main10 and 8-bit must not be silently reconciled");

        assert_eq!(error.code, ExportDeliveryIssueCode::IncompatibleVideoSignal);
    }

    #[test]
    fn follow_sequence_values_are_resolved_before_execution() {
        let mut settings = SequenceSettings::default();
        settings.delivery.bit_depth = DeliveryBitDepth::Eight;
        settings.delivery.video_range = VideoRange::Full;
        let preset = ExportPreset {
            name: "follow".to_owned(),
            container: Container::Mp4,
            video: VideoCodecConfig::H264 {
                profile: H264Profile::High,
                rate_control: VideoRateControl::constant_quality(18),
            },
            audio: AudioCodecConfig::Aac { bitrate_kbps: 192 },
            resolution: None,
            video_signal: ExportVideoSignal::default(),
            alpha_mode: ExportAlphaMode::FlattenBlack,
            color_target: crate::preset::ExportColorTarget::FollowSequence,
        };

        let contract =
            resolve_export_delivery(&preset, &settings, &ProjectColorEnvironment::default())
                .expect("sequence defaults should resolve to a concrete contract");
        assert_eq!(contract.bit_depth, DeliveryBitDepth::Eight);
        assert_eq!(contract.video_range, VideoRange::Full);
    }

    #[test]
    fn audio_layout_admission_is_codec_specific_and_fail_closed() {
        let environment = ProjectColorEnvironment::default();
        let mut settings = SequenceSettings {
            audio_channel_layout: AudioChannelLayout::Surround71,
            ..SequenceSettings::default()
        };
        let mut preset = ExportPreset::h264_aac_sdr_1080p();
        resolve_export_delivery(&preset, &settings, &environment)
            .expect("AAC admits an explicit 7.1 lowering");

        preset.audio = AudioCodecConfig::Mp3 { bitrate_kbps: 192 };
        let error = resolve_export_delivery(&preset, &settings, &environment)
            .expect_err("MP3 must not accept 7.1 by channel count");
        assert_eq!(error.code, ExportDeliveryIssueCode::UnsupportedAudioLayout);

        settings.audio_channel_layout = AudioChannelLayout::speakers([
            mondrian_core::AudioChannelPosition::FrontLeft,
            mondrian_core::AudioChannelPosition::FrontRight,
            mondrian_core::AudioChannelPosition::TopCenter,
        ])
        .expect("custom speaker layout");
        preset.audio = AudioCodecConfig::Aac { bitrate_kbps: 192 };
        let error = resolve_export_delivery(&preset, &settings, &environment)
            .expect_err("custom order needs an explicit encoding Adapter");
        assert_eq!(error.code, ExportDeliveryIssueCode::UnsupportedAudioLayout);
        assert!(error.detail.contains(&settings.audio_channel_layout.to_string()));
    }

    #[test]
    fn explicit_log_target_does_not_mutate_sequence_program_output() {
        let settings = SequenceSettings::default();
        let sequence_output = settings.color.program_output.clone();
        let mut preset = ExportPreset::prores_4444_alpha();
        preset.alpha_mode = ExportAlphaMode::FlattenBlack;
        preset.color_target = ExportColorTarget::Colorimetric(ColorSpace::AppleLogBt2020);

        let contract =
            resolve_export_delivery(&preset, &settings, &ProjectColorEnvironment::default())
                .expect("explicit 12-bit ProRes log output should be admitted");

        assert_eq!(settings.color.program_output, sequence_output);
        assert_eq!(
            contract.color_target.color_space,
            ColorSpace::AppleLogBt2020
        );
        assert!(!contract.color_target.tone_map);
        assert_eq!(
            contract.color_target.output_transform,
            OutputTransformIntent::Colorimetric
        );
    }

    #[test]
    fn subsampled_dimensions_fail_instead_of_being_normalized() {
        let mut preset = ExportPreset::h264_aac_sdr_1080p();
        preset.resolution = Some(Resolution { width: 1_919, height: 1_080 });

        let error = resolve_export_delivery(
            &preset,
            &SequenceSettings::default(),
            &ProjectColorEnvironment::default(),
        )
        .expect_err("odd 4:2:0 width must fail closed");
        assert_eq!(error.code, ExportDeliveryIssueCode::InvalidResolution);
        assert!(error.detail.contains("不会静默裁切"));
    }

    #[test]
    fn rate_control_requires_a_complete_vbv_pair() {
        let mut preset = ExportPreset::h264_aac_sdr_1080p();
        preset.video = VideoCodecConfig::H264 {
            profile: H264Profile::High,
            rate_control: VideoRateControl {
                crf: 18,
                max_bitrate_kbps: Some(8_000),
                buffer_size_kbits: None,
            },
        };

        let error = resolve_export_delivery(
            &preset,
            &SequenceSettings::default(),
            &ProjectColorEnvironment::default(),
        )
        .expect_err("partial VBV state must be rejected");
        assert_eq!(error.code, ExportDeliveryIssueCode::InvalidRateControl);
    }
}
