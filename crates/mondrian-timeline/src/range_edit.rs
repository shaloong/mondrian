//! Atomic Lift and Extract edits over an explicit Sequence-time range.
//!
//! Track Targeting and Sync-Lock are editor-session policy. Callers resolve
//! those controls into explicit content and ripple Track sets; this Module owns
//! the complete author transform without consulting selection or UI state.

use crate::{
    clip::Clip,
    clip_fragment::{split_clip_at, trim_clip_in_to, trim_clip_out_to},
    sequence::Sequence,
    sequence_time_edit::{edit_sequence_automation, SequenceTimeEdit},
    track::Track,
};
use mondrian_core::{
    AudioComponentEditId, AudioTransitionId, ClipId, ClipLinkGroupId, TimelineTime,
    TimelineTimeRange, TrackId, VideoTransitionId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Program-time removal behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeEditKind {
    /// Remove content inside the range and leave program time unchanged.
    Lift,
    /// Remove content and close the range on every ripple Track.
    Extract,
}

/// Policy for Sequence-time automation owned by affected routing closures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeEditAutomationPolicy {
    /// Keep Sequence-time keys at their absolute coordinates.
    PreserveSequenceTime,
    /// Extract the same range from automation whose complete input follows.
    FollowEditorialContent,
}

/// Policy for Transitions whose endpoints or range are changed by the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeEditTransitionPolicy {
    /// Reject the complete operation instead of changing Transition intent.
    RejectAffected,
    /// Remove affected Transitions as an explicit part of the range edit.
    RemoveAffected,
}

/// Policy for authored Sequence navigation coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RangeEditTimelineStatePolicy {
    /// Preserve playhead and In/Out coordinates.
    PreserveSequenceTime,
    /// Place the playhead at the range start and clear the consumed In/Out.
    CollapseToRangeStart,
}

/// Complete deterministic request for one Lift or Extract operation.
#[derive(Debug, Clone)]
pub struct RangeEditRequest {
    /// Lift or Extract semantics.
    pub kind: RangeEditKind,
    /// Positive half-open Sequence-time range to remove.
    pub range: TimelineTimeRange,
    /// Tracks whose content is cut by the range.
    pub content_tracks: BTreeSet<TrackId>,
    /// Tracks whose downstream placements close the gap for Extract.
    ///
    /// This must be empty for Lift and contain every content Track for Extract.
    pub ripple_tracks: BTreeSet<TrackId>,
    /// Sequence-time automation behavior.
    pub automation_policy: RangeEditAutomationPolicy,
    /// Disposition for affected video or audio Transitions.
    pub transition_policy: RangeEditTransitionPolicy,
    /// Playhead and In/Out behavior.
    pub timeline_state_policy: RangeEditTimelineStatePolicy,
}

/// One original Clip split into two independently addressable survivors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeEditSplitOutcome {
    /// Stable identity retained by the left fragment.
    pub left_clip_id: ClipId,
    /// Fresh identity assigned to the right fragment.
    pub right_clip_id: ClipId,
}

/// Structural evidence returned by a successful range edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeEditOutcome {
    /// Clips completely removed by the range.
    pub removed_clip_ids: Vec<ClipId>,
    /// Existing Clips whose in or out edge was trimmed.
    pub trimmed_clip_ids: Vec<ClipId>,
    /// Existing whole or surviving right-side Clips shifted by Extract.
    pub shifted_clip_ids: Vec<ClipId>,
    /// Clips that survived as independent left and right fragments.
    pub split_clips: Vec<RangeEditSplitOutcome>,
    /// Video Transitions explicitly removed by policy.
    pub removed_video_transition_ids: Vec<VideoTransitionId>,
    /// Audio Transitions explicitly removed by policy.
    pub removed_audio_transition_ids: Vec<AudioTransitionId>,
}

/// Fail-closed Lift/Extract validation or execution failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RangeEditError {
    /// Range start must be non-negative and duration positive.
    #[error("Lift/Extract requires a non-negative start and positive duration")]
    InvalidRange,
    /// At least one content Track must be targeted.
    #[error("Lift/Extract requires at least one content Track")]
    EmptyContentScope,
    /// One referenced Track does not exist.
    #[error("Lift/Extract references an unknown Track: {0}")]
    UnknownTrack(TrackId),
    /// A locked Track cannot be cut or rippled.
    #[error("Lift/Extract cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
    /// Lift must not carry a ripple scope.
    #[error("Lift cannot carry ripple Tracks")]
    LiftHasRippleScope,
    /// Every Extract content Track must also participate in ripple.
    #[error("Extract content Track is outside the ripple scope: {0}")]
    ContentOutsideRippleScope(TrackId),
    /// A Sync-Locked-only Track contains protected content in the cut range.
    #[error(
        "Extract cannot close Track {0} without cutting its intersecting content; target the Track or disable Sync-Lock"
    )]
    ProtectedRippleContent(TrackId),
    /// A linked group would receive inconsistent temporal transforms.
    #[error("Lift/Extract would break Clip link group: {0}")]
    PartialLinkedGroup(ClipLinkGroupId),
    /// A Transition cannot preserve its edit geometry.
    #[error("Lift/Extract affects a video Transition: {0}")]
    AffectedVideoTransition(VideoTransitionId),
    /// An audio Transition cannot preserve its interval/endpoints.
    #[error("Lift/Extract affects an audio Transition: {0}")]
    AffectedAudioTransition(AudioTransitionId),
    /// Exact-time arithmetic or final author validation failed.
    #[error("Lift/Extract failed: {0}")]
    AuthorState(String),
}

/// Assess deterministic structural blockers without cloning or mutating author state.
///
/// Execution additionally validates the fully transformed automation, audio,
/// identity, and strong-reference candidate before publication.
pub fn assess_range_edit(
    sequence: &Sequence,
    request: &RangeEditRequest,
) -> Result<(), RangeEditError> {
    validate_request(sequence, request)?;
    validate_link_groups(sequence, request)?;
    assess_transitions(sequence, request)
}

/// Apply one Lift or Extract atomically to a Sequence.
///
/// The receiver is replaced only after Clip identities, link groups,
/// Transitions, audio routing, and automation all validate successfully.
pub fn apply_range_edit(
    sequence: &mut Sequence,
    request: &RangeEditRequest,
) -> Result<RangeEditOutcome, RangeEditError> {
    let mut candidate = sequence.clone();
    let outcome = apply_range_edit_candidate(&mut candidate, request)?;
    validate_range_edit_candidate(&mut candidate)?;
    *sequence = candidate;
    Ok(outcome)
}

fn validate_range_edit_candidate(sequence: &mut Sequence) -> Result<(), RangeEditError> {
    sequence.compact_structural_references();
    sequence
        .validate_author_identities()
        .map_err(|error| RangeEditError::AuthorState(error.to_string()))?;
    sequence
        .audio_program
        .validate(
            &sequence.audio_tracks,
            &sequence.audio_roles,
            sequence.settings.audio_channel_layout,
        )
        .map_err(|error| RangeEditError::AuthorState(error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipRangeClass {
    Before,
    After,
    Inside,
    OverlapStart,
    OverlapEnd,
    Spanning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipTransform {
    Unchanged,
    Shift,
    Remove,
    TrimOut,
    TrimIn,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransitionEdit {
    Retain,
    Shift,
    Affected,
}

fn apply_range_edit_candidate(
    sequence: &mut Sequence,
    request: &RangeEditRequest,
) -> Result<RangeEditOutcome, RangeEditError> {
    validate_request(sequence, request)?;
    validate_link_groups(sequence, request)?;
    let (removed_video_transition_ids, removed_audio_transition_ids) =
        migrate_transitions(sequence, request)?;

    let mut outcome = RangeEditOutcome {
        removed_clip_ids: Vec::new(),
        trimmed_clip_ids: Vec::new(),
        shifted_clip_ids: Vec::new(),
        split_clips: Vec::new(),
        removed_video_transition_ids,
        removed_audio_transition_ids,
    };
    let mut split_groups = BTreeMap::<ClipLinkGroupId, ClipLinkGroupId>::new();
    for track in sequence.video_tracks.iter_mut().chain(&mut sequence.audio_tracks) {
        if request.content_tracks.contains(&track.id) {
            edit_content_track(track, request, &mut split_groups, &mut outcome)?;
        } else if request.kind == RangeEditKind::Extract
            && request.ripple_tracks.contains(&track.id)
        {
            close_protected_track_gap(track, request.range, &mut outcome)?;
        }
    }

    if request.kind == RangeEditKind::Extract
        && request.automation_policy == RangeEditAutomationPolicy::FollowEditorialContent
    {
        edit_sequence_automation(
            sequence,
            &request.ripple_tracks,
            SequenceTimeEdit::Extract { range: request.range },
        )
        .map_err(author_state)?;
    }
    if request.timeline_state_policy == RangeEditTimelineStatePolicy::CollapseToRangeStart {
        sequence.playhead = request.range.start;
        sequence.in_point = None;
        sequence.out_point = None;
    }
    Ok(outcome)
}

fn validate_request(sequence: &Sequence, request: &RangeEditRequest) -> Result<(), RangeEditError> {
    if request.range.start.is_negative() || request.range.duration <= TimelineTime::ZERO {
        return Err(RangeEditError::InvalidRange);
    }
    request.range.end().map_err(author_state)?;
    if request.content_tracks.is_empty() {
        return Err(RangeEditError::EmptyContentScope);
    }
    match request.kind {
        RangeEditKind::Lift if !request.ripple_tracks.is_empty() => {
            return Err(RangeEditError::LiftHasRippleScope);
        }
        RangeEditKind::Extract => {
            if let Some(track_id) = request
                .content_tracks
                .iter()
                .find(|track_id| !request.ripple_tracks.contains(track_id))
            {
                return Err(RangeEditError::ContentOutsideRippleScope(*track_id));
            }
        }
        RangeEditKind::Lift => {}
    }

    for track_id in request.content_tracks.iter().chain(&request.ripple_tracks) {
        let track = track(sequence, *track_id).ok_or(RangeEditError::UnknownTrack(*track_id))?;
        if track.is_locked {
            return Err(RangeEditError::LockedTrack(*track_id));
        }
    }

    if request.kind == RangeEditKind::Extract {
        for track_id in request.ripple_tracks.difference(&request.content_tracks) {
            let track =
                track(sequence, *track_id).ok_or(RangeEditError::UnknownTrack(*track_id))?;
            if track.clips.iter().any(|clip| {
                classify_clip(clip, request.range).is_ok_and(|class| {
                    !matches!(class, ClipRangeClass::Before | ClipRangeClass::After)
                })
            }) {
                return Err(RangeEditError::ProtectedRippleContent(*track_id));
            }
        }
    }
    Ok(())
}

fn validate_link_groups(
    sequence: &Sequence,
    request: &RangeEditRequest,
) -> Result<(), RangeEditError> {
    let mut groups = BTreeMap::<ClipLinkGroupId, Vec<(TrackId, ClipRangeClass)>>::new();
    for owner in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
        for clip in &owner.clips {
            if let Some(group) = clip.link_group {
                groups
                    .entry(group)
                    .or_default()
                    .push((owner.id, classify_clip(clip, request.range)?));
            }
        }
    }

    for (group, members) in groups {
        let transforms = members
            .iter()
            .map(|(track_id, class)| clip_transform(*track_id, *class, request))
            .collect::<Vec<_>>();
        let Some(expected) = transforms.first().copied() else {
            continue;
        };
        if transforms.iter().any(|transform| *transform != expected) {
            return Err(RangeEditError::PartialLinkedGroup(group));
        }
    }
    Ok(())
}

fn clip_transform(
    track_id: TrackId,
    class: ClipRangeClass,
    request: &RangeEditRequest,
) -> ClipTransform {
    if request.content_tracks.contains(&track_id) {
        match class {
            ClipRangeClass::Before => ClipTransform::Unchanged,
            ClipRangeClass::After => {
                if request.kind == RangeEditKind::Extract {
                    ClipTransform::Shift
                } else {
                    ClipTransform::Unchanged
                }
            }
            ClipRangeClass::Inside => ClipTransform::Remove,
            ClipRangeClass::OverlapStart => ClipTransform::TrimOut,
            ClipRangeClass::OverlapEnd => ClipTransform::TrimIn,
            ClipRangeClass::Spanning => ClipTransform::Split,
        }
    } else if request.kind == RangeEditKind::Extract
        && request.ripple_tracks.contains(&track_id)
        && class == ClipRangeClass::After
    {
        ClipTransform::Shift
    } else {
        ClipTransform::Unchanged
    }
}

fn edit_content_track(
    track: &mut Track,
    request: &RangeEditRequest,
    split_groups: &mut BTreeMap<ClipLinkGroupId, ClipLinkGroupId>,
    outcome: &mut RangeEditOutcome,
) -> Result<(), RangeEditError> {
    let end = request.range.end().map_err(author_state)?;
    let mut edited = Vec::with_capacity(track.clips.len().saturating_add(1));
    for mut clip in std::mem::take(&mut track.clips) {
        match classify_clip(&clip, request.range)? {
            ClipRangeClass::Before => edited.push(clip),
            ClipRangeClass::After => {
                if request.kind == RangeEditKind::Extract {
                    clip.position =
                        clip.position.checked_sub(request.range.duration).map_err(author_state)?;
                    outcome.shifted_clip_ids.push(clip.id);
                }
                edited.push(clip);
            }
            ClipRangeClass::Inside => outcome.removed_clip_ids.push(clip.id),
            ClipRangeClass::OverlapStart => {
                trim_clip_out_to(&mut clip, request.range.start).map_err(author_state)?;
                outcome.trimmed_clip_ids.push(clip.id);
                edited.push(clip);
            }
            ClipRangeClass::OverlapEnd => {
                trim_clip_in_to(&mut clip, end).map_err(author_state)?;
                outcome.trimmed_clip_ids.push(clip.id);
                if request.kind == RangeEditKind::Extract {
                    clip.position =
                        clip.position.checked_sub(request.range.duration).map_err(author_state)?;
                    outcome.shifted_clip_ids.push(clip.id);
                }
                edited.push(clip);
            }
            ClipRangeClass::Spanning => {
                let (left, mut right) =
                    split_clip_at(clip, request.range.start).map_err(author_state)?;
                trim_clip_in_to(&mut right, end).map_err(author_state)?;
                if let Some(group) = right.link_group {
                    right.link_group = Some(*split_groups.entry(group).or_default());
                }
                if request.kind == RangeEditKind::Extract {
                    right.position =
                        right.position.checked_sub(request.range.duration).map_err(author_state)?;
                    outcome.shifted_clip_ids.push(right.id);
                }
                outcome
                    .split_clips
                    .push(RangeEditSplitOutcome { left_clip_id: left.id, right_clip_id: right.id });
                edited.push(left);
                edited.push(right);
            }
        }
    }
    edited.sort_unstable_by_key(|clip| clip.position);
    track.clips = edited;
    Ok(())
}

fn close_protected_track_gap(
    track: &mut Track,
    range: TimelineTimeRange,
    outcome: &mut RangeEditOutcome,
) -> Result<(), RangeEditError> {
    let end = range.end().map_err(author_state)?;
    for clip in &mut track.clips {
        if clip.position >= end {
            clip.position = clip.position.checked_sub(range.duration).map_err(author_state)?;
            outcome.shifted_clip_ids.push(clip.id);
        }
    }
    track.clips.sort_unstable_by_key(|clip| clip.position);
    Ok(())
}

fn migrate_transitions(
    sequence: &mut Sequence,
    request: &RangeEditRequest,
) -> Result<(Vec<VideoTransitionId>, Vec<AudioTransitionId>), RangeEditError> {
    if sequence.video_transitions.is_empty() && sequence.audio_program.transitions.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let locations = transition_locations(sequence, request.range)?;

    let mut removed_video = Vec::new();
    let mut retained_video = Vec::with_capacity(sequence.video_transitions.len());
    for mut transition in std::mem::take(&mut sequence.video_transitions) {
        let Some(left) = locations.clips.get(&transition.left).copied() else {
            return Err(author_state(format!(
                "video Transition {} has an unresolved left endpoint",
                transition.id
            )));
        };
        let Some(right) = locations.clips.get(&transition.right).copied() else {
            return Err(author_state(format!(
                "video Transition {} has an unresolved right endpoint",
                transition.id
            )));
        };
        match transition_edit(left, right, request) {
            TransitionEdit::Retain => retained_video.push(transition),
            TransitionEdit::Shift => {
                transition.sequence_range.start = transition
                    .sequence_range
                    .start
                    .checked_sub(request.range.duration)
                    .map_err(author_state)?;
                retained_video.push(transition);
            }
            TransitionEdit::Affected
                if request.transition_policy == RangeEditTransitionPolicy::RemoveAffected =>
            {
                removed_video.push(transition.id);
            }
            TransitionEdit::Affected => {
                return Err(RangeEditError::AffectedVideoTransition(transition.id));
            }
        }
    }
    sequence.video_transitions = retained_video;

    let mut removed_audio = Vec::new();
    let mut retained_audio = Vec::with_capacity(sequence.audio_program.transitions.len());
    for mut transition in std::mem::take(&mut sequence.audio_program.transitions) {
        let Some(left) = locations.components.get(&transition.left).copied() else {
            return Err(author_state(format!(
                "audio Transition {} has an unresolved left endpoint",
                transition.id
            )));
        };
        let Some(right) = locations.components.get(&transition.right).copied() else {
            return Err(author_state(format!(
                "audio Transition {} has an unresolved right endpoint",
                transition.id
            )));
        };
        match transition_edit(left, right, request) {
            TransitionEdit::Retain => retained_audio.push(transition),
            TransitionEdit::Shift => {
                transition.sequence_range.start = transition
                    .sequence_range
                    .start
                    .checked_sub(request.range.duration)
                    .map_err(author_state)?;
                retained_audio.push(transition);
            }
            TransitionEdit::Affected
                if request.transition_policy == RangeEditTransitionPolicy::RemoveAffected =>
            {
                removed_audio.push(transition.id);
            }
            TransitionEdit::Affected => {
                return Err(RangeEditError::AffectedAudioTransition(transition.id));
            }
        }
    }
    sequence.audio_program.transitions = retained_audio;
    Ok((removed_video, removed_audio))
}

struct TransitionLocations {
    clips: BTreeMap<ClipId, (TrackId, ClipRangeClass)>,
    components: BTreeMap<AudioComponentEditId, (TrackId, ClipRangeClass)>,
}

fn transition_locations(
    sequence: &Sequence,
    range: TimelineTimeRange,
) -> Result<TransitionLocations, RangeEditError> {
    let clips = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| {
            track.clips.iter().map(move |clip| {
                classify_clip(clip, range).map(|class| (clip.id, (track.id, class)))
            })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let mut components = BTreeMap::new();
    for track in &sequence.audio_tracks {
        for clip in &track.clips {
            let class = classify_clip(clip, range)?;
            for edit in &clip.audio_components {
                components.insert(edit.id, (track.id, class));
            }
        }
    }
    Ok(TransitionLocations { clips, components })
}

fn assess_transitions(
    sequence: &Sequence,
    request: &RangeEditRequest,
) -> Result<(), RangeEditError> {
    if sequence.video_transitions.is_empty() && sequence.audio_program.transitions.is_empty() {
        return Ok(());
    }
    let locations = transition_locations(sequence, request.range)?;
    for transition in &sequence.video_transitions {
        let left = locations.clips.get(&transition.left).copied().ok_or_else(|| {
            author_state(format!(
                "video Transition {} has an unresolved left endpoint",
                transition.id
            ))
        })?;
        let right = locations.clips.get(&transition.right).copied().ok_or_else(|| {
            author_state(format!(
                "video Transition {} has an unresolved right endpoint",
                transition.id
            ))
        })?;
        if transition_edit(left, right, request) == TransitionEdit::Affected
            && request.transition_policy == RangeEditTransitionPolicy::RejectAffected
        {
            return Err(RangeEditError::AffectedVideoTransition(transition.id));
        }
    }
    for transition in &sequence.audio_program.transitions {
        let left = locations.components.get(&transition.left).copied().ok_or_else(|| {
            author_state(format!(
                "audio Transition {} has an unresolved left endpoint",
                transition.id
            ))
        })?;
        let right = locations.components.get(&transition.right).copied().ok_or_else(|| {
            author_state(format!(
                "audio Transition {} has an unresolved right endpoint",
                transition.id
            ))
        })?;
        if transition_edit(left, right, request) == TransitionEdit::Affected
            && request.transition_policy == RangeEditTransitionPolicy::RejectAffected
        {
            return Err(RangeEditError::AffectedAudioTransition(transition.id));
        }
    }
    Ok(())
}

fn transition_edit(
    left: (TrackId, ClipRangeClass),
    right: (TrackId, ClipRangeClass),
    request: &RangeEditRequest,
) -> TransitionEdit {
    let (left_track, left_class) = left;
    let (right_track, right_class) = right;
    let affected_scope = request.content_tracks.contains(&left_track)
        || request.ripple_tracks.contains(&left_track)
        || request.content_tracks.contains(&right_track)
        || request.ripple_tracks.contains(&right_track);
    if !affected_scope
        || (left_class == ClipRangeClass::Before && right_class == ClipRangeClass::Before)
        || (request.kind == RangeEditKind::Lift
            && left_class == ClipRangeClass::After
            && right_class == ClipRangeClass::After)
    {
        TransitionEdit::Retain
    } else if request.kind == RangeEditKind::Extract
        && left_class == ClipRangeClass::After
        && right_class == ClipRangeClass::After
        && request.ripple_tracks.contains(&left_track)
        && request.ripple_tracks.contains(&right_track)
    {
        TransitionEdit::Shift
    } else {
        TransitionEdit::Affected
    }
}

fn classify_clip(clip: &Clip, range: TimelineTimeRange) -> Result<ClipRangeClass, RangeEditError> {
    let end = range.end().map_err(author_state)?;
    let clip_end = clip.end_position().map_err(author_state)?;
    Ok(if clip_end <= range.start {
        ClipRangeClass::Before
    } else if clip.position >= end {
        ClipRangeClass::After
    } else if clip.position >= range.start && clip_end <= end {
        ClipRangeClass::Inside
    } else if clip.position < range.start && clip_end <= end {
        ClipRangeClass::OverlapStart
    } else if clip.position >= range.start && clip_end > end {
        ClipRangeClass::OverlapEnd
    } else {
        ClipRangeClass::Spanning
    })
}

fn track(sequence: &Sequence, id: TrackId) -> Option<&Track> {
    sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .find(|track| track.id == id)
}

fn author_state(error: impl std::fmt::Display) -> RangeEditError {
    RangeEditError::AuthorState(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clip;
    use mondrian_core::{AssetId, AudioSourceComponentId, FramePosition, Rational};

    fn tt(frame: i64) -> TimelineTime {
        TimelineTime::from_frame_position(FramePosition::new(frame, Rational::new(1, 25)))
            .expect("test time")
    }

    fn video_clip(start: i64, duration: i64) -> Clip {
        Clip::new(AssetId::new(), tt(start), tt(duration)).expect("video Clip")
    }

    fn request(
        kind: RangeEditKind,
        start: i64,
        duration: i64,
        content_tracks: impl IntoIterator<Item = TrackId>,
        ripple_tracks: impl IntoIterator<Item = TrackId>,
    ) -> RangeEditRequest {
        RangeEditRequest {
            kind,
            range: TimelineTimeRange::new(tt(start), tt(duration)).expect("range"),
            content_tracks: content_tracks.into_iter().collect(),
            ripple_tracks: ripple_tracks.into_iter().collect(),
            automation_policy: RangeEditAutomationPolicy::FollowEditorialContent,
            transition_policy: RangeEditTransitionPolicy::RemoveAffected,
            timeline_state_policy: RangeEditTimelineStatePolicy::CollapseToRangeStart,
        }
    }

    #[test]
    fn lift_trims_removes_and_splits_without_closing_program_time() {
        let mut sequence = Sequence::new("lift");
        let track_id = sequence.video_tracks[0].id;
        for clip in [
            video_clip(0, 15),
            video_clip(15, 5),
            video_clip(20, 15),
            video_clip(40, 30),
        ] {
            sequence.video_tracks[0].add_clip(clip).expect("Clip");
        }

        let outcome = apply_range_edit(
            &mut sequence,
            &request(RangeEditKind::Lift, 10, 20, [track_id], []),
        )
        .expect("Lift");
        let track = &sequence.video_tracks[0];
        assert_eq!(outcome.removed_clip_ids.len(), 1);
        assert_eq!(outcome.trimmed_clip_ids.len(), 2);
        assert!(outcome.shifted_clip_ids.is_empty());
        assert_eq!(
            track
                .clips
                .iter()
                .map(|clip| (clip.position, clip.duration))
                .collect::<Vec<_>>(),
            [(tt(0), tt(10)), (tt(30), tt(5)), (tt(40), tt(30))]
        );
        assert_eq!(sequence.playhead, tt(10));
        assert!(sequence.in_point.is_none());
        assert!(sequence.out_point.is_none());
    }

    #[test]
    fn extract_closes_content_and_sync_locked_empty_tracks() {
        let mut sequence = Sequence::new("extract");
        let content_track = sequence.video_tracks[0].id;
        let sync_track = sequence.video_tracks[1].id;
        sequence.video_tracks[0].add_clip(video_clip(0, 40)).expect("content");
        let downstream = video_clip(50, 10);
        let downstream_id = downstream.id;
        sequence.video_tracks[1].add_clip(downstream).expect("downstream");

        let outcome = apply_range_edit(
            &mut sequence,
            &request(
                RangeEditKind::Extract,
                10,
                20,
                [content_track],
                [content_track, sync_track],
            ),
        )
        .expect("Extract");
        assert_eq!(outcome.split_clips.len(), 1);
        assert!(outcome.shifted_clip_ids.contains(&downstream_id));
        assert_eq!(sequence.video_tracks[1].clips[0].position, tt(30));
        assert_eq!(
            sequence.video_tracks[0]
                .clips
                .iter()
                .map(|clip| (clip.position, clip.duration))
                .collect::<Vec<_>>(),
            [(tt(0), tt(10)), (tt(10), tt(10))]
        );
    }

    #[test]
    fn extract_rejects_protected_sync_lock_content_atomically() {
        let mut sequence = Sequence::new("protected");
        let content_track = sequence.video_tracks[0].id;
        let sync_track = sequence.video_tracks[1].id;
        sequence.video_tracks[0].add_clip(video_clip(0, 40)).expect("content");
        sequence.video_tracks[1].add_clip(video_clip(15, 5)).expect("protected");
        let before = serde_json::to_value(&sequence).expect("before");

        let error = apply_range_edit(
            &mut sequence,
            &request(
                RangeEditKind::Extract,
                10,
                20,
                [content_track],
                [content_track, sync_track],
            ),
        )
        .expect_err("protected content");
        assert_eq!(error, RangeEditError::ProtectedRippleContent(sync_track));
        assert_eq!(serde_json::to_value(&sequence).expect("after"), before);
    }

    #[test]
    fn assessment_and_execution_share_transition_disposition() {
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
            TimelineTimeRange::new(tt(8), tt(4)).expect("transition range"),
        );
        let transition_id = transition.id;
        sequence.video_transitions.push(transition);

        let mut request = request(RangeEditKind::Lift, 9, 2, [track_id], []);
        request.transition_policy = RangeEditTransitionPolicy::RejectAffected;
        assert_eq!(
            assess_range_edit(&sequence, &request),
            Err(RangeEditError::AffectedVideoTransition(transition_id))
        );

        request.transition_policy = RangeEditTransitionPolicy::RemoveAffected;
        assess_range_edit(&sequence, &request).expect("removal policy");
        let outcome = apply_range_edit(&mut sequence, &request).expect("Lift");
        assert_eq!(outcome.removed_video_transition_ids, [transition_id]);
        assert!(sequence.video_transitions.is_empty());
    }

    #[test]
    fn range_edit_requires_linked_members_to_receive_one_transform() {
        let mut sequence = Sequence::new("links");
        let video_track = sequence.video_tracks[0].id;
        let audio_track = sequence.audio_tracks[0].id;
        let group = ClipLinkGroupId::new();
        let mut video = video_clip(0, 40);
        video.link_group = Some(group);
        sequence.video_tracks[0].add_clip(video).expect("video");
        let mut audio = video_clip(0, 40);
        audio.link_group = Some(group);
        sequence
            .attach_default_media_audio_component(&mut audio, AudioSourceComponentId::primary())
            .expect("audio component");
        sequence.audio_tracks[0].add_clip(audio).expect("audio");

        let error = apply_range_edit(
            &mut sequence,
            &request(RangeEditKind::Lift, 10, 10, [video_track], []),
        )
        .expect_err("partial link");
        assert_eq!(error, RangeEditError::PartialLinkedGroup(group));

        apply_range_edit(
            &mut sequence,
            &request(RangeEditKind::Lift, 10, 10, [video_track, audio_track], []),
        )
        .expect("coherent Lift");
        let left_groups = sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.position == tt(0))
            .map(|clip| clip.link_group)
            .collect::<BTreeSet<_>>();
        let right_groups = sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.position == tt(20))
            .map(|clip| clip.link_group)
            .collect::<BTreeSet<_>>();
        assert_eq!(left_groups, [Some(group)].into_iter().collect());
        assert_eq!(right_groups.len(), 1);
        assert!(!right_groups.contains(&Some(group)));
    }
}
