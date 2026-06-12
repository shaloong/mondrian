use mondrian_core::types::{ClipId, TrackId};

/// UI-agnostic reference to a selected clip in the active sequence.
///
/// This lives in the application state layer so legacy egui panels and
/// self-hosted UI adapters can share selection semantics without depending on
/// each other's widget modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectedClipRef {
    pub track_id: TrackId,
    pub is_video_track: bool,
    pub clip_id: ClipId,
}
