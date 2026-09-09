//! Raw Preview worker closure and exact failed-start inventory.
//!
//! Normal-runtime qualification and partial-construction cleanup are distinct;
//! both consume the same worker outcomes rather than interpreting thread counts
//! in Window, Headless or performance adapters.
use super::preview_work_notification::PreviewWorkCallbackEvidence;
use super::preview_worker_lifecycle::PreviewOwnedWorkerShutdown;

/// Construction state of one native owner, independent of Runtime readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewStartupOwnerState {
    /// No native construction was attempted.
    NotAttempted,
    /// Construction unwound before returning a known owner or ordinary failure.
    InProgress,
    /// A returned native owner was installed in the unpublished Runtime.
    Installed,
    /// Construction returned an ordinary error without a native owner.
    Failed,
    /// The owner was explicitly disabled by policy (cache only).
    Disabled,
}

impl PreviewStartupOwnerState {
    pub(crate) fn from_installed(installed: bool) -> Self {
        if installed {
            Self::Installed
        } else {
            Self::Failed
        }
    }

    fn matches_closed(self, outcome: PreviewOwnedWorkerShutdown) -> bool {
        match self {
            Self::Installed => outcome == PreviewOwnedWorkerShutdown::Terminated,
            Self::NotAttempted | Self::Failed => outcome == PreviewOwnedWorkerShutdown::NotStarted,
            Self::InProgress | Self::Disabled => false,
        }
    }
}

/// Exact per-owner inventory before an unpublished Runtime failed construction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewStartupInventory {
    /// Required cache worker or explicitly disabled test cache.
    pub cache: PreviewStartupOwnerState,
    /// Visual execution worker.
    pub visual: PreviewStartupOwnerState,
    /// CPU fallback worker.
    pub cpu_fallback: PreviewStartupOwnerState,
    /// Visual dependency observer.
    pub observer: PreviewStartupOwnerState,
    /// One state per requested media worker, in construction order.
    pub media: Vec<PreviewStartupOwnerState>,
}

impl PreviewStartupInventory {
    pub(crate) fn new(worker_count: usize, cache_required: bool) -> Self {
        Self {
            cache: if cache_required {
                PreviewStartupOwnerState::NotAttempted
            } else {
                PreviewStartupOwnerState::Disabled
            },
            visual: PreviewStartupOwnerState::NotAttempted,
            cpu_fallback: PreviewStartupOwnerState::NotAttempted,
            observer: PreviewStartupOwnerState::NotAttempted,
            media: vec![PreviewStartupOwnerState::NotAttempted; worker_count],
        }
    }
}

/// Exact outcomes recorded by the sole Runtime consuming shutdown algorithm.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewStartupWorkerShutdown {
    /// Only installed media workers, preserving their construction order.
    pub media: Vec<PreviewOwnedWorkerShutdown>,
    /// Actual visual worker outcome.
    pub visual: PreviewOwnedWorkerShutdown,
    /// Actual CPU fallback worker outcome.
    pub cpu_fallback: PreviewOwnedWorkerShutdown,
    /// Lazy title worker must remain absent before Runtime publication.
    pub title: PreviewOwnedWorkerShutdown,
}

/// Owner-free closure of a partial Preview, never normal-runtime qualification.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewStartupShutdownEvidence {
    /// Partial-construction evidence schema.
    pub schema_version: u32,
    /// The original constructor panic payload could not safely be destroyed.
    pub opaque_panic_payload_abandoned: bool,
    /// Native creation states captured before consuming any owner.
    pub inventory: PreviewStartupInventory,
    /// Exact individual outcomes produced during consuming shutdown.
    pub workers: PreviewStartupWorkerShutdown,
    /// Unmodified normal raw receipt; its normal admission predicate stays strict.
    pub runtime: PreviewRuntimeShutdownEvidence,
}

impl PreviewStartupShutdownEvidence {
    /// Prove closure of only the actual created inventory, rejecting unknowns.
    pub fn all_created_resources_released(&self) -> bool {
        use PreviewOwnedWorkerShutdown::{NotStarted, Terminated};
        use PreviewStartupOwnerState::{Disabled, Failed, InProgress, Installed, NotAttempted};
        let raw = self.runtime;
        if self.schema_version != 1
            || self.opaque_panic_payload_abandoned
            || raw.schema_version != 4
            || !self.inventory.visual.matches_closed(self.workers.visual)
            || !self.inventory.cpu_fallback.matches_closed(self.workers.cpu_fallback)
            || !raw
                .visual_dependency_worker
                .is_some_and(|worker| self.inventory.observer.matches_closed(worker))
            || self.workers.title != NotStarted
            || self.inventory.media.iter().any(|state| matches!(state, InProgress | Disabled))
            || self.inventory.media.iter().filter(|state| **state == Installed).count()
                != self.workers.media.len()
            || self.workers.media.iter().any(|worker| *worker != Terminated)
        {
            return false;
        }
        let cache = raw.timeline_render_cache;
        let cache_valid = cache.schema_version == 1
            && match self.inventory.cache {
                Installed => {
                    cache.required
                        && !cache.start_failed
                        && cache.aggregate_outcome == Terminated
                        && cache.worker.is_some_and(|worker| {
                            worker.worker_started && worker.all_workers_terminated()
                        })
                }
                NotAttempted => {
                    cache.required
                        && !cache.start_failed
                        && cache.worker.is_none()
                        && cache.aggregate_outcome == NotStarted
                }
                Failed => {
                    cache.required
                        && cache.start_failed
                        && cache.worker.is_none()
                        && cache.aggregate_outcome == NotStarted
                }
                Disabled => {
                    !cache.required
                        && !cache.start_failed
                        && cache.worker.is_none()
                        && cache.aggregate_outcome == NotStarted
                }
                InProgress => false,
            };
        let Some(callbacks) = raw.work_callbacks else {
            return false;
        };
        if !cache_valid
            || !callbacks.all_resources_released()
            || callbacks.registrations_accepted != 0
            || callbacks.worker_started
        {
            return false;
        }
        // Recompute the aggregate from the same individual outcomes. This also
        // rejects extra workers, asynchronous reaps and contradictory counters.
        let mut expected = PreviewRuntimeShutdownEvidence {
            schema_version: 4,
            visual_dependency_worker: raw.visual_dependency_worker,
            work_callbacks: raw.work_callbacks,
            timeline_render_cache: cache,
            ..PreviewRuntimeShutdownEvidence::default()
        };
        for &worker in &self.workers.media {
            expected.record(worker);
        }
        expected.record(self.workers.visual);
        expected.record(self.workers.cpu_fallback);
        expected.record(self.workers.title);
        if let Some(worker) = raw.visual_dependency_worker {
            expected.record(worker);
        }
        expected.record(cache.aggregate_outcome);
        if let Some(worker) = callbacks.worker {
            expected.record(worker);
        }
        raw == expected
    }
}

/// Synchronous terminal evidence for every worker owned by Preview Runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreviewRuntimeShutdownEvidence {
    /// Evidence schema version.
    pub schema_version: u32,
    /// Workers that successfully started during this Runtime lifetime.
    pub workers_started: u32,
    /// Started workers synchronously joined by the shutdown caller.
    pub workers_terminated: u32,
    /// Joined workers whose thread body panicked.
    pub worker_panics: u32,
    /// Joined workers whose opaque panic payload could not safely be released.
    pub worker_panic_payloads_abandoned: u32,
    /// Workers detached because shutdown ran on that same worker thread.
    pub current_thread_detachments: u32,
    /// Workers previously transferred to the ordinary asynchronous UI reaper.
    pub unverified_async_reaps: u32,
    /// Workers still running at the shared qualification deadline.
    pub worker_timeouts: u32,
    /// Worker handles detached after the shared qualification deadline.
    pub worker_deadline_detachments: u32,
    /// Exact required visual-dependency observer receipt; absent is unverified.
    pub visual_dependency_worker: Option<PreviewOwnedWorkerShutdown>,
    /// Exact callback registration, invocation and retirement-owner receipt.
    pub work_callbacks: Option<PreviewWorkCallbackEvidence>,
    /// Exact startup and terminal evidence for the persistent Timeline render cache.
    pub(crate) timeline_render_cache:
        super::preview_render_cache::PreviewTimelineRenderCacheShutdownEvidence,
}

impl PreviewRuntimeShutdownEvidence {
    /// Whether every started worker returned synchronously and without panic.
    pub const fn all_workers_terminated(self) -> bool {
        self.schema_version == 4
            && matches!(self.work_callbacks, Some(callbacks) if callbacks.all_resources_released())
            && matches!(
                self.visual_dependency_worker,
                Some(PreviewOwnedWorkerShutdown::Terminated)
            )
            && self.workers_started == self.workers_terminated
            && self.workers_started
                >= 1 + matches!(self.timeline_render_cache.worker, Some(worker) if worker.worker_started)
                    as u32
                    + matches!(self.work_callbacks, Some(callbacks) if callbacks.worker_started)
                        as u32
            && self.worker_panics == 0
            && self.worker_panic_payloads_abandoned == 0
            && self.current_thread_detachments == 0
            && self.unverified_async_reaps == 0
            && self.worker_timeouts == 0
            && self.worker_deadline_detachments == 0
            && self.timeline_render_cache.all_resources_released()
    }

    pub(crate) fn record(&mut self, outcome: PreviewOwnedWorkerShutdown) {
        match outcome {
            PreviewOwnedWorkerShutdown::NotStarted => {}
            PreviewOwnedWorkerShutdown::Terminated => {
                self.workers_started = self.workers_started.saturating_add(1);
                self.workers_terminated = self.workers_terminated.saturating_add(1);
            }
            PreviewOwnedWorkerShutdown::Panicked
            | PreviewOwnedWorkerShutdown::PanickedPayloadAbandoned => {
                self.workers_started = self.workers_started.saturating_add(1);
                self.workers_terminated = self.workers_terminated.saturating_add(1);
                self.worker_panics = self.worker_panics.saturating_add(1);
                if outcome == PreviewOwnedWorkerShutdown::PanickedPayloadAbandoned {
                    self.worker_panic_payloads_abandoned =
                        self.worker_panic_payloads_abandoned.saturating_add(1);
                }
            }
            PreviewOwnedWorkerShutdown::CurrentThreadSkipped => {
                self.workers_started = self.workers_started.saturating_add(1);
                self.current_thread_detachments = self.current_thread_detachments.saturating_add(1);
            }
            PreviewOwnedWorkerShutdown::TimedOutDetached => {
                self.workers_started = self.workers_started.saturating_add(1);
                self.worker_timeouts = self.worker_timeouts.saturating_add(1);
                self.worker_deadline_detachments =
                    self.worker_deadline_detachments.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/preview_shutdown_evidence.rs"]
mod tests;
