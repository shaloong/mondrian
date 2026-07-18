//! Application media adaptation for Viewer preview.
//!
//! This Module owns asset lookup, proxy/source selection, media fingerprinting,
//! input-color interpretation, decode-key construction, and cache admission.

use super::*;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PreviewMediaDecodePath {
    pub(super) path: PathBuf,
    pub(super) resolution: PreviewMediaDecodePathResolution,
    pub(super) fingerprint: PreviewFileFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum PreviewMediaDecodePathResolution {
    Source,
    Proxy,
    ProxyMissing,
    ProxyStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct PreviewProxyGenerationRequestKey {
    pub(super) asset_id: AssetId,
    pub(super) source_fingerprint: PreviewFileFingerprint,
    pub(super) resolution: PreviewMediaDecodePathResolution,
    pub(super) color: mondrian_media::ProxyColorContract,
}

pub(super) fn should_request_preview_proxy_generation(
    request_missing_proxy_generation: bool,
    project_proxy_enabled: bool,
    asset_proxy_mode: bool,
    resolution: PreviewMediaDecodePathResolution,
) -> bool {
    request_missing_proxy_generation
        && project_proxy_enabled
        && asset_proxy_mode
        && matches!(
            resolution,
            PreviewMediaDecodePathResolution::ProxyMissing
                | PreviewMediaDecodePathResolution::ProxyStale
        )
}

pub(super) fn resolve_preview_media_decode_path(
    prefer_proxy: bool,
    source_has_alpha: bool,
    source_path: &Path,
    proxy_config: &mondrian_media::ProxyConfig,
    proxy_color: Option<mondrian_media::ProxyColorContract>,
) -> Option<PreviewMediaDecodePath> {
    let source_metadata = media_path_metadata(source_path)?;
    if !prefer_proxy || source_has_alpha {
        return Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::Source,
            fingerprint: source_metadata.fingerprint,
        });
    }
    let Some(proxy_color) = proxy_color else {
        return Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::Source,
            fingerprint: source_metadata.fingerprint,
        });
    };
    let proxy_generator = mondrian_media::ProxyGenerator::new(proxy_config.clone());
    let proxy_path = proxy_generator.proxy_path(source_path, proxy_color).ok()?;
    match (
        proxy_generator.proxy_status(source_path, proxy_color),
        media_path_metadata(&proxy_path),
    ) {
        (mondrian_media::ProxyStatus::Fresh, Some(proxy_metadata)) => {
            Some(PreviewMediaDecodePath {
                path: proxy_path,
                resolution: PreviewMediaDecodePathResolution::Proxy,
                fingerprint: proxy_metadata.fingerprint,
            })
        }
        (mondrian_media::ProxyStatus::Missing, _) => Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::ProxyMissing,
            fingerprint: source_metadata.fingerprint,
        }),
        (mondrian_media::ProxyStatus::Stale, _) | (_, None) => Some(PreviewMediaDecodePath {
            path: source_path.to_path_buf(),
            resolution: PreviewMediaDecodePathResolution::ProxyStale,
            fingerprint: source_metadata.fingerprint,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaPathMetadata {
    fingerprint: PreviewFileFingerprint,
}

fn media_path_metadata(path: &Path) -> Option<MediaPathMetadata> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(MediaPathMetadata {
        fingerprint: PreviewFileFingerprint::from_metadata(&metadata),
    })
}

pub(super) fn source_micros(source_secs: f64) -> i64 {
    (source_secs.max(0.0) * 1_000_000.0).round() as i64
}

impl AppUiPreviewService {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn media_frame_for_plan(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        alpha_interpretation: AlphaInterpretation,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
        _sequence_frame_rate: Rational,
    ) -> Option<MediaPreviewFrame> {
        let access_mode = media_preview_access_mode_for_intent(media_preview_viewer_access_intent(
            state.is_playing(),
            state.last_timeline_seek_source,
        ));
        let (key, source_secs) = self.media_preview_key_for_asset(
            state,
            asset_id,
            color_space_override,
            alpha_interpretation,
            source_frame,
            source_secs,
            target_width,
            target_height,
            color_context,
            true,
            access_mode == PreviewDecodeAccessMode::PlaybackCursor,
        )?;
        if let Some(frame) = self.cached_media_frame(&key) {
            return Some(frame);
        }
        if self.failed_media_key(&key) {
            return None;
        }
        self.execution.borrow_mut().set_pending(true);
        let adaptive_hints = self.preview_decode_adaptive_hints(access_mode, &key);
        self.request_media_preview(
            key,
            source_secs,
            MediaPreviewRequestPriority::Current,
            access_mode,
            (access_mode == PreviewDecodeAccessMode::PlaybackCursor)
                .then(|| state.playback_frame_deadline_at(Instant::now()))
                .flatten(),
            (access_mode == PreviewDecodeAccessMode::PlaybackCursor)
                .then(|| state.pending_playback_frame_demand_identity())
                .flatten(),
            adaptive_hints,
        );
        None
    }

    pub(super) fn cached_media_frame(&self, key: &MediaPreviewKey) -> Option<MediaPreviewFrame> {
        let frame = self.frame_store.borrow_mut().media_frame(key);
        if frame.is_some() {
            bump(&self.metrics.media_cache_hits);
        } else {
            bump(&self.metrics.media_cache_misses);
        }
        frame
    }

    pub(super) fn failed_media_key(&self, key: &MediaPreviewKey) -> bool {
        let failed = self.frame_store.borrow_mut().contains_failure(key);
        if failed {
            bump(&self.metrics.media_failure_hits);
        }
        failed
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn media_preview_key_for_asset(
        &self,
        state: &AppState,
        asset_id: &AssetId,
        color_space_override: Option<ColorSpace>,
        alpha_interpretation: AlphaInterpretation,
        source_frame: i64,
        source_secs: f64,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
        record_color_rejection: bool,
        request_missing_proxy_generation: bool,
    ) -> Option<(MediaPreviewKey, f64)> {
        let library = state.asset_library.as_ref()?;
        let asset = match library.get_asset(*asset_id) {
            Ok(Some(asset)) if asset.kind == AssetKind::Video => asset,
            Ok(_) => return None,
            Err(err) => {
                tracing::debug!(asset_id = %asset_id, "viewer preview asset lookup failed: {err}");
                return None;
            }
        };
        let proxy_config = state.proxy_config();
        let source_has_alpha =
            asset.media_info.primary_video().is_some_and(|video| video.has_alpha);
        let proxy_color = resolve_asset_proxy_color_contract(&asset, color_context).ok();
        let resolved_path = resolve_preview_media_decode_path(
            state.project_settings.proxy_enabled && state.is_asset_proxy_mode(*asset_id),
            source_has_alpha,
            &asset.path,
            &proxy_config,
            proxy_color,
        )?;
        match resolved_path.resolution {
            PreviewMediaDecodePathResolution::Proxy => bump(&self.metrics.media_proxy_path_hits),
            PreviewMediaDecodePathResolution::ProxyMissing => {
                bump(&self.metrics.media_proxy_path_misses);
            }
            PreviewMediaDecodePathResolution::ProxyStale => {
                bump(&self.metrics.media_proxy_path_stale);
            }
            PreviewMediaDecodePathResolution::Source => {
                bump(&self.metrics.media_proxy_path_bypasses);
            }
        }
        self.maybe_request_preview_proxy_generation(
            request_missing_proxy_generation,
            state,
            *asset_id,
            &asset.path,
            &proxy_config,
            &resolved_path,
            proxy_color,
        );

        let detected_color_space = asset
            .media_info
            .video_streams
            .first()
            .and_then(|video| video.detected_color_space);
        let input_color_resolution = resolve_preview_input_color_space(
            color_space_override,
            asset.interpretation,
            detected_color_space,
            color_context,
        );
        self.record_input_color_resolution(input_color_resolution.source);
        let input_color_space = match input_color_resolution.resolved {
            ResolvedInputColor::Color(color_space) => color_space,
            ResolvedInputColor::Data | ResolvedInputColor::Rejected => {
                let diagnostic = asset
                    .media_info
                    .primary_video()
                    .map(VideoColorDiagnostic::from_stream)
                    .unwrap_or_else(|| VideoColorDiagnostic {
                        detected_color_space: None,
                        color_range: mondrian_media::DecodedVideoRange::Unknown,
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
                    });
                let diagnostic_summary = diagnostic.summary();
                let diagnostic_issue_summary = diagnostic.issue_summary();
                if record_color_rejection {
                    self.record_color_rejection(AppUiPreviewColorRejection::new(
                        *asset_id,
                        asset.path.clone(),
                        input_color_resolution,
                        diagnostic_summary.clone(),
                        diagnostic_issue_summary,
                    ));
                }
                tracing::warn!(
                    asset_id = %asset_id,
                    path = %asset.path.display(),
                    missing_metadata_policy = ?color_context.missing_metadata_policy,
                    color_resolution_source = ?input_color_resolution.source,
                    override_color_space = ?input_color_resolution.override_color_space,
                    detected_color_space = ?input_color_resolution.detected_color_space,
                    working_color_space = ?input_color_resolution.working_color_space,
                    color_diagnostic = %diagnostic_summary,
                    color_diagnostic_issue_summary = ?diagnostic_issue_summary,
                    "viewer preview rejected media with missing color metadata"
                );
                return None;
            }
        };
        let key = MediaPreviewKey {
            asset_id: *asset_id,
            path: resolved_path.path,
            fingerprint: Some(resolved_path.fingerprint),
            source_frame: source_frame.max(0),
            source_micros: source_micros(source_secs),
            target_width,
            target_height,
            source_width: asset
                .media_info
                .primary_video()
                .map_or(target_width, |video| video.width.max(1)),
            source_height: asset
                .media_info
                .primary_video()
                .map_or(target_height, |video| video.height.max(1)),
            input_color_space,
            input_video_range: DecodedVideoRangeContract::from_interpretation(
                asset.interpretation.range,
                asset
                    .media_info
                    .primary_video()
                    .map(|video| video.color_range)
                    .unwrap_or(DecodedVideoRange::Unknown),
            ),
            native_surface_hint: asset.media_info.primary_video().and_then(|video| {
                match video.pixel_format {
                    mondrian_media::info::PixelFormat::Yuv420p
                    | mondrian_media::info::PixelFormat::Nv12 => {
                        Some(MediaPreviewNativeSurfaceHint::Nv12)
                    }
                    mondrian_media::info::PixelFormat::Yuv420p10le
                    | mondrian_media::info::PixelFormat::P010 => {
                        Some(MediaPreviewNativeSurfaceHint::P010)
                    }
                    _ => None,
                }
            }),
            source_has_alpha,
            alpha_interpretation,
            working_color_space: color_context.working_color_space,
            tone_map: color_context.tone_map,
            engine: color_context.engine.clone(),
            ocio_generation: mondrian_core::ocio_config_generation(),
        };
        Some((
            self.canonicalize_media_decode_geometry(key),
            source_secs.max(0.0),
        ))
    }

    /// Keep native decoded-surface identity independent of Viewer output scale.
    ///
    /// A GPU-resident NV12/P010 decode is source-sized; Half/Quarter playback
    /// quality changes only the later Viewer presentation extent. Encoding the
    /// presentation size in that decode key would discard valid lookahead and
    /// turn one late presentation into a cache-invalidating feedback loop.
    pub(super) fn canonicalize_media_decode_geometry(
        &self,
        mut key: MediaPreviewKey,
    ) -> MediaPreviewKey {
        let native_source_decode = !key.source_has_alpha
            && self.hardware_decode_request_for_key(PreviewDecodeAccessMode::PlaybackCursor, &key)
                == PreviewHardwareDecodeRequest::PreferGpuResident;
        if native_source_decode {
            key.target_width = key.source_width;
            key.target_height = key.source_height;
        }
        key
    }

    fn maybe_request_preview_proxy_generation(
        &self,
        request_missing_proxy_generation: bool,
        state: &AppState,
        asset_id: AssetId,
        source_path: &Path,
        proxy_config: &mondrian_media::ProxyConfig,
        resolved_path: &PreviewMediaDecodePath,
        proxy_color: Option<mondrian_media::ProxyColorContract>,
    ) {
        if !should_request_preview_proxy_generation(
            request_missing_proxy_generation,
            state.project_settings.proxy_enabled,
            state.is_asset_proxy_mode(asset_id),
            resolved_path.resolution,
        ) {
            return;
        }
        let Some(proxy_color) = proxy_color else {
            return;
        };

        let request_key = PreviewProxyGenerationRequestKey {
            asset_id,
            source_fingerprint: resolved_path.fingerprint,
            resolution: resolved_path.resolution,
            color: proxy_color,
        };
        if !self.requested_proxy_generations.borrow_mut().insert(request_key) {
            bump(&self.metrics.media_proxy_generation_request_dedupes);
            return;
        }

        bump(&self.metrics.media_proxy_generation_requests);
        request_proxy_generation(
            asset_id,
            source_path.to_path_buf(),
            proxy_config.clone(),
            proxy_color,
        );
    }
}
