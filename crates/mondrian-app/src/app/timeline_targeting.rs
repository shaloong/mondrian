//! UI-independent Timeline Track Targeting and Sync-Lock session policy.
//!
//! These controls are editor workspace state, not renderable Sequence author
//! data. The Module stores only explicit exceptions to safe defaults so newly
//! created Tracks participate without requiring a parallel initialization
//! transaction.

use super::AppState;
use mondrian_core::{MondrianError, SequenceId, TrackId};
use mondrian_timeline::Sequence;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Resolved immutable scopes consumed by structural editorial commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineEditTargetSnapshot {
    /// Tracks whose content is directly edited.
    pub content_tracks: BTreeSet<TrackId>,
    /// Tracks whose downstream placements follow ripple edits.
    pub ripple_tracks: BTreeSet<TrackId>,
}

/// Per-open-session targeting policy keyed by stable Sequence and Track IDs.
#[derive(Debug, Clone, Default)]
pub(crate) struct TimelineTargetingState {
    by_sequence: HashMap<SequenceId, SequenceTargetingOverrides>,
}

#[derive(Debug, Clone, Default)]
struct SequenceTargetingOverrides {
    /// Targeting defaults on; only explicit disabled Track IDs are retained.
    untargeted_tracks: HashSet<TrackId>,
    /// Sync-Lock defaults on; only explicit disabled Track IDs are retained.
    sync_unlocked_tracks: HashSet<TrackId>,
}

impl TimelineTargetingState {
    fn track_targeted(&self, sequence_id: SequenceId, track_id: TrackId) -> bool {
        !self
            .by_sequence
            .get(&sequence_id)
            .is_some_and(|state| state.untargeted_tracks.contains(&track_id))
    }

    fn track_sync_locked(&self, sequence_id: SequenceId, track_id: TrackId) -> bool {
        !self
            .by_sequence
            .get(&sequence_id)
            .is_some_and(|state| state.sync_unlocked_tracks.contains(&track_id))
    }

    fn set_track_targeted(
        &mut self,
        sequence_id: SequenceId,
        track_id: TrackId,
        targeted: bool,
    ) -> bool {
        if self.track_targeted(sequence_id, track_id) == targeted {
            return false;
        }
        let state = self.by_sequence.entry(sequence_id).or_default();
        if targeted {
            state.untargeted_tracks.remove(&track_id);
        } else {
            state.untargeted_tracks.insert(track_id);
        }
        self.compact_sequence(sequence_id);
        true
    }

    fn set_track_sync_locked(
        &mut self,
        sequence_id: SequenceId,
        track_id: TrackId,
        sync_locked: bool,
    ) -> bool {
        if self.track_sync_locked(sequence_id, track_id) == sync_locked {
            return false;
        }
        let state = self.by_sequence.entry(sequence_id).or_default();
        if sync_locked {
            state.sync_unlocked_tracks.remove(&track_id);
        } else {
            state.sync_unlocked_tracks.insert(track_id);
        }
        self.compact_sequence(sequence_id);
        true
    }

    fn snapshot(&self, sequence: &Sequence) -> TimelineEditTargetSnapshot {
        let mut content_tracks = BTreeSet::new();
        let mut ripple_tracks = BTreeSet::new();
        for track in sequence.video_tracks.iter().chain(&sequence.audio_tracks) {
            let targeted = self.track_targeted(sequence.id, track.id);
            if targeted {
                content_tracks.insert(track.id);
                ripple_tracks.insert(track.id);
            }
            if self.track_sync_locked(sequence.id, track.id) {
                ripple_tracks.insert(track.id);
            }
        }
        TimelineEditTargetSnapshot { content_tracks, ripple_tracks }
    }

    fn retain_topology(&mut self, topology: &[(SequenceId, HashSet<TrackId>)]) {
        let valid_sequences =
            topology.iter().map(|(sequence_id, _)| *sequence_id).collect::<HashSet<_>>();
        self.by_sequence.retain(|sequence_id, _| valid_sequences.contains(sequence_id));
        for (sequence_id, valid_tracks) in topology {
            if let Some(state) = self.by_sequence.get_mut(sequence_id) {
                state.untargeted_tracks.retain(|track_id| valid_tracks.contains(track_id));
                state.sync_unlocked_tracks.retain(|track_id| valid_tracks.contains(track_id));
            }
            self.compact_sequence(*sequence_id);
        }
    }

    fn compact_sequence(&mut self, sequence_id: SequenceId) {
        if self.by_sequence.get(&sequence_id).is_some_and(|state| {
            state.untargeted_tracks.is_empty() && state.sync_unlocked_tracks.is_empty()
        }) {
            self.by_sequence.remove(&sequence_id);
        }
    }

    fn clear(&mut self) {
        self.by_sequence.clear();
    }
}

impl AppState {
    /// Whether one current Track receives direct range/source edits.
    pub fn timeline_track_targeted(&self, sequence_id: SequenceId, track_id: TrackId) -> bool {
        self.timeline_targeting.track_targeted(sequence_id, track_id)
    }

    /// Whether one current Track follows program-time ripple edits.
    pub fn timeline_track_sync_locked(&self, sequence_id: SequenceId, track_id: TrackId) -> bool {
        self.timeline_targeting.track_sync_locked(sequence_id, track_id)
    }

    /// Resolve stable Track controls into one immutable command scope.
    pub fn timeline_edit_targets(&self, sequence: &Sequence) -> TimelineEditTargetSnapshot {
        self.timeline_targeting.snapshot(sequence)
    }

    /// Change direct edit targeting without creating an author transaction.
    pub fn set_timeline_track_targeted(
        &mut self,
        track_id: TrackId,
        targeted: bool,
    ) -> mondrian_core::Result<bool> {
        let sequence = self.active_sequence().ok_or_else(|| targeting_error("当前无序列"))?;
        if !sequence_has_track(sequence, track_id) {
            return Err(MondrianError::TrackNotFound { track_id: track_id.to_string() });
        }
        let sequence_id = sequence.id;
        Ok(self.timeline_targeting.set_track_targeted(sequence_id, track_id, targeted))
    }

    /// Change Sync-Lock participation without creating an author transaction.
    pub fn set_timeline_track_sync_locked(
        &mut self,
        track_id: TrackId,
        sync_locked: bool,
    ) -> mondrian_core::Result<bool> {
        let sequence = self.active_sequence().ok_or_else(|| targeting_error("当前无序列"))?;
        if !sequence_has_track(sequence, track_id) {
            return Err(MondrianError::TrackNotFound { track_id: track_id.to_string() });
        }
        let sequence_id = sequence.id;
        Ok(self
            .timeline_targeting
            .set_track_sync_locked(sequence_id, track_id, sync_locked))
    }

    pub(super) fn reconcile_timeline_targeting(&mut self) {
        if self.timeline_targeting.by_sequence.is_empty() {
            return;
        }
        let topology = self
            .sequences()
            .iter()
            .map(|sequence| {
                (
                    sequence.id,
                    sequence
                        .video_tracks
                        .iter()
                        .chain(&sequence.audio_tracks)
                        .map(|track| track.id)
                        .collect::<HashSet<_>>(),
                )
            })
            .collect::<Vec<_>>();
        self.timeline_targeting.retain_topology(&topology);
    }

    pub(super) fn clear_timeline_targeting(&mut self) {
        self.timeline_targeting.clear();
    }
}

fn sequence_has_track(sequence: &Sequence, track_id: TrackId) -> bool {
    sequence
        .video_tracks
        .iter()
        .chain(&sequence.audio_tracks)
        .any(|track| track.id == track_id)
}

fn targeting_error(reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: "timeline_targeting".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracks_default_to_targeted_and_sync_locked() {
        let mut sequence = Sequence::new("targets");
        let state = TimelineTargetingState::default();
        let first = state.snapshot(&sequence);
        assert_eq!(
            first.content_tracks.len(),
            sequence.video_tracks.len() + sequence.audio_tracks.len()
        );
        assert_eq!(first.content_tracks, first.ripple_tracks);

        let added = sequence.add_video_track();
        let second = state.snapshot(&sequence);
        assert!(second.content_tracks.contains(&added));
        assert!(second.ripple_tracks.contains(&added));
    }

    #[test]
    fn targeting_and_sync_lock_are_independent_explicit_exceptions() {
        let sequence = Sequence::new("targets");
        let track_id = sequence.video_tracks[0].id;
        let mut state = TimelineTargetingState::default();
        state.set_track_targeted(sequence.id, track_id, false);
        let snapshot = state.snapshot(&sequence);
        assert!(!snapshot.content_tracks.contains(&track_id));
        assert!(snapshot.ripple_tracks.contains(&track_id));

        state.set_track_sync_locked(sequence.id, track_id, false);
        let snapshot = state.snapshot(&sequence);
        assert!(!snapshot.content_tracks.contains(&track_id));
        assert!(!snapshot.ripple_tracks.contains(&track_id));

        state.set_track_targeted(sequence.id, track_id, true);
        let snapshot = state.snapshot(&sequence);
        assert!(snapshot.content_tracks.contains(&track_id));
        assert!(snapshot.ripple_tracks.contains(&track_id));
    }

    #[test]
    fn author_transactions_prune_targeting_overrides_for_removed_tracks() {
        let sequence = Sequence::new("targets");
        let sequence_id = sequence.id;
        let track_id = sequence.video_tracks[0].id;
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.set_timeline_track_targeted(track_id, false).expect("untarget");
        assert!(!state.timeline_track_targeted(sequence_id, track_id));

        state.remove_track(track_id, true).expect("remove Track");
        assert!(state.timeline_targeting.by_sequence.is_empty());

        assert!(state.undo_timeline().expect("undo"));
        assert!(state.timeline_track_targeted(sequence_id, track_id));
    }
}
