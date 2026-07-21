//! Worker-owned Preview decoder residency phase coordination.
//!
//! FFmpeg codec contexts, decoded-picture buffers, and native hardware surface
//! pools are destroyed only by the worker thread that owns them. This Module
//! coordinates the transport-family boundary without moving those resources
//! across threads or allowing playback and interactive pools to accumulate.

use std::sync::{Mutex, MutexGuard};

use mondrian_media::PreviewDecodeAccessMode;

use super::preview_access_mode::MediaPreviewWorkerLane;

const WORKER_ANY: u8 = 1 << 0;
const WORKER_PLAYBACK: u8 = 1 << 1;
const WORKER_NON_PLAYBACK: u8 = 1 << 2;

/// Mutually exclusive family of decoder sessions allowed to remain resident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewDecodeResidencyFamily {
    Playback,
    Interactive,
}

impl PreviewDecodeResidencyFamily {
    pub(crate) const fn for_access_mode(access_mode: PreviewDecodeAccessMode) -> Self {
        match access_mode {
            PreviewDecodeAccessMode::PlaybackCursor => Self::Playback,
            PreviewDecodeAccessMode::ScrubCursor
            | PreviewDecodeAccessMode::RandomAccessStillFrame => Self::Interactive,
        }
    }
}

/// One worker's action at a published residency revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewDecodeResidencyDirective {
    revision: u64,
    retire_context: bool,
}

impl PreviewDecodeResidencyDirective {
    pub(crate) const fn revision(self) -> u64 {
        self.revision
    }

    pub(crate) const fn retire_context(self) -> bool {
        self.retire_context
    }
}

#[derive(Debug, Clone, Copy)]
struct PreviewDecodeResidencyState {
    revision: u64,
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
    pub(crate) active_family: Option<PreviewDecodeResidencyFamily>,
    pub(crate) transitions: u64,
    pub(crate) blocked_admissions: u64,
    pub(crate) required_acknowledgements: u32,
    pub(crate) acknowledged_retirements: u32,
}

/// Synchronizes worker-local decoder destruction with cross-family admission.
pub(crate) struct PreviewDecodeResidencyCoordinator {
    state: Mutex<PreviewDecodeResidencyState>,
}

impl PreviewDecodeResidencyCoordinator {
    pub(crate) const fn new() -> Self {
        Self {
            state: Mutex::new(PreviewDecodeResidencyState {
                revision: 0,
                active_family: None,
                worker_mask: 0,
                required_ack_mask: 0,
                acknowledged_mask: 0,
                transitions: 0,
                blocked_admissions: 0,
            }),
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
        let mut state = lock_state(&self.state);
        state.worker_mask &= !bit;
        state.required_ack_mask &= !bit;
        state.acknowledged_mask &= !bit;
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
        state.required_ack_mask = state.worker_mask & retired_worker_mask(family);
        state.acknowledged_mask = 0;
        true
    }

    /// Current coordination revision for a newly started worker loop.
    #[cfg(test)]
    pub(crate) fn revision(&self) -> u64 {
        lock_state(&self.state).revision
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
        })
    }

    /// Confirm that one worker destroyed its context for the current revision.
    pub(crate) fn acknowledge_retirement(&self, lane: MediaPreviewWorkerLane, revision: u64) {
        let mut state = lock_state(&self.state);
        if state.revision == revision {
            state.acknowledged_mask |= worker_lane_bit(lane) & state.required_ack_mask;
        }
    }

    /// Return whether the selected family may create or reuse decoder state.
    pub(crate) fn admits(&self, access_mode: PreviewDecodeAccessMode) -> bool {
        let mut state = lock_state(&self.state);
        let admitted = state.active_family.is_none_or(|family| {
            family == PreviewDecodeResidencyFamily::for_access_mode(access_mode)
        }) && state.acknowledged_mask == state.required_ack_mask;
        if !admitted {
            state.blocked_admissions = state.blocked_admissions.saturating_add(1);
        }
        admitted
    }

    pub(crate) fn diagnostics(&self) -> PreviewDecodeResidencyDiagnostics {
        let state = lock_state(&self.state);
        PreviewDecodeResidencyDiagnostics {
            revision: state.revision,
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
    }
}

const fn retired_worker_mask(family: PreviewDecodeResidencyFamily) -> u8 {
    match family {
        PreviewDecodeResidencyFamily::Playback => WORKER_ANY | WORKER_NON_PLAYBACK,
        PreviewDecodeResidencyFamily::Interactive => WORKER_ANY | WORKER_PLAYBACK,
    }
}

fn lock_state(
    state: &Mutex<PreviewDecodeResidencyState>,
) -> MutexGuard<'_, PreviewDecodeResidencyState> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_admission_waits_for_playback_owner_retirement() {
        let coordinator = PreviewDecodeResidencyCoordinator::new();
        coordinator.register_worker(MediaPreviewWorkerLane::Playback);
        coordinator.register_worker(MediaPreviewWorkerLane::NonPlayback);
        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Playback));
        let playback_revision = coordinator.revision();
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::NonPlayback, 0)
            .expect("non-playback worker should observe playback phase");
        assert!(directive.retire_context());
        coordinator
            .acknowledge_retirement(MediaPreviewWorkerLane::NonPlayback, directive.revision());
        assert!(coordinator.admits(PreviewDecodeAccessMode::PlaybackCursor));

        assert!(coordinator.activate(PreviewDecodeResidencyFamily::Interactive));
        assert!(!coordinator.admits(PreviewDecodeAccessMode::RandomAccessStillFrame));
        let directive = coordinator
            .worker_directive(MediaPreviewWorkerLane::Playback, playback_revision)
            .expect("playback worker should observe interactive phase");
        assert!(directive.retire_context());
        coordinator.acknowledge_retirement(MediaPreviewWorkerLane::Playback, directive.revision());
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
}
