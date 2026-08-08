//! Atomic authoring operations for Sequence-local Clip Link Groups.
//!
//! A link group is a set, not an audio/video pair. Selecting any member for a
//! link edit expands to the complete existing group so the operation cannot
//! silently strand a subset of synchronized placements.

use crate::sequence::Sequence;
use mondrian_core::{ClipId, ClipLinkGroupId, TrackId};
use std::collections::{BTreeMap, BTreeSet};

/// Explicit Clip Link Group authoring operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipLinkEditKind {
    /// Link the selected units into one group.
    Link,
    /// Remove every selected unit from its current group.
    Unlink,
}

/// Stable-ID request for one Clip Link Group edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipLinkEditRequest {
    /// Operation to perform.
    pub kind: ClipLinkEditKind,
    /// Directly selected Clip identities. Existing groups are expanded before
    /// validation or mutation.
    pub clip_ids: Vec<ClipId>,
}

impl ClipLinkEditRequest {
    /// Create a request over stable Clip identities.
    pub fn new(kind: ClipLinkEditKind, clip_ids: impl IntoIterator<Item = ClipId>) -> Self {
        Self { kind, clip_ids: clip_ids.into_iter().collect() }
    }
}

/// Read-only feasibility result for a Clip Link Group edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipLinkEditAssessment {
    /// Complete group-expanded Clip set in canonical Sequence order.
    pub affected_clip_ids: Vec<ClipId>,
    /// Whether applying the request would change author state.
    pub would_change: bool,
}

/// Evidence returned by a successful Clip Link Group edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipLinkEditOutcome {
    /// Complete group-expanded Clip set in canonical Sequence order.
    pub affected_clip_ids: Vec<ClipId>,
    /// Resulting group for a Link operation, or `None` after Unlink.
    pub resulting_group: Option<ClipLinkGroupId>,
    /// Whether author state changed.
    pub changed: bool,
}

/// Fail-closed Clip Link Group validation or execution failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipLinkEditError {
    /// At least one direct Clip selection is required.
    #[error("Clip link edit requires at least one selected Clip")]
    EmptySelection,
    /// Every requested stable identity must still exist.
    #[error("Clip link edit references an unknown Clip: {0}")]
    UnknownClip(ClipId),
    /// A new group requires at least two group-expanded members.
    #[error("linking requires at least two Clips")]
    InsufficientLinkMembers,
    /// Link edits cannot partially mutate a locked Track.
    #[error("Clip link edit cannot modify locked Track: {0}")]
    LockedTrack(TrackId),
    /// Final author validation failed.
    #[error("Clip link edit failed: {0}")]
    AuthorState(String),
}

/// Return the complete selection unit containing `clip_id`.
///
/// An unlinked Clip is a one-member selection unit. A linked Clip resolves to
/// every member with the same Sequence-local group identity, in canonical
/// Sequence order.
pub fn clip_selection_unit(sequence: &Sequence, clip_id: ClipId) -> Option<Vec<ClipId>> {
    let group = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .find(|clip| clip.id == clip_id)?
        .link_group;

    Some(
        sequence
            .video_tracks
            .iter()
            .chain(&sequence.audio_tracks)
            .flat_map(|track| &track.clips)
            .filter(|clip| clip.id == clip_id || group.is_some() && clip.link_group == group)
            .map(|clip| clip.id)
            .collect(),
    )
}

/// Expand a Clip selection set in place so it contains the complete selection
/// unit of every member.
///
/// Unknown identities are ignored; the operation never removes members.
pub fn expand_clip_selection_units(
    sequence: &Sequence,
    clip_ids: &mut std::collections::HashSet<ClipId>,
) {
    let selected = clip_ids.iter().copied().collect::<Vec<_>>();
    for clip_id in selected {
        if let Some(unit) = clip_selection_unit(sequence, clip_id) {
            clip_ids.extend(unit);
        }
    }
}

/// Validate and describe one Clip Link Group edit without mutating author state.
pub fn assess_clip_link_edit(
    sequence: &Sequence,
    request: &ClipLinkEditRequest,
) -> Result<ClipLinkEditAssessment, ClipLinkEditError> {
    let prepared = prepare_clip_link_edit(sequence, request)?;
    Ok(ClipLinkEditAssessment {
        affected_clip_ids: prepared.affected_clip_ids,
        would_change: prepared.would_change,
    })
}

/// Apply one Clip Link Group edit atomically.
///
/// The receiver is replaced only after the complete candidate validates.
pub fn apply_clip_link_edit(
    sequence: &mut Sequence,
    request: &ClipLinkEditRequest,
) -> Result<ClipLinkEditOutcome, ClipLinkEditError> {
    let mut candidate = sequence.clone();
    let prepared = prepare_clip_link_edit(&candidate, request)?;
    if !prepared.would_change {
        return Ok(ClipLinkEditOutcome {
            affected_clip_ids: prepared.affected_clip_ids,
            resulting_group: prepared.retained_group,
            changed: false,
        });
    }

    let resulting_group = match request.kind {
        ClipLinkEditKind::Link => {
            Some(prepared.retained_group.unwrap_or_else(ClipLinkGroupId::new))
        }
        ClipLinkEditKind::Unlink => None,
    };
    let affected = prepared.affected_clip_ids.iter().copied().collect::<BTreeSet<_>>();
    for clip in candidate
        .video_tracks
        .iter_mut()
        .chain(&mut candidate.audio_tracks)
        .flat_map(|track| &mut track.clips)
    {
        if affected.contains(&clip.id) {
            clip.link_group = resulting_group;
        }
    }
    candidate
        .validate_author_identities()
        .map_err(|error| ClipLinkEditError::AuthorState(error.to_string()))?;
    *sequence = candidate;

    Ok(ClipLinkEditOutcome {
        affected_clip_ids: prepared.affected_clip_ids,
        resulting_group,
        changed: true,
    })
}

#[derive(Debug)]
struct PreparedClipLinkEdit {
    affected_clip_ids: Vec<ClipId>,
    retained_group: Option<ClipLinkGroupId>,
    would_change: bool,
}

fn prepare_clip_link_edit(
    sequence: &Sequence,
    request: &ClipLinkEditRequest,
) -> Result<PreparedClipLinkEdit, ClipLinkEditError> {
    let requested = request.clip_ids.iter().copied().collect::<BTreeSet<_>>();
    if requested.is_empty() {
        return Err(ClipLinkEditError::EmptySelection);
    }

    let locations = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| {
            track
                .clips
                .iter()
                .map(move |clip| (clip.id, (track.id, track.is_locked, clip.link_group)))
        })
        .collect::<BTreeMap<_, _>>();
    if let Some(unknown) = requested.iter().find(|clip_id| !locations.contains_key(clip_id)) {
        return Err(ClipLinkEditError::UnknownClip(*unknown));
    }

    let selected_groups = requested
        .iter()
        .filter_map(|clip_id| locations.get(clip_id).and_then(|(_, _, group)| *group))
        .collect::<BTreeSet<_>>();
    let affected_clip_ids = locations
        .iter()
        .filter(|(clip_id, (_, _, group))| {
            requested.contains(clip_id)
                || group.is_some_and(|group| selected_groups.contains(&group))
        })
        .map(|(clip_id, _)| *clip_id)
        .collect::<BTreeSet<_>>();
    let affected_clip_ids = sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .flat_map(|track| &track.clips)
        .filter(|clip| affected_clip_ids.contains(&clip.id))
        .map(|clip| clip.id)
        .collect::<Vec<_>>();

    let (affected_clip_ids, retained_group, would_change) = match request.kind {
        ClipLinkEditKind::Link => {
            if affected_clip_ids.len() < 2 {
                return Err(ClipLinkEditError::InsufficientLinkMembers);
            }
            let retained_group = (selected_groups.len() == 1)
                .then(|| selected_groups.iter().next().copied())
                .flatten();
            let would_change = retained_group.is_none_or(|group| {
                affected_clip_ids.iter().any(|clip_id| {
                    locations.get(clip_id).is_some_and(|(_, _, current)| *current != Some(group))
                })
            });
            (affected_clip_ids, retained_group, would_change)
        }
        ClipLinkEditKind::Unlink => {
            let linked = affected_clip_ids
                .into_iter()
                .filter(|clip_id| {
                    locations.get(clip_id).is_some_and(|(_, _, group)| group.is_some())
                })
                .collect::<Vec<_>>();
            let would_change = !linked.is_empty();
            (linked, None, would_change)
        }
    };

    for clip_id in &affected_clip_ids {
        if let Some((track_id, true, _)) = locations.get(clip_id) {
            return Err(ClipLinkEditError::LockedTrack(*track_id));
        }
    }

    Ok(PreparedClipLinkEdit { affected_clip_ids, retained_group, would_change })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Clip;
    use mondrian_core::{AssetId, FramePosition, TimelineTime};

    fn clip_at(sequence: &Sequence, frame: i64) -> Clip {
        Clip::new(
            AssetId::new(),
            TimelineTime::from_frame_position(FramePosition::new(frame, sequence.time_base()))
                .expect("test position"),
            TimelineTime::from_frame_position(FramePosition::new(10, sequence.time_base()))
                .expect("test duration"),
        )
        .expect("test Clip")
    }

    fn sequence_with_three_clips() -> (Sequence, [ClipId; 3]) {
        let mut sequence = Sequence::new("Linking");
        let clips = [
            clip_at(&sequence, 0),
            clip_at(&sequence, 10),
            clip_at(&sequence, 20),
        ];
        let ids = clips.each_ref().map(|clip| clip.id);
        sequence.video_tracks[0].clips.extend(clips);
        (sequence, ids)
    }

    #[test]
    fn link_and_unlink_expand_complete_groups() {
        let (mut sequence, [first, second, third]) = sequence_with_three_clips();
        let linked = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, second]),
        )
        .expect("link pair");
        let first_group = linked.resulting_group.expect("new group");
        assert!(linked.changed);

        let expanded = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, third]),
        )
        .expect("extend group");
        assert_eq!(expanded.affected_clip_ids, vec![first, second, third]);
        assert_eq!(expanded.resulting_group, Some(first_group));
        assert!(sequence.video_tracks[0]
            .clips
            .iter()
            .all(|clip| clip.link_group == Some(first_group)));

        let unlinked = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Unlink, [second]),
        )
        .expect("unlink group");
        assert_eq!(unlinked.affected_clip_ids, vec![first, second, third]);
        assert!(unlinked.changed);
        assert!(sequence.video_tracks[0].clips.iter().all(|clip| clip.link_group.is_none()));
    }

    #[test]
    fn merging_existing_groups_uses_a_fresh_identity() {
        let (mut sequence, [first, second, third]) = sequence_with_three_clips();
        let fourth = clip_at(&sequence, 30);
        let fourth_id = fourth.id;
        sequence.video_tracks[0].clips.push(fourth);
        let left = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, second]),
        )
        .expect("left group")
        .resulting_group
        .expect("left identity");
        let right = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [third, fourth_id]),
        )
        .expect("right group")
        .resulting_group
        .expect("right identity");

        let merged = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, third]),
        )
        .expect("merge groups");
        let merged_group = merged.resulting_group.expect("merged identity");
        assert_ne!(merged_group, left);
        assert_ne!(merged_group, right);
        assert_eq!(
            merged.affected_clip_ids,
            vec![first, second, third, fourth_id]
        );
    }

    #[test]
    fn locked_expanded_member_rejects_without_partial_mutation() {
        let (mut sequence, [first, second, third]) = sequence_with_three_clips();
        apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, second]),
        )
        .expect("initial group");
        let before = sequence.clone();
        sequence.video_tracks[0].is_locked = true;
        let before_groups = before.video_tracks[0]
            .clips
            .iter()
            .map(|clip| clip.link_group)
            .collect::<Vec<_>>();

        let error = apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, third]),
        )
        .expect_err("locked Track must reject");
        assert_eq!(
            error,
            ClipLinkEditError::LockedTrack(sequence.video_tracks[0].id)
        );
        assert_eq!(
            sequence.video_tracks[0]
                .clips
                .iter()
                .map(|clip| clip.link_group)
                .collect::<Vec<_>>(),
            before_groups
        );
    }

    #[test]
    fn assessment_reports_noop_for_an_existing_complete_group() {
        let (mut sequence, [first, second, _]) = sequence_with_three_clips();
        apply_clip_link_edit(
            &mut sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first, second]),
        )
        .expect("initial group");

        let assessment = assess_clip_link_edit(
            &sequence,
            &ClipLinkEditRequest::new(ClipLinkEditKind::Link, [first]),
        )
        .expect("assessment");
        assert_eq!(assessment.affected_clip_ids, vec![first, second]);
        assert!(!assessment.would_change);
        assert_eq!(
            clip_selection_unit(&sequence, first),
            Some(vec![first, second])
        );
    }
}
