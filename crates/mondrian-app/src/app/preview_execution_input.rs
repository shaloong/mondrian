//! App composition adapter for immutable production Preview input.

use std::time::Instant;

use crate::app::preview_media_source::PreviewProxyGenerationIntent;
use crate::app::preview_runtime::{
    PreviewAuthoringSnapshot, PreviewExecutionSnapshot, PreviewFrameDemandSnapshot,
    PreviewFrameExecutionRequest, PreviewProxyDemandSink, PreviewProxySelectionSnapshot,
    PreviewTransportSnapshot, PreviewVideoPrerollRequest,
};
use crate::app::proxy_generation::{ProxyGenerationOrigin, ProxyGenerationRequestOutcome};
use crate::app::AppState;

impl AppState {
    /// Capture one borrowed, immutable and internally coherent Preview input.
    pub(crate) fn preview_execution_snapshot(
        &self,
        sampled_at: Instant,
    ) -> PreviewExecutionSnapshot<'_> {
        let authoring = self.authoring.as_ref().map(|session| {
            let document = session.document();
            PreviewAuthoringSnapshot::new(
                session.session_id(),
                session.author_generation().get(),
                &document.sequences,
                &document.color_environment,
                session.asset_library().as_ref(),
                PreviewProxySelectionSnapshot::new(
                    &document.settings,
                    &document.proxy_mode_assets,
                    self.proxy_config(),
                ),
            )
        });
        let playback = self.playback_engine.snapshot();
        let demand = self.playback_engine.pending_frame_demand().map(|demand| {
            PreviewFrameDemandSnapshot::new(demand, self.playback_frame_deadline_at(sampled_at))
        });
        let transport = PreviewTransportSnapshot::new(
            playback.state,
            playback.position,
            playback.rate,
            playback.epoch,
            playback.quality_revision,
            self.playback_preview_resolution_scale(),
            self.last_timeline_seek_source,
            demand,
        );
        PreviewExecutionSnapshot::new(authoring, transport, &self.viewer_display_management)
    }

    /// Capture a frame-producing request with the App's narrow Proxy command Adapter.
    pub(crate) fn preview_frame_execution_request(
        &self,
        sampled_at: Instant,
    ) -> PreviewFrameExecutionRequest<'_> {
        PreviewFrameExecutionRequest::new(self.preview_execution_snapshot(sampled_at), self)
    }

    /// Capture ticketless preparation for the exact immediate playback successor.
    pub(crate) fn preview_successor_execution_request(
        &self,
        sampled_at: Instant,
    ) -> Option<PreviewFrameExecutionRequest<'_>> {
        PreviewFrameExecutionRequest::successor(self.preview_execution_snapshot(sampled_at), self)
    }

    /// Capture ticketless CPU preparation for a bounded future playback frame.
    pub(crate) fn preview_lookahead_execution_request(
        &self,
        sampled_at: Instant,
        offset: usize,
    ) -> Option<PreviewFrameExecutionRequest<'_>> {
        PreviewFrameExecutionRequest::lookahead(
            self.preview_execution_snapshot(sampled_at),
            self,
            offset,
        )
    }

    /// Recapture preroll facts after terminal Frame Deliveries were applied.
    pub(crate) fn preview_video_preroll_request(
        &self,
        sampled_at: Instant,
    ) -> PreviewVideoPrerollRequest<'_> {
        PreviewVideoPrerollRequest::new(
            self.preview_execution_snapshot(sampled_at),
            self.playback_engine.frame_demand().map(|demand| demand.identity()),
            self,
        )
    }
}

impl PreviewProxyDemandSink for AppState {
    fn request_preview_proxy(
        &self,
        intent: PreviewProxyGenerationIntent,
    ) -> ProxyGenerationRequestOutcome {
        self.request_proxy_generation(
            intent.key.asset_id,
            intent.source_path,
            intent.config,
            intent.color,
            ProxyGenerationOrigin::PlaybackRecovery,
        )
    }
}
