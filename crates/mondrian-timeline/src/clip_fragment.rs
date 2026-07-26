//! Canonical Clip-placement fragmentation primitives.
//!
//! Structural edit Modules share these operations so source time, Clip-local
//! visual time, audio edit origins, stable identities, and edge fades cannot
//! drift between Insert, Lift, and Extract implementations.

use crate::clip::Clip;
use mondrian_core::{MondrianError, TimelineTime};

/// Split one Clip strictly inside its placement range.
///
/// The left fragment retains the incoming Clip identity. The right fragment
/// receives fresh placement-local identities while retaining shared processing
/// definitions. Callers own link-group reassignment for the complete edit set.
pub(crate) fn split_clip_at(
    mut clip: Clip,
    at: TimelineTime,
) -> mondrian_core::Result<(Clip, Clip)> {
    let end = clip.end_position()?;
    if at <= clip.position || at >= end {
        return Err(fragment_error(
            "split boundary must be strictly inside the Clip",
        ));
    }

    let split_offset = at.checked_sub(clip.position)?;
    let right_duration = end.checked_sub(at)?;
    let right_clip_time_in = clip.timeline_to_clip_time(at)?;
    let right_source_origin = clip.timeline_to_source_time(at)?;

    let mut right = clip.clone();
    right.fork_placement_identities_for_split(split_offset)?;
    right.position = at;
    right.duration = right_duration;
    right.clip_time_in = right_clip_time_in;
    right.set_source_origin(right_source_origin)?;
    for edit in &mut right.audio_components {
        edit.fades.fade_in = None;
    }

    clip.duration = split_offset;
    for edit in &mut clip.audio_components {
        edit.fades.fade_out = None;
    }
    Ok((clip, right))
}

/// Move a Clip in-edge later while retaining its complete author identity.
pub(crate) fn trim_clip_in_to(clip: &mut Clip, at: TimelineTime) -> mondrian_core::Result<()> {
    let end = clip.end_position()?;
    if at <= clip.position || at >= end {
        return Err(fragment_error(
            "trim-in boundary must be strictly inside the Clip",
        ));
    }

    let delta = at.checked_sub(clip.position)?;
    let duration = end.checked_sub(at)?;
    let clip_time_in = clip.timeline_to_clip_time(at)?;
    let source_origin = clip.timeline_to_source_time(at)?;
    clip.shift_audio_component_in(delta)?;
    clip.position = at;
    clip.duration = duration;
    clip.clip_time_in = clip_time_in;
    clip.set_source_origin(source_origin)?;
    for edit in &mut clip.audio_components {
        edit.fades.fade_in = None;
    }
    Ok(())
}

/// Move a Clip out-edge earlier while retaining its complete author identity.
pub(crate) fn trim_clip_out_to(clip: &mut Clip, at: TimelineTime) -> mondrian_core::Result<()> {
    let end = clip.end_position()?;
    if at <= clip.position || at >= end {
        return Err(fragment_error(
            "trim-out boundary must be strictly inside the Clip",
        ));
    }

    clip.duration = at.checked_sub(clip.position)?;
    for edit in &mut clip.audio_components {
        edit.fades.fade_out = None;
    }
    Ok(())
}

fn fragment_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "clip_fragment".to_owned(),
        reason: reason.into(),
    }
}
