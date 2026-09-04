//! Payload-free progress notification for Preview worker domains.
//!
//! Result channels remain the sole authority for media, visual, title, and
//! dependency-refresh payloads. Coordination Modules may also publish when an
//! asynchronous retry barrier becomes actionable. This Module carries neither
//! payload nor completion authority: it only advances a monotonic revision so
//! Window and Headless Adapters can wake without inventing a second queue.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
#[cfg(any(test, feature = "validation"))]
use std::time::Duration;

type PreviewWorkWaker = Arc<dyn Fn() + Send + Sync + 'static>;

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
    waker: Mutex<Option<PreviewWorkWaker>>,
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
        waker: Mutex::new(None),
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
        let gate = lock_unpoisoned(&self.shared.wait_gate);
        let current = self.shared.revision.load(Ordering::Relaxed);
        let next = current.saturating_add(1);
        self.shared.revision.store(next, Ordering::Release);
        self.shared.changed.notify_all();
        drop(gate);

        // Never invoke an Adapter while holding the notification-state locks.
        // Sending a native-loop event is allowed to re-enter arbitrary OS code.
        let waker = lock_unpoisoned(&self.shared.waker).clone();
        if let Some(waker) = waker {
            invoke_waker(&self.shared, waker);
        }
        PreviewWorkRevision(next)
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
    pub(crate) fn install_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        let waker: PreviewWorkWaker = Arc::new(waker);
        *lock_unpoisoned(&self.shared.waker) = Some(Arc::clone(&waker));
        invoke_waker(&self.shared, waker);
    }
}

fn invoke_waker(shared: &PreviewWorkNotificationState, waker: PreviewWorkWaker) {
    if catch_unwind(AssertUnwindSafe(|| waker())).is_ok() {
        return;
    }
    let mut installed = lock_unpoisoned(&shared.waker);
    if installed.as_ref().is_some_and(|current| Arc::ptr_eq(current, &waker)) {
        *installed = None;
    }
    tracing::warn!(
        "Preview work-watch Adapter panicked and was detached; revision and bounded waits remain active"
    );
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
#[path = "../../tests/protocol/preview_work_notification.rs"]
mod tests;
