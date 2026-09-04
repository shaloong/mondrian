//! Native thumbnail ownership, admission revocation and immutable shutdown facts.
use super::*;
use crate::app::owned_worker_lifecycle::OwnedWorkerShutdown;
use std::thread::JoinHandle;

#[derive(Default)]
pub(super) struct ThumbnailLifecycle {
    prepared: Option<(
        mpsc::Receiver<ThumbnailJob>,
        mpsc::SyncSender<ThumbnailResult>,
    )>,
    pub(super) worker: Option<JoinHandle<()>>,
    pub(super) attempted: bool,
    pub(super) started: bool,
    start_failed: bool,
    receipt: Option<ThumbnailShutdownEvidence>,
}

impl ThumbnailLifecycle {
    pub(super) fn prepared(
        jobs: mpsc::Receiver<ThumbnailJob>,
        results: mpsc::SyncSender<ThumbnailResult>,
    ) -> Self {
        Self { prepared: Some((jobs, results)), ..Self::default() }
    }
}

/// Another caller currently owns native startup or consuming shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, serde::Serialize)]
pub enum ThumbnailShutdownUnavailable {
    /// No inventory is inferred while the lifecycle owner is unavailable.
    #[error("thumbnail lifecycle is already owned by another operation")]
    LifecycleBusy,
}

/// Immutable original-deadline receipt for the thumbnail worker and its transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ThumbnailShutdownEvidence {
    /// Receipt schema.
    pub schema_version: u32,
    /// Native startup was attempted, independently of its result.
    pub startup_attempted: bool,
    /// Native construction returned one handle.
    pub worker_started: bool,
    /// Native construction returned an ordinary no-owner error.
    pub worker_start_failed: bool,
    /// Exact consuming worker outcome.
    pub worker: OwnedWorkerShutdown,
    /// Shared admission and result publication were closed.
    pub admission_closed: bool,
    /// Both transport endpoints owned by the service were released.
    pub transports_released: bool,
    /// Authoritative requests retained after cancellation.
    pub pending_requests: usize,
    /// Deferred requests retained after cancellation.
    pub deferred_requests: usize,
    /// Resident cached raster bytes remaining in the service.
    pub cached_bytes: usize,
    /// The result-publication activity transport was permanently revoked.
    pub publication_revoked: bool,
    /// Actual current execution owners observed after the join attempt.
    pub active_requests_remaining: usize,
    /// Result identities retained after their transport was retired.
    pub awaiting_publication_remaining: usize,
}

impl ThumbnailShutdownEvidence {
    /// Closure of a successfully started production thumbnail worker.
    pub fn all_resources_released(self) -> bool {
        self.startup_attempted
            && self.worker_started
            && !self.worker_start_failed
            && self.all_created_resources_released()
    }

    /// Closure of exact prepared/failed/started inventory; unknown startup fails.
    pub fn all_created_resources_released(self) -> bool {
        self.schema_version == 1
            && self.admission_closed
            && self.transports_released
            && self.pending_requests == 0
            && self.deferred_requests == 0
            && self.cached_bytes == 0
            && self.publication_revoked
            && self.active_requests_remaining == 0
            && self.awaiting_publication_remaining == 0
            && match (
                self.startup_attempted,
                self.worker_started,
                self.worker_start_failed,
            ) {
                (true, true, false) => self.worker == OwnedWorkerShutdown::Terminated,
                (false, false, false) | (true, false, true) => {
                    self.worker == OwnedWorkerShutdown::NotStarted
                }
                _ => false,
            }
    }
}

impl AssetThumbnailService {
    /// Install the returned worker before any subsequent Host construction step.
    pub(crate) fn start_in_place(&self) {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.attempted || self.dispatch_gate.shutdown.load(Ordering::Acquire) {
            return;
        }
        let Some((jobs, results)) = lifecycle.prepared.take() else {
            return;
        };
        lifecycle.attempted = true;
        let gate = Arc::clone(&self.dispatch_gate);
        let activity = Arc::clone(&self.worker_activity);
        match std::thread::Builder::new()
            .name("mondrian-asset-thumbnails".to_owned())
            .spawn(move || thumbnail_worker(jobs, results, gate, activity))
        {
            Ok(worker) => {
                lifecycle.worker = Some(worker);
                lifecycle.started = true;
            }
            Err(error) => {
                lifecycle.start_failed = true;
                tracing::error!(%error, "failed to start asset thumbnail worker");
            }
        }
    }

    pub(super) fn send_job(&self, job: ThumbnailJob) -> ThumbnailDispatchOutcome {
        match self.jobs.lock().as_ref() {
            Some(jobs) => match jobs.try_send(job) {
                Ok(()) => ThumbnailDispatchOutcome::Sent,
                Err(mpsc::TrySendError::Full(job)) => ThumbnailDispatchOutcome::Full(job),
                Err(mpsc::TrySendError::Disconnected(job)) => ThumbnailDispatchOutcome::Closed(job),
            },
            None => ThumbnailDispatchOutcome::Closed(job),
        }
    }

    /// Revoke all admission and publication without waiting for native decoding.
    pub fn begin_shutdown(&self) {
        self.dispatch_gate.shutdown.store(true, Ordering::Release);
        self.dispatch_gate.changed.notify_all();
        self.worker_activity.revoke_publication();
        {
            let mut state = self.state.lock();
            state.closed = true;
            state.admit_automatic = false;
            state.dispatch_enabled = false;
            for pending in state.pending.values() {
                pending.cancellation.cancel();
            }
            for job in &state.deferred {
                job.cancellation.cancel();
            }
            state.pending.clear();
            state.deferred.clear();
            state.active.clear();
            state.cache.clear();
            state.cache_lru.clear();
            state.cached_bytes = 0;
            state.failures.clear();
            state.failure_lru.clear();
            state.color_context = None;
        }
        // Do not hold state while taking the result lock: polling publishes into state.
        self.jobs.lock().take();
        self.results.lock().take();
    }

    /// Consume the actual handle once; later calls retain the original deadline outcome.
    /// Concurrent lifecycle ownership returns immediately without inventing a receipt.
    /// Admission revocation uses short state locks; this is not a hard-realtime interface.
    pub fn shutdown_until(
        &self,
        deadline: Instant,
    ) -> Result<ThumbnailShutdownEvidence, ThumbnailShutdownUnavailable> {
        let mut lifecycle =
            self.lifecycle.try_lock().ok_or(ThumbnailShutdownUnavailable::LifecycleBusy)?;
        self.begin_shutdown();
        if let Some(receipt) = lifecycle.receipt {
            return Ok(receipt);
        }
        lifecycle.prepared.take();
        let worker = lifecycle.worker.take().map_or(OwnedWorkerShutdown::NotStarted, |worker| {
            OwnedWorkerShutdown::join_until(worker, deadline)
        });
        let state = self.state.lock();
        let activity = self.worker_activity.snapshot();
        let receipt = ThumbnailShutdownEvidence {
            schema_version: 1,
            startup_attempted: lifecycle.attempted,
            worker_started: lifecycle.started,
            worker_start_failed: lifecycle.start_failed,
            worker,
            admission_closed: state.closed,
            transports_released: self.jobs.lock().is_none() && self.results.lock().is_none(),
            pending_requests: state.pending.len(),
            deferred_requests: state.deferred.len(),
            cached_bytes: state.cached_bytes,
            publication_revoked: activity.publication_revoked,
            active_requests_remaining: usize::from(activity.current.is_some()),
            awaiting_publication_remaining: activity.awaiting_publication.len(),
        };
        lifecycle.receipt = Some(receipt);
        Ok(receipt)
    }
}
