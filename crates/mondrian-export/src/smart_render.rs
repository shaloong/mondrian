//! Conservative Smart Render qualification.
//!
//! Timeline owns author-semantic identity, Media owns physical packet
//! identity, and Delivery owns the encoded output contract. This Module joins
//! those already-deep interfaces into one executable plan; it never scans
//! author Tracks or weakens ordinary render admission.

use crate::delivery::{ResolvedExportArtifactEncoding, ResolvedExportDeliveryContract};
use crate::preset::{
    ExportAlphaMode, ExportChromaSampling, ExportConfig, ExportSmartRenderPolicy, H264Profile,
    HevcProfile, ProResProfile, TimelineExportSnapshot, VideoCodecConfig,
};
use mondrian_core::timeline_data::FieldOrder;
use mondrian_core::{
    AssetId, ColorSpace, PictureOrientation, PixelFormat, SampleAspectRatio, TimelineTime,
    TimelineTimeRange, VideoCodec, VideoCodecProfile,
};
use mondrian_media::DecodedVideoRange;
use mondrian_timeline::sequence::{DeliveryBitDepth, ResolvedInputColor, VideoRange};
use mondrian_timeline::PictureSourceExtent;
use std::path::PathBuf;

/// Fully qualified full-source video essence reuse plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SmartRenderPlan {
    pub(crate) asset_id: AssetId,
    pub(crate) path: PathBuf,
    pub(crate) video_stream_index: u32,
    pub(crate) source_fingerprint: mondrian_media::MediaFileFingerprint,
}

/// Stable reason why one automatic Smart Render attempt must use normal render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmartRenderBlocker {
    PolicyDisabled,
    UnsupportedArtifact,
    VisualNotIdentity,
    MissingSourceEvidence,
    NotFullSource,
    SourceRevisionIncomplete,
    CodecMismatch,
    RasterMismatch,
    CadenceMismatch,
    PictureGeometryMismatch,
    SignalMismatch,
    ColorTransformRequired,
    LegalizerRequiresRender,
    HdrMetadataRequiresRender,
}

/// Qualify one immutable export for conservative full-stream video reuse.
pub(crate) fn qualify_smart_render(
    config: &ExportConfig,
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    selected_range: TimelineTimeRange,
    total_frames: u64,
) -> Result<SmartRenderPlan, SmartRenderBlocker> {
    qualify_full_source_identity(
        config,
        timeline,
        delivery,
        selected_range,
        total_frames,
        FullSourceIdentityPolicy {
            require_automatic_smart_render: true,
            allow_source_hdr_metadata: false,
        },
    )
}

/// Qualify the exact full-source picture identity required by byte-identical
/// Dynamic HDR file preservation.
///
/// Unlike Automatic Smart Render, this explicit policy may inspect a source
/// that contains HDR metadata and never authorizes fallback rendering.
pub(crate) fn qualify_exact_source_file_preservation(
    config: &ExportConfig,
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    selected_range: TimelineTimeRange,
    total_frames: u64,
) -> Result<SmartRenderPlan, SmartRenderBlocker> {
    qualify_full_source_identity(
        config,
        timeline,
        delivery,
        selected_range,
        total_frames,
        FullSourceIdentityPolicy {
            require_automatic_smart_render: false,
            allow_source_hdr_metadata: true,
        },
    )
}

#[derive(Debug, Clone, Copy)]
struct FullSourceIdentityPolicy {
    require_automatic_smart_render: bool,
    allow_source_hdr_metadata: bool,
}

fn qualify_full_source_identity(
    config: &ExportConfig,
    timeline: &TimelineExportSnapshot,
    delivery: &ResolvedExportDeliveryContract,
    selected_range: TimelineTimeRange,
    total_frames: u64,
    policy: FullSourceIdentityPolicy,
) -> Result<SmartRenderPlan, SmartRenderBlocker> {
    if policy.require_automatic_smart_render
        && config.smart_render != ExportSmartRenderPolicy::Automatic
    {
        return Err(SmartRenderBlocker::PolicyDisabled);
    }
    if delivery.legalizer.is_active() {
        return Err(SmartRenderBlocker::LegalizerRequiresRender);
    }
    let ResolvedExportArtifactEncoding::MediaFile { video, .. } = &delivery.artifact else {
        return Err(SmartRenderBlocker::UnsupportedArtifact);
    };
    let visual = timeline
        .prepared_execution()
        .map(|execution| execution.visual())
        .ok_or(SmartRenderBlocker::MissingSourceEvidence)?;
    let program = visual
        .program_by_id(timeline.sequence.id)
        .ok_or(SmartRenderBlocker::MissingSourceEvidence)?;
    let visual_identity = program.schedule().source_identity(selected_range);
    let candidate = visual_identity
        .map_err(|_| SmartRenderBlocker::VisualNotIdentity)?
        .ok_or(SmartRenderBlocker::VisualNotIdentity)?;
    let dependency = timeline
        .media
        .get(&candidate.asset_id)
        .ok_or(SmartRenderBlocker::MissingSourceEvidence)?;
    let stream = dependency
        .source_video_stream
        .as_ref()
        .ok_or(SmartRenderBlocker::MissingSourceEvidence)?;
    if dependency.source_container.trim().is_empty()
        || dependency.video_stream_index != Some(stream.index)
        || !dependency.source_fingerprint.authorizes_reuse()
    {
        return Err(SmartRenderBlocker::SourceRevisionIncomplete);
    }
    let PictureSourceExtent::TimelineRange(source_extent) = dependency
        .picture_source_extent
        .ok_or(SmartRenderBlocker::MissingSourceEvidence)?
    else {
        return Err(SmartRenderBlocker::NotFullSource);
    };
    if source_extent.start != TimelineTime::ZERO
        || candidate.source_range != source_extent
        || stream.total_frames != Some(total_frames)
    {
        return Err(SmartRenderBlocker::NotFullSource);
    }
    if !video_codec_matches(video, &stream.codec, stream.codec_profile) {
        return Err(SmartRenderBlocker::CodecMismatch);
    }
    if stream.width != delivery.resolution.width || stream.height != delivery.resolution.height {
        return Err(SmartRenderBlocker::RasterMismatch);
    }
    if !stream.frame_rate_proven || stream.frame_rate != delivery.frame_rate {
        return Err(SmartRenderBlocker::CadenceMismatch);
    }
    if stream.picture.orientation != PictureOrientation::Identity
        || stream.picture.field_order.unwrap_or(FieldOrder::Progressive) != delivery.field_order
        || stream.picture.sample_aspect_ratio.unwrap_or(SampleAspectRatio::SQUARE)
            != delivery.sample_aspect_ratio
    {
        return Err(SmartRenderBlocker::PictureGeometryMismatch);
    }
    let sampling = stream.proven_sampling().ok_or(SmartRenderBlocker::SignalMismatch)?;
    if !sampling_matches_delivery(
        sampling.pixel_format,
        sampling.bit_depth,
        sampling.has_alpha,
        delivery.bit_depth,
        delivery.chroma_sampling,
        config.preset.alpha_mode,
    ) || source_video_range(dependency) != Some(delivery.video_range)
    {
        return Err(SmartRenderBlocker::SignalMismatch);
    }
    if timeline.sequence.settings.color.input.auto_tone_map_media
        || delivery.color_target.tone_map
        || resolved_source_color(timeline, dependency) != Some(delivery.color_target.color_space)
    {
        return Err(SmartRenderBlocker::ColorTransformRequired);
    }
    if (!policy.allow_source_hdr_metadata && !stream.hdr_metadata.is_empty())
        || timeline
            .sequence
            .settings
            .delivery
            .static_hdr_metadata_policy
            .writes_authored_metadata()
    {
        return Err(SmartRenderBlocker::HdrMetadataRequiresRender);
    }
    Ok(SmartRenderPlan {
        asset_id: candidate.asset_id,
        path: dependency.path.clone(),
        video_stream_index: stream.index,
        source_fingerprint: dependency.source_fingerprint,
    })
}

fn video_codec_matches(
    requested: &VideoCodecConfig,
    source: &VideoCodec,
    profile: VideoCodecProfile,
) -> bool {
    match (requested, source) {
        (VideoCodecConfig::H264 { profile: H264Profile::High, .. }, VideoCodec::H264) => {
            profile == VideoCodecProfile::H264High
        }
        (VideoCodecConfig::Hevc { profile: HevcProfile::Main, .. }, VideoCodec::H265) => {
            profile == VideoCodecProfile::HevcMain
        }
        (VideoCodecConfig::Hevc { profile: HevcProfile::Main10, .. }, VideoCodec::H265) => {
            profile == VideoCodecProfile::HevcMain10
        }
        (VideoCodecConfig::ProRes { profile }, VideoCodec::ProRes(source)) => matches!(
            (profile, source),
            (ProResProfile::Proxy, mondrian_core::ProResVariant::Proxy)
                | (ProResProfile::Lt, mondrian_core::ProResVariant::Lt)
                | (
                    ProResProfile::Standard,
                    mondrian_core::ProResVariant::Standard
                )
                | (ProResProfile::Hq, mondrian_core::ProResVariant::Hq)
                | (
                    ProResProfile::FourFourFourFour,
                    mondrian_core::ProResVariant::R4444
                )
                | (
                    ProResProfile::FourFourFourFourXq,
                    mondrian_core::ProResVariant::R4444Xq
                )
        ),
        (
            VideoCodecConfig::Av1 { .. }
            | VideoCodecConfig::DnxHr { .. }
            | VideoCodecConfig::AvcIntra { .. }
            | VideoCodecConfig::Uncompressed { .. }
            | VideoCodecConfig::Gif { .. },
            _,
        )
        | (VideoCodecConfig::H264 { .. }, _)
        | (VideoCodecConfig::Hevc { .. }, _)
        | (VideoCodecConfig::ProRes { .. }, _) => false,
    }
}

fn sampling_matches_delivery(
    pixel_format: PixelFormat,
    bit_depth: u8,
    has_alpha: bool,
    delivery_depth: DeliveryBitDepth,
    chroma: ExportChromaSampling,
    alpha: ExportAlphaMode,
) -> bool {
    let expected_depth = match delivery_depth {
        DeliveryBitDepth::Eight => 8,
        DeliveryBitDepth::Ten => 10,
        DeliveryBitDepth::Twelve => 12,
    };
    let chroma_matches = match chroma {
        ExportChromaSampling::Yuv420 => matches!(
            pixel_format,
            PixelFormat::Yuv420p
                | PixelFormat::Yuv420p10le
                | PixelFormat::Yuv420p12le
                | PixelFormat::Yuv420p16le
        ),
        ExportChromaSampling::Yuv422 => matches!(
            pixel_format,
            PixelFormat::Yuv422p
                | PixelFormat::Yuv422p10le
                | PixelFormat::Yuv422p12le
                | PixelFormat::Yuv422p16le
        ),
        ExportChromaSampling::Yuv444 => matches!(
            pixel_format,
            PixelFormat::Yuv444p
                | PixelFormat::Yuv444p10le
                | PixelFormat::Yuv444p12le
                | PixelFormat::Yuv444p16le
                | PixelFormat::Gbrap10le
                | PixelFormat::Gbrap12le
                | PixelFormat::Gbrap16le
        ),
        ExportChromaSampling::Rgb => pixel_format.is_rgb(),
    };
    let alpha_matches = match alpha {
        ExportAlphaMode::FlattenBlack => !has_alpha,
        ExportAlphaMode::Preserve => has_alpha,
    };
    bit_depth == expected_depth && chroma_matches && alpha_matches
}

fn source_video_range(dependency: &crate::preset::ExportMediaDependency) -> Option<VideoRange> {
    let detected = dependency
        .color_diagnostic
        .as_ref()
        .map(|diagnostic| diagnostic.color_range)
        .unwrap_or(DecodedVideoRange::Unknown);
    match mondrian_media::resolve_decoded_video_range(dependency.interpretation.range, detected) {
        DecodedVideoRange::Limited => Some(VideoRange::Legal),
        DecodedVideoRange::Full => Some(VideoRange::Full),
        DecodedVideoRange::Unknown => None,
    }
}

fn resolved_source_color(
    timeline: &TimelineExportSnapshot,
    dependency: &crate::preset::ExportMediaDependency,
) -> Option<ColorSpace> {
    let settings = &timeline.sequence.settings;
    let decision = settings.color.input.missing_metadata_policy.resolve_asset_input_decision(
        None,
        dependency.interpretation,
        dependency
            .color_diagnostic
            .as_ref()
            .and_then(mondrian_media::VideoColorDiagnostic::executable_color_space),
        settings.color.working_color_space,
    );
    match decision.resolved {
        ResolvedInputColor::Color(color_space) => Some(color_space),
        // DataTexture pixels must execute the numeric working-frame route;
        // packet reuse would skip the authored visual program entirely.
        ResolvedInputColor::Data => None,
        ResolvedInputColor::Rejected => None,
    }
}
