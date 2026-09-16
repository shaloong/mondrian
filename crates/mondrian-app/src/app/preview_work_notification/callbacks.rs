//! Registration and destruction ownership for native Preview wake Adapters.
//!
//! Producers invoke outside locks. One lazily started owner retains every
//! accepted registration until no invocation can hold it, then destroys it off
//! the producer and shutdown stacks. A failed callback is explicitly abandoned.

use super::super::preview_worker_lifecycle::PreviewOwnedWorkerShutdown;
use super::{lock_unpoisoned, PreviewWorkNotificationState};
use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Instant;

const REGISTRATION_CAPACITY: usize = 8;

thread_local! {
    // Dependency detection only: no callback or Runtime ownership lives in TLS.
    static INVOKING: RefCell<Vec<*const RetirementState>> = const { RefCell::new(Vec::new()) };
}

struct CurrentInvocation(bool);

impl CurrentInvocation {
    fn enter(owner: *const RetirementState) -> Self {
        Self(INVOKING.try_with(|stack| stack.borrow_mut().push(owner)).is_ok())
    }

    fn contains(owner: *const RetirementState) -> bool {
        INVOKING
            .try_with(|stack| stack.try_borrow().map_or(true, |stack| stack.contains(&owner)))
            .unwrap_or(true)
    }
}

impl Drop for CurrentInvocation {
    fn drop(&mut self) {
        if self.0 {
            let _ = INVOKING.try_with(|stack| {
                if let Ok(mut stack) = stack.try_borrow_mut() {
                    stack.pop();
                }
            });
        }
    }
}

/// A caller was not granted consuming authority; this is not a terminal receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewCallbackShutdownRejection {
    /// Waiting would depend on the callback or its Drop executing on this thread.
    CurrentInvocation,
    /// A different caller already owns the consuming operation.
    ConcurrentConsumer,
    /// A previous consuming operation unwound before sealing its receipt.
    ConsumerFaulted,
}

/// Raw callback-owner facts, independent of Preview payload-worker success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewWorkCallbackEvidence {
    /// Exact callback evidence schema; missing/default evidence is not closure.
    pub schema_version: u32,
    /// No further registration or invocation can enter this owner.
    pub admission_closed: bool,
    /// Total accepted registration owners.
    pub registrations_accepted: u64,
    /// Owners whose callback destruction returned normally.
    pub registrations_released: u64,
    /// Owners deliberately retained or whose destruction did not return normally.
    pub registrations_abandoned: u64,
    /// Accepted registrations still retained outside active destruction.
    pub registrations_retained: usize,
    /// Callback invocations that have not yet left their producer.
    pub invocations_in_flight: u64,
    /// Callback owners currently being destroyed by the retirement worker.
    pub retirements_active: usize,
    /// Actual caught invocation panics (not merely failed registrations).
    pub invocation_panics: u64,
    /// Actual caught callback-destructor panics.
    pub destructor_panics: u64,
    /// Opaque panic payloads deliberately retained without calling their Drop.
    pub opaque_payloads_abandoned: u64,
    /// Failed attempts to create the one retirement worker.
    pub worker_start_failures: u64,
    /// Whether this owner ever successfully started its retirement worker.
    pub worker_started: bool,
    /// Unexpected retirement worker faults caught outside its loop.
    pub worker_panics: u64,
    /// The exact consuming worker join outcome; absence is unverified.
    #[serde(deserialize_with = "deserialize_required_option")]
    pub worker: Option<PreviewOwnedWorkerShutdown>,
    /// Actual worker completion occurred within the original consuming deadline.
    pub deadline_met: bool,
    /// Explicit consuming-admission rejection; never substitutes for closure.
    #[serde(deserialize_with = "deserialize_required_option")]
    pub shutdown_rejection: Option<PreviewCallbackShutdownRejection>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer)
}

impl PreviewWorkCallbackEvidence {
    /// Whether this explicit receipt proves clean callback-owner closure.
    pub const fn all_resources_released(self) -> bool {
        self.schema_version == 1
            && self.shutdown_rejection.is_none()
            && self.admission_closed
            && self.registrations_accepted == self.registrations_released
            && self.registrations_abandoned == 0
            && self.registrations_retained == 0
            && self.invocations_in_flight == 0
            && self.retirements_active == 0
            && self.invocation_panics == 0
            && self.destructor_panics == 0
            && self.opaque_payloads_abandoned == 0
            && self.worker_start_failures == 0
            && self.worker_panics == 0
            && self.deadline_met
            && matches!(
                (self.worker_started, self.worker),
                (false, Some(PreviewOwnedWorkerShutdown::NotStarted))
                    | (true, Some(PreviewOwnedWorkerShutdown::Terminated))
            )
            && self.worker_started == (self.registrations_accepted != 0)
    }

    /// Whether any sticky callback or retirement-owner failure was observed.
    pub const fn has_failure(self) -> bool {
        self.registrations_abandoned != 0
            || self.invocation_panics != 0
            || self.destructor_panics != 0
            || self.opaque_payloads_abandoned != 0
            || self.worker_start_failures != 0
            || self.worker_panics != 0
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistrationRejectionReason {
    #[error("Preview callback admission is closed")]
    Closed,
    #[error("Preview callback registration capacity is exhausted")]
    Capacity,
    #[error("Preview callback owner is faulted")]
    Faulted,
    #[error("Preview callback retirement worker failed to start: {0}")]
    WorkerStart(#[source] std::io::Error),
}

/// Rejection returns unaccepted callback ownership to the installing Adapter.
pub(crate) struct RegistrationRejected<F> {
    pub(crate) reason: RegistrationRejectionReason,
    pub(crate) callback: F,
}

impl<F> RegistrationRejected<F> {
    pub(crate) fn into_parts(self) -> (RegistrationRejectionReason, F) {
        (self.reason, self.callback)
    }
}

struct Registration {
    callback: Box<dyn Fn() + Send + Sync + 'static>,
    failed: AtomicBool,
}

#[derive(Default)]
struct State {
    installed: Option<Arc<Registration>>,
    retained: Vec<Arc<Registration>>,
    evidence: PreviewWorkCallbackEvidence,
    completed_at: Option<Instant>,
    terminal: Option<PreviewWorkCallbackEvidence>,
}

#[derive(Default)]
struct RetirementState {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
pub(super) struct CallbackOwner {
    retirement: Arc<RetirementState>,
    worker: Mutex<Option<JoinHandle<()>>>,
    consuming: Mutex<()>,
}

impl CallbackOwner {
    pub(super) fn install<F>(
        &self,
        shared: &Arc<PreviewWorkNotificationState>,
        callback: F,
    ) -> Result<(), RegistrationRejected<F>>
    where
        F: Fn() + Send + Sync + 'static,
    {
        self.install_with_spawn(shared, callback, |task| {
            std::thread::Builder::new()
                .name("mondrian-preview-wake-retirement".to_owned())
                .spawn(task)
        })
    }

    fn install_with_spawn<F>(
        &self,
        shared: &Arc<PreviewWorkNotificationState>,
        callback: F,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>>,
    ) -> Result<(), RegistrationRejected<F>>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let mut state = lock_unpoisoned(&self.retirement.state);
        let reason = if state.evidence.admission_closed {
            Some(RegistrationRejectionReason::Closed)
        } else if state.evidence.has_failure() {
            Some(RegistrationRejectionReason::Faulted)
        } else if state.retained.len() + state.evidence.retirements_active >= REGISTRATION_CAPACITY
            || state.evidence.registrations_accepted == u64::MAX
        {
            Some(RegistrationRejectionReason::Capacity)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(RegistrationRejected { reason, callback });
        }
        if !state.evidence.worker_started {
            let retirement = Arc::clone(&self.retirement);
            let notification = Arc::downgrade(shared);
            let worker = match spawn(Box::new(move || run_retirement(retirement, notification))) {
                Ok(worker) => worker,
                Err(error) => {
                    state.evidence.worker_start_failures += 1;
                    drop(state);
                    shared.publish_revision_only();
                    return Err(RegistrationRejected {
                        reason: RegistrationRejectionReason::WorkerStart(error),
                        callback,
                    });
                }
            };
            *lock_unpoisoned(&self.worker) = Some(worker);
            state.evidence.worker_started = true;
        }
        let registration = Arc::new(Registration {
            callback: Box::new(callback),
            failed: AtomicBool::new(false),
        });
        state.retained.push(Arc::clone(&registration));
        // The retained owner makes replacement safe while this gate is held.
        state.installed = Some(Arc::clone(&registration));
        state.evidence.registrations_accepted += 1;
        state.evidence.invocations_in_flight += 1;
        self.retirement.changed.notify_all();
        drop(state);
        self.invoke_admitted(shared, registration);
        Ok(())
    }

    pub(super) fn invoke(&self, shared: &PreviewWorkNotificationState) {
        let registration = {
            let mut state = lock_unpoisoned(&self.retirement.state);
            if state.evidence.admission_closed {
                return;
            }
            let Some(registration) = state.installed.as_ref().cloned() else {
                return;
            };
            if registration.failed.load(Ordering::Acquire) {
                return;
            }
            state.evidence.invocations_in_flight += 1;
            registration
        };
        self.invoke_admitted(shared, registration);
    }

    fn invoke_admitted(
        &self,
        shared: &PreviewWorkNotificationState,
        registration: Arc<Registration>,
    ) {
        let _current_invocation = CurrentInvocation::enter(Arc::as_ptr(&self.retirement));
        let failure = catch_unwind(AssertUnwindSafe(|| (registration.callback)())).err();
        let failed = failure.is_some();
        let opaque = failure.is_some_and(dispose_panic_payload);
        let mut state = lock_unpoisoned(&self.retirement.state);
        if failed {
            registration.failed.store(true, Ordering::Release);
            state.evidence.invocation_panics = state.evidence.invocation_panics.saturating_add(1);
            state.evidence.opaque_payloads_abandoned =
                state.evidence.opaque_payloads_abandoned.saturating_add(u64::from(opaque));
            if state
                .installed
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &registration))
            {
                state.installed = None;
            }
        }
        state.evidence.invocations_in_flight -= 1;
        // Release under the same gate used by the retirement predicate. The
        // owner's retained Arc prevents foreign Drop and prevents lost wakes.
        drop(registration);
        self.retirement.changed.notify_all();
        drop(state);
        if failed {
            // Latch failure first. Never recursively invoke an Adapter here.
            shared.publish_revision_only();
        }
    }

    pub(super) fn begin_shutdown(&self) {
        let mut state = lock_unpoisoned(&self.retirement.state);
        state.evidence.admission_closed = true;
        state.installed = None;
        self.retirement.changed.notify_all();
    }

    pub(super) fn diagnostics(&self) -> PreviewWorkCallbackEvidence {
        let state = lock_unpoisoned(&self.retirement.state);
        snapshot(&state)
    }

    /// Exactly one caller can claim consuming authority. Concurrent/reentrant
    /// callers get explicit rejection, not a fabricated terminal receipt.
    /// Accepted repeats return the original receipt without upgrading it.
    pub(super) fn shutdown(&self, deadline: Option<Instant>) -> PreviewWorkCallbackEvidence {
        self.begin_shutdown();
        if let Some(terminal) = lock_unpoisoned(&self.retirement.state).terminal {
            return terminal;
        }
        if CurrentInvocation::contains(Arc::as_ptr(&self.retirement)) {
            return self.rejected_shutdown(PreviewCallbackShutdownRejection::CurrentInvocation);
        }
        let _consuming = match self.consuming.try_lock() {
            Ok(guard) => guard,
            Err(std::sync::TryLockError::WouldBlock) => {
                return self
                    .rejected_shutdown(PreviewCallbackShutdownRejection::ConcurrentConsumer);
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return self.rejected_shutdown(PreviewCallbackShutdownRejection::ConsumerFaulted);
            }
        };
        if let Some(terminal) = lock_unpoisoned(&self.retirement.state).terminal {
            return terminal;
        }
        let worker = lock_unpoisoned(&self.worker).take();
        let outcome = worker.map_or(
            PreviewOwnedWorkerShutdown::NotStarted,
            |worker| match deadline {
                Some(deadline) => PreviewOwnedWorkerShutdown::join_until(worker, deadline),
                None => PreviewOwnedWorkerShutdown::join(worker),
            },
        );
        let mut state = lock_unpoisoned(&self.retirement.state);
        let mut evidence = snapshot(&state);
        evidence.worker = Some(outcome);
        evidence.deadline_met = if evidence.worker_started {
            state
                .completed_at
                .is_some_and(|completed| deadline.is_none_or(|end| completed <= end))
        } else {
            true
        };
        state.terminal = Some(evidence);
        evidence
    }

    fn rejected_shutdown(
        &self,
        reason: PreviewCallbackShutdownRejection,
    ) -> PreviewWorkCallbackEvidence {
        PreviewWorkCallbackEvidence {
            shutdown_rejection: Some(reason),
            ..self.diagnostics()
        }
    }
}

impl Drop for CallbackOwner {
    fn drop(&mut self) {
        self.begin_shutdown();
        if let Some(worker) =
            self.worker.get_mut().unwrap_or_else(|error| error.into_inner()).take()
        {
            if worker.is_finished() {
                let _ = PreviewOwnedWorkerShutdown::join(worker);
            } else {
                // Worker retains every queued/in-flight callback; ordinary Drop
                // neither waits nor claims a consuming qualification receipt.
                drop(worker);
            }
        }
    }
}

fn snapshot(state: &State) -> PreviewWorkCallbackEvidence {
    PreviewWorkCallbackEvidence {
        schema_version: 1,
        registrations_retained: state.retained.len(),
        ..state.evidence
    }
}

fn dispose_panic_payload(payload: Box<dyn std::any::Any + Send>) -> bool {
    if payload.is::<String>() || payload.is::<&'static str>() {
        drop(payload);
        false
    } else {
        std::mem::forget(payload);
        true
    }
}

fn run_retirement(owner: Arc<RetirementState>, notification: Weak<PreviewWorkNotificationState>) {
    let result = catch_unwind(AssertUnwindSafe(|| retire_callbacks(&owner, &notification)));
    if let Err(payload) = result {
        let opaque = dispose_panic_payload(payload);
        let mut state = lock_unpoisoned(&owner.state);
        state.evidence.worker_panics = state.evidence.worker_panics.saturating_add(1);
        state.evidence.opaque_payloads_abandoned =
            state.evidence.opaque_payloads_abandoned.saturating_add(u64::from(opaque));
        state.installed = None;
        while let Some(registration) = state.retained.pop() {
            std::mem::forget(registration);
            state.evidence.registrations_abandoned += 1;
        }
        drop(state);
        publish_failure(&notification);
    }
    lock_unpoisoned(&owner.state).completed_at = Some(Instant::now());
    owner.changed.notify_all();
}

fn retire_callbacks(owner: &RetirementState, notification: &Weak<PreviewWorkNotificationState>) {
    loop {
        let registration = {
            let mut state = lock_unpoisoned(&owner.state);
            loop {
                if let Some(index) =
                    state.retained.iter().position(|value| Arc::strong_count(value) == 1)
                {
                    state.evidence.retirements_active = 1;
                    break state.retained.swap_remove(index);
                }
                if state.evidence.admission_closed && state.retained.is_empty() {
                    return;
                }
                state = owner.changed.wait(state).unwrap_or_else(|error| error.into_inner());
            }
        };
        let _current_retirement = CurrentInvocation::enter(std::ptr::from_ref(owner));
        let (abandoned, destructor_panicked, opaque) =
            if registration.failed.load(Ordering::Acquire) {
                std::mem::forget(registration);
                (true, false, false)
            } else {
                match catch_unwind(AssertUnwindSafe(|| drop(registration))) {
                    Ok(()) => (false, false, false),
                    Err(payload) => (true, true, dispose_panic_payload(payload)),
                }
            };
        let mut state = lock_unpoisoned(&owner.state);
        state.evidence.retirements_active = 0;
        if abandoned {
            state.evidence.registrations_abandoned += 1;
        } else {
            state.evidence.registrations_released += 1;
        }
        state.evidence.destructor_panics += u64::from(destructor_panicked);
        state.evidence.opaque_payloads_abandoned += u64::from(opaque);
        drop(state);
        if destructor_panicked {
            publish_failure(notification);
        }
    }
}

fn publish_failure(notification: &Weak<PreviewWorkNotificationState>) {
    if let Some(notification) = notification.upgrade() {
        notification.publish_revision_only();
    }
}

#[cfg(test)]
#[path = "../../../tests/protocol/preview_work_callbacks.rs"]
mod tests;
