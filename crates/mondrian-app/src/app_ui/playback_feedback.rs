//! Typed adaptation from Viewer lifecycle state to playback observations.
//!
//! This module deliberately owns no transport or preview work. It prevents UI
//! lifecycle details from leaking into the Playback Engine Interface.

use super::panels::{ViewerPanelModel, ViewerPreviewState};

/// Playback-relevant meaning of the current Viewer lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerPlaybackFeedback {
    /// No output target is active, so no Program frame is expected.
    #[default]
    Unavailable,
    /// Current-frame work is pending; this is not a terminal Frame Delivery.
    Loading,
    /// A prior frame remains visible but cannot satisfy current readiness.
    Stale,
    /// The current target frame is ready.
    Ready,
    /// Correctness policy rejected the current frame.
    Blocked,
}

impl ViewerPlaybackFeedback {
    /// Adapt the Viewer model without exposing its frame payload to transport.
    pub fn from_viewer_model(model: &ViewerPanelModel) -> Self {
        if model.color_rejection.is_some() && model.frame_content.is_none() {
            return Self::Blocked;
        }
        match model.preview_state_kind() {
            ViewerPreviewStateKind::Unavailable => Self::Unavailable,
            ViewerPreviewStateKind::Loading => Self::Loading,
            ViewerPreviewStateKind::Stale => Self::Stale,
            ViewerPreviewStateKind::Ready => Self::Ready,
        }
    }

    /// Adapt the raw preview lifecycle for headless/perf Adapters.
    pub fn from_preview_state(state: &ViewerPreviewState) -> Self {
        match state {
            ViewerPreviewState::Unavailable(_) => Self::Unavailable,
            ViewerPreviewState::Transparent => Self::Ready,
            ViewerPreviewState::StaleTransparent => Self::Stale,
            ViewerPreviewState::Loading => Self::Loading,
            ViewerPreviewState::Stale(_) => Self::Stale,
            ViewerPreviewState::Ready(_) => Self::Ready,
        }
    }

    /// Whether redraw should avoid synchronously rebuilding the same pending GPU candidate.
    pub const fn should_defer_gpu_prepare(self) -> bool {
        matches!(self, Self::Loading)
    }
}

/// Payload-free classification used by the feedback Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerPreviewStateKind {
    /// No preview expected.
    Unavailable,
    /// Current work pending.
    Loading,
    /// Previous frame retained.
    Stale,
    /// Current frame ready.
    Ready,
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::RasterImageColorSpace;
    use mondrian_ui_widgets::{ViewerFrameContent, ViewerFrameImage};

    fn raster_content(key: &str) -> ViewerFrameContent {
        ViewerFrameContent::Raster(
            ViewerFrameImage::new(key, 1, 1, RasterImageColorSpace::Srgb, vec![0, 0, 0, 255])
                .expect("valid test raster"),
        )
    }

    #[test]
    fn only_loading_defers_duplicate_gpu_prepare() {
        assert!(ViewerPlaybackFeedback::Loading.should_defer_gpu_prepare());
        assert!(!ViewerPlaybackFeedback::Ready.should_defer_gpu_prepare());
        assert!(!ViewerPlaybackFeedback::Stale.should_defer_gpu_prepare());
        assert!(!ViewerPlaybackFeedback::Blocked.should_defer_gpu_prepare());
    }

    #[test]
    fn raw_preview_adapter_preserves_ready_loading_and_stale_meanings() {
        assert_eq!(
            ViewerPlaybackFeedback::from_preview_state(&ViewerPreviewState::Unavailable(
                crate::app::preview_unavailability::PreviewUnavailability::no_content(
                    crate::app::preview_unavailability::PreviewOutputStage::TimelineEvaluation,
                    "test no content",
                ),
            )),
            ViewerPlaybackFeedback::Unavailable
        );
        assert_eq!(
            ViewerPlaybackFeedback::from_preview_state(&ViewerPreviewState::Loading),
            ViewerPlaybackFeedback::Loading
        );
        assert_eq!(
            ViewerPlaybackFeedback::from_preview_state(&ViewerPreviewState::Stale(raster_content(
                "stale"
            ))),
            ViewerPlaybackFeedback::Stale
        );
        assert_eq!(
            ViewerPlaybackFeedback::from_preview_state(&ViewerPreviewState::Ready(raster_content(
                "ready"
            ))),
            ViewerPlaybackFeedback::Ready
        );
    }
}
