//! Atomic professional Insert Edit over explicit target and ripple scopes.
//!
//! Track targeting and Sync-Lock are editor policy, not renderable Sequence
//! author state. Callers resolve those controls into one immutable request;
//! this module applies the resulting structural edit without consulting UI
//! globals or inferring participation from Track order.

use crate::{
    clip::Clip,
    clip_fragment::split_clip_at,
    sequence::Sequence,
    sequence_time_edit::{edit_sequence_automation, SequenceTimeEdit},
    track::{Track, TrackType},
};
use mondrian_core::{
    AudioTransitionId, ClipId, ClipLinkGroupId, TimelineTime, TimelineTimeRange, TrackId,
    VideoTransitionId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One Clip placement created inside an Insert Edit gap.
#[derive(Debug, Clone)]
pub struct InsertEditPlacement {
    /// Existing target Track.
    pub track_id: TrackId,
    /// Exact offset from the Insert boundary.
    pub offset: TimelineTime,
    /// Fully authored Clip to place. Its incoming `position` is ignored.
    pub clip: Clip,
}

/// Policy for Sequence-time automation affected by an Insert Edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertAutomationPolicy {
    /// Keep Sequence-time keys at their absolute positions.
    PreserveSequenceTime,
    /// Shift keys owned exclusively by editorial content in the ripple scope.
    ///
    /// Track controls and Track-source Routes follow their Track. A Bus or
    /// Output follows only when every authored input follows, preventing a
    /// partial multi-input mix from acquiring an arbitrary time interpretation.
    FollowEditorialContent,
}

/// Policy for a Transition whose edit geometry intersects the Insert boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertTransitionPolicy {
    /// Reject the complete edit instead of silently changing creative intent.
    RejectAffected,
    /// Explicitly remove only the Transitions whose geometry cannot survive.
    RemoveAffected,
}

/// Policy for Sequence navigation/range coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertTimelineStatePolicy {
    /// Leave playhead and In/Out points at absolute Sequence time.
    PreserveSequenceTime,
    /// Shift each coordinate at or after the Insert boundary.
    FollowEdit,
}

/// Complete deterministic request for one professional Insert Edit.
#[derive(Debug, Clone)]
pub struct InsertEditRequest {
    /// Exact Sequence-time insertion boundary.
    pub at: TimelineTime,
    /// Exact duration opened on every ripple Track.
    pub duration: TimelineTime,
    /// New placements, each contained by the opened interval.
    pub placements: Vec<InsertEditPlacement>,
    /// Explicit Track set resolved from targeting, Sync-Lock, and user intent.
    pub ripple_tracks: BTreeSet<TrackId>,
    /// Sequence-time automation behavior.
    pub automation_policy: InsertAutomationPolicy,
    /// Disposition for intersected video or audio Transitions.
    pub transition_policy: InsertTransitionPolicy,
    /// Playhead and In/Out behavior.
    pub timeline_state_policy: InsertTimelineStatePolicy,
}

/// One original Clip split at the Insert boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InsertSplitOutcome {
    /// Stable identity retained by the left fragment.
    pub left_clip_id: ClipId,
    /// New independently addressable right-fragment identity.
    pub right_clip_id: ClipId,
}

/// Structural evidence returned by a successful Insert Edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertEditOutcome {
    /// New Clip identities in request order.
    pub inserted_clip_ids: Vec<ClipId>,
    /// Existing whole Clips shifted by the opened duration.
    pub shifted_clip_ids: Vec<ClipId>,
    /// Existing Clips split at the boundary.
    pub split_clips: Vec<InsertSplitOutcome>,
    /// Video Transitions removed only under explicit removal policy.
    pub removed_video_transition_ids: Vec<VideoTransitionId>,
    /// Audio Transitions removed only under explicit removal policy.
    pub removed_audio_transition_ids: Vec<AudioTransitionId>,
}

/// Fail-closed Insert Edit validation or execution failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InsertEditError {
    /// Insert coordinates must describe a positive interval at Sequence zero or later.
    #[error("Insert Edit requires a non-negative boundary and positive duration")]
    InvalidRange,
    /// One referenced Track does not exist.
    #[error("Insert Edit references an unknown Track: {0}")]
    UnknownTrack(TrackId),
    /// Every target Track must also participate in the ripple scope.
    #[error("Insert Edit target Track is outside the ripple scope: {0}")]
    TargetOutsideRippleScope(TrackId),
    /// Locked Tracks cannot be targeted or rippled.
    #[error("Insert Edit cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
    /// One inserted placement is outside the opened interval.
    #[error("Insert Edit placement is outside the opened interval: {0}")]
    PlacementOutsideGap(ClipId),
    /// Inserted media does not match the target Track's author contract.
    #[error("Insert Edit placement {clip_id} is incompatible with Track {track_id}")]
    IncompatiblePlacement { clip_id: ClipId, track_id: TrackId },
    /// New Clip identities must be disjoint from the current Sequence and each other.
    #[error("Insert Edit contains a duplicate Clip identity: {0}")]
    DuplicateClip(ClipId),
    /// Placements on one target Track cannot overlap.
    #[error("Insert Edit placements overlap on Track: {0}")]
    OverlappingPlacements(TrackId),
    /// A linked group would receive different temporal transforms.
    #[error("Insert Edit would break Clip link group: {0}")]
    PartialLinkedGroup(ClipLinkGroupId),
    /// A new link group is incomplete or aliases an existing group.
    #[error("Insert Edit contains an invalid new Clip link group: {0}")]
    InvalidNewLinkGroup(ClipLinkGroupId),
    /// A Transition cannot preserve its edit geometry across this Insert.
    #[error("Insert Edit intersects a video Transition: {0}")]
    AffectedVideoTransition(VideoTransitionId),
    /// An audio Transition cannot preserve its interval/endpoints across this Insert.
    #[error("Insert Edit intersects an audio Transition: {0}")]
    AffectedAudioTransition(AudioTransitionId),
    /// Exact-time arithmetic or final author validation failed.
    #[error("Insert Edit failed: {0}")]
    AuthorState(String),
}

/// Apply one Insert Edit atomically to a Sequence.
///
/// The receiver is replaced only after the complete candidate, including
/// strong references and audio routing author state, validates successfully.
pub fn apply_insert_edit(
    sequence: &mut Sequence,
    request: &InsertEditRequest,
) -> Result<InsertEditOutcome, InsertEditError> {
    let mut candidate = sequence.clone();
    let outcome = apply_insert_edit_candidate(&mut candidate, request)?;
    candidate
        .validate_author_identities()
        .map_err(|error| InsertEditError::AuthorState(error.to_string()))?;
    candidate
        .audio_program
        .validate(
            &candidate.audio_tracks,
            &candidate.audio_roles,
            candidate.settings.audio_channel_layout,
        )
        .map_err(|error| InsertEditError::AuthorState(error.to_string()))?;
    *sequence = candidate;
    Ok(outcome)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemporalClass {
    Before,
    Crossing,
    Downstream,
}

fn apply_insert_edit_candidate(
    sequence: &mut Sequence,
    request: &InsertEditRequest,
) -> Result<InsertEditOutcome, InsertEditError> {
    validate_request(sequence, request)?;
    validate_existing_links(sequence, request)?;

    let (removed_video_transition_ids, removed_audio_transition_ids) =
        migrate_transitions(sequence, request)?;

    let mut shifted_clip_ids = Vec::new();
    let mut split_clips = Vec::new();
    let mut split_groups = BTreeMap::<ClipLinkGroupId, ClipLinkGroupId>::new();
    for track in sequence.video_tracks.iter_mut().chain(&mut sequence.audio_tracks) {
        if !request.ripple_tracks.contains(&track.id) {
            continue;
        }
        open_track_gap(
            track,
            request.at,
            request.duration,
            &mut split_groups,
            &mut shifted_clip_ids,
            &mut split_clips,
        )?;
    }

    if request.automation_policy == InsertAutomationPolicy::FollowEditorialContent {
        edit_sequence_automation(
            sequence,
            &request.ripple_tracks,
            SequenceTimeEdit::Insert { at: request.at, duration: request.duration },
        )
        .map_err(author_state)?;
    }
    if request.timeline_state_policy == InsertTimelineStatePolicy::FollowEdit {
        shift_timeline_state(sequence, request.at, request.duration)?;
    }

    let mut inserted_clip_ids = Vec::with_capacity(request.placements.len());
    for placement in &request.placements {
        let mut clip = placement.clip.clone();
        clip.position = request.at.checked_add(placement.offset).map_err(author_state)?;
        inserted_clip_ids.push(clip.id);
        let track = track_mut(sequence, placement.track_id)
            .ok_or(InsertEditError::UnknownTrack(placement.track_id))?;
        track.add_clip(clip).map_err(author_state)?;
    }
    for track in sequence.video_tracks.iter_mut().chain(&mut sequence.audio_tracks) {
        track.clips.sort_unstable_by_key(|clip| clip.position);
    }

    Ok(InsertEditOutcome {
        inserted_clip_ids,
        shifted_clip_ids,
        split_clips,
        removed_video_transition_ids,
        removed_audio_transition_ids,
    })
}

fn validate_request(
    sequence: &Sequence,
    request: &InsertEditRequest,
) -> Result<(), InsertEditError> {
    if request.at.is_negative() || request.duration <= TimelineTime::ZERO {
        return Err(InsertEditError::InvalidRange);
    }
    for track_id in &request.ripple_tracks {
        let track = track(sequence, *track_id).ok_or(InsertEditError::UnknownTrack(*track_id))?;
        if track.is_locked {
            return Err(InsertEditError::LockedTrack(*track_id));
        }
    }

    let existing_clip_ids = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .map(|clip| clip.id)
        .collect::<BTreeSet<_>>();
    let existing_groups = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .filter_map(|clip| clip.link_group)
        .collect::<BTreeSet<_>>();
    let mut inserted_clip_ids = BTreeSet::new();
    let mut inserted_groups = BTreeMap::<ClipLinkGroupId, usize>::new();
    let mut ranges = BTreeMap::<TrackId, Vec<TimelineTimeRange>>::new();

    for placement in &request.placements {
        let target = track(sequence, placement.track_id)
            .ok_or(InsertEditError::UnknownTrack(placement.track_id))?;
        if !request.ripple_tracks.contains(&placement.track_id) {
            return Err(InsertEditError::TargetOutsideRippleScope(
                placement.track_id,
            ));
        }
        if target.is_locked {
            return Err(InsertEditError::LockedTrack(placement.track_id));
        }
        if existing_clip_ids.contains(&placement.clip.id)
            || !inserted_clip_ids.insert(placement.clip.id)
        {
            return Err(InsertEditError::DuplicateClip(placement.clip.id));
        }
        if placement.offset.is_negative()
            || placement.clip.duration <= TimelineTime::ZERO
            || placement.offset.checked_add(placement.clip.duration).map_err(author_state)?
                > request.duration
        {
            return Err(InsertEditError::PlacementOutsideGap(placement.clip.id));
        }
        let compatible = match target.track_type {
            TrackType::Video => placement.clip.audio_components.is_empty(),
            TrackType::Audio => !placement.clip.audio_components.is_empty(),
            TrackType::Subtitle => false,
        };
        if !compatible {
            return Err(InsertEditError::IncompatiblePlacement {
                clip_id: placement.clip.id,
                track_id: placement.track_id,
            });
        }
        if let Some(group) = placement.clip.link_group {
            *inserted_groups.entry(group).or_default() += 1;
        }
        ranges.entry(placement.track_id).or_default().push(
            TimelineTimeRange::new(placement.offset, placement.clip.duration)
                .map_err(author_state)?,
        );
    }

    for (group, count) in inserted_groups {
        if count < 2 || existing_groups.contains(&group) {
            return Err(InsertEditError::InvalidNewLinkGroup(group));
        }
    }
    for (track_id, mut track_ranges) in ranges {
        track_ranges.sort_unstable_by_key(|range| range.start);
        for pair in track_ranges.windows(2) {
            if pair[1].start < pair[0].end().map_err(author_state)? {
                return Err(InsertEditError::OverlappingPlacements(track_id));
            }
        }
    }
    Ok(())
}

fn validate_existing_links(
    sequence: &Sequence,
    request: &InsertEditRequest,
) -> Result<(), InsertEditError> {
    let mut groups = BTreeMap::<ClipLinkGroupId, Vec<(TrackId, TemporalClass)>>::new();
    for owner in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
        for clip in &owner.clips {
            if let Some(group) = clip.link_group {
                groups
                    .entry(group)
                    .or_default()
                    .push((owner.id, classify_clip(clip, request.at)?));
            }
        }
    }
    for (group, members) in groups {
        let affected = members.iter().any(|(track_id, class)| {
            request.ripple_tracks.contains(track_id) && *class != TemporalClass::Before
        });
        if !affected {
            continue;
        }
        let expected = members[0].1;
        if members.iter().any(|(track_id, class)| {
            !request.ripple_tracks.contains(track_id) || *class != expected
        }) {
            return Err(InsertEditError::PartialLinkedGroup(group));
        }
    }
    Ok(())
}

fn migrate_transitions(
    sequence: &mut Sequence,
    request: &InsertEditRequest,
) -> Result<(Vec<VideoTransitionId>, Vec<AudioTransitionId>), InsertEditError> {
    let clip_locations = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| {
            track
                .clips
                .iter()
                .map(move |clip| (clip.id, (track.id, classify_clip(clip, request.at))))
        })
        .map(|(id, (track_id, class))| class.map(|class| (id, (track_id, class))))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut component_locations = BTreeMap::new();
    for track in &sequence.audio_tracks {
        for clip in &track.clips {
            let class = classify_clip(clip, request.at)?;
            for edit in &clip.audio_components {
                component_locations.insert(edit.id, (track.id, class));
            }
        }
    }

    let mut removed_video = Vec::new();
    let mut retained_video = Vec::with_capacity(sequence.video_transitions.len());
    for mut transition in std::mem::take(&mut sequence.video_transitions) {
        let Some((track_id, left_class)) = clip_locations.get(&transition.left).copied() else {
            return Err(InsertEditError::AuthorState(format!(
                "video Transition {} has an unresolved left endpoint",
                transition.id
            )));
        };
        let Some((right_track_id, right_class)) = clip_locations.get(&transition.right).copied()
        else {
            return Err(InsertEditError::AuthorState(format!(
                "video Transition {} has an unresolved right endpoint",
                transition.id
            )));
        };
        if track_id != right_track_id || !request.ripple_tracks.contains(&track_id) {
            retained_video.push(transition);
            continue;
        }
        match (left_class, right_class) {
            (TemporalClass::Before, TemporalClass::Before) => retained_video.push(transition),
            (TemporalClass::Downstream, TemporalClass::Downstream) => {
                transition.sequence_range.start = transition
                    .sequence_range
                    .start
                    .checked_add(request.duration)
                    .map_err(author_state)?;
                retained_video.push(transition);
            }
            _ if request.transition_policy == InsertTransitionPolicy::RemoveAffected => {
                removed_video.push(transition.id);
            }
            _ => return Err(InsertEditError::AffectedVideoTransition(transition.id)),
        }
    }
    sequence.video_transitions = retained_video;

    let mut removed_audio = Vec::new();
    let mut retained_audio = Vec::with_capacity(sequence.audio_program.transitions.len());
    for mut transition in std::mem::take(&mut sequence.audio_program.transitions) {
        let Some((track_id, left_class)) = component_locations.get(&transition.left).copied()
        else {
            return Err(InsertEditError::AuthorState(format!(
                "audio Transition {} has an unresolved left endpoint",
                transition.id
            )));
        };
        let Some((right_track_id, right_class)) =
            component_locations.get(&transition.right).copied()
        else {
            return Err(InsertEditError::AuthorState(format!(
                "audio Transition {} has an unresolved right endpoint",
                transition.id
            )));
        };
        let affected = request.ripple_tracks.contains(&track_id)
            || request.ripple_tracks.contains(&right_track_id);
        if !affected {
            retained_audio.push(transition);
            continue;
        }
        match (left_class, right_class) {
            (TemporalClass::Before, TemporalClass::Before) => retained_audio.push(transition),
            (TemporalClass::Downstream, TemporalClass::Downstream)
                if request.ripple_tracks.contains(&track_id)
                    && request.ripple_tracks.contains(&right_track_id) =>
            {
                transition.sequence_range.start = transition
                    .sequence_range
                    .start
                    .checked_add(request.duration)
                    .map_err(author_state)?;
                retained_audio.push(transition);
            }
            _ if request.transition_policy == InsertTransitionPolicy::RemoveAffected => {
                removed_audio.push(transition.id);
            }
            _ => return Err(InsertEditError::AffectedAudioTransition(transition.id)),
        }
    }
    sequence.audio_program.transitions = retained_audio;
    Ok((removed_video, removed_audio))
}

fn open_track_gap(
    track: &mut Track,
    at: TimelineTime,
    duration: TimelineTime,
    split_groups: &mut BTreeMap<ClipLinkGroupId, ClipLinkGroupId>,
    shifted_clip_ids: &mut Vec<ClipId>,
    split_clips: &mut Vec<InsertSplitOutcome>,
) -> Result<(), InsertEditError> {
    let mut opened = Vec::with_capacity(track.clips.len().saturating_add(1));
    for clip in std::mem::take(&mut track.clips) {
        match classify_clip(&clip, at)? {
            TemporalClass::Before => opened.push(clip),
            TemporalClass::Downstream => {
                let mut shifted = clip;
                shifted.position = shifted.position.checked_add(duration).map_err(author_state)?;
                shifted_clip_ids.push(shifted.id);
                opened.push(shifted);
            }
            TemporalClass::Crossing => {
                let (left, mut right) = split_clip_at(clip, at).map_err(author_state)?;
                if let Some(group) = right.link_group {
                    right.link_group = Some(*split_groups.entry(group).or_default());
                }
                right.position = right.position.checked_add(duration).map_err(author_state)?;
                split_clips
                    .push(InsertSplitOutcome { left_clip_id: left.id, right_clip_id: right.id });
                opened.push(left);
                opened.push(right);
            }
        }
    }
    opened.sort_unstable_by_key(|clip| clip.position);
    track.clips = opened;
    Ok(())
}

fn shift_timeline_state(
    sequence: &mut Sequence,
    boundary: TimelineTime,
    delta: TimelineTime,
) -> Result<(), InsertEditError> {
    if sequence.playhead >= boundary {
        sequence.playhead = sequence.playhead.checked_add(delta).map_err(author_state)?;
    }
    if let Some(time) = sequence.in_point.filter(|time| *time >= boundary) {
        sequence.in_point = Some(time.checked_add(delta).map_err(author_state)?);
    }
    if let Some(time) = sequence.out_point.filter(|time| *time >= boundary) {
        sequence.out_point = Some(time.checked_add(delta).map_err(author_state)?);
    }
    Ok(())
}

fn classify_clip(clip: &Clip, boundary: TimelineTime) -> Result<TemporalClass, InsertEditError> {
    let end = clip.end_position().map_err(author_state)?;
    Ok(if end <= boundary {
        TemporalClass::Before
    } else if clip.position >= boundary {
        TemporalClass::Downstream
    } else {
        TemporalClass::Crossing
    })
}

fn track(sequence: &Sequence, id: TrackId) -> Option<&Track> {
    sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .find(|track| track.id == id)
}

fn track_mut(sequence: &mut Sequence, id: TrackId) -> Option<&mut Track> {
    sequence
        .video_tracks
        .iter_mut()
        .chain(&mut sequence.audio_tracks)
        .find(|track| track.id == id)
}

fn author_state(error: impl std::fmt::Display) -> InsertEditError {
    InsertEditError::AuthorState(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clip;
    use mondrian_core::{
        AssetId, AudioSourceComponentId, ExactAutomationCurve, ExactAutomationKeyframe,
        FramePosition, Keyframe, ParameterId, PropertyValue, Rational,
    };

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, Rational::new(1, 25)))
            .expect("test time")
    }

    fn video_clip(start: i64, duration: i64) -> Clip {
        Clip::new(AssetId::new(), tt(start), tt(duration)).expect("video Clip")
    }

    fn add_audio_clip(
        sequence: &mut Sequence,
        track_id: TrackId,
        start: i64,
        duration: i64,
    ) -> ClipId {
        let clip = Clip::new(AssetId::new(), tt(start), tt(duration)).expect("audio Clip");
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("add audio Clip")
    }

    fn request(
        at: i64,
        duration: i64,
        ripple_tracks: impl IntoIterator<Item = TrackId>,
    ) -> InsertEditRequest {
        InsertEditRequest {
            at: tt(at),
            duration: tt(duration),
            placements: Vec::new(),
            ripple_tracks: ripple_tracks.into_iter().collect(),
            automation_policy: InsertAutomationPolicy::FollowEditorialContent,
            transition_policy: InsertTransitionPolicy::RejectAffected,
            timeline_state_policy: InsertTimelineStatePolicy::FollowEdit,
        }
    }

    #[test]
    fn insert_splits_crossing_clips_shifts_downstream_and_places_content() {
        let mut sequence = Sequence::new("insert");
        let track_id = sequence.video_tracks[0].id;
        let crossing = video_clip(0, 20);
        let crossing_id = crossing.id;
        let downstream = video_clip(25, 5);
        let downstream_id = downstream.id;
        sequence.video_tracks[0].add_clip(crossing).expect("crossing");
        sequence.video_tracks[0].add_clip(downstream).expect("downstream");
        let inserted = video_clip(99, 5);
        let inserted_id = inserted.id;
        let mut edit = request(10, 5, [track_id]);
        edit.placements.push(InsertEditPlacement {
            track_id,
            offset: TimelineTime::ZERO,
            clip: inserted,
        });

        let outcome = apply_insert_edit(&mut sequence, &edit).expect("Insert Edit");
        let track = &sequence.video_tracks[0];
        assert_eq!(outcome.inserted_clip_ids, [inserted_id]);
        assert_eq!(outcome.shifted_clip_ids, [downstream_id]);
        assert_eq!(outcome.split_clips.len(), 1);
        assert_eq!(outcome.split_clips[0].left_clip_id, crossing_id);
        assert_eq!(
            track.clips.iter().map(|clip| clip.position).collect::<Vec<_>>(),
            [tt(0), tt(10), tt(15), tt(30)]
        );
        assert_eq!(
            track.clips.iter().find(|clip| clip.id == crossing_id).expect("left").duration,
            tt(10)
        );
    }

    #[test]
    fn linked_group_must_receive_one_coherent_transform() {
        let mut sequence = Sequence::new("links");
        let video_track = sequence.video_tracks[0].id;
        let audio_track = sequence.audio_tracks[0].id;
        let group = ClipLinkGroupId::new();
        let mut video = video_clip(10, 10);
        video.link_group = Some(group);
        sequence.video_tracks[0].add_clip(video).expect("video");
        let audio_id = add_audio_clip(&mut sequence, audio_track, 10, 10);
        sequence.audio_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == audio_id)
            .expect("audio")
            .link_group = Some(group);

        let error =
            apply_insert_edit(&mut sequence, &request(5, 5, [video_track])).expect_err("partial");
        assert_eq!(error, InsertEditError::PartialLinkedGroup(group));

        let outcome = apply_insert_edit(&mut sequence, &request(5, 5, [video_track, audio_track]))
            .expect("coherent linked Insert");
        assert_eq!(outcome.shifted_clip_ids.len(), 2);
    }

    #[test]
    fn affected_transition_rejects_without_mutating_sequence() {
        let mut sequence = Sequence::new("transition");
        let track_id = sequence.video_tracks[0].id;
        let left = video_clip(0, 10);
        let left_id = left.id;
        let right = video_clip(10, 10);
        let right_id = right.id;
        sequence.video_tracks[0].add_clip(left).expect("left");
        sequence.video_tracks[0].add_clip(right).expect("right");
        let transition = crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8), tt(4)).expect("range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);
        let before = serde_json::to_value(&sequence).expect("serialize before");

        let error =
            apply_insert_edit(&mut sequence, &request(10, 5, [track_id])).expect_err("reject");
        assert_eq!(
            error,
            InsertEditError::AffectedVideoTransition(transition_id)
        );
        assert_eq!(
            serde_json::to_value(&sequence).expect("serialize after"),
            before
        );
    }

    #[test]
    fn follow_content_shifts_track_automation_but_not_unrelated_track() {
        let mut sequence = Sequence::new("automation");
        let shifted_track = sequence.video_tracks[0].id;
        let stationary_track = sequence.video_tracks[1].id;
        for track in &mut sequence.video_tracks[..2] {
            track.opacity.set_animation_enabled(true);
            track
                .opacity
                .set_exact_keyframe(Keyframe::linear(tt(12), PropertyValue::Float(0.5)))
                .expect("keyframe");
        }

        apply_insert_edit(&mut sequence, &request(10, 5, [shifted_track])).expect("Insert Edit");
        assert_eq!(sequence.video_tracks[0].opacity.keyframe_times(), [tt(17)]);
        assert_eq!(sequence.video_tracks[1].id, stationary_track);
        assert_eq!(sequence.video_tracks[1].opacity.keyframe_times(), [tt(12)]);
    }

    #[test]
    fn crossing_audio_split_preserves_only_outer_edge_fades() {
        use crate::audio::{AudioFade, AudioFadeCurve};

        let mut sequence = Sequence::new("fades");
        let track_id = sequence.audio_tracks[0].id;
        let clip_id = add_audio_clip(&mut sequence, track_id, 0, 20);
        let clip = sequence.audio_tracks[0]
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id)
            .expect("Clip");
        let fade = AudioFade { duration: tt(2), curve: AudioFadeCurve::EqualPower };
        clip.audio_components[0].fades.fade_in = Some(fade);
        clip.audio_components[0].fades.fade_out = Some(fade);

        let outcome =
            apply_insert_edit(&mut sequence, &request(10, 5, [track_id])).expect("Insert Edit");
        let right_id = outcome.split_clips[0].right_clip_id;
        let left = sequence.audio_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("left");
        let right = sequence.audio_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == right_id)
            .expect("right");
        assert_eq!(left.audio_components[0].fades.fade_in, Some(fade));
        assert_eq!(left.audio_components[0].fades.fade_out, None);
        assert_eq!(right.audio_components[0].fades.fade_in, None);
        assert_eq!(right.audio_components[0].fades.fade_out, Some(fade));
    }

    #[test]
    fn downstream_transition_moves_as_one_valid_edit() {
        let mut sequence = Sequence::new("downstream transition");
        let track_id = sequence.video_tracks[0].id;
        let left = video_clip(20, 10);
        let left_id = left.id;
        let right = video_clip(30, 10);
        let right_id = right.id;
        sequence.video_tracks[0].add_clip(left).expect("left");
        sequence.video_tracks[0].add_clip(right).expect("right");
        let transition = crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(28), tt(4)).expect("range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);

        apply_insert_edit(&mut sequence, &request(10, 5, [track_id]))
            .expect("downstream transition Insert");
        let moved = sequence
            .video_transitions
            .iter()
            .find(|transition| transition.id == transition_id)
            .expect("transition retained");
        assert_eq!(moved.sequence_range.start, tt(33));
        assert_eq!(sequence.video_tracks[0].clips[0].position, tt(25));
        assert_eq!(sequence.video_tracks[0].clips[1].position, tt(35));
    }

    #[test]
    fn explicit_policy_can_remove_only_intersected_transition() {
        let mut sequence = Sequence::new("remove transition");
        let track_id = sequence.video_tracks[0].id;
        let left = video_clip(0, 10);
        let left_id = left.id;
        let right = video_clip(10, 10);
        let right_id = right.id;
        sequence.video_tracks[0].add_clip(left).expect("left");
        sequence.video_tracks[0].add_clip(right).expect("right");
        let transition = crate::VideoTransition::cross_dissolve(
            left_id,
            right_id,
            TimelineTimeRange::new(tt(8), tt(4)).expect("range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);
        let mut edit = request(10, 5, [track_id]);
        edit.transition_policy = InsertTransitionPolicy::RemoveAffected;

        let outcome = apply_insert_edit(&mut sequence, &edit).expect("remove affected");
        assert_eq!(outcome.removed_video_transition_ids, [transition_id]);
        assert!(sequence.video_transitions.is_empty());
    }

    #[test]
    fn aggregate_output_automation_follows_only_complete_audio_input_scope() {
        fn curve() -> ExactAutomationCurve {
            let mut curve = ExactAutomationCurve::new(
                ParameterId::new_static(crate::audio::FADER_DB_PARAMETER_ID),
                0.0,
            )
            .expect("curve");
            curve.set_keyframe(ExactAutomationKeyframe::linear(tt(12), -6.0)).expect("key");
            curve
        }

        let mut partial = Sequence::new("partial audio");
        let first_track = partial.audio_tracks[0].id;
        partial.audio_program.outputs[0].strip.fader_automation = Some(curve());
        partial
            .audio_program
            .track_channels
            .get_mut(&first_track)
            .expect("track channel")
            .strip
            .fader_automation = Some(curve());
        apply_insert_edit(&mut partial, &request(10, 5, [first_track]))
            .expect("partial audio Insert");
        assert_eq!(
            partial.audio_program.track_channels[&first_track]
                .strip
                .fader_automation
                .as_ref()
                .expect("track curve")
                .keyframes[0]
                .time,
            tt(17)
        );
        assert_eq!(
            partial.audio_program.outputs[0]
                .strip
                .fader_automation
                .as_ref()
                .expect("output curve")
                .keyframes[0]
                .time,
            tt(12)
        );

        let mut complete = Sequence::new("complete audio");
        complete.audio_program.outputs[0].strip.fader_automation = Some(curve());
        let all_tracks = complete.audio_tracks.iter().map(|track| track.id).collect::<Vec<_>>();
        apply_insert_edit(&mut complete, &request(10, 5, all_tracks))
            .expect("complete audio Insert");
        assert_eq!(
            complete.audio_program.outputs[0]
                .strip
                .fader_automation
                .as_ref()
                .expect("output curve")
                .keyframes[0]
                .time,
            tt(17)
        );
    }

    #[test]
    fn locked_ripple_track_rejects_without_mutation() {
        let mut sequence = Sequence::new("locked");
        let track_id = sequence.video_tracks[0].id;
        sequence.video_tracks[0].is_locked = true;
        let before = serde_json::to_value(&sequence).expect("serialize before");
        assert_eq!(
            apply_insert_edit(&mut sequence, &request(0, 5, [track_id])),
            Err(InsertEditError::LockedTrack(track_id))
        );
        assert_eq!(
            serde_json::to_value(&sequence).expect("serialize after"),
            before
        );
    }

    #[test]
    fn invalid_inserted_audio_is_rejected_before_mutation() {
        let mut sequence = Sequence::new("invalid");
        let track_id = sequence.audio_tracks[0].id;
        let mut edit = request(0, 5, [track_id]);
        edit.placements.push(InsertEditPlacement {
            track_id,
            offset: TimelineTime::ZERO,
            clip: video_clip(0, 5),
        });
        assert!(matches!(
            apply_insert_edit(&mut sequence, &edit),
            Err(InsertEditError::IncompatiblePlacement { .. })
        ));
    }
}
