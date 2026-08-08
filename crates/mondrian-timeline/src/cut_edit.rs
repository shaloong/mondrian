//! Validated single-Clip cut edits: Split, Roll, Slip, Slide, and edge Trim.
//!
//! These algorithms are the only production authority for their author
//! semantics. Product Adapters lower frame-grid pointer gestures to exact
//! [`TimelineTime`] at their own seam before calling; every operation runs
//! against a detached authoring candidate inside one Author Transaction and
//! never observes playback, persistence, or media execution state.
//!
//! Roll and Slide carry a caller-declared `minimum_duration` gesture policy
//! so the domain stays free of frame-grid assumptions while the product can
//! keep gestures from creating slivers shorter than one Sequence frame.

use crate::clip::TrimEdge;
use crate::clip_fragment::split_clip_at;
use crate::{Clip, Sequence, Track};
use mondrian_core::{ClipId, MondrianError, TimelineTime, TrackId};

/// Deterministic request to split one Clip at an exact Sequence time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitEditRequest {
    /// Clip to split.
    pub clip_id: ClipId,
    /// Exact split boundary; must be strictly inside the Clip placement.
    pub at: TimelineTime,
}

/// Structural evidence of one executed Split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitEditOutcome {
    /// Track that owned the split Clip.
    pub track_id: TrackId,
    /// Whether the split Clip lives on a video Track.
    pub is_video_track: bool,
    /// Stable identity retained by the left fragment.
    pub left_clip_id: ClipId,
    /// Fresh identity assigned to the right fragment.
    ///
    /// The right fragment's link group is always cleared; grouping the right
    /// fragments of a multi-member Split is the caller's editorial decision.
    pub right_clip_id: ClipId,
}

/// Deterministic request to roll one cut between two adjacent Clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollEditRequest {
    /// Clip adjacent to the cut; either neighbouring boundary is eligible and
    /// the one nearest to `target` is rolled.
    pub clip_id: ClipId,
    /// Requested cut position, clamped to the executable boundary window.
    pub target: TimelineTime,
    /// Positive minimum duration each side of the cut must retain.
    pub minimum_duration: TimelineTime,
}

/// Structural evidence of one executed Roll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RollEditOutcome {
    /// Clip left of the rolled cut.
    pub left_clip_id: ClipId,
    /// Clip right of the rolled cut.
    pub right_clip_id: ClipId,
    /// Cut position before the edit.
    pub previous_cut: TimelineTime,
    /// Cut position after the edit.
    pub new_cut: TimelineTime,
}

/// Deterministic request to slip one Clip's source window in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlipEditRequest {
    /// Clip to slip.
    pub clip_id: ClipId,
    /// Signed source-origin displacement in exact author time.
    pub delta: TimelineTime,
    /// Total source extent when the caller can prove one (bounded media);
    /// `None` admits an unbounded positive slip.
    pub estimated_total_source_extent: Option<TimelineTime>,
}

/// Structural evidence of one executed Slip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlipEditOutcome {
    /// Slipped Clip.
    pub clip_id: ClipId,
    /// Source origin before the edit.
    pub previous_source_origin: TimelineTime,
    /// Source origin after the edit.
    pub new_source_origin: TimelineTime,
}

/// Deterministic request to slide one Clip between its adjacent neighbours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlideEditRequest {
    /// Clip to slide; both neighbours must be exactly adjacent.
    pub clip_id: ClipId,
    /// Signed placement displacement in exact author time.
    pub delta: TimelineTime,
    /// Positive minimum duration both neighbours must retain.
    pub minimum_duration: TimelineTime,
}

/// Structural evidence of one executed Slide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlideEditOutcome {
    /// Slid Clip.
    pub clip_id: ClipId,
    /// Placement before the edit.
    pub previous_position: TimelineTime,
    /// Placement after the edit.
    pub new_position: TimelineTime,
}

/// Fail-closed Split/Roll/Slip/Slide validation or execution failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CutEditError {
    /// The addressed Clip does not exist in the Sequence.
    #[error("Clip {clip_id} does not exist in the Sequence")]
    UnknownClip {
        /// Addressed Clip.
        clip_id: ClipId,
    },
    /// The owning Track is locked.
    #[error("Track {track_id} is locked")]
    LockedTrack {
        /// Owning Track.
        track_id: TrackId,
    },
    /// The boundary is not strictly inside the Clip placement.
    #[error("Split boundary must be strictly inside the Clip placement")]
    SplitOutOfRange,
    /// Slip target carries no bounded source window (Adjustment Layer).
    #[error("Adjustment Layer Clips have no slip source window")]
    AdjustmentLayerSlip,
    /// The request violates a structural precondition (non-positive minimum
    /// duration, non-adjacent neighbours, or an Adjustment Layer target).
    #[error("cut edit rejected: {reason}")]
    InvalidRequest {
        /// Concrete machine-readable reason.
        reason: String,
    },
    /// Exact-time arithmetic overflowed.
    #[error("cut edit time arithmetic overflowed")]
    TimeOverflow,
}

/// One located Clip slot inside a Sequence.
#[derive(Debug, Clone, Copy)]
struct ClipSlot {
    is_video_track: bool,
    track_index: usize,
    clip_index: usize,
}

fn locate_clip(sequence: &Sequence, clip_id: ClipId) -> Option<ClipSlot> {
    for (track_index, track) in sequence.video_tracks.iter().enumerate() {
        if let Some(clip_index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            return Some(ClipSlot { is_video_track: true, track_index, clip_index });
        }
    }
    for (track_index, track) in sequence.audio_tracks.iter().enumerate() {
        if let Some(clip_index) = track.clips.iter().position(|clip| clip.id == clip_id) {
            return Some(ClipSlot { is_video_track: false, track_index, clip_index });
        }
    }
    None
}

fn track_mut<'a>(sequence: &'a mut Sequence, slot: &ClipSlot) -> &'a mut Track {
    if slot.is_video_track {
        &mut sequence.video_tracks[slot.track_index]
    } else {
        &mut sequence.audio_tracks[slot.track_index]
    }
}

fn track_ref<'a>(sequence: &'a Sequence, slot: &ClipSlot) -> &'a Track {
    if slot.is_video_track {
        &sequence.video_tracks[slot.track_index]
    } else {
        &sequence.audio_tracks[slot.track_index]
    }
}

fn locked_error(track: &Track) -> CutEditError {
    CutEditError::LockedTrack { track_id: track.id }
}

fn overflow<T, E>(result: Result<T, E>) -> Result<T, CutEditError> {
    result.map_err(|_| CutEditError::TimeOverflow)
}

/// Whether one exact Split can execute against current author state.
pub fn assess_split_edit(
    sequence: &Sequence,
    request: &SplitEditRequest,
) -> Result<(), CutEditError> {
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_ref(sequence, &slot);
    if track.is_locked {
        return Err(locked_error(track));
    }
    let clip = &track.clips[slot.clip_index];
    let end = overflow(clip.end_position())?;
    if request.at <= clip.position || request.at >= end {
        return Err(CutEditError::SplitOutOfRange);
    }
    Ok(())
}

/// Split one Clip at an exact Sequence time.
///
/// The left fragment retains the incoming identity; the right fragment
/// receives fresh placement-local identities, cleared inner edge fades, and a
/// cleared link group. Track membership, order, and all other Clips are
/// unchanged.
pub fn apply_split_edit(
    sequence: &mut Sequence,
    request: &SplitEditRequest,
) -> Result<SplitEditOutcome, CutEditError> {
    assess_split_edit(sequence, request)?;
    let slot = locate_clip(sequence, request.clip_id).expect("assessment located the Clip");
    let track = track_mut(sequence, &slot);
    let clip = track.clips.remove(slot.clip_index);
    let (left, mut right) =
        split_clip_at(clip, request.at).map_err(|_| CutEditError::SplitOutOfRange)?;
    right.link_group = None;
    let outcome = SplitEditOutcome {
        track_id: track.id,
        is_video_track: slot.is_video_track,
        left_clip_id: left.id,
        right_clip_id: right.id,
    };
    track.clips.insert(slot.clip_index, right);
    track.clips.insert(slot.clip_index, left);
    Ok(outcome)
}

/// One executable roll boundary adjacent to the addressed Clip.
#[derive(Debug, Clone, Copy)]
struct RollBoundary {
    left_index: usize,
    right_index: usize,
    current_cut: TimelineTime,
    minimum_cut: TimelineTime,
    maximum_cut: TimelineTime,
}

fn resolve_roll_boundary(
    track: &Track,
    clip_index: usize,
    request: &RollEditRequest,
) -> Result<Option<(RollBoundary, TimelineTime)>, CutEditError> {
    let mut boundaries = Vec::with_capacity(2);
    let candidate_pairs = [
        clip_index.checked_sub(1).map(|left_index| (left_index, clip_index)),
        (clip_index + 1 < track.clips.len()).then_some((clip_index, clip_index + 1)),
    ];
    for (left_index, right_index) in candidate_pairs.into_iter().flatten() {
        let left = &track.clips[left_index];
        let right = &track.clips[right_index];
        let left_end = overflow(left.end_position())?;
        let right_start = right.position;
        if left_end != right_start {
            continue;
        }
        // The cut may never pull more source than the right Clip holds before
        // its current origin, and both sides must retain minimum_duration.
        let source_room = right_start
            .checked_sub(right.source_origin())
            .map_err(|_| CutEditError::TimeOverflow)?;
        let minimum_cut =
            overflow(left.position.checked_add(request.minimum_duration))?.max(source_room);
        let maximum_cut =
            overflow(overflow(right.end_position())?.checked_sub(request.minimum_duration))?;
        if minimum_cut <= maximum_cut {
            boundaries.push(RollBoundary {
                left_index,
                right_index,
                current_cut: right_start,
                minimum_cut,
                maximum_cut,
            });
        }
    }

    let Some(boundary) = boundaries
        .into_iter()
        .min_by_key(|candidate| time_distance(candidate.current_cut, request.target))
    else {
        return Ok(None);
    };

    let new_cut = request.target.clamp(boundary.minimum_cut, boundary.maximum_cut);
    if new_cut == boundary.current_cut {
        return Ok(None);
    }
    Ok(Some((boundary, new_cut)))
}

fn time_distance(left: TimelineTime, right: TimelineTime) -> TimelineTime {
    let difference = if left >= right {
        left.checked_sub(right)
    } else {
        right.checked_sub(left)
    };
    difference.unwrap_or(TimelineTime::ZERO)
}

/// Whether one exact Roll can execute and would move the cut.
pub fn assess_roll_edit(
    sequence: &Sequence,
    request: &RollEditRequest,
) -> Result<bool, CutEditError> {
    if request.minimum_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "roll minimum_duration must be positive".to_owned(),
        });
    }
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_ref(sequence, &slot);
    if track.is_locked {
        return Ok(false);
    }
    Ok(resolve_roll_boundary(track, slot.clip_index, request)?.is_some())
}

/// Roll the nearest executable cut adjacent to one Clip.
///
/// Both sides keep their identities; the right Clip's source window and
/// audio edit origins shift exactly with the cut.
pub fn apply_roll_edit(
    sequence: &mut Sequence,
    request: &RollEditRequest,
) -> Result<Option<RollEditOutcome>, CutEditError> {
    if request.minimum_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "roll minimum_duration must be positive".to_owned(),
        });
    }
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_mut(sequence, &slot);
    if track.is_locked {
        return Err(locked_error(track));
    }
    let Some((boundary, new_cut)) = resolve_roll_boundary(track, slot.clip_index, request)? else {
        return Ok(None);
    };

    let left_original = track.clips[boundary.left_index].clone();
    let right_original = track.clips[boundary.right_index].clone();

    let new_left_duration = overflow(new_cut.checked_sub(left_original.position))?;
    let new_right_duration =
        overflow(overflow(right_original.end_position())?.checked_sub(new_cut))?;
    if new_left_duration <= TimelineTime::ZERO || new_right_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "roll would erase one side of the cut".to_owned(),
        });
    }

    let new_right_clip_time_in = overflow(right_original.timeline_to_clip_time(new_cut))?;
    let new_right_source_origin = overflow(right_original.timeline_to_source_time(new_cut))?;

    let mut left_updated = left_original;
    left_updated.duration = new_left_duration;

    let mut right_updated = right_original;
    let right_shift = overflow(new_cut.checked_sub(right_updated.position))?;
    overflow(right_updated.shift_audio_component_in(right_shift))?;
    right_updated.position = new_cut;
    right_updated.duration = new_right_duration;
    right_updated.clip_time_in = new_right_clip_time_in;
    overflow(right_updated.set_source_origin(new_right_source_origin))?;

    let outcome = RollEditOutcome {
        left_clip_id: left_updated.id,
        right_clip_id: right_updated.id,
        previous_cut: boundary.current_cut,
        new_cut,
    };
    track.clips[boundary.left_index] = left_updated;
    track.clips[boundary.right_index] = right_updated;
    Ok(Some(outcome))
}

/// Whether one exact Slip can execute and would move the source window.
pub fn assess_slip_edit(
    sequence: &Sequence,
    request: &SlipEditRequest,
) -> Result<bool, CutEditError> {
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_ref(sequence, &slot);
    if track.is_locked {
        return Ok(false);
    }
    let clip = &track.clips[slot.clip_index];
    Ok(slip_target_origin(clip, request)?.is_some_and(|origin| origin != clip.source_origin()))
}

fn slip_target_origin(
    clip: &Clip,
    request: &SlipEditRequest,
) -> Result<Option<TimelineTime>, CutEditError> {
    if clip.is_adjustment_layer() {
        return Err(CutEditError::AdjustmentLayerSlip);
    }
    let source_origin = clip.source_origin();
    let source_span =
        overflow(overflow(clip.source_terminal_boundary())?.checked_sub(source_origin))?;
    if source_span <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "Clip source window is not positive".to_owned(),
        });
    }
    let maximum_origin = request.estimated_total_source_extent.map(|total| {
        let room = total.checked_sub(source_span).unwrap_or(TimelineTime::ZERO);
        if room.is_negative() {
            TimelineTime::ZERO
        } else {
            room
        }
    });
    let proposed = overflow(source_origin.checked_add(request.delta))?;
    let proposed = if proposed.is_negative() {
        TimelineTime::ZERO
    } else {
        proposed
    };
    let new_origin = match maximum_origin {
        Some(maximum) => proposed.min(maximum),
        None => proposed,
    };
    if new_origin == source_origin {
        return Ok(None);
    }
    Ok(Some(new_origin))
}

/// Slip one Clip's source window without moving its placement.
pub fn apply_slip_edit(
    sequence: &mut Sequence,
    request: &SlipEditRequest,
) -> Result<Option<SlipEditOutcome>, CutEditError> {
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_mut(sequence, &slot);
    if track.is_locked {
        return Err(locked_error(track));
    }
    let clip = &track.clips[slot.clip_index];
    let previous_origin = clip.source_origin();
    let Some(new_origin) = slip_target_origin(clip, request)? else {
        return Ok(None);
    };
    let clip = &mut track.clips[slot.clip_index];
    overflow(clip.set_source_origin(new_origin))?;
    Ok(Some(SlipEditOutcome {
        clip_id: clip.id,
        previous_source_origin: previous_origin,
        new_source_origin: new_origin,
    }))
}

/// Whether one exact Slide can execute and would move the Clip.
pub fn assess_slide_edit(
    sequence: &Sequence,
    request: &SlideEditRequest,
) -> Result<bool, CutEditError> {
    if request.minimum_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "slide minimum_duration must be positive".to_owned(),
        });
    }
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_ref(sequence, &slot);
    if track.is_locked {
        return Ok(false);
    }
    Ok(resolve_slide_target(track, slot.clip_index, request)?.is_some())
}

fn resolve_slide_target(
    track: &Track,
    clip_index: usize,
    request: &SlideEditRequest,
) -> Result<Option<TimelineTime>, CutEditError> {
    if clip_index == 0 || clip_index + 1 >= track.clips.len() {
        return Ok(None);
    }
    let left = &track.clips[clip_index - 1];
    let center = &track.clips[clip_index];
    let right = &track.clips[clip_index + 1];

    let left_end = overflow(left.end_position())?;
    let center_end = overflow(center.end_position())?;
    if left_end != center.position || center_end != right.position {
        return Ok(None);
    }

    let source_room = right
        .position
        .checked_sub(center.duration)
        .and_then(|earliest| earliest.checked_sub(right.source_origin()))
        .map_err(|_| CutEditError::TimeOverflow)?;
    let minimum_start =
        overflow(left.position.checked_add(request.minimum_duration))?.max(source_room);
    let right_end = overflow(right.end_position())?;
    let maximum_start = overflow(
        overflow(right_end.checked_sub(center.duration))?.checked_sub(request.minimum_duration),
    )?;
    if minimum_start > maximum_start {
        return Ok(None);
    }

    let proposed = overflow(center.position.checked_add(request.delta))?;
    let proposed = if proposed.is_negative() {
        TimelineTime::ZERO
    } else {
        proposed
    };
    let new_start = proposed.clamp(minimum_start, maximum_start);
    if new_start == center.position {
        return Ok(None);
    }
    Ok(Some(new_start))
}

/// Slide one Clip between its exactly adjacent neighbours.
///
/// The slid Clip keeps identity, duration, and source window; the left
/// neighbour's out-edge and the right neighbour's in-edge follow exactly.
pub fn apply_slide_edit(
    sequence: &mut Sequence,
    request: &SlideEditRequest,
) -> Result<Option<SlideEditOutcome>, CutEditError> {
    if request.minimum_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "slide minimum_duration must be positive".to_owned(),
        });
    }
    let Some(slot) = locate_clip(sequence, request.clip_id) else {
        return Err(CutEditError::UnknownClip { clip_id: request.clip_id });
    };
    let track = track_mut(sequence, &slot);
    if track.is_locked {
        return Err(locked_error(track));
    }
    let Some(new_start) = resolve_slide_target(track, slot.clip_index, request)? else {
        return Ok(None);
    };

    let clip_index = slot.clip_index;
    let left_original = track.clips[clip_index - 1].clone();
    let center_original = track.clips[clip_index].clone();
    let right_original = track.clips[clip_index + 1].clone();

    let new_center_end = overflow(new_start.checked_add(center_original.duration))?;
    let new_left_duration = overflow(new_start.checked_sub(left_original.position))?;
    let right_end = overflow(right_original.end_position())?;
    let new_right_duration = overflow(right_end.checked_sub(new_center_end))?;
    if new_left_duration <= TimelineTime::ZERO || new_right_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "slide would erase a neighbouring Clip".to_owned(),
        });
    }

    let new_right_clip_time_in = overflow(right_original.timeline_to_clip_time(new_center_end))?;
    let new_right_source_origin = overflow(right_original.timeline_to_source_time(new_center_end))?;

    let previous_position = center_original.position;
    let mut left_updated = left_original;
    left_updated.duration = new_left_duration;

    let mut center_updated = center_original;
    center_updated.position = new_start;

    let mut right_updated = right_original;
    let right_shift = overflow(new_center_end.checked_sub(right_updated.position))?;
    overflow(right_updated.shift_audio_component_in(right_shift))?;
    right_updated.position = new_center_end;
    right_updated.duration = new_right_duration;
    right_updated.clip_time_in = new_right_clip_time_in;
    overflow(right_updated.set_source_origin(new_right_source_origin))?;

    let outcome = SlideEditOutcome {
        clip_id: center_updated.id,
        previous_position,
        new_position: new_start,
    };
    track.clips[clip_index - 1] = left_updated;
    track.clips[clip_index] = center_updated;
    track.clips[clip_index + 1] = right_updated;
    Ok(Some(outcome))
}

/// Prepare one trimmed replacement Clip for an exact edge gesture.
///
/// Returns `Ok(None)` when the gesture is a no-op (target at the current edge
/// or fully clamped). A frame-grid gesture must not make an ordinary Clip
/// shorter than `minimum_duration`; existing exact sub-frame placements
/// remain valid and are never lengthened merely to satisfy that policy.
pub fn prepare_trimmed_clip_at_time(
    original: &Clip,
    edge: TrimEdge,
    target: TimelineTime,
    minimum_duration: TimelineTime,
) -> Result<Option<Clip>, CutEditError> {
    if minimum_duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "trim minimum_duration must be positive".to_owned(),
        });
    }
    let start = original.position;
    let end = overflow(original.end_position())?;
    if end <= start {
        return Ok(None);
    }
    let retained_duration = original.duration.min(minimum_duration);

    let mut updated = original.clone();
    match edge {
        TrimEdge::In => {
            let latest_start = overflow(end.checked_sub(retained_duration))?;
            if latest_start <= start {
                return Ok(None);
            }
            let new_start = target.max(start).min(latest_start);
            if new_start == start {
                return Ok(None);
            }
            let new_in = overflow(original.timeline_to_source_time(new_start))?;
            let new_clip_time_in = overflow(original.timeline_to_clip_time(new_start))?;
            let shift = overflow(new_start.checked_sub(updated.position))?;
            overflow(updated.shift_audio_component_in(shift))?;
            updated.position = new_start;
            updated.duration = overflow(end.checked_sub(new_start))?;
            updated.clip_time_in = new_clip_time_in;
            overflow(updated.set_source_origin(new_in))?;
        }
        TrimEdge::Out => {
            let minimum_end = overflow(start.checked_add(retained_duration))?;
            let new_end = if original.source_time_scale().numerator() == 0 {
                target.max(minimum_end)
            } else if minimum_end > end {
                return Ok(None);
            } else {
                target.max(minimum_end).min(end)
            };
            if new_end == end {
                return Ok(None);
            }
            updated.duration = overflow(new_end.checked_sub(start))?;
        }
    }

    if updated.duration <= TimelineTime::ZERO {
        return Err(CutEditError::InvalidRequest {
            reason: "trim would erase the Clip".to_owned(),
        });
    }
    Ok(Some(updated))
}

/// Convert a domain cut-edit failure into the shared error vocabulary.
impl From<CutEditError> for MondrianError {
    fn from(error: CutEditError) -> Self {
        match error {
            CutEditError::UnknownClip { clip_id } => {
                MondrianError::ClipNotFound { clip_id: clip_id.to_string() }
            }
            CutEditError::LockedTrack { track_id } => {
                MondrianError::TrackLocked { track_id: track_id.to_string() }
            }
            other => MondrianError::WorkflowStepFailed {
                step_id: "cut_edit".to_owned(),
                reason: other.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AssetId, FramePosition, Rational};

    fn at(tb: Rational, frame: i64) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, tb)).expect("test time")
    }

    fn clip_at(tb: Rational, frame: i64, frames: i64) -> Clip {
        Clip::new(AssetId::new(), at(tb, frame), at(tb, frames)).expect("test Clip")
    }

    fn sequence_with_clips(placements: &[(i64, i64)]) -> (Sequence, Vec<ClipId>) {
        let mut sequence = Sequence::new("cut-edit-tests");
        let tb = sequence.time_base();
        let mut ids = Vec::new();
        for (frame, frames) in placements {
            let clip = clip_at(tb, *frame, *frames);
            ids.push(clip.id);
            sequence.video_tracks[0].clips.push(clip);
        }
        (sequence, ids)
    }

    #[test]
    fn split_divides_placement_with_exact_fragments() {
        let (mut sequence, ids) = sequence_with_clips(&[(10, 20)]);
        let tb = sequence.time_base();
        let clip_id = ids[0];
        let outcome =
            apply_split_edit(&mut sequence, &SplitEditRequest { clip_id, at: at(tb, 14) })
                .expect("split");

        assert_eq!(outcome.left_clip_id, clip_id);
        assert_ne!(outcome.right_clip_id, clip_id);
        let track = &sequence.video_tracks[0];
        assert_eq!(track.clips.len(), 2);
        let left = &track.clips[0];
        let right = &track.clips[1];
        assert_eq!(left.duration, at(tb, 4));
        assert_eq!(right.position, at(tb, 14));
        assert_eq!(right.duration, at(tb, 16));
        assert_eq!(right.link_group, None);
        assert_eq!(
            right.source_origin(),
            at(tb, 4),
            "right fragment starts at the cut"
        );
    }

    #[test]
    fn split_rejects_out_of_range_locked_and_unknown() {
        let (mut sequence, ids) = sequence_with_clips(&[(10, 20)]);
        let tb = sequence.time_base();
        let clip_id = ids[0];
        assert!(matches!(
            apply_split_edit(&mut sequence, &SplitEditRequest { clip_id, at: at(tb, 30) }),
            Err(CutEditError::SplitOutOfRange)
        ));
        assert!(matches!(
            apply_split_edit(&mut sequence, &SplitEditRequest { clip_id, at: at(tb, 10) }),
            Err(CutEditError::SplitOutOfRange)
        ));
        assert!(matches!(
            apply_split_edit(
                &mut sequence,
                &SplitEditRequest { clip_id: ClipId::new(), at: at(tb, 12) },
            ),
            Err(CutEditError::UnknownClip { .. })
        ));

        sequence.video_tracks[0].is_locked = true;
        assert!(matches!(
            apply_split_edit(&mut sequence, &SplitEditRequest { clip_id, at: at(tb, 12) }),
            Err(CutEditError::LockedTrack { .. })
        ));
    }

    #[test]
    fn roll_moves_the_cut_and_shifts_the_right_source_window() {
        let (mut sequence, ids) = sequence_with_clips(&[(0, 10), (10, 10)]);
        let tb = sequence.time_base();
        let outcome = apply_roll_edit(
            &mut sequence,
            &RollEditRequest {
                clip_id: ids[1],
                target: at(tb, 14),
                minimum_duration: at(tb, 1),
            },
        )
        .expect("roll")
        .expect("cut moved");

        assert_eq!(outcome.previous_cut, at(tb, 10));
        assert_eq!(outcome.new_cut, at(tb, 14));
        let track = &sequence.video_tracks[0];
        assert_eq!(track.clips[0].duration, at(tb, 14));
        assert_eq!(track.clips[1].position, at(tb, 14));
        assert_eq!(track.clips[1].duration, at(tb, 6));
        assert_eq!(track.clips[1].source_origin(), at(tb, 4));
    }

    #[test]
    fn roll_clamps_to_minimum_duration_and_source_availability() {
        let (mut sequence, ids) = sequence_with_clips(&[(0, 10), (10, 10)]);
        let tb = sequence.time_base();
        // Right Clip starts two frames into its source: the cut can move left
        // by at most two frames.
        sequence.video_tracks[0].clips[1].set_source_origin(at(tb, 2)).expect("origin");

        let too_far_left = apply_roll_edit(
            &mut sequence,
            &RollEditRequest {
                clip_id: ids[0],
                target: at(tb, 5),
                minimum_duration: at(tb, 1),
            },
        )
        .expect("roll")
        .expect("clamped move");
        assert_eq!(too_far_left.new_cut, at(tb, 8));

        let too_far_right = apply_roll_edit(
            &mut sequence,
            &RollEditRequest {
                clip_id: ids[0],
                target: at(tb, 30),
                minimum_duration: at(tb, 1),
            },
        )
        .expect("roll")
        .expect("clamped move");
        assert_eq!(too_far_right.new_cut, at(tb, 19));
    }

    #[test]
    fn roll_without_adjacency_is_a_no_op() {
        let (mut sequence, ids) = sequence_with_clips(&[(0, 10), (15, 10)]);
        let tb = sequence.time_base();
        let outcome = apply_roll_edit(
            &mut sequence,
            &RollEditRequest {
                clip_id: ids[0],
                target: at(tb, 12),
                minimum_duration: at(tb, 1),
            },
        )
        .expect("roll");
        assert!(outcome.is_none());
    }

    #[test]
    fn slip_shifts_source_origin_within_the_proven_extent() {
        let (mut sequence, ids) = sequence_with_clips(&[(10, 10)]);
        let tb = sequence.time_base();
        let outcome = apply_slip_edit(
            &mut sequence,
            &SlipEditRequest {
                clip_id: ids[0],
                delta: at(tb, 3),
                estimated_total_source_extent: Some(at(tb, 20)),
            },
        )
        .expect("slip")
        .expect("window moved");
        assert_eq!(outcome.previous_source_origin, TimelineTime::ZERO);
        assert_eq!(outcome.new_source_origin, at(tb, 3));

        // The Clip spans 10 frames of a 20-frame source: origin clamps at 10.
        let clamped = apply_slip_edit(
            &mut sequence,
            &SlipEditRequest {
                clip_id: ids[0],
                delta: at(tb, 50),
                estimated_total_source_extent: Some(at(tb, 20)),
            },
        )
        .expect("slip")
        .expect("clamped move");
        assert_eq!(clamped.new_source_origin, at(tb, 10));

        // Negative slips clamp at zero.
        let negative = TimelineTime::ZERO.checked_sub(at(tb, 99)).expect("negative");
        let back = apply_slip_edit(
            &mut sequence,
            &SlipEditRequest {
                clip_id: ids[0],
                delta: negative,
                estimated_total_source_extent: Some(at(tb, 20)),
            },
        )
        .expect("slip")
        .expect("clamped move");
        assert_eq!(back.new_source_origin, TimelineTime::ZERO);
    }

    #[test]
    fn slip_rejects_adjustment_layers() {
        let mut sequence = Sequence::new("slip-adjustment");
        let tb = sequence.time_base();
        let clip = Clip::new_adjustment_layer(AssetId::new(), at(tb, 0), at(tb, 10))
            .expect("adjustment Clip");
        let clip_id = clip.id;
        sequence.video_tracks[0].clips.push(clip);
        assert!(matches!(
            apply_slip_edit(
                &mut sequence,
                &SlipEditRequest {
                    clip_id,
                    delta: at(tb, 1),
                    estimated_total_source_extent: None,
                },
            ),
            Err(CutEditError::AdjustmentLayerSlip)
        ));
    }

    #[test]
    fn slide_moves_between_exactly_adjacent_neighbours() {
        let (mut sequence, ids) = sequence_with_clips(&[(0, 10), (10, 10), (20, 10)]);
        let tb = sequence.time_base();
        let outcome = apply_slide_edit(
            &mut sequence,
            &SlideEditRequest {
                clip_id: ids[1],
                delta: at(tb, 4),
                minimum_duration: at(tb, 1),
            },
        )
        .expect("slide")
        .expect("moved");
        assert_eq!(outcome.previous_position, at(tb, 10));
        assert_eq!(outcome.new_position, at(tb, 14));
        let track = &sequence.video_tracks[0];
        assert_eq!(track.clips[0].duration, at(tb, 14));
        assert_eq!(track.clips[1].position, at(tb, 14));
        assert_eq!(track.clips[1].duration, at(tb, 10));
        assert_eq!(track.clips[2].position, at(tb, 24));
        assert_eq!(track.clips[2].duration, at(tb, 6));
        assert_eq!(track.clips[2].source_origin(), at(tb, 4));
    }

    #[test]
    fn slide_requires_adjacent_neighbours() {
        let (mut sequence, ids) = sequence_with_clips(&[(0, 10), (12, 10), (24, 10)]);
        let tb = sequence.time_base();
        let outcome = apply_slide_edit(
            &mut sequence,
            &SlideEditRequest {
                clip_id: ids[1],
                delta: at(tb, 2),
                minimum_duration: at(tb, 1),
            },
        )
        .expect("slide");
        assert!(outcome.is_none());
    }

    #[test]
    fn trim_in_edge_advances_source_and_clip_time() {
        let (sequence, ids) = sequence_with_clips(&[(10, 20)]);
        let tb = sequence.time_base();
        let original = sequence.find_clip(ids[0]).expect("Clip").clone();
        let updated = prepare_trimmed_clip_at_time(&original, TrimEdge::In, at(tb, 15), at(tb, 1))
            .expect("trim")
            .expect("changed");
        assert_eq!(updated.position, at(tb, 15));
        assert_eq!(updated.duration, at(tb, 15));
        assert_eq!(updated.source_origin(), at(tb, 5));
        assert_eq!(updated.id, original.id, "trim retains Clip identity");
    }

    #[test]
    fn trim_out_edge_rejects_slivers_and_no_ops() {
        let (sequence, ids) = sequence_with_clips(&[(10, 20)]);
        let tb = sequence.time_base();
        let original = sequence.find_clip(ids[0]).expect("Clip").clone();
        let same = prepare_trimmed_clip_at_time(&original, TrimEdge::Out, at(tb, 30), at(tb, 1))
            .expect("trim");
        assert!(same.is_none());

        let sliver = prepare_trimmed_clip_at_time(&original, TrimEdge::In, at(tb, 29), at(tb, 5))
            .expect("trim")
            .expect("clamped to the minimum");
        assert_eq!(sliver.duration, at(tb, 5));
    }
}
