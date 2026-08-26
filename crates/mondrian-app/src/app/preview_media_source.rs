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
use mondrian_core::{
    PictureInterpretationOverrides, Resolution, ResolvedPictureGeometry, TimelineTime,
};
use mondrian_media::{
    DecodedVideoMatrix, DecodedVideoRangeContract, MediaFileFingerprint, PreviewDecodeKey,
    PreviewDecodePayloadRequirement, PreviewDecodeRepresentation, PreviewDecodeSource,
    PreviewSourceColorContract, ProxyArtifactManifest, ProxyColorContract, ProxyConfig,
    ProxyGenerator, ProxyStatus, VideoColorDiagnostic, VideoStreamInfo,
};
use mondrian_timeline::sequence::{
    InputColorResolution, InputColorResolutionSource, MediaInputColorContext, ResolvedInputColor,
};

use super::preview_access_mode::MediaPreviewKey;
use super::preview_hardware_admission::PreviewHardwareDecodeAdmissionState;

/// Source/proxy path selected for one Preview media request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreviewMediaDecodePath {
    /// Exact selected physical source, stream, revision, Alpha, and surface evidence.
    pub(crate) source: PreviewDecodeSource,
    pub(crate) resolution: PreviewMediaDecodePathResolution,
    /// Current bounded revision evidence for the canonical original source,
    /// even when [`Self::source`] selects a generated proxy.
    pub(crate) source_fingerprint: MediaFileFingerprint,
}

/// Why Preview decoded the source path or an optimized proxy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PreviewMediaDecodePathResolution {
    Source,
    Proxy,
    /// A fresh proxy existed but its source-referred color identity did not
    /// match the exact color contract requested by this Clip occurrence.
    ProxyColorIncompatible,
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
    pub(crate) picture_overrides: PictureInterpretationOverrides,
    pub(crate) source_sample: mondrian_core::SourceSampleTarget,
    pub(crate) input_color: &'a MediaInputColorContext,
    pub(crate) prefer_proxy: bool,
    pub(crate) request_missing_proxy_generation: bool,
    pub(crate) proxy_config: &'a ProxyConfig,
    pub(crate) proxy_color: Option<ProxyColorContract>,
    pub(crate) hardware_admission: PreviewHardwareDecodeAdmissionState,
    /// Bind CPU-addressability into the decoded-frame cache key.
    pub(crate) cpu_working_required: bool,
    /// Working raster quality for the media's own representation. A reduced
    /// quality is a decode-policy choice that keeps the media raster identity
    /// decoupled from every consumer/output extent.
    pub(crate) representation_quality: mondrian_media::PreviewRepresentationQuality,
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
    /// Physical source when the Asset is file-backed.
    pub(crate) path: Option<PathBuf>,
    pub(crate) reason: PreviewMediaSourceUnavailableReason,
}

/// Stable reason canonical Preview media-source resolution was blocked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PreviewMediaSourceUnavailableReason {
    #[error("asset is not a video source")]
    NotVideo,
    #[error("asset has no file-backed media source")]
    NoFileSource,
    #[error("asset has no coherent admitted media probe")]
    SourceProbeUnavailable,
    #[error("admitted media probe has no video stream")]
    SourceVideoStreamUnavailable,
    #[error(
        "admitted video stream has no proven, internally consistent sampling contract; open Interpret Asset and set source color space/range before Preview"
    )]
    SourceSamplingUnavailable,
    #[error("source file revision changed after the admitted media probe")]
    SourceRevisionChanged,
    #[error("negative source target is invalid: {source_time}")]
    NegativeSourceTime { source_time: TimelineTime },
    #[error("source metadata unavailable: {reason}")]
    SourceMetadataUnavailable { reason: String },
    #[error("proxy path resolution failed: {reason}")]
    ProxyPathResolutionFailed { reason: String },
    #[error("physical Preview decode contract is invalid: {reason}")]
    DecodeContractInvalid { reason: String },
    #[error("picture interpretation is unsupported: {reason}")]
    PictureInterpretationUnsupported { reason: String },
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
    if !matches!(request.asset.kind, AssetKind::Video | AssetKind::StillImage) {
        return unavailable(&request, PreviewMediaSourceUnavailableReason::NotVideo);
    }
    if request.source_sample.time().is_negative() {
        return unavailable(
            &request,
            PreviewMediaSourceUnavailableReason::NegativeSourceTime {
                source_time: request.source_sample.time(),
            },
        );
    }

    let Some(source_path) = request.asset.file_path() else {
        return unavailable(&request, PreviewMediaSourceUnavailableReason::NoFileSource);
    };
    let Some(media_probe) = request.asset.media_probe() else {
        return unavailable(
            &request,
            PreviewMediaSourceUnavailableReason::SourceProbeUnavailable,
        );
    };
    let Some(primary_video) = media_probe.primary_video() else {
        return unavailable(
            &request,
            PreviewMediaSourceUnavailableReason::SourceVideoStreamUnavailable,
        );
    };
    let Some(proven_sampling) = primary_video.proven_sampling() else {
        return unavailable(
            &request,
            PreviewMediaSourceUnavailableReason::SourceSamplingUnavailable,
        );
    };
    let executable_color_space = primary_video.executable_color_space();
    let input_color_resolution = resolve_preview_input_color_space(
        request.color_space_override,
        request.asset.interpretation,
        executable_color_space,
        request.input_color,
    );
    let input_color_space = match input_color_resolution.resolved {
        ResolvedInputColor::Color(color_space) => color_space,
        ResolvedInputColor::Data | ResolvedInputColor::Rejected => {
            return PreviewMediaSourceOutcome::ColorRejected(RejectedPreviewMediaSource {
                asset_id: request.asset.id,
                path: source_path.to_path_buf(),
                input_color_resolution,
                diagnostic: VideoColorDiagnostic::from_stream(primary_video),
            });
        }
    };
    let mut source_color = PreviewSourceColorContract::new(
        input_color_space,
        DecodedVideoRangeContract::from_interpretation(
            request.asset.interpretation.range,
            primary_video.color_range,
        ),
    );
    if input_color_resolution.source == InputColorResolutionSource::MissingPolicyAssumeRec709 {
        // "Assume Rec.709" must authorize the complete YUV-to-RGB
        // interpretation. Binding BT.709 here makes the fallback explicit in
        // the decode/cache identity instead of relying on swscale defaults.
        source_color = source_color.with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709);
    }
    let source_has_alpha = proven_sampling.has_alpha;
    let resolved_path = match resolve_preview_media_decode_path(
        request.prefer_proxy,
        source_has_alpha,
        source_path,
        primary_video,
        source_color,
        request.proxy_config,
        request.proxy_color,
    ) {
        Ok(path) => path,
        Err(reason) => return unavailable(&request, reason),
    };
    if request.asset.source_fingerprint() != Some(resolved_path.source_fingerprint) {
        return unavailable(
            &request,
            PreviewMediaSourceUnavailableReason::SourceRevisionChanged,
        );
    }
    let PreviewMediaDecodePath {
        source: decode_source,
        resolution: path_resolution,
        source_fingerprint,
    } = resolved_path;

    let source_resolution = Resolution {
        width: primary_video.width,
        height: primary_video.height,
    };
    let picture_geometry = match ResolvedPictureGeometry::resolve_with_overrides(
        source_resolution,
        primary_video.picture,
        request.picture_overrides,
    ) {
        Ok(geometry) => geometry,
        Err(error) => {
            return unavailable(
                &request,
                PreviewMediaSourceUnavailableReason::PictureInterpretationUnsupported {
                    reason: error.to_string(),
                },
            );
        }
    };
    let payload_requirement = if request.cpu_working_required {
        PreviewDecodePayloadRequirement::CpuAddressable
    } else {
        PreviewDecodePayloadRequirement::NativeAllowed
    };
    let hardware_request = request
        .hardware_admission
        .request_for_surface(decode_source.native_surface_hint());
    let representation = match PreviewDecodeRepresentation::canonical(
        &decode_source,
        payload_requirement,
        hardware_request,
        request.representation_quality,
        source_color,
    ) {
        Ok(representation) => representation,
        Err(error) => {
            return unavailable(
                &request,
                PreviewMediaSourceUnavailableReason::DecodeContractInvalid {
                    reason: error.to_string(),
                },
            );
        }
    };
    let decode = match PreviewDecodeKey::new(
        decode_source,
        request.source_sample,
        representation,
        source_color,
    ) {
        Ok(decode) => decode,
        Err(error) => {
            return unavailable(
                &request,
                PreviewMediaSourceUnavailableReason::DecodeContractInvalid {
                    reason: error.to_string(),
                },
            );
        }
    };
    let key = MediaPreviewKey {
        asset_id: request.asset.id,
        decode,
        source_resolution,
        picture_geometry,
        alpha_interpretation: request.alpha_interpretation,
        working_color_space: request.input_color.working_color_space,
        input_tone_map: request.input_color.input_tone_map,
        engine: request.input_color.engine.clone(),
    };

    let proxy_generation =
        proxy_generation_intent(&request, source_path, path_resolution, source_fingerprint);
    PreviewMediaSourceOutcome::Ready(ResolvedPreviewMediaSource {
        key,
        path_resolution,
        input_color_resolution,
        proxy_generation,
    })
}

/// Resolve the exact input-color decision shared by Preview evaluation paths.
pub(crate) fn resolve_preview_input_color_space(
    override_color_space: Option<ColorSpace>,
    asset_interpretation: mondrian_core::timeline_data::AssetMediaInterpretation,
    executable_color_space: Option<ColorSpace>,
    input_color: &MediaInputColorContext,
) -> InputColorResolution {
    input_color.missing_metadata_policy.resolve_asset_input_decision(
        override_color_space,
        asset_interpretation,
        executable_color_space,
        input_color.working_color_space,
    )
}

fn resolve_preview_media_decode_path(
    prefer_proxy: bool,
    source_has_alpha: bool,
    source_path: &Path,
    primary_video: &VideoStreamInfo,
    source_color: PreviewSourceColorContract,
    proxy_config: &ProxyConfig,
    proxy_color: Option<ProxyColorContract>,
) -> Result<PreviewMediaDecodePath, PreviewMediaSourceUnavailableReason> {
    let source_fingerprint = media_path_fingerprint(source_path).map_err(|error| {
        PreviewMediaSourceUnavailableReason::SourceMetadataUnavailable { reason: error.to_string() }
    })?;
    if !prefer_proxy || source_has_alpha {
        return source_decode_path(source_path, source_fingerprint, primary_video);
    }
    let Some(proxy_color) = proxy_color else {
        return source_decode_path(source_path, source_fingerprint, primary_video);
    };
    if proxy_color.source_color_space() != source_color.color_space
        || proxy_color.source_range() != source_color.range.baseline()
    {
        return source_decode_path_with_resolution(
            source_path,
            source_fingerprint,
            primary_video,
            PreviewMediaDecodePathResolution::ProxyColorIncompatible,
        );
    }

    let proxy_generator = ProxyGenerator::new(proxy_config.clone());
    let proxy_path = proxy_generator.proxy_path(source_path, proxy_color).map_err(|error| {
        PreviewMediaSourceUnavailableReason::ProxyPathResolutionFailed { reason: error.to_string() }
    })?;
    let proxy_status = proxy_generator
        .proxy_status_for_source_fingerprint(source_path, source_fingerprint, proxy_color)
        .map_err(
            |error| PreviewMediaSourceUnavailableReason::ProxyPathResolutionFailed {
                reason: error.to_string(),
            },
        )?;
    match (proxy_status, media_path_fingerprint(&proxy_path)) {
        (ProxyStatus::Fresh, Ok(proxy_fingerprint)) => {
            let manifest: ProxyArtifactManifest =
                proxy_generator.expected_manifest(source_path, proxy_color).map_err(|error| {
                    PreviewMediaSourceUnavailableReason::ProxyPathResolutionFailed {
                        reason: error.to_string(),
                    }
                })?;
            if manifest.color != proxy_color {
                return Err(
                    PreviewMediaSourceUnavailableReason::ProxyPathResolutionFailed {
                        reason: "fresh proxy manifest color differs from requested proxy identity"
                            .to_owned(),
                    },
                );
            }
            let proxy_height = manifest.settings.resolution.height();
            let proxy_extent = fit_proxy_extent(
                Resolution {
                    width: primary_video.width,
                    height: primary_video.height,
                },
                proxy_height,
            );
            let source = PreviewDecodeSource::from_proxy_artifact(
                proxy_path,
                proxy_fingerprint,
                &manifest,
                proxy_extent,
            )
            .map_err(decode_contract_unavailable)?;
            Ok(PreviewMediaDecodePath {
                source,
                resolution: PreviewMediaDecodePathResolution::Proxy,
                source_fingerprint,
            })
        }
        (ProxyStatus::Missing, _) => source_decode_path_with_resolution(
            source_path,
            source_fingerprint,
            primary_video,
            PreviewMediaDecodePathResolution::ProxyMissing,
        ),
        (ProxyStatus::Stale, _) | (_, Err(_)) => source_decode_path_with_resolution(
            source_path,
            source_fingerprint,
            primary_video,
            PreviewMediaDecodePathResolution::ProxyStale,
        ),
    }
}

fn source_decode_path(
    source_path: &Path,
    fingerprint: MediaFileFingerprint,
    primary_video: &VideoStreamInfo,
) -> Result<PreviewMediaDecodePath, PreviewMediaSourceUnavailableReason> {
    source_decode_path_with_resolution(
        source_path,
        fingerprint,
        primary_video,
        PreviewMediaDecodePathResolution::Source,
    )
}

fn source_decode_path_with_resolution(
    source_path: &Path,
    fingerprint: MediaFileFingerprint,
    primary_video: &VideoStreamInfo,
    resolution: PreviewMediaDecodePathResolution,
) -> Result<PreviewMediaDecodePath, PreviewMediaSourceUnavailableReason> {
    let source = PreviewDecodeSource::from_probed_stream(source_path, fingerprint, primary_video)
        .map_err(decode_contract_unavailable)?;
    Ok(PreviewMediaDecodePath {
        source,
        resolution,
        source_fingerprint: fingerprint,
    })
}

fn media_path_fingerprint(path: &Path) -> std::io::Result<MediaFileFingerprint> {
    let fingerprint = MediaFileFingerprint::capture(path);
    if fingerprint.authorizes_reuse() {
        Ok(fingerprint)
    } else {
        Err(std::io::Error::other(
            "media source lacks complete filesystem revision evidence",
        ))
    }
}

/// Aspect-preserving proxy raster extent for a source extent and proxy height.
///
/// This is the artifact's own decode representation extent, never an
/// output/consumer extent.
fn fit_proxy_extent(source: Resolution, proxy_height: u32) -> Resolution {
    let scale = f64::from(proxy_height) / f64::from(source.height).max(1.0);
    let mut width = (f64::from(source.width) * scale).round().max(1.0) as u32;
    let mut height = (f64::from(source.height) * scale).round().max(1.0) as u32;
    if width % 2 == 1 {
        width = width.saturating_sub(1).max(1);
    }
    if height % 2 == 1 {
        height = height.saturating_sub(1).max(1);
    }
    Resolution { width, height }
}

fn proxy_generation_intent(
    request: &PreviewMediaSourceRequest<'_>,
    source_path: &Path,
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
        source_path: source_path.to_path_buf(),
        config: request.proxy_config.clone(),
        color,
    })
}

fn decode_contract_unavailable(
    error: mondrian_media::PreviewDecodeContractError,
) -> PreviewMediaSourceUnavailableReason {
    PreviewMediaSourceUnavailableReason::DecodeContractInvalid { reason: error.to_string() }
}

fn unavailable(
    request: &PreviewMediaSourceRequest<'_>,
    reason: PreviewMediaSourceUnavailableReason,
) -> PreviewMediaSourceOutcome {
    PreviewMediaSourceOutcome::Unavailable(UnavailablePreviewMediaSource {
        asset_id: request.asset.id,
        path: request.asset.file_path().map(Path::to_path_buf),
        reason,
    })
}

#[cfg(test)]
#[path = "preview_media_source/tests.rs"]
mod tests;
