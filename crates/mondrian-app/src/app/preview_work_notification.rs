//! Payload-free progress notification for Preview worker domains.
//!
//! Result channels remain the sole authority for media, visual, title, and
//! dependency-refresh payloads. Coordination Modules may also publish when an
//! asynchronous retry barrier becomes actionable. This Module carries neither
//! payload nor completion authority: it only advances a monotonic revision so
//! Window and Headless Adapters can wake without inventing a second queue.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(any(test, feature = "validation"))]
use std::time::Duration;
#[cfg(any(test, feature = "validation"))]
use std::time::Instant;

#[path = "preview_work_notification/callbacks.rs"]
mod callbacks;
use callbacks::CallbackOwner;
pub(crate) use callbacks::RegistrationRejected;
pub use callbacks::{PreviewCallbackShutdownRejection, PreviewWorkCallbackEvidence};

/// Monotonic process-local revision of actionable Preview worker progress.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PreviewWorkRevision(u64);

/// Worker-side publisher for the Preview progress-notification seam.
///
/// A worker must call [`Self::result_became_pollable`] only after its result
/// transport accepts the payload. The revision is therefore a wake hint, never
/// evidence that a particular result exists or remains current.
#[derive(Clone)]
pub(crate) struct PreviewWorkNotifier {
    shared: Arc<PreviewWorkNotificationState>,
}

/// Consumer-side observer for Preview worker completion.
///
/// Window installs one payload-free native-loop waker. Headless consumers may
/// instead compare revisions or perform a bounded wait. Both still drain the
/// Runtime's ordinary result transports to learn what completed.
#[derive(Clone)]
pub(crate) struct PreviewWorkWatch {
    shared: Arc<PreviewWorkNotificationState>,
}

/// RAII publication of one worker's terminal lifecycle transition.
///
/// The guard also runs during unwinding, so a worker that exits before sending
/// a result cannot strand a Pending Window or Headless consumer.
#[must_use]
pub(crate) struct PreviewWorkerExitNotification {
    notifier: PreviewWorkNotifier,
}

struct PreviewWorkNotificationState {
    revision: AtomicU64,
    wait_gate: Mutex<()>,
    changed: Condvar,
    callbacks: CallbackOwner,
}

impl PreviewWorkNotificationState {
    fn publish_revision_only(&self) -> PreviewWorkRevision {
        let gate = lock_unpoisoned(&self.wait_gate);
        let next = self.revision.load(Ordering::Relaxed).saturating_add(1);
        self.revision.store(next, Ordering::Release);
        self.changed.notify_all();
        drop(gate);
        PreviewWorkRevision(next)
    }
}

impl fmt::Debug for PreviewWorkNotifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreviewWorkNotifier")
            .field("revision", &self.shared.revision.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PreviewWorkWatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreviewWorkWatch")
            .field("revision", &self.revision())
            .finish_non_exhaustive()
    }
}

impl Default for PreviewWorkNotifier {
    fn default() -> Self {
        preview_work_notification_channel().0
    }
}

/// Create one paired worker notifier and consumer watch.
pub(crate) fn preview_work_notification_channel() -> (PreviewWorkNotifier, PreviewWorkWatch) {
    let shared = Arc::new(PreviewWorkNotificationState {
        revision: AtomicU64::new(0),
        wait_gate: Mutex::new(()),
        changed: Condvar::new(),
        callbacks: CallbackOwner::default(),
    });
    (
        PreviewWorkNotifier { shared: Arc::clone(&shared) },
        PreviewWorkWatch { shared },
    )
}

impl PreviewWorkNotifier {
    /// Advance the revision and wake observers after a result is pollable.
    pub(crate) fn result_became_pollable(&self) -> PreviewWorkRevision {
        self.publish_progress()
    }

    /// Advance the revision after an asynchronous coordination barrier changes
    /// from blocking to actionable.
    ///
    /// The coordination Module must publish only on the aggregate transition,
    /// not for every partial acknowledgement. The consumer still re-evaluates
    /// the authoritative state after waking.
    pub(crate) fn retry_became_actionable(&self) -> PreviewWorkRevision {
        self.publish_progress()
    }

    /// Create a guard that publishes when the owning worker exits.
    pub(crate) fn worker_exit_notification(&self) -> PreviewWorkerExitNotification {
        PreviewWorkerExitNotification { notifier: self.clone() }
    }

    fn publish_progress(&self) -> PreviewWorkRevision {
        let revision = self.shared.publish_revision_only();
        self.shared.callbacks.invoke(&self.shared);
        revision
    }

    #[cfg(test)]
    pub(crate) fn watch(&self) -> PreviewWorkWatch {
        PreviewWorkWatch { shared: Arc::clone(&self.shared) }
    }
}

impl Drop for PreviewWorkerExitNotification {
    fn drop(&mut self) {
        self.notifier.publish_progress();
    }
}

impl PreviewWorkWatch {
    /// Build a payload-free producer callback for an external asynchronous
    /// completion source that belongs to this Preview Runtime.
    pub(crate) fn completion_waker(&self) -> impl Fn() + Send + Sync + 'static {
        let notifier = PreviewWorkNotifier { shared: Arc::clone(&self.shared) };
        move || {
            notifier.result_became_pollable();
        }
    }

    /// Return the latest published completion revision without blocking.
    pub(crate) fn revision(&self) -> PreviewWorkRevision {
        PreviewWorkRevision(self.shared.revision.load(Ordering::Acquire))
    }

    /// Wait for a revision different from `observed`, bounded by `timeout`.
    ///
    /// The comparison occurs while holding the same gate used by publishers,
    /// so a publication immediately before or during the wait cannot be lost.
    /// Returning the unchanged revision means only that the timeout elapsed.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn wait_for_change(
        &self,
        observed: PreviewWorkRevision,
        timeout: Duration,
    ) -> PreviewWorkRevision {
        let gate = lock_unpoisoned(&self.shared.wait_gate);
        if self.revision() != observed {
            return self.revision();
        }
        let (gate, _) = self
            .shared
            .changed
            .wait_timeout_while(gate, timeout, |_| self.revision() == observed)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        drop(gate);
        self.revision()
    }

    /// Install the single native-consumer wake Adapter.
    ///
    /// Installation performs one initial wake so results published before
    /// registration cannot strand a consumer in its native wait state. Later
    /// calls replace the Adapter; Headless revision waits remain independent.
    /// Rejection transfers the unaccepted callback back to the caller; no
    /// callback is destroyed on an internal rejection or replacement path.
    pub(crate) fn install_waker<F>(&self, waker: F) -> Result<(), RegistrationRejected<F>>
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.shared.callbacks.install(&self.shared, waker)
    }

    /// Close callback registration and invocation before any Runtime join.
    pub(crate) fn begin_shutdown(&self) {
        self.shared.callbacks.begin_shutdown();
    }

    /// Read sticky callback facts without pumping a Preview result transport.
    pub(crate) fn callback_evidence(&self) -> PreviewWorkCallbackEvidence {
        self.shared.callbacks.diagnostics()
    }

    /// Consume callback ownership under the Runtime's original deadline.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn shutdown_until(&self, deadline: Instant) -> PreviewWorkCallbackEvidence {
        self.shared.callbacks.shutdown(Some(deadline))
    }

    /// Explicit unbounded counterpart used by synchronous Runtime shutdown.
    pub(crate) fn shutdown_and_wait(&self) -> PreviewWorkCallbackEvidence {
        self.shared.callbacks.shutdown(None)
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "../../tests/protocol/preview_work_notification.rs"]
mod tests;
