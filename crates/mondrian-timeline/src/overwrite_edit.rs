//! Overwrite and push-forward conflict resolution for Track placements.
//!
//! These algorithms are the only production authority for overlap semantics
//! when Clips land on occupied Track ranges. They run against a detached
//! authoring candidate inside one Author Transaction; App Adapters own the
//! gesture and selection policy that selects the focus set.

use std::collections::HashSet;

use crate::{Clip, Sequence, Track};
use mondrian_core::{ClipId, TimelineTime};

/// How an incoming Clip placement resolves an overlap with existing content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipOverlapMode {
    /// Incoming content replaces the overlapped range; covered Clips are cut
    /// or removed exactly at the incoming boundary.
    #[default]
    Overwrite,
    /// Existing content keeps its relative order and is pushed later to make
    /// room; nothing is cut.
    PushForward,
}

/// Push any overlapped Clips forward until the Track is strictly sequential.
///
/// Clip order is canonicalized first; each overlapping Clip moves to its
/// predecessor's exact end. Durations and source windows are unchanged.
pub fn resolve_track_overlaps(track: &mut Track) -> mondrian_core::Result<()> {
    track.clips.sort_by_key(|clip| clip.position);
    let mut cursor = TimelineTime::ZERO;
    for clip in &mut track.clips {
        if clip.position < cursor {
            clip.position = cursor;
        }
        cursor = clip.end_position()?;
    }
    Ok(())
}

/// Subtract one half-open overwrite range from one Clip.
///
/// Returns zero, one, or two surviving fragments. A middle cut forks fresh
/// placement-local identities for the right fragment and clears both
/// fragments' link groups; fragment edge fades follow the shared
/// fragmentation contract.
pub fn subtract_overwrite_range_from_clip(
    clip: Clip,
    overlap_start: TimelineTime,
    overlap_end: TimelineTime,
) -> mondrian_core::Result<Vec<Clip>> {
    if overlap_end <= overlap_start {
        return Ok(vec![clip]);
    }

    let clip_start = clip.position;
    let clip_end = clip.end_position()?;
    let cut_start = overlap_start.max(clip_start);
    let cut_end = overlap_end.min(clip_end);
    if cut_end <= cut_start {
        return Ok(vec![clip]);
    }
    if cut_start <= clip_start && cut_end >= clip_end {
        return Ok(Vec::new());
    }

    if cut_start <= clip_start {
        let mut right = clip;
        let new_start = cut_end.max(clip_start);
        let new_clip_time_in = right.timeline_to_clip_time(new_start)?;
        let new_source_origin = right.timeline_to_source_time(new_start)?;
        right.shift_audio_component_in(new_start.checked_sub(clip_start)?)?;
        right.position = new_start;
        right.duration = clip_end.checked_sub(new_start)?.max(TimelineTime::ZERO);
        right.clip_time_in = new_clip_time_in;
        right.set_source_origin(new_source_origin)?;
        right.link_group = None;
        return Ok(if right.duration > TimelineTime::ZERO {
            vec![right]
        } else {
            Vec::new()
        });
    }

    if cut_end >= clip_end {
        let mut left = clip;
        let new_end = cut_start.min(clip_end);
        left.duration = new_end.checked_sub(clip_start)?.max(TimelineTime::ZERO);
        left.link_group = None;
        return Ok(if left.duration > TimelineTime::ZERO {
            vec![left]
        } else {
            Vec::new()
        });
    }

    let mut left = clip.clone();
    let left_new_end = cut_start;
    left.duration = left_new_end.checked_sub(clip_start)?.max(TimelineTime::ZERO);
    left.link_group = None;

    let mut right = clip;
    let right_new_start = cut_end;
    let right_new_clip_time_in = right.timeline_to_clip_time(right_new_start)?;
    let right_new_source_origin = right.timeline_to_source_time(right_new_start)?;
    right.fork_placement_identities_for_split(right_new_start.checked_sub(clip_start)?)?;
    right.position = right_new_start;
    right.duration = clip_end.checked_sub(right_new_start)?.max(TimelineTime::ZERO);
    right.clip_time_in = right_new_clip_time_in;
    right.set_source_origin(right_new_source_origin)?;
    right.link_group = None;

    let mut result = Vec::with_capacity(2);
    if left.duration > TimelineTime::ZERO {
        result.push(left);
    }
    if right.duration > TimelineTime::ZERO {
        result.push(right);
    }
    Ok(result)
}

/// Merge overlapping or adjacent half-open ranges into a minimal cover.
fn merge_time_ranges(
    mut ranges: Vec<(TimelineTime, TimelineTime)>,
) -> Vec<(TimelineTime, TimelineTime)> {
    ranges.retain(|(start, end)| end > start);
    ranges.sort_by_key(|(start, _)| *start);
    let mut merged = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        if let Some((_, last_end)) = merged.last_mut()
            && start <= *last_end
        {
            *last_end = (*last_end).max(end);
            continue;
        }
        merged.push((start, end));
    }
    merged
}

/// Apply the focus ranges against every non-focus Clip on one Track.
pub fn apply_overwrite_conflicts(
    track: &mut Track,
    focus_ids: &HashSet<ClipId>,
    focus_ranges: Vec<(TimelineTime, TimelineTime)>,
) -> mondrian_core::Result<()> {
    let merged_ranges = merge_time_ranges(focus_ranges);
    if merged_ranges.is_empty() {
        track.clips.sort_by_key(|clip| clip.position);
        return Ok(());
    }

    let mut resolved = Vec::<Clip>::with_capacity(track.clips.len());
    for clip in std::mem::take(&mut track.clips) {
        if focus_ids.contains(&clip.id) {
            resolved.push(clip);
            continue;
        }

        let mut segments = vec![clip];
        for (range_start, range_end) in &merged_ranges {
            if segments.is_empty() {
                break;
            }
            let mut next_segments = Vec::with_capacity(segments.len());
            for segment in segments {
                next_segments.extend(subtract_overwrite_range_from_clip(
                    segment,
                    *range_start,
                    *range_end,
                )?);
            }
            segments = next_segments;
        }
        resolved.extend(segments);
    }

    track.clips = resolved.into();
    track.clips.sort_by_key(|clip| clip.position);
    Ok(())
}

/// Resolve the overlap introduced by one focused Clip on its Track.
pub fn resolve_track_conflicts(
    track: &mut Track,
    focus_clip_id: ClipId,
    mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    match mode {
        ClipOverlapMode::PushForward => resolve_track_overlaps(track)?,
        ClipOverlapMode::Overwrite => {
            let Some(focus) = track.clips.iter().find(|clip| clip.id == focus_clip_id).cloned()
            else {
                track.clips.sort_by_key(|clip| clip.position);
                return Ok(());
            };
            let focus_ids = HashSet::from([focus_clip_id]);
            apply_overwrite_conflicts(
                track,
                &focus_ids,
                vec![(focus.position, focus.end_position()?)],
            )?;
        }
    }
    Ok(())
}

/// Resolve overlaps introduced by one moved focus set on one Track.
///
/// A Track without a moved member is outside the mutation footprint; in
/// particular its COW Clip storage is neither detached nor sorted.
pub fn apply_track_conflicts_for_focus_group(
    track: &mut Track,
    focus_ids: &HashSet<ClipId>,
    mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    if focus_ids.is_empty() {
        return Ok(());
    }
    let focus_ranges = track
        .clips
        .iter()
        .filter(|clip| focus_ids.contains(&clip.id))
        .map(|clip| Ok((clip.position, clip.end_position()?)))
        .collect::<mondrian_core::Result<Vec<_>>>()?;
    if focus_ranges.is_empty() {
        return Ok(());
    }
    match mode {
        ClipOverlapMode::PushForward => resolve_track_overlaps(track)?,
        ClipOverlapMode::Overwrite => {
            apply_overwrite_conflicts(track, focus_ids, focus_ranges)?;
        }
    }
    Ok(())
}

/// Resolve overlaps introduced by one moved focus set across the Sequence.
///
/// Only Tracks containing a focus member are touched.
pub fn apply_sequence_track_conflicts_for_focus_group(
    sequence: &mut Sequence,
    focus_ids: &HashSet<ClipId>,
    mode: ClipOverlapMode,
) -> mondrian_core::Result<()> {
    let video_indices = sequence
        .video_tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            track.clips.iter().any(|clip| focus_ids.contains(&clip.id)).then_some(index)
        })
        .collect::<Vec<_>>();
    let audio_indices = sequence
        .audio_tracks
        .iter()
        .enumerate()
        .filter_map(|(index, track)| {
            track.clips.iter().any(|clip| focus_ids.contains(&clip.id)).then_some(index)
        })
        .collect::<Vec<_>>();

    for index in video_indices {
        apply_track_conflicts_for_focus_group(&mut sequence.video_tracks[index], focus_ids, mode)?;
    }
    for index in audio_indices {
        apply_track_conflicts_for_focus_group(&mut sequence.audio_tracks[index], focus_ids, mode)?;
    }
    Ok(())
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
        let mut sequence = Sequence::new("overwrite-tests");
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
    fn subtract_middle_range_forks_two_survivors() {
        let (sequence, ids) = sequence_with_clips(&[(10, 20)]);
        let tb = sequence.time_base();
        let original = sequence.find_clip(ids[0]).expect("Clip").clone();
        let survivors =
            subtract_overwrite_range_from_clip(original.clone(), at(tb, 15), at(tb, 20))
                .expect("subtract");

        assert_eq!(survivors.len(), 2);
        let left = &survivors[0];
        let right = &survivors[1];
        assert_eq!(left.id, original.id);
        assert_eq!(left.duration, at(tb, 5));
        assert!(left.link_group.is_none());
        assert_ne!(
            right.id, original.id,
            "right fragment owns a fresh identity"
        );
        assert_eq!(right.position, at(tb, 20));
        assert_eq!(right.duration, at(tb, 10));
        assert_eq!(right.source_origin(), at(tb, 10));
        assert!(right.link_group.is_none());
    }

    #[test]
    fn subtract_covering_or_disjoint_ranges() {
        let (sequence, ids) = sequence_with_clips(&[(10, 10)]);
        let tb = sequence.time_base();
        let original = sequence.find_clip(ids[0]).expect("Clip").clone();

        let covered = subtract_overwrite_range_from_clip(original.clone(), at(tb, 0), at(tb, 30))
            .expect("subtract");
        assert!(covered.is_empty());

        let disjoint = subtract_overwrite_range_from_clip(original.clone(), at(tb, 30), at(tb, 40))
            .expect("subtract");
        assert_eq!(disjoint.len(), 1);
        assert_eq!(disjoint[0].id, original.id);

        let head = subtract_overwrite_range_from_clip(original.clone(), at(tb, 0), at(tb, 14))
            .expect("subtract");
        assert_eq!(head.len(), 1);
        assert_eq!(head[0].position, at(tb, 14));
        assert_eq!(head[0].duration, at(tb, 6));
        assert_eq!(head[0].source_origin(), at(tb, 4));

        let tail =
            subtract_overwrite_range_from_clip(original, at(tb, 16), at(tb, 40)).expect("subtract");
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].duration, at(tb, 6));
    }

    #[test]
    fn overwrite_conflicts_cut_only_non_focus_clips() {
        let (mut sequence, ids) = sequence_with_clips(&[(0, 10), (8, 10)]);
        let tb = sequence.time_base();
        let focus = HashSet::from([ids[1]]);
        apply_overwrite_conflicts(
            &mut sequence.video_tracks[0],
            &focus,
            vec![(at(tb, 8), at(tb, 18))],
        )
        .expect("overwrite");

        let track = &sequence.video_tracks[0];
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].id, ids[0]);
        assert_eq!(track.clips[0].duration, at(tb, 8));
        assert_eq!(track.clips[1].id, ids[1]);
        assert_eq!(track.clips[1].duration, at(tb, 10));
    }

    #[test]
    fn push_forward_preserves_every_clip() {
        let (mut sequence, ids) = sequence_with_clips(&[(8, 10), (0, 10)]);
        resolve_track_overlaps(&mut sequence.video_tracks[0]).expect("push forward");
        let track = &sequence.video_tracks[0];
        assert_eq!(track.clips.len(), 2);
        assert_eq!(track.clips[0].id, ids[1]);
        assert_eq!(track.clips[0].position, at(sequence.time_base(), 0));
        assert_eq!(track.clips[1].id, ids[0]);
        assert_eq!(track.clips[1].position, at(sequence.time_base(), 10));
    }
}
