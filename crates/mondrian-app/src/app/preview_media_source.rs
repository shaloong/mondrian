//! UI-independent Preview media-source resolution.
//!
//! This Module resolves one asset and complete Viewer intent into either one
//! canonical decode request, one explicit color rejection, or one structured
//! source-path failure. It owns proxy/source selection, file fingerprinting,
//! input color and range interpretation, native-surface classification, and
//! decode-geometry canonicalization. It performs no scheduling, proxy dispatch,
//! cache mutation, diagnostics accumulation, or presentation.

use std::path::{Path, PathBuf};

use mondrian_assets::{AssetKind, AssetRecord};
use mondrian_core::timeline_data::AlphaInterpretation;
use mondrian_core::types::{AssetId, ColorSpace};
use mondrian_core::{Resolution, TimelineTime};
use mondrian_media::info::PixelFormat;
use mondrian_media::{
    DecodedVideoRange, DecodedVideoRangeContract, MediaFileFingerprint,
    PreviewHardwareDecodeRequest, ProxyColorContract, ProxyConfig, ProxyGenerator, ProxyStatus,
    VideoColorDiagnostic,
};
use mondrian_timeline::sequence::{ColorContext, InputColorResolution, ResolvedInputColor};

use super::preview_access_mode::{MediaPreviewKey, MediaPreviewNativeSurfaceHint};
use super::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;

/// Source/proxy path selected for one Preview media request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewMediaDecodePath {
    pub(crate) path: PathBuf,
    pub(crate) resolution: PreviewMediaDecodePathResolution,
    pub(crate) fingerprint: MediaFileFingerprint,
}

/// Why Preview decoded the source path or an optimized proxy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PreviewMediaDecodePathResolution {
    Source,
    Proxy,
    ProxyMissing,
    ProxyStale,
}

/// Stable identity for deduplicating one proxy-generation side effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PreviewProxyGenerationRequestKey {
    pub(crate) asset_id: AssetId,
    pub(crate) source_fingerprint: MediaFileFingerprint,
    pub(crate) resolution: PreviewMediaDecodePathResolution,
    pub(crate) color: ProxyColorContract,
}

/// Complete proxy-generation side effect selected by media-source resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewProxyGenerationIntent {
    pub(crate) key: PreviewProxyGenerationRequestKey,
    pub(crate) source_path: PathBuf,
    pub(crate) config: ProxyConfig,
    pub(crate) color: ProxyColorContract,
}

/// Complete immutable input required to resolve one Preview media source.
pub(crate) struct PreviewMediaSourceRequest<'a> {
    pub(crate) asset: &'a AssetRecord,
    pub(crate) color_space_override: Option<ColorSpace>,
    pub(crate) alpha_interpretation: AlphaInterpretation,
    pub(crate) source_time: TimelineTime,
    pub(crate) target_resolution: Resolution,
    pub(crate) color_context: &'a ColorContext,
    pub(crate) prefer_proxy: bool,
    pub(crate) request_missing_proxy_generation: bool,
    pub(crate) proxy_config: &'a ProxyConfig,
    pub(crate) proxy_color: Option<ProxyColorContract>,
    pub(crate) hardware_admission: PreviewHardwareDecodeAdmissionState,
}

/// Canonical media request and side-effect intent produced by resolution.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedPreviewMediaSource {
    pub(crate) key: MediaPreviewKey,
    pub(crate) path_resolution: PreviewMediaDecodePathResolution,
    pub(crate) input_color_resolution: InputColorResolution,
    pub(crate) proxy_generation: Option<PreviewProxyGenerationIntent>,
}

/// Color evidence for a source that cannot enter the working-color pipeline.
#[derive(Debug, Clone)]
pub(crate) struct RejectedPreviewMediaSource {
    pub(crate) asset_id: AssetId,
    pub(crate) path: PathBuf,
    pub(crate) input_color_resolution: InputColorResolution,
    pub(crate) diagnostic: VideoColorDiagnostic,
}

/// Source failure that prevented construction of a canonical media request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnavailablePreviewMediaSource {
    pub(crate) asset_id: AssetId,
    pub(crate) path: PathBuf,
    pub(crate) reason: String,
}

/// Exhaustive result of adapting one asset into Preview execution semantics.
#[derive(Debug, Clone)]
pub(crate) enum PreviewMediaSourceOutcome {
    Ready(ResolvedPreviewMediaSource),
    ColorRejected(RejectedPreviewMediaSource),
    Unavailable(UnavailablePreviewMediaSource),
}

/// Resolve one asset and Viewer intent without Window or Widget state.
pub(crate) fn resolve_preview_media_source(
    request: PreviewMediaSourceRequest<'_>,
) -> PreviewMediaSourceOutcome {
    if request.asset.kind != AssetKind::Video {
        return unavailable(&request, "asset is not a video source".to_owned());
    }
    if request.source_time.is_negative() {
        return unavailable(
            &request,
            format!("negative source target is invalid: {}", request.source_time),
        );
    }

    let primary_video = request.asset.media_info.primary_video();
    let source_has_alpha = primary_video.is_some_and(|video| video.has_alpha);
    let resolved_path = match resolve_preview_media_decode_path(
        request.prefer_proxy,
        source_has_alpha,
        &request.asset.path,
        request.proxy_config,
        request.proxy_color,
    ) {
        Ok(path) => path,
        Err(reason) => return unavailable(&request, reason),
    };

    let detected_color_space = primary_video.and_then(|video| video.detected_color_space);
    let input_color_resolution = resolve_preview_input_color_space(
        request.color_space_override,
        request.asset.interpretation,
        detected_color_space,
        request.color_context,
    );
    let input_color_space = match input_color_resolution.resolved {
        ResolvedInputColor::Color(color_space) => color_space,
        ResolvedInputColor::Data | ResolvedInputColor::Rejected => {
            return PreviewMediaSourceOutcome::ColorRejected(RejectedPreviewMediaSource {
                asset_id: request.asset.id,
                path: request.asset.path.clone(),
                input_color_resolution,
                diagnostic: source_color_diagnostic(request.asset),
            });
        }
    };

    let native_surface_hint = primary_video.and_then(|video| match video.pixel_format {
        PixelFormat::Yuv420p | PixelFormat::Nv12 => Some(MediaPreviewNativeSurfaceHint::Nv12),
        PixelFormat::Yuv420p10le | PixelFormat::P010 => Some(MediaPreviewNativeSurfaceHint::P010),
        _ => None,
    });
    let source_resolution = primary_video.map_or(request.target_resolution, |video| Resolution {
        width: video.width.max(1),
        height: video.height.max(1),
    });
    let mut key = MediaPreviewKey {
        asset_id: request.asset.id,
        path: resolved_path.path,
        fingerprint: Some(resolved_path.fingerprint),
        source_time: request.source_time,
        target_width: request.target_resolution.width,
        target_height: request.target_resolution.height,
        source_width: source_resolution.width,
        source_height: source_resolution.height,
        input_color_space,
        input_video_range: DecodedVideoRangeContract::from_interpretation(
            request.asset.interpretation.range,
            primary_video.map_or(DecodedVideoRange::Unknown, |video| video.color_range),
        ),
        native_surface_hint,
        source_has_alpha,
        alpha_interpretation: request.alpha_interpretation,
        working_color_space: request.color_context.working_color_space,
        tone_map: request.color_context.tone_map,
        engine: request.color_context.engine.clone(),
        ocio_generation: mondrian_core::ocio_config_generation(),
    };
    canonicalize_media_decode_geometry(&mut key, request.hardware_admission);

    let proxy_generation = proxy_generation_intent(
        &request,
        resolved_path.resolution,
        resolved_path.fingerprint,
    );
    PreviewMediaSourceOutcome::Ready(ResolvedPreviewMediaSource {
        key,
        path_resolution: resolved_path.resolution,
        input_color_resolution,
        proxy_generation,
    })
}

/// Resolve the exact input-color decision shared by Preview evaluation paths.
pub(crate) fn resolve_preview_input_color_space(
    override_color_space: Option<ColorSpace>,
    asset_interpretation: mondrian_core::timeline_data::AssetMediaInterpretation,
    detected_color_space: Option<ColorSpace>,
    color_context: &ColorContext,
) -> InputColorResolution {
    color_context.missing_metadata_policy.resolve_asset_input_decision(
        override_color_space,
        asset_interpretation,
        detected_color_space,
        color_context.working_color_space,
    )
}

fn resolve_preview_media_decode_path(
    prefer_proxy: bool,
    source_has_alpha: bool,
    source_path: &Path,
    proxy_config: &ProxyConfig,
    proxy_color: Option<ProxyColorContract>,
) -> Result<PreviewMediaDecodePath, String> {
    let source_fingerprint = media_path_fingerprint(source_path)
        .map_err(|error| format!("source metadata unavailable: {error}"))?;
    if !prefer_proxy || source_has_alpha {
        return Ok(source_decode_path(source_path, source_fingerprint));
    }
    let Some(proxy_color) = proxy_color else {
        return Ok(source_decode_path(source_path, source_fingerprint));
    };

    let proxy_generator = ProxyGenerator::new(proxy_config.clone());
    let proxy_path = proxy_generator
        .proxy_path(source_path, proxy_color)
        .map_err(|error| format!("proxy path resolution failed: {error}"))?;
    match (
        proxy_generator.proxy_status(source_path, proxy_color),
        media_path_fingerprint(&proxy_path),
    ) {
        (ProxyStatus::Fresh, Ok(proxy_fingerprint)) => Ok(PreviewMediaDecodePath {
            path: proxy_path,
            resolution: PreviewMediaDecodePathResolution::Proxy,
            fingerprint: proxy_fingerprint,
        }),
        (ProxyStatus::Missing, _) => Ok(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::ProxyMissing,
            fingerprint: source_fingerprint,
        }),
        (ProxyStatus::Stale, _) | (_, Err(_)) => Ok(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::ProxyStale,
            fingerprint: source_fingerprint,
        }),
    }
}

fn source_decode_path(
    source_path: &Path,
    fingerprint: MediaFileFingerprint,
) -> PreviewMediaDecodePath {
    PreviewMediaDecodePath {
        path: source_path.to_path_buf(),
        resolution: PreviewMediaDecodePathResolution::Source,
        fingerprint,
    }
}

fn media_path_fingerprint(path: &Path) -> std::io::Result<MediaFileFingerprint> {
    std::fs::metadata(path).map(|metadata| MediaFileFingerprint::from_metadata(&metadata))
}

fn proxy_generation_intent(
    request: &PreviewMediaSourceRequest<'_>,
    resolution: PreviewMediaDecodePathResolution,
    source_fingerprint: MediaFileFingerprint,
) -> Option<PreviewProxyGenerationIntent> {
    if !request.request_missing_proxy_generation
        || !request.prefer_proxy
        || !matches!(
            resolution,
            PreviewMediaDecodePathResolution::ProxyMissing
                | PreviewMediaDecodePathResolution::ProxyStale
        )
    {
        return None;
    }
    let color = request.proxy_color?;
    Some(PreviewProxyGenerationIntent {
        key: PreviewProxyGenerationRequestKey {
            asset_id: request.asset.id,
            source_fingerprint,
            resolution,
            color,
        },
        source_path: request.asset.path.clone(),
        config: request.proxy_config.clone(),
        color,
    })
}

fn canonicalize_media_decode_geometry(
    key: &mut MediaPreviewKey,
    hardware_admission: PreviewHardwareDecodeAdmissionState,
) {
    let native_source_decode = !key.source_has_alpha
        && hardware_admission.request_for_surface(key.native_surface_hint)
            == PreviewHardwareDecodeRequest::PreferGpuResident;
    if native_source_decode {
        key.target_width = key.source_width;
        key.target_height = key.source_height;
    }
}

fn unavailable(
    request: &PreviewMediaSourceRequest<'_>,
    reason: String,
) -> PreviewMediaSourceOutcome {
    PreviewMediaSourceOutcome::Unavailable(UnavailablePreviewMediaSource {
        asset_id: request.asset.id,
        path: request.asset.path.clone(),
        reason,
    })
}

fn source_color_diagnostic(asset: &AssetRecord) -> VideoColorDiagnostic {
    asset
        .media_info
        .primary_video()
        .map(VideoColorDiagnostic::from_stream)
        .unwrap_or_else(|| VideoColorDiagnostic {
            detected_color_space: None,
            color_range: DecodedVideoRange::Unknown,
            interpretation: mondrian_media::DetectedColorInterpretation {
                color_space: None,
                confidence: mondrian_media::VideoColorInterpretationConfidence::None,
                source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
                method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
                evidence: Vec::new(),
                warnings: Vec::new(),
                user_overridable: true,
            },
            source: mondrian_media::VideoColorSpaceSource::MissingMetadata,
            method: mondrian_media::VideoColorDetectionMethod::MissingMetadata,
            metadata: None,
            metadata_hints: Vec::new(),
            hdr_metadata: Vec::new(),
        })
}

#[cfg(test)]
#[path = "preview_media_source/tests.rs"]
mod tests;
