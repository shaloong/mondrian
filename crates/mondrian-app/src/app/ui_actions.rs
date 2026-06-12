//! UI action payloads consumed by [`AppState`](crate::app::AppState).
//!
//! Reusable widget crates remain domain-light. UI adapters attach stable app ids
//! to `Action::Custom` payloads before actions reach the app state layer.

use mondrian_core::types::{ClipId, TrackId};
use mondrian_editor_state::Action;
use serde::{Deserialize, Serialize};

/// Custom action namespace for timeline UI operations.
pub const TIMELINE_NAMESPACE: &str = "ui.timeline";

/// Action name for selecting a timeline clip.
pub const TIMELINE_SELECT_CLIP: &str = "select_clip";
/// Action name for moving a timeline clip.
pub const TIMELINE_MOVE_CLIP: &str = "move_clip";
/// Action name for trimming a timeline clip.
pub const TIMELINE_TRIM_CLIP: &str = "trim_clip";
/// Action name for seeking the active timeline.
pub const TIMELINE_SEEK: &str = "seek";

/// Clip edge being trimmed by a timeline UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimelineTrimPayloadEdge {
    In,
    Out,
}

/// Select a clip in the active sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSelectClipPayload {
    /// Track that owns the selected clip.
    pub track_id: TrackId,
    /// Whether `track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Clip selected by the UI.
    pub clip_id: ClipId,
}

/// Move a clip to a target track and frame in the active sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineMoveClipPayload {
    /// Track that should own the clip after the move.
    pub target_track_id: TrackId,
    /// Whether `target_track_id` is a video track rather than an audio track.
    pub is_video_track: bool,
    /// Clip being moved.
    pub clip_id: ClipId,
    /// Target timeline frame for the clip start.
    pub frame: i64,
}

/// Trim one clip edge to a target timeline frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineTrimClipPayload {
    /// Clip being trimmed.
    pub clip_id: ClipId,
    /// Edge that should be trimmed.
    pub edge: TimelineTrimPayloadEdge,
    /// Target timeline frame for the selected edge.
    pub frame: i64,
}

/// Seek the active timeline to a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSeekPayload {
    /// Target timeline frame.
    pub frame: i64,
}

/// Build an action that selects a clip in the active timeline.
pub fn timeline_select_clip_action(payload: TimelineSelectClipPayload) -> Action {
    custom_timeline_action(TIMELINE_SELECT_CLIP, payload)
}

/// Build an action that moves a clip in the active timeline.
pub fn timeline_move_clip_action(payload: TimelineMoveClipPayload) -> Action {
    custom_timeline_action(TIMELINE_MOVE_CLIP, payload)
}

/// Build an action that trims a clip edge in the active timeline.
pub fn timeline_trim_clip_action(payload: TimelineTrimClipPayload) -> Action {
    custom_timeline_action(TIMELINE_TRIM_CLIP, payload)
}

/// Build an action that seeks the active timeline.
pub fn timeline_seek_action(frame: i64) -> Action {
    custom_timeline_action(TIMELINE_SEEK, TimelineSeekPayload { frame: frame.max(0) })
}

fn custom_timeline_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: TIMELINE_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}
