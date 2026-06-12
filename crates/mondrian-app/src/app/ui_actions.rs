//! UI action payloads consumed by [`AppState`](crate::app::AppState).
//!
//! Reusable widget crates remain domain-light. UI adapters attach stable app ids
//! to `Action::Custom` payloads before actions reach the app state layer.

use mondrian_core::effect_data::EffectType;
use mondrian_core::types::{ClipId, EffectId, TrackId};
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

/// Custom action namespace for inspector UI operations.
pub const INSPECTOR_NAMESPACE: &str = "ui.inspector";

/// Action name for toggling a selected clip's enabled state.
pub const INSPECTOR_SET_CLIP_ENABLED: &str = "set_clip_enabled";
/// Action name for changing a selected clip's opacity percentage.
pub const INSPECTOR_SET_CLIP_OPACITY: &str = "set_clip_opacity";
/// Action name for changing a selected clip's solid/tint color.
pub const INSPECTOR_SET_CLIP_TINT: &str = "set_clip_tint";
/// Action name for changing one selected clip transform field.
pub const INSPECTOR_SET_CLIP_TRANSFORM_FIELD: &str = "set_clip_transform_field";
/// Action name for toggling one effect on a selected clip.
pub const INSPECTOR_SET_EFFECT_ENABLED: &str = "set_effect_enabled";

/// Custom action namespace for effect browser operations.
pub const EFFECTS_NAMESPACE: &str = "ui.effects";

/// Action name for adding an effect to a selected clip.
pub const EFFECTS_ADD_TO_CLIP: &str = "add_to_clip";

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

/// Application-level identity for an inspector-selected clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorClipRefPayload {
    /// Track that owned the selected clip when the panel snapshot was built.
    pub track_id: TrackId,
    /// Whether `track_id` was a video track rather than an audio track.
    pub is_video_track: bool,
    /// Clip targeted by the inspector mutation.
    pub clip_id: ClipId,
}

/// Toggle the enabled state for a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorSetClipEnabledPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// `true` when the clip should participate in rendering/playback.
    pub enabled: bool,
}

/// Change a clip opacity value in UI percentage units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipOpacityPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Opacity in the same `0.0..=100.0` percentage range used by the slider.
    pub opacity_percent: f32,
}

/// Change a clip solid/tint color.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipTintPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// New color value.
    pub color: mondrian_core::Color,
}

/// Transform field exposed by the self-hosted inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InspectorClipTransformField {
    /// Horizontal position in sequence pixels.
    PositionX,
    /// Vertical position in sequence pixels.
    PositionY,
    /// Uniform scale displayed in percent units.
    ScalePercent,
    /// Rotation in degrees.
    RotationDegrees,
}

/// Change a single transform field on a selected clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InspectorSetClipTransformFieldPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Transform field being changed.
    pub field: InspectorClipTransformField,
    /// New UI-space value for the field.
    pub value: f32,
}

/// Toggle a clip effect enabled state from an inspector panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectorSetEffectEnabledPayload {
    /// Clip targeted by the inspector mutation.
    pub clip: InspectorClipRefPayload,
    /// Effect instance being toggled.
    pub effect_id: EffectId,
    /// Whether the effect should participate in rendering.
    pub enabled: bool,
}

/// Add an effect from the effect browser to a clip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectsAddToClipPayload {
    /// Clip targeted by the effect insertion.
    pub clip: InspectorClipRefPayload,
    /// Effect type to instantiate with defaults.
    pub effect_type: EffectType,
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

/// Build an action that toggles a clip enabled state from an inspector panel.
pub fn inspector_set_clip_enabled_action(payload: InspectorSetClipEnabledPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_ENABLED, payload)
}

/// Build an action that changes clip opacity from an inspector panel.
pub fn inspector_set_clip_opacity_action(payload: InspectorSetClipOpacityPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_OPACITY, payload)
}

/// Build an action that changes clip solid/tint color from an inspector panel.
pub fn inspector_set_clip_tint_action(payload: InspectorSetClipTintPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_TINT, payload)
}

/// Build an action that changes a clip transform field from an inspector panel.
pub fn inspector_set_clip_transform_field_action(
    payload: InspectorSetClipTransformFieldPayload,
) -> Action {
    custom_inspector_action(INSPECTOR_SET_CLIP_TRANSFORM_FIELD, payload)
}

/// Build an action that toggles an effect on a selected clip.
pub fn inspector_set_effect_enabled_action(payload: InspectorSetEffectEnabledPayload) -> Action {
    custom_inspector_action(INSPECTOR_SET_EFFECT_ENABLED, payload)
}

/// Build an action that adds an effect to a selected clip.
pub fn effects_add_to_clip_action(payload: EffectsAddToClipPayload) -> Action {
    custom_effects_action(EFFECTS_ADD_TO_CLIP, payload)
}

fn custom_timeline_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: TIMELINE_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_inspector_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: INSPECTOR_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}

fn custom_effects_action<T: Serialize>(name: &'static str, payload: T) -> Action {
    Action::Custom {
        namespace: EFFECTS_NAMESPACE.into(),
        name: name.into(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    }
}
