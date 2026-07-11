//! Typed adaptation from Viewer lifecycle state to playback observations.
//!
//! This module deliberately owns no transport or preview work. It prevents UI
//! lifecycle details from leaking into the Playback Engine Interface.

use super::panels::{ViewerPanelModel, ViewerPreviewState};
use mondrian_playback::FrameDeliveryKind;

/// Playback-relevant meaning of the current Viewer lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerPlaybackFeedback {
    /// No media frame is expected, such as an empty sequence.
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
            ViewerPreviewState::Unavailable => Self::Unavailable,
            ViewerPreviewState::Loading => Self::Loading,
            ViewerPreviewState::Stale(_) => Self::Stale,
            ViewerPreviewState::Ready(_) => Self::Ready,
        }
    }

    /// Convert only payload-free correctness feedback into Playback Engine vocabulary.
    ///
    /// A stale raster describes what remains visible while current work is
    /// pending. It must not consume the current Frame Demand before its worker
    /// can return Ready/Late/Canceled.
    pub const fn terminal_delivery(self) -> Option<FrameDeliveryKind> {
        match self {
            Self::Blocked => Some(FrameDeliveryKind::Blocked),
            Self::Unavailable | Self::Loading | Self::Stale | Self::Ready => None,
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
    fn loading_is_not_falsely_reported_as_terminal_delivery() {
        assert_eq!(ViewerPlaybackFeedback::Loading.terminal_delivery(), None);
        assert!(ViewerPlaybackFeedback::Loading.should_defer_gpu_prepare());
    }

    #[test]
    fn ready_lifecycle_requires_an_exact_presentation_delivery() {
        assert_eq!(ViewerPlaybackFeedback::Ready.terminal_delivery(), None);
    }

    #[test]
    fn stale_visibility_does_not_consume_current_demand_but_blocked_is_terminal() {
        assert_eq!(ViewerPlaybackFeedback::Stale.terminal_delivery(), None);
        assert_eq!(
            ViewerPlaybackFeedback::Blocked.terminal_delivery(),
            Some(FrameDeliveryKind::Blocked)
        );
    }

    #[test]
    fn raw_preview_adapter_preserves_ready_loading_and_stale_meanings() {
        assert_eq!(
            ViewerPlaybackFeedback::from_preview_state(&ViewerPreviewState::Unavailable),
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
