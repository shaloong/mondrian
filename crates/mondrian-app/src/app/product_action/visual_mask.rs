//! Closed product Actions for Clip-local visual Mask authoring.
//!
//! The external envelope carries stable Clip, Mask, parameter, and relative
//! placement identities. Track placement and Clip-local author time are always
//! re-derived from the current Authoring Session at dispatch.

use mondrian_core::automation::{AnimationParameterAddress, PropertyValue};
use mondrian_core::mask_data::{MaskComponent, MaskShape, MaskShapeInterpolation};
use mondrian_core::{ClipId, MaskId};
use mondrian_timeline::{clip::Clip, sequence::Sequence, MaskRelativePlacement};
use serde::{Deserialize, Serialize};

use super::super::AppState;

/// External custom-action namespace for Clip-local visual Mask operations.
pub const VISUAL_MASK_NAMESPACE: &str = "ui.visual_mask";

/// External action name for appending one Mask to a video Clip.
pub const VISUAL_MASK_ADD_TO_CLIP: &str = "add_to_clip";
/// External action name for selecting one Mask instance.
pub const VISUAL_MASK_SELECT: &str = "select";
/// External action name for changing one Mask's execution-enabled state.
pub const VISUAL_MASK_SET_ENABLED: &str = "set_enabled";
/// External action name for changing one Mask's author-edit lock.
pub const VISUAL_MASK_SET_LOCKED: &str = "set_locked";
/// External action name for removing one Mask instance.
pub const VISUAL_MASK_REMOVE: &str = "remove";
/// External action name for moving one Mask relative to another instance.
pub const VISUAL_MASK_REORDER: &str = "reorder";
/// External action name for changing shape-animation authority.
pub const VISUAL_MASK_SET_SHAPE_ANIMATION_ENABLED: &str = "set_shape_animation_enabled";
/// External action name for writing one complete Mask shape.
pub const VISUAL_MASK_WRITE_SHAPE: &str = "write_shape";
/// External action name for writing one stable-address Mask scalar parameter.
pub const VISUAL_MASK_SET_PARAMETER_VALUE: &str = "set_parameter_value";

/// Closed product operations owned by one Clip-local Mask stack.
#[derive(Debug, Clone, PartialEq)]
pub enum VisualMaskProductAction {
    /// Append one Mask with exact initial geometry.
    AddToClip(VisualMaskAddToClipPayload),
    /// Select one Mask without creating author history.
    Select(VisualMaskTargetPayload),
    /// Change whether one Mask participates in picture execution.
    SetEnabled(VisualMaskSetEnabledPayload),
    /// Change whether one Mask accepts geometry and parameter edits.
    SetLocked(VisualMaskSetLockedPayload),
    /// Remove one unlocked Mask.
    Remove(VisualMaskTargetPayload),
    /// Move one unlocked Mask relative to a stable anchor.
    Reorder(VisualMaskReorderPayload),
    /// Enable or collapse exact Clip-local shape animation.
    SetShapeAnimationEnabled(VisualMaskSetShapeAnimationEnabledPayload),
    /// Write a complete static value or exact-time shape key.
    WriteShape(VisualMaskWriteShapePayload),
    /// Write one scalar parameter through stable author identity.
    SetParameterValue(Box<VisualMaskSetParameterValuePayload>),
}

/// Clip and initial geometry for one Mask insertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskAddToClipPayload {
    /// Canonical Clip identity; current video Track is derived at dispatch.
    pub clip_id: ClipId,
    /// Complete initial Mask geometry.
    pub shape: MaskShape,
}

/// Stable identity of one Mask instance on one Clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskTargetPayload {
    /// Canonical Clip identity; current Track placement is derived at dispatch.
    pub clip_id: ClipId,
    /// Stable Mask identity owned by the Clip.
    pub mask_id: MaskId,
}

/// Change one Mask's execution-enabled state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskSetEnabledPayload {
    /// Canonical Clip identity.
    pub clip_id: ClipId,
    /// Stable Mask identity.
    pub mask_id: MaskId,
    /// Whether the Mask participates in prepared visual execution.
    pub enabled: bool,
}

/// Change one Mask's author-edit lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskSetLockedPayload {
    /// Canonical Clip identity.
    pub clip_id: ClipId,
    /// Stable Mask identity.
    pub mask_id: MaskId,
    /// Whether geometry, parameter, order, and removal edits are rejected.
    pub locked: bool,
}

/// Move one Mask relative to another stable instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskReorderPayload {
    /// Canonical Clip identity.
    pub clip_id: ClipId,
    /// Stable Mask identity being moved.
    pub mask_id: MaskId,
    /// Stable relative placement inside the same Mask stack.
    pub placement: MaskRelativePlacement,
}

/// Change whether one Mask shape is static or keyframed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskSetShapeAnimationEnabledPayload {
    /// Canonical Clip identity.
    pub clip_id: ClipId,
    /// Stable Mask identity.
    pub mask_id: MaskId,
    /// Whether writes target exact Clip-local shape keys.
    pub enabled: bool,
}

/// Write one complete Mask shape at the current Clip-local author time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskWriteShapePayload {
    /// Canonical Clip identity.
    pub clip_id: ClipId,
    /// Stable Mask identity.
    pub mask_id: MaskId,
    /// Complete geometry; partial geometry patches are not an author contract.
    pub shape: MaskShape,
    /// Interpolation from the affected key to its successor.
    pub interpolation: MaskShapeInterpolation,
}

/// Write one scalar Mask parameter through stable author identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualMaskSetParameterValuePayload {
    /// Canonical Clip identity.
    pub clip_id: ClipId,
    /// Stable Mask identity.
    pub mask_id: MaskId,
    /// Stable parameter instance plus definition identity.
    pub parameter: AnimationParameterAddress,
    /// Value to write statically or at the current Clip-local author time.
    pub value: PropertyValue,
}

pub(super) fn visual_mask_action_available(
    state: &AppState,
    action: &VisualMaskProductAction,
) -> bool {
    let Some(sequence) = state.active_sequence() else {
        return false;
    };
    match action {
        VisualMaskProductAction::AddToClip(payload) => {
            visual_mask_clip(sequence, payload.clip_id).is_some_and(|target| target.track_unlocked)
        }
        VisualMaskProductAction::Select(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id).is_some()
                && state.selection.selected_mask.is_none_or(|(mask_id, clip_id, _)| {
                    mask_id != payload.mask_id || clip_id != payload.clip_id
                })
        }
        VisualMaskProductAction::SetEnabled(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id).is_some_and(|target| {
                target.track_unlocked && target.mask.enabled != payload.enabled
            })
        }
        VisualMaskProductAction::SetLocked(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id)
                .is_some_and(|target| target.track_unlocked && target.mask.locked != payload.locked)
        }
        VisualMaskProductAction::Remove(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id)
                .is_some_and(|target| target.track_unlocked && !target.mask.locked)
        }
        VisualMaskProductAction::Reorder(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id).is_some_and(|target| {
                target.track_unlocked
                    && !target.mask.locked
                    && target
                        .clip
                        .mask_relative_placement_would_change(payload.mask_id, payload.placement)
                        .unwrap_or(false)
            })
        }
        VisualMaskProductAction::SetShapeAnimationEnabled(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id).is_some_and(|target| {
                target.track_unlocked
                    && !target.mask.locked
                    && target.mask.shape_animation_enabled != payload.enabled
            })
        }
        VisualMaskProductAction::WriteShape(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id).is_some_and(|target| {
                if !target.track_unlocked || target.mask.locked {
                    return false;
                }
                let Some(time) = visual_mask_author_time(state, sequence, target.clip) else {
                    return false;
                };
                let mut candidate = target.mask.clone();
                candidate
                    .write_shape(time, payload.shape.clone(), payload.interpolation)
                    .ok()
                    .flatten()
                    .is_some()
            })
        }
        VisualMaskProductAction::SetParameterValue(payload) => {
            visual_mask_target(sequence, payload.clip_id, payload.mask_id).is_some_and(|target| {
                if !target.track_unlocked || target.mask.locked {
                    return false;
                }
                visual_mask_author_time(state, sequence, target.clip)
                    .and_then(|time| {
                        target
                            .clip
                            .prepare_mask_parameter_value(
                                payload.mask_id,
                                &payload.parameter,
                                time,
                                payload.value.clone(),
                            )
                            .ok()
                            .flatten()
                    })
                    .is_some()
            })
        }
    }
}

#[derive(Clone, Copy)]
struct VisualMaskClip<'a> {
    clip: &'a Clip,
    track_unlocked: bool,
}

#[derive(Clone, Copy)]
struct VisualMaskTarget<'a> {
    clip: &'a Clip,
    mask: &'a MaskComponent,
    track_unlocked: bool,
}

fn visual_mask_clip(sequence: &Sequence, clip_id: ClipId) -> Option<VisualMaskClip<'_>> {
    sequence.video_tracks.iter().find_map(|track| {
        track
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .map(|clip| VisualMaskClip { clip, track_unlocked: !track.is_locked })
    })
}

fn visual_mask_target(
    sequence: &Sequence,
    clip_id: ClipId,
    mask_id: MaskId,
) -> Option<VisualMaskTarget<'_>> {
    let target = visual_mask_clip(sequence, clip_id)?;
    let mask = target.clip.mask(mask_id)?;
    Some(VisualMaskTarget {
        clip: target.clip,
        mask,
        track_unlocked: target.track_unlocked,
    })
}

fn visual_mask_author_time(
    state: &AppState,
    sequence: &Sequence,
    clip: &Clip,
) -> Option<mondrian_core::TimelineTime> {
    let sequence_time = state.current_timeline_time().ok().flatten().unwrap_or(sequence.playhead);
    let end = clip.end_position().ok()?;
    clip.timeline_to_clip_time(sequence_time.clamp(clip.position, end)).ok()
}
