//! Worker-owned Preview decoder residency phase coordination.
//!
//! FFmpeg codec contexts, decoded-picture buffers, and native hardware surface
//! pools are destroyed only by the worker thread that owns them. This Module
//! coordinates the transport-family boundary without moving those resources
//! across threads or allowing playback and interactive pools to accumulate.

use std::sync::{Mutex, MutexGuard};

use mondrian_media::PreviewDecodeAccessMode;

use super::preview_access_mode::MediaPreviewWorkerLane;
use super::preview_work_notification::PreviewWorkNotifier;

const WORKER_ANY: u8 = 1 << 0;
const WORKER_PLAYBACK: u8 = 1 << 1;
const WORKER_NON_PLAYBACK: u8 = 1 << 2;
const WORKER_STILL: u8 = 1 << 3;

/// Mutually exclusive family of decoder Sessions allowed to remain resident.
pub(crate) use mondrian_media::PreviewDecodeSessionFamily as PreviewDecodeResidencyFamily;

/// One worker's action at a published residency revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewDecodeResidencyDirective {
    revision: u64,
    retire_context: bool,
    retired_family: Option<PreviewDecodeResidencyFamily>,
}

impl PreviewDecodeResidencyDirective {
    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn retire_context(self) -> bool {
        self.retire_context
    }

    pub(crate) const fn retired_family(self) -> Option<PreviewDecodeResidencyFamily> {
        self.retired_family
    }
}

#[derive(Debug, Clone, Copy)]
struct PreviewDecodeResidencyState {
    revision: u64,
    actionable_retry_revision: u64,
    active_family: Option<PreviewDecodeResidencyFamily>,
    worker_mask: u8,
    required_ack_mask: u8,
    acknowledged_mask: u8,
    transitions: u64,
    blocked_admissions: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PreviewDecodeResidencyDiagnostics {
    pub(crate) revision: u64,
    pub(crate) actionable_retry_revision: u64,
    pub(crate) active_family: Option<PreviewDecodeResidencyFamily>,
    pub(crate) transitions: u64,
    pub(crate) blocked_admissions: u64,
    pub(crate) required_acknowledgements: u32,
    pub(crate) acknowledged_retirements: u32,
}

/// Synchronizes worker-local decoder destruction with cross-family admission.
pub(crate) struct PreviewDecodeResidencyCoordinator {
    state: Mutex<PreviewDecodeResidencyState>,
    work_notifier: PreviewWorkNotifier,
}

impl PreviewDecodeResidencyCoordinator {
    pub(crate) fn new() -> Self {
        Self::new_with_notifier(PreviewWorkNotifier::default())
    }

    pub(crate) fn new_with_notifier(work_notifier: PreviewWorkNotifier) -> Self {
        Self {
            state: Mutex::new(PreviewDecodeResidencyState {
                revision: 0,
                actionable_retry_revision: 0,
                active_family: None,
                worker_mask: 0,
                required_ack_mask: 0,
                acknowledged_mask: 0,
                transitions: 0,
                blocked_admissions: 0,
            }),
            work_notifier,
        }
    }

    /// Add a successfully constructed worker to future retirement barriers.
    pub(crate) fn register_worker(&self, lane: MediaPreviewWorkerLane) {
        let mut state = lock_state(&self.state);
        state.worker_mask |= worker_lane_bit(lane);
    }

    /// Remove a worker whose thread could not be constructed.
    pub(crate) fn unregister_worker(&self, lane: MediaPreviewWorkerLane) {
        let bit = worker_lane_bit(lane);
        let retry_became_actionable = {
            let mut state = lock_state(&self.state);
            let was_blocked = retirement_barrier_blocked(&state);
            state.worker_mask &= !bit;
            state.required_ack_mask &= !bit;
            state.acknowledged_mask &= !bit;
            let retry_became_actionable = was_blocked && !retirement_barrier_blocked(&state);
            if retry_became_actionable {
                state.actionable_retry_revision = state.actionable_retry_revision.saturating_add(1);
            }
            retry_became_actionable
        };
        if retry_became_actionable {
            self.work_notifier.retry_became_actionable();
        }
    }

    /// Publish a new residency family. Returns whether workers must be woken.
    pub(crate) fn activate(&self, family: PreviewDecodeResidencyFamily) -> bool {
        let mut state = lock_state(&self.state);
        if state.active_family == Some(family) {
            return false;
        }
        state.revision = state.revision.saturating_add(1);
        state.transitions = state.transitions.saturating_add(1);
        state.active_family = Some(family);
        // Every worker can own either family: the NonPlayback lane may own a
        // cold Playback Session and the Playback lane may have an exceptional
        // failover history. Each worker retires only the obsolete Session
        // family, preserving unrelated locality inside the same context.
        state.required_ack_mask = state.worker_mask;
        state.acknowledged_mask = 0;
        true
    }

    /// Current coordination revision for a newly started worker loop.
    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        lock_state(&self.state).revision
    }

    /// Return the monotonic edge revision for completed retirement barriers.
    ///
    /// Consumers compare this revision with their own last projection. It is
    /// retry authority only; decoded payloads and terminal facts remain on
    /// their typed result transports.
    pub(crate) fn actionable_retry_revision(&self) -> u64 {
        lock_state(&self.state).actionable_retry_revision
    }

    /// Return the next directive when a worker has not observed the revision.
    pub(crate) fn worker_directive(
        &self,
        lane: MediaPreviewWorkerLane,
        observed_revision: u64,
    ) -> Option<PreviewDecodeResidencyDirective> {
        let state = lock_state(&self.state);
        (state.revision != observed_revision).then_some(PreviewDecodeResidencyDirective {
            revision: state.revision,
            retire_context: state.required_ack_mask & worker_lane_bit(lane) != 0,
            retired_family: state.active_family.map(opposite_residency_family),
        })
    }

    /// Confirm that one worker destroyed its context for the current revision.
    pub(crate) fn acknowledge_retirement(&self, lane: MediaPreviewWorkerLane, revision: u64) {
        let retry_became_actionable = {
            let mut state = lock_state(&self.state);
            let was_blocked = retirement_barrier_blocked(&state);
            if state.revision == revision {
                state.acknowledged_mask |= worker_lane_bit(lane) & state.required_ack_mask;
            }
            let retry_became_actionable = was_blocked && !retirement_barrier_blocked(&state);
            if retry_became_actionable {
                state.actionable_retry_revision = state.actionable_retry_revision.saturating_add(1);
            }
            retry_became_actionable
        };
        if retry_became_actionable {
            self.work_notifier.retry_became_actionable();
        }
    }

    /// Return whether the selected family may create or reuse decoder state.
    pub(crate) fn admits(&self, access_mode: PreviewDecodeAccessMode) -> bool {
        let mut state = lock_state(&self.state);
        let admitted = residency_state_admits(&state, access_mode);
        if !admitted {
            state.blocked_admissions = state.blocked_admissions.saturating_add(1);
        }
        admitted
    }

    /// Observe admission without recording a new request attempt.
    pub(crate) fn admission_ready(&self, access_mode: PreviewDecodeAccessMode) -> bool {
        residency_state_admits(&lock_state(&self.state), access_mode)
    }

    pub(crate) fn diagnostics(&self) -> PreviewDecodeResidencyDiagnostics {
        let state = lock_state(&self.state);
        PreviewDecodeResidencyDiagnostics {
            revision: state.revision,
            actionable_retry_revision: state.actionable_retry_revision,
            active_family: state.active_family,
            transitions: state.transitions,
            blocked_admissions: state.blocked_admissions,
            required_acknowledgements: state.required_ack_mask.count_ones(),
            acknowledged_retirements: (state.acknowledged_mask & state.required_ack_mask)
                .count_ones(),
        }
    }
}

impl Default for PreviewDecodeResidencyCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

const fn worker_lane_bit(lane: MediaPreviewWorkerLane) -> u8 {
    match lane {
        MediaPreviewWorkerLane::Any => WORKER_ANY,
        MediaPreviewWorkerLane::Playback => WORKER_PLAYBACK,
        MediaPreviewWorkerLane::NonPlayback => WORKER_NON_PLAYBACK,
        MediaPreviewWorkerLane::Still => WORKER_STILL,
    }
}

const fn opposite_residency_family(
    family: PreviewDecodeResidencyFamily,
) -> PreviewDecodeResidencyFamily {
    match family {
        PreviewDecodeResidencyFamily::Playback => PreviewDecodeResidencyFamily::Interactive,
        PreviewDecodeResidencyFamily::Interactive => PreviewDecodeResidencyFamily::Playback,
    }
}

fn lock_state(
    state: &Mutex<PreviewDecodeResidencyState>,
) -> MutexGuard<'_, PreviewDecodeResidencyState> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

const fn retirement_barrier_blocked(state: &PreviewDecodeResidencyState) -> bool {
    state.acknowledged_mask != state.required_ack_mask
}

fn residency_state_admits(
    state: &PreviewDecodeResidencyState,
    access_mode: PreviewDecodeAccessMode,
) -> bool {
    state
        .active_family
        .is_none_or(|family| family == PreviewDecodeResidencyFamily::for_access_mode(access_mode))
        && !retirement_barrier_blocked(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_admission_waits_for_playback_owner_retirement() {
        let coordinator = PreviewDecodeResidencyCoordinator::new();
        coordinator.register_worker(MediaPreviewWorkerLane::Playback);
        coordinator.register_worker(MediaPreviewWorkerLane::NonPlayback);
        coordinator.register_worker(MediaPreviewWorkerLane::Still);
        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Playback));
        let playback_revision = coordinator.revision();
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::NonPlayback, 0)
            .expect("non-playback worker should observe playback phase");
        assert!(directive.retire_context());
        coordinator
            .acknowledge_retirement(MediaPreviewWorkerLane::NonPlayback, directive.revision());
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::Playback, 0)
            .expect("playback worker should retire stale interactive Sessions");
        assert_eq!(
            directive.retired_family(),
            Some(PreviewDecodeResidencyFamily::Interactive)
        );
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Playback, directive.revision());
        assert!(
            !coordinator.admission_ready(PreviewDecodeAccessMode::PlaybackCursor),
            "the Still worker can own an interactive decoder and remains part of the family barrier"
        );
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::Still, 0)
            .expect("still worker should retire stale interactive Sessions");
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Still, directive.revision());
        assert!(coordinator.admits(PreviewDecodeAccessMode::PlaybackCursor));

        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Interactive));
        assert!(!coordinator.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::Playback, playback_revision)
            .expect("playback worker should observe interactive phase");
        assert!(directive.retire_context());
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Playback, directive.revision());
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::NonPlayback, playback_revision)
            .expect("shared worker may own a borrowed Playback Session");
        assert_eq!(
            directive.retired_family(),
            Some(PreviewDecodeResidencyFamily::Playback)
        );
        coordinator
            .acknowledge_retirement(MediaPreviewWorkerLane::NonPlayback, directive.revision());
        assert!(!coordinator.admission_ready(PreviewDecodeAccessMode::ScrubCursor));
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::Still, playback_revision)
            .expect("still worker should retire its Playback-family static Sessions");
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Still, directive.revision());
        assert!(coordinator.admits(PreviewDecodeAccessMode::ScrubCursor));
        assert!(coordinator.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));
        assert!(!coordinator.admits(PreviewDecodeAccessMode::PlaybackCursor));
    }

    #[test]
    fn stale_acknowledgement_cannot_satisfy_a_new_transition() {
        let coordinator = PreviewDecodeResidencyCoordinator::new();
        coordinator.register_worker(MediaPreviewWorkerLane::Any);
        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Playback));
        let stale = coordinator
            .worker_directive(MediaPreviewWorkerLane::Any, 0)
            .expect("first directive");

        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Interactive));
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Any, stale.revision());
        assert!(!coordinator.admits(PreviewDecodeAccessMode::ScrubCursor));
        let current = coordinator
            .worker_directive(MediaPreviewWorkerLane::Any, stale.revision())
            .expect("current directive");
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Any, current.revision());
        assert!(coordinator.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));
    }

    #[test]
    fn final_required_retirement_ack_publishes_one_retry_edge() {
        let notifier = PreviewWorkNotifier::default();
        let watch = notifier.watch();
        let coordinator = PreviewDecodeResidencyCoordinator::new_with_notifier(notifier);
        coordinator.register_worker(MediaPreviewWorkerLane::Any);
        coordinator.register_worker(MediaPreviewWorkerLane::Playback);
        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Interactive));
        let revision = coordinator.revision();
        let before = watch.revision();

        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Any, revision);
        assert_eq!(
            watch.revision(),
            before,
            "a partial retirement barrier must not create a speculative retry"
        );

        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Playback, revision);
        let completed = watch.revision();
        assert_ne!(
            completed, before,
            "the aggregate retirement transition must wake a deferred demand"
        );

        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Playback, revision);
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Any, revision - 1);
        assert_eq!(
            watch.revision(),
            completed,
            "duplicate and stale acknowledgements must not manufacture wakeups"
        );
    }
}
