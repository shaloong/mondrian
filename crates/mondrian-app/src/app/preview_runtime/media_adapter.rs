//! Application media adaptation for production Preview.
//!
//! This production Adapter owns asset-library lookup, cache observation, proxy-job
//! dispatch, and diagnostics projection. Canonical media-source interpretation
//! lives in `app::preview_media_source`.

use super::*;
use crate::app::preview_media_source::{
    resolve_preview_media_source, PreviewMediaDecodePathResolution, PreviewMediaSourceOutcome,
    PreviewMediaSourceRequest, PreviewProxyGenerationIntent,
};
use crate::app::preview_timeline_execution::{
    PreviewTimelineMediaFrame, PreviewTimelineMediaRequest,
};
use mondrian_core::TimelineTime;

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(super) fn media_frame_for_plan(
        &self,
        state: &AppState,
        request: PreviewTimelineMediaRequest,
    ) -> PreviewTimelineMediaFrame {
        let access_mode = media_preview_access_mode_for_intent(media_preview_viewer_access_intent(
            state.is_playing(),
            state.last_timeline_seek_source,
        ));
        let Some(key) = self.media_preview_key_for_asset(
            state,
            &request.asset_id,
            request.color_space_override,
            request.alpha_interpretation,
            request.source_time,
            request.target_resolution.width,
            request.target_resolution.height,
            &request.color_context,
            true,
            access_mode == PreviewDecodeAccessMode::PlaybackCursor,
        ) else {
            return PreviewTimelineMediaFrame::Unavailable {
                reason: "media source resolution failed".to_owned(),
            };
        };
        if let Some(frame) = self.cached_media_frame(&key) {
            return PreviewTimelineMediaFrame::Ready(frame);
        }
        if self.failed_media_key(&key) {
            return PreviewTimelineMediaFrame::Unavailable {
                reason: "media key is in terminal failure memory".to_owned(),
            };
        }
        self.execution.borrow_mut().set_pending(true);
        let adaptive_hints = self.preview_decode_adaptive_hints(access_mode, &key);
        self.request_media_preview(
            key,
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
        PreviewTimelineMediaFrame::Pending
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
        source_time: TimelineTime,
        target_width: u32,
        target_height: u32,
        color_context: &ColorContext,
        record_color_rejection: bool,
        request_missing_proxy_generation: bool,
    ) -> Option<MediaPreviewKey> {
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
        let proxy_color = resolve_asset_proxy_color_contract(&asset, color_context).ok();
        let prefer_proxy =
            state.project_settings.proxy_enabled && state.is_asset_proxy_mode(*asset_id);
        match resolve_preview_media_source(PreviewMediaSourceRequest {
            asset: &asset,
            color_space_override,
            alpha_interpretation,
            source_time,
            target_resolution: Resolution { width: target_width, height: target_height },
            color_context,
            prefer_proxy,
            request_missing_proxy_generation,
            proxy_config: &proxy_config,
            proxy_color,
            hardware_admission: self.hardware_decode_admission.get(),
        }) {
            PreviewMediaSourceOutcome::Ready(resolved) => {
                match resolved.path_resolution {
                    PreviewMediaDecodePathResolution::Proxy => {
                        bump(&self.metrics.media_proxy_path_hits);
                    }
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
                self.record_input_color_resolution(resolved.input_color_resolution.source);
                self.maybe_request_preview_proxy_generation(state, resolved.proxy_generation);
                Some(resolved.key)
            }
            PreviewMediaSourceOutcome::ColorRejected(rejection) => {
                let resolution = rejection.input_color_resolution;
                self.record_input_color_resolution(resolution.source);
                let diagnostic_summary = rejection.diagnostic.summary();
                let diagnostic_issue_summary = rejection.diagnostic.issue_summary();
                if record_color_rejection {
                    self.record_color_rejection(PreviewColorRejection::new(
                        rejection.asset_id,
                        rejection.path.clone(),
                        resolution,
                        diagnostic_summary.clone(),
                        diagnostic_issue_summary,
                    ));
                }
                tracing::warn!(
                    asset_id = %rejection.asset_id,
                    path = %rejection.path.display(),
                    missing_metadata_policy = ?color_context.missing_metadata_policy,
                    color_resolution_source = ?resolution.source,
                    override_color_space = ?resolution.override_color_space,
                    detected_color_space = ?resolution.detected_color_space,
                    working_color_space = ?resolution.working_color_space,
                    color_diagnostic = %diagnostic_summary,
                    color_diagnostic_issue_summary = ?diagnostic_issue_summary,
                    "viewer preview rejected media with missing color metadata"
                );
                None
            }
            PreviewMediaSourceOutcome::Unavailable(unavailable) => {
                tracing::debug!(
                    asset_id = %unavailable.asset_id,
                    path = %unavailable.path.display(),
                    reason = %unavailable.reason,
                    "viewer preview media source is unavailable"
                );
                None
            }
        }
    }

    fn maybe_request_preview_proxy_generation(
        &self,
        state: &AppState,
        intent: Option<PreviewProxyGenerationIntent>,
    ) {
        let Some(intent) = intent else {
            return;
        };

        let outcome = state.request_proxy_generation(
            intent.key.asset_id,
            intent.source_path,
            intent.config,
            intent.color,
            ProxyGenerationOrigin::PlaybackRecovery,
        );
        match outcome {
            ProxyGenerationRequestOutcome::Admitted { .. } => {
                bump(&self.metrics.media_proxy_generation_requests);
            }
            ProxyGenerationRequestOutcome::AlreadyFresh
            | ProxyGenerationRequestOutcome::Deduplicated { .. }
            | ProxyGenerationRequestOutcome::RetainedFailure(_) => {
                bump(&self.metrics.media_proxy_generation_request_dedupes);
            }
            ProxyGenerationRequestOutcome::Failed(failure) => {
                tracing::warn!(
                    target: "mondrian::proxy",
                    asset_id = %intent.key.asset_id,
                    reason = failure.reason.code(),
                    detail = %failure.detail,
                    "preview proxy generation request failed"
                );
            }
        }
    }
}
