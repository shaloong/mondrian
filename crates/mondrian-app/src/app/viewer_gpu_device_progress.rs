//! Device-scoped progress and cleanup authority for Viewer GPU submissions.
//!
//! Window and Headless presentation Adapters submit through different output
//! seams, but they share one invariant: progress capacity and worker health are
//! reserved before recording, while the exact wgpu `SubmissionIndex` becomes
//! available only after `Queue::submit`. This Module models that split with a
//! move-only permit. Committing the returned index is therefore infallible from
//! the caller's perspective. If recording instead reports bounded
//! backpressure, that same permit becomes a typed renderer-cleanup barrier; no
//! Adapter thread introduces an unindexed Viewer-completion poll.
//!
//! The worker never decides semantic presentation. An exact queue callback
//! publishes completion into `ViewerGpuSubmissionLifecycle` and marks the
//! cleanup ticket carried by the progress command. Native waits merely drive
//! that callback. A non-timeout failure terminalizes the complete device
//! generation, rejects future permits, and continues bounded latest-submission
//! waits until the already-submitted cleanup ticket is observed. Device-owner
//! retirement closes admission and appends one FIFO retirement envelope to the
//! same non-UI worker, so submitted GPU/native owners outlive both Window and
//! Headless Adapter teardown without blocking either caller thread.

use std::any::Any;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::viewer_gpu_submission::{ViewerGpuSubmissionId, VIEWER_GPU_SUBMISSION_CAPACITY};

const VIEWER_GPU_PROGRESS_SLOT_CAPACITY: usize = VIEWER_GPU_SUBMISSION_CAPACITY;
const DEFAULT_VIEWER_GPU_WAIT_QUANTUM: Duration = Duration::from_millis(8);
// Admission is acquired before a device-generation worker exists and stays in
// that worker through retirement. Repeated rebuild/teardown therefore cannot
// accumulate an unbounded number of detached progress domains or envelopes.
const MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS: usize = 4;
static LIVE_VIEWER_GPU_DEVICE_GENERATIONS: AtomicUsize = AtomicUsize::new(0);
static NEXT_VIEWER_GPU_DEVICE_GENERATION_ID: AtomicU64 = AtomicU64::new(1);

/// Process-local identity of one actual wgpu device/progress generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ViewerGpuDeviceGenerationId(u64);

impl ViewerGpuDeviceGenerationId {
    fn next() -> Result<Self, ViewerGpuDeviceProgressStartError> {
        NEXT_VIEWER_GPU_DEVICE_GENERATION_ID
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current.checked_add(1)
            })
            .map(Self)
            .map_err(|_| ViewerGpuDeviceProgressStartError::GenerationIdentityExhausted)
    }

    /// Nonzero process-local generation value sealed into recovery evidence.
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Wake ownership shared with the bounded production callback retirement Module.
#[derive(Clone)]
pub(crate) struct ViewerGpuDeviceProgressWake {
    notifier: super::preview_work_notification::PreviewWorkNotifier,
    watch: super::preview_work_notification::PreviewWorkWatch,
    native_failures: Arc<AtomicU64>,
    registration_rejections: Arc<AtomicU64>,
}

impl ViewerGpuDeviceProgressWake {
    /// Construct a wake seam with one concrete Adapter notification.
    #[cfg(test)]
    pub(crate) fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let owner = Self::default();
        owner.install_owned(wake);
        owner
    }

    /// Record native event delivery failure independently of callback return.
    #[cfg(test)]
    pub(crate) fn native(wake: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        let owner = Self::default();
        owner.install_native(wake);
        owner
    }

    fn install_native(&self, wake: impl Fn() -> bool + Send + Sync + 'static) {
        let failures = Arc::clone(&self.native_failures);
        self.install_owned(move || {
            if !wake() {
                failures.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    fn install_owned(&self, wake: impl Fn() + Send + Sync + 'static) {
        if let Err(rejected) = self.watch.install_waker(wake) {
            let (_, callback) = rejected.into_parts();
            // No consumer accepted this capture. Never run foreign Drop on
            // the producer/registration stack; retain the failure explicitly.
            std::mem::forget(callback);
            self.registration_rejections.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Replace the concrete notification for a test-owned GPU device.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn install(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.install_owned(wake);
    }

    /// Notify without allowing Adapter code to unwind through wgpu.
    pub(crate) fn notify(&self) {
        self.notifier.result_became_pollable();
    }

    fn has_failure(&self) -> bool {
        self.watch.callback_evidence().has_failure()
            || self.native_failures.load(Ordering::Acquire) != 0
            || self.registration_rejections.load(Ordering::Acquire) != 0
    }
}

impl Default for ViewerGpuDeviceProgressWake {
    fn default() -> Self {
        let (notifier, watch) =
            super::preview_work_notification::preview_work_notification_channel();
        Self {
            notifier,
            watch,
            native_failures: Arc::new(AtomicU64::new(0)),
            registration_rejections: Arc::new(AtomicU64::new(0)),
        }
    }
}

/// Bound for one native `Device::poll(Wait)` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewerGpuDeviceProgressPolicy {
    wait_quantum: Duration,
}

impl ViewerGpuDeviceProgressPolicy {
    #[cfg(test)]
    fn new(wait_quantum: Duration) -> Result<Self, ViewerGpuDeviceProgressPolicyError> {
        if wait_quantum.is_zero() {
            return Err(ViewerGpuDeviceProgressPolicyError::ZeroWaitQuantum);
        }
        Ok(Self { wait_quantum })
    }
}

impl Default for ViewerGpuDeviceProgressPolicy {
    fn default() -> Self {
        Self { wait_quantum: DEFAULT_VIEWER_GPU_WAIT_QUANTUM }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[cfg(test)]
enum ViewerGpuDeviceProgressPolicyError {
    #[error("Viewer GPU device progress wait quantum must be non-zero")]
    ZeroWaitQuantum,
}

/// Non-authoritative evidence emitted by the progress execution domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ViewerGpuDeviceProgressObservation {
    /// The exact callback cleanup ticket was observed.
    WaitSatisfied {
        /// Process-local Viewer submission identity.
        submission_id: ViewerGpuSubmissionId,
        /// Monotonic observation time.
        observed_at: Instant,
    },
    /// Renderer-internal work from a failed pre-submit attempt was drained.
    RendererCleanupSatisfied {
        /// Reserved Viewer attempt that requested the cleanup barrier.
        attempt_id: ViewerGpuSubmissionId,
        /// Monotonic cleanup observation time.
        observed_at: Instant,
    },
    /// A non-timeout wait failure terminalized the device generation.
    DevicePollFailed {
        /// Reserved submission attempt being driven when the generation failed.
        submission_id: ViewerGpuSubmissionId,
        /// Stable wgpu or panic-isolation diagnostic.
        reason: String,
        /// Monotonic failure observation time.
        observed_at: Instant,
    },
}

/// Why one complete Viewer device generation became terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewerGpuDeviceGenerationTerminalKind {
    /// wgpu reported an unexpected concrete device loss.
    DeviceLost,
    /// The application explicitly destroyed the concrete device generation.
    DeviceDestroyed,
    /// The progress domain could no longer prove submitted work completion.
    ProgressFailure,
}

#[cfg(any(test, feature = "validation"))]
impl ViewerGpuDeviceGenerationTerminalKind {
    /// One unexpected physical device-loss observation.
    pub const fn device_loss_count(self) -> u64 {
        matches!(self, Self::DeviceLost) as u64
    }

    /// One non-loss terminal fault, including explicit device destruction.
    pub const fn fatal_error_count(self) -> u64 {
        matches!(self, Self::DeviceDestroyed | Self::ProgressFailure) as u64
    }
}

/// First terminal of one Viewer device generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ViewerGpuDeviceGenerationTerminal {
    /// Reserved submission attempt that first proved the generation unusable.
    /// Device loss may be observed while no Viewer attempt is active.
    pub(crate) submission_id: Option<ViewerGpuSubmissionId>,
    /// Typed release semantics for the terminal.
    pub(crate) kind: ViewerGpuDeviceGenerationTerminalKind,
    /// A later device-lost callback may strengthen an earlier progress failure
    /// without replacing its first-cause diagnostics.
    wgpu_work_terminal: bool,
    /// Stable wgpu or panic-isolation diagnostic.
    pub(crate) reason: String,
    /// Monotonic terminal observation time.
    pub(crate) observed_at: Instant,
}

impl ViewerGpuDeviceGenerationTerminal {
    /// Whether wgpu has invalidated all work and resources in this generation.
    ///
    /// Native decoder-queue copies are separate physical work and must still be
    /// retired by the Adapter-specific retirement envelope before their media
    /// owners are released.
    pub(crate) const fn wgpu_work_is_terminal(&self) -> bool {
        self.wgpu_work_terminal
    }
}

/// Failure to create the dedicated progress execution domain.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ViewerGpuDeviceProgressStartError {
    /// Active plus retiring generations consumed the process-wide bound.
    #[error(
        "Viewer GPU device generation capacity is exhausted (maximum {capacity} active or retiring generations)"
    )]
    GenerationCapacityExhausted {
        /// Process-wide generation bound.
        capacity: usize,
    },
    /// Process-local generation identity space was exhausted.
    #[error("Viewer GPU device generation identity space is exhausted")]
    GenerationIdentityExhausted,
    /// The operating system rejected the worker thread.
    #[error("failed to start Viewer GPU device progress worker: {0}")]
    ThreadSpawn(#[source] std::io::Error),
}

#[derive(Clone)]
struct ViewerGpuDeviceGenerationAdmission {
    _token: Arc<ViewerGpuDeviceGenerationAdmissionToken>,
}

struct ViewerGpuDeviceGenerationAdmissionToken {
    active: &'static AtomicUsize,
}

impl ViewerGpuDeviceGenerationAdmission {
    fn reserve() -> Result<Self, ViewerGpuDeviceProgressStartError> {
        Self::reserve_from(
            &LIVE_VIEWER_GPU_DEVICE_GENERATIONS,
            MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS,
        )
        .ok_or(
            ViewerGpuDeviceProgressStartError::GenerationCapacityExhausted {
                capacity: MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS,
            },
        )
    }

    fn reserve_from(active: &'static AtomicUsize, capacity: usize) -> Option<Self> {
        try_reserve_generation_slot(active, capacity).then(|| Self {
            _token: Arc::new(ViewerGpuDeviceGenerationAdmissionToken { active }),
        })
    }
}

impl Drop for ViewerGpuDeviceGenerationAdmissionToken {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn try_reserve_generation_slot(active: &AtomicUsize, capacity: usize) -> bool {
    let mut current = active.load(Ordering::Acquire);
    loop {
        if current >= capacity {
            return false;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

/// Failure to reserve progress authority before GPU recording.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ViewerGpuDeviceProgressReserveError {
    /// Both bounded handoff/cleanup slots are retained.
    #[error("Viewer GPU device progress permits are backpressured")]
    Backpressured,
    /// A prior non-timeout failure terminalized this device generation.
    #[error(
        "Viewer GPU device generation is terminal after submission attempt {submission_id:?}: {reason}"
    )]
    GenerationTerminal {
        /// Reserved submission attempt that first failed.
        submission_id: Option<ViewerGpuSubmissionId>,
        /// Stable terminal reason.
        reason: String,
    },
    /// The owner has already begun explicit shutdown.
    #[error("Viewer GPU device progress owner is shut down")]
    OwnerShutdown,
}

/// Failure observed while explicitly joining the progress domain.
#[derive(Debug, thiserror::Error)]
#[cfg(test)]
pub(crate) enum ViewerGpuDeviceProgressShutdownError {
    /// The worker unwound outside per-wait panic isolation.
    #[error("Viewer GPU device progress worker panicked: {0}")]
    WorkerPanicked(String),
}

/// Bounded synchronous closure evidence for one Viewer GPU progress domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewerGpuDeviceProgressShutdownEvidence {
    /// Exact native-thread consuming join outcome, never an exit-channel hint.
    pub worker_shutdown: super::owned_worker_lifecycle::OwnedWorkerShutdown,
    /// Complete callback capture, invocation, destruction, and retirement inventory.
    pub wake_callbacks: super::preview_work_notification::PreviewWorkCallbackEvidence,
    /// Native event-loop delivery rejections observed by the concrete Adapter.
    pub native_wake_failures: u64,
    /// Unaccepted callback captures deliberately retained without running foreign Drop.
    pub wake_registration_rejections: u64,
    /// Whether this exact progress worker started.
    pub worker_started: bool,
    /// Whether its actual thread was joined within the deadline.
    pub worker_terminated: bool,
    /// Whether progress execution or its thread panicked.
    pub worker_panicked: bool,
    /// Whether the consuming shutdown deadline elapsed.
    pub timed_out: bool,
    /// Whether the caller requested generation retirement, not just command drain.
    pub retirement_requested: bool,
    /// Whether the complete retirement envelope reached the progress domain.
    pub retirement_handoff_accepted: bool,
    /// Whether the envelope proved safe release through all independent barriers.
    pub retirement_completed: bool,
    /// Present only after a completed retirement of a created Renderer runtime.
    pub renderer_retirement: Option<mondrian_renderer::ViewerGpuRetirementReceipt>,
    /// Terminal observed through the final bounded progress-worker join.
    pub generation_terminal_kind: Option<ViewerGpuDeviceGenerationTerminalKind>,
}

impl ViewerGpuDeviceProgressShutdownEvidence {
    /// Qualify a complete normal-runtime shutdown, never authorize resource release.
    ///
    /// A destroyed/lost device or joined upload panic can permit physical release
    /// while failing this stronger predicate. Partial startup has its own inventory
    /// and must not fabricate a Renderer receipt to satisfy normal qualification.
    #[cfg(any(test, feature = "validation"))]
    pub const fn qualifies_normal_runtime(self) -> bool {
        self.qualifies_created_inventory(true)
    }

    /// Check the explicitly observed startup inventory, never infer it from a
    /// missing retirement receipt or use this predicate to release live owners.
    pub(crate) const fn qualifies_created_inventory(self, renderer_created: bool) -> bool {
        self.worker_started
            && matches!(
                self.worker_shutdown,
                super::owned_worker_lifecycle::OwnedWorkerShutdown::Terminated
            )
            && self.wake_callbacks.all_resources_released()
            && self.native_wake_failures == 0
            && self.wake_registration_rejections == 0
            && self.worker_terminated
            && !self.worker_panicked
            && !self.timed_out
            && self.retirement_requested
            && self.retirement_handoff_accepted
            && self.retirement_completed
            && match self.renderer_retirement {
                Some(receipt) => renderer_created && receipt.is_healthy(),
                None => !renderer_created,
            }
            && self.generation_terminal_kind.is_none()
    }

    /// Unexpected device losses observed through final shutdown.
    #[cfg(any(test, feature = "validation"))]
    pub const fn device_loss_count(self) -> u64 {
        match self.generation_terminal_kind {
            Some(kind) => kind.device_loss_count(),
            None => 0,
        }
    }

    /// Non-loss terminal faults, including explicit destruction during qualification.
    #[cfg(any(test, feature = "validation"))]
    pub const fn fatal_error_count(self) -> u64 {
        match self.generation_terminal_kind {
            Some(kind) => kind.fatal_error_count(),
            None => 0,
        }
    }
}

/// Clone captured by the exact queue callback.
#[derive(Clone)]
pub(crate) struct ViewerGpuDeviceCompletionSignal {
    observed: Arc<AtomicBool>,
}

impl ViewerGpuDeviceCompletionSignal {
    /// Publish callback cleanup authority with release ordering.
    pub(crate) fn mark_observed(&self) {
        self.observed.store(true, Ordering::Release);
    }
}

/// Adapter-specific resources transferred to the device worker during teardown.
///
/// `poll_retirement` must remain non-blocking. It may observe native fences,
/// consume exact lifecycle callbacks, and release owners only after its own
/// physical contracts are satisfied. A receipt gives the worker authority to
/// drop the complete envelope after its independent whole-queue barrier.
pub(crate) trait ViewerGpuDeviceGenerationRetirement: Send + 'static {
    /// Stable diagnostic label for this retirement envelope.
    fn label(&self) -> &'static str;

    /// Observe whether every Adapter-specific owner is safe to release.
    fn poll_retirement(
        &mut self,
        terminal: Option<&ViewerGpuDeviceGenerationTerminal>,
    ) -> Option<ViewerGpuDeviceGenerationRetirementReceipt>;
}

/// Safe Adapter release with explicit created/not-created Renderer inventory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewerGpuDeviceGenerationRetirementReceipt {
    /// `None` means no runtime was constructed, never an unobserved worker exit.
    pub(crate) renderer: Option<mondrian_renderer::ViewerGpuRetirementReceipt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerGpuDeviceProgressExit {
    DrainedWithoutRetirement,
    Retired(ViewerGpuDeviceGenerationRetirementReceipt),
    RetainedAfterFailure,
    WorkerPanicked,
}

impl ViewerGpuDeviceProgressExit {
    fn permits_admission_release(self) -> bool {
        matches!(self, Self::DrainedWithoutRetirement | Self::Retired(_))
    }
}

/// Safely movable member of an Adapter's device-generation authority.
///
/// Window and Headless keep ordinary field-like access through `Deref`, while
/// their `Drop` implementations can take the value exactly once and transfer it
/// to the non-UI retirement envelope without `unsafe` moves.
pub(crate) struct ViewerGpuDeviceGenerationMember<T> {
    value: Option<T>,
}

impl<T> ViewerGpuDeviceGenerationMember<T> {
    /// Construct a temporary Adapter shell that has not yet received the
    /// existing device-generation authority during Window replacement.
    pub(crate) const fn empty() -> Self {
        Self { value: None }
    }

    /// Wrap one live generation member.
    pub(crate) const fn new(value: T) -> Self {
        Self { value: Some(value) }
    }

    /// Move the member into the retiring-generation envelope.
    pub(crate) fn take(&mut self) -> Option<T> {
        self.value.take()
    }
}

impl<T> Deref for ViewerGpuDeviceGenerationMember<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().expect("live Viewer GPU device-generation member")
    }
}

impl<T> DerefMut for ViewerGpuDeviceGenerationMember<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().expect("live Viewer GPU device-generation member")
    }
}

impl ViewerGpuDeviceGenerationMember<ViewerGpuDeviceProgressOwner> {
    /// Identity of the live concrete device generation, when installed.
    pub(crate) fn generation_id(&self) -> Option<ViewerGpuDeviceGenerationId> {
        self.value.as_ref().map(ViewerGpuDeviceProgressOwner::generation_id)
    }

    /// Progress terminal for the live owner.
    ///
    /// An empty replacement shell has no authoritative device generation yet,
    /// so it reports no terminal rather than dereferencing a missing owner.
    pub(crate) fn generation_terminal(&self) -> Option<ViewerGpuDeviceGenerationTerminal> {
        self.value.as_ref().and_then(|owner| owner.generation_terminal())
    }

    /// Consume one progress observation from the live owner.
    ///
    /// An empty replacement shell has no progress domain and yields none.
    pub(crate) fn try_observe(&self) -> Option<ViewerGpuDeviceProgressObservation> {
        self.value.as_ref().and_then(|owner| owner.try_observe())
    }

    /// Reserve progress authority from the live owner.
    ///
    /// An empty replacement shell behaves like a shut-down owner so callers
    /// take the explicit CPU-fallback path instead of dereferencing nothing.
    pub(crate) fn reserve_submission(
        &self,
    ) -> Result<ViewerGpuDeviceProgressPermit<'_>, ViewerGpuDeviceProgressReserveError> {
        match self.value.as_ref() {
            Some(owner) => owner.reserve_submission(),
            None => Err(ViewerGpuDeviceProgressReserveError::OwnerShutdown),
        }
    }
}

struct ViewerGpuDeviceGenerationState {
    terminal: Mutex<Option<ViewerGpuDeviceGenerationTerminal>>,
}

impl ViewerGpuDeviceGenerationState {
    fn new() -> Self {
        Self { terminal: Mutex::new(None) }
    }

    fn terminal(&self) -> Option<ViewerGpuDeviceGenerationTerminal> {
        let terminal = match self.terminal.lock() {
            Ok(terminal) => terminal,
            Err(poisoned) => poisoned.into_inner(),
        };
        terminal.clone()
    }

    fn mark_terminal(
        &self,
        submission_id: Option<ViewerGpuSubmissionId>,
        kind: ViewerGpuDeviceGenerationTerminalKind,
        reason: String,
        observed_at: Instant,
    ) -> ViewerGpuDeviceGenerationTerminal {
        let mut terminal = match self.terminal.lock() {
            Ok(terminal) => terminal,
            Err(poisoned) => poisoned.into_inner(),
        };
        let terminal = terminal.get_or_insert_with(|| ViewerGpuDeviceGenerationTerminal {
            submission_id,
            kind,
            wgpu_work_terminal: matches!(
                kind,
                ViewerGpuDeviceGenerationTerminalKind::DeviceLost
                    | ViewerGpuDeviceGenerationTerminalKind::DeviceDestroyed
            ),
            reason,
            observed_at,
        });
        if matches!(
            kind,
            ViewerGpuDeviceGenerationTerminalKind::DeviceLost
                | ViewerGpuDeviceGenerationTerminalKind::DeviceDestroyed
        ) {
            terminal.wgpu_work_terminal = true;
        }
        terminal.clone()
    }
}

/// Shared terminal and wake authority installed exactly once per wgpu device.
#[derive(Clone)]
pub(crate) struct ViewerGpuDeviceGenerationHealth {
    state: Arc<ViewerGpuDeviceGenerationState>,
    wake: ViewerGpuDeviceProgressWake,
}

impl ViewerGpuDeviceGenerationHealth {
    fn install(device: &wgpu::Device, wake: ViewerGpuDeviceProgressWake) -> Self {
        let health = Self {
            state: Arc::new(ViewerGpuDeviceGenerationState::new()),
            wake,
        };
        let callback_health = health.clone();
        device.set_device_lost_callback(move |reason, message| {
            let (kind, label) = classify_wgpu_device_loss(reason);
            let diagnostic = if message.is_empty() {
                format!("wgpu device {label}: {reason:?}")
            } else {
                format!("wgpu device {label} ({reason:?}): {message}")
            };
            callback_health.state.mark_terminal(None, kind, diagnostic, Instant::now());
            callback_health.wake.notify();
        });
        health
    }

    #[cfg(test)]
    fn for_test(wake: ViewerGpuDeviceProgressWake) -> Self {
        Self {
            state: Arc::new(ViewerGpuDeviceGenerationState::new()),
            wake,
        }
    }

    fn terminal(&self) -> Option<ViewerGpuDeviceGenerationTerminal> {
        if self.wake.has_failure() {
            self.mark_progress_failure(
                None,
                "Viewer GPU wake ownership or native event delivery failed".to_owned(),
                Instant::now(),
            );
        }
        self.state.terminal()
    }

    fn mark_progress_failure(
        &self,
        submission_id: Option<ViewerGpuSubmissionId>,
        reason: String,
        observed_at: Instant,
    ) -> ViewerGpuDeviceGenerationTerminal {
        self.state.mark_terminal(
            submission_id,
            ViewerGpuDeviceGenerationTerminalKind::ProgressFailure,
            reason,
            observed_at,
        )
    }

    #[cfg(test)]
    fn mark_device_lost(&self, reason: impl Into<String>) -> ViewerGpuDeviceGenerationTerminal {
        let terminal = self.state.mark_terminal(
            None,
            ViewerGpuDeviceGenerationTerminalKind::DeviceLost,
            reason.into(),
            Instant::now(),
        );
        self.wake.notify();
        terminal
    }
}

fn classify_wgpu_device_loss(
    reason: wgpu::DeviceLostReason,
) -> (ViewerGpuDeviceGenerationTerminalKind, &'static str) {
    match reason {
        wgpu::DeviceLostReason::Destroyed => (
            ViewerGpuDeviceGenerationTerminalKind::DeviceDestroyed,
            "destroyed",
        ),
        wgpu::DeviceLostReason::Unknown => {
            (ViewerGpuDeviceGenerationTerminalKind::DeviceLost, "lost")
        }
    }
}

struct ViewerGpuDeviceProgressState {
    active_slots: AtomicUsize,
}

impl ViewerGpuDeviceProgressState {
    fn new() -> Self {
        Self { active_slots: AtomicUsize::new(0) }
    }

    fn reserve_slot(
        self: &Arc<Self>,
        health: &ViewerGpuDeviceGenerationHealth,
    ) -> Result<ViewerGpuDeviceProgressSlot, ViewerGpuDeviceProgressReserveError> {
        loop {
            if let Some(terminal) = health.terminal() {
                return Err(ViewerGpuDeviceProgressReserveError::GenerationTerminal {
                    submission_id: terminal.submission_id,
                    reason: terminal.reason,
                });
            }
            let active = self.active_slots.load(Ordering::Acquire);
            if active >= VIEWER_GPU_PROGRESS_SLOT_CAPACITY {
                return Err(ViewerGpuDeviceProgressReserveError::Backpressured);
            }
            if self
                .active_slots
                .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let slot = ViewerGpuDeviceProgressSlot { state: Arc::clone(self) };
                if let Some(terminal) = health.terminal() {
                    drop(slot);
                    return Err(ViewerGpuDeviceProgressReserveError::GenerationTerminal {
                        submission_id: terminal.submission_id,
                        reason: terminal.reason,
                    });
                }
                return Ok(slot);
            }
        }
    }
}

struct ViewerGpuDeviceProgressSlot {
    state: Arc<ViewerGpuDeviceProgressState>,
}

impl Drop for ViewerGpuDeviceProgressSlot {
    fn drop(&mut self) {
        self.state.active_slots.fetch_sub(1, Ordering::AcqRel);
    }
}

enum ViewerGpuDeviceProgressCommand<I> {
    Track {
        submission_id: ViewerGpuSubmissionId,
        submission_index: I,
        callback_observed: Arc<AtomicBool>,
        _slot: ViewerGpuDeviceProgressSlot,
    },
    DriveRendererCleanup {
        attempt_id: ViewerGpuSubmissionId,
        _slot: ViewerGpuDeviceProgressSlot,
    },
    RetireDeviceGeneration {
        retirement: Box<dyn ViewerGpuDeviceGenerationRetirement>,
    },
}

struct ViewerGpuDeviceProgressWorker<I> {
    command_sender: Option<mpsc::Sender<ViewerGpuDeviceProgressCommand<I>>>,
    observation_receiver: mpsc::Receiver<ViewerGpuDeviceProgressObservation>,
    join_handle: Option<thread::JoinHandle<()>>,
    exit_receiver: mpsc::Receiver<ViewerGpuDeviceProgressExit>,
    wake: ViewerGpuDeviceProgressWake,
    health: ViewerGpuDeviceGenerationHealth,
    progress_state: Arc<ViewerGpuDeviceProgressState>,
    generation_admission: Option<ViewerGpuDeviceGenerationAdmission>,
}

struct ViewerGpuDeviceProgressWorkerPermit<'a, I> {
    sender: mpsc::Sender<ViewerGpuDeviceProgressCommand<I>>,
    health: ViewerGpuDeviceGenerationHealth,
    wake: ViewerGpuDeviceProgressWake,
    callback_observed: Arc<AtomicBool>,
    slot: ViewerGpuDeviceProgressSlot,
    _worker: PhantomData<&'a ViewerGpuDeviceProgressWorker<I>>,
}

impl<I> ViewerGpuDeviceProgressWorkerPermit<'_, I> {
    fn completion_signal(&self) -> ViewerGpuDeviceCompletionSignal {
        ViewerGpuDeviceCompletionSignal { observed: Arc::clone(&self.callback_observed) }
    }

    fn commit(self, submission_id: ViewerGpuSubmissionId, submission_index: I) {
        let Self {
            sender,
            health,
            wake,
            callback_observed,
            slot,
            _worker: _,
        } = self;
        let command = ViewerGpuDeviceProgressCommand::Track {
            submission_id,
            submission_index,
            callback_observed,
            _slot: slot,
        };
        send_progress_command(sender, health, wake, submission_id, command);
    }

    fn drive_renderer_cleanup(self, attempt_id: ViewerGpuSubmissionId) {
        let Self {
            sender,
            health,
            wake,
            callback_observed: _,
            slot,
            _worker: _,
        } = self;
        let command =
            ViewerGpuDeviceProgressCommand::DriveRendererCleanup { attempt_id, _slot: slot };
        send_progress_command(sender, health, wake, attempt_id, command);
    }
}

fn send_progress_command<I>(
    sender: mpsc::Sender<ViewerGpuDeviceProgressCommand<I>>,
    health: ViewerGpuDeviceGenerationHealth,
    wake: ViewerGpuDeviceProgressWake,
    attempt_id: ViewerGpuSubmissionId,
    command: ViewerGpuDeviceProgressCommand<I>,
) {
    if sender.send(command).is_err() {
        health.mark_progress_failure(
            Some(attempt_id),
            "Viewer GPU progress worker disconnected after progress admission".to_owned(),
            Instant::now(),
        );
        wake.notify();
    }
}

/// Move-only progress capacity acquired before fallible recording.
pub(crate) struct ViewerGpuDeviceProgressPermit<'a> {
    inner: ViewerGpuDeviceProgressWorkerPermit<'a, wgpu::SubmissionIndex>,
}

impl ViewerGpuDeviceProgressPermit<'_> {
    /// Signal to capture in the exact queue callback before submission.
    pub(crate) fn completion_signal(&self) -> ViewerGpuDeviceCompletionSignal {
        self.inner.completion_signal()
    }

    /// Commit the already-returned queue index without a recoverable error.
    pub(crate) fn commit(
        self,
        submission_id: ViewerGpuSubmissionId,
        submission_index: wgpu::SubmissionIndex,
    ) {
        self.inner.commit(submission_id, submission_index);
    }

    /// Convert a failed pre-submit attempt into typed renderer cleanup work.
    ///
    /// This path can drive renderer-internal queue work, but it owns no Viewer
    /// lifecycle completion or publication authority.
    pub(crate) fn drive_renderer_cleanup(self, attempt_id: ViewerGpuSubmissionId) {
        self.inner.drive_renderer_cleanup(attempt_id);
    }
}

/// Device-scoped owner shared by Window and Headless Viewer Adapters.
pub(crate) struct ViewerGpuDeviceProgressOwner {
    generation_id: ViewerGpuDeviceGenerationId,
    worker: ViewerGpuDeviceProgressWorker<wgpu::SubmissionIndex>,
}

impl ViewerGpuDeviceProgressOwner {
    /// Start the first progress domain and install this device generation's
    /// unique lost callback before any Viewer submission can be admitted.
    pub(crate) fn new(
        device: &wgpu::Device,
        wake: ViewerGpuDeviceProgressWake,
    ) -> Result<Self, ViewerGpuDeviceProgressStartError> {
        let generation_admission = ViewerGpuDeviceGenerationAdmission::reserve()?;
        let generation_id = ViewerGpuDeviceGenerationId::next()?;
        let health = ViewerGpuDeviceGenerationHealth::install(device, wake);
        let worker = ViewerGpuDeviceProgressWorker::spawn(
            "mondrian-viewer-gpu-progress",
            WgpuViewerGpuDeviceWait { device: device.clone() },
            ViewerGpuDeviceProgressPolicy::default(),
            health,
            Some(generation_admission),
        )?;
        Ok(Self { generation_id, worker })
    }

    /// Identity of the concrete wgpu device generation owned by this progress domain.
    pub(crate) const fn generation_id(&self) -> ViewerGpuDeviceGenerationId {
        self.generation_id
    }

    /// Register native wake only after the progress domain has a consuming owner.
    pub(crate) fn install_native_waker(&self, wake: impl Fn() -> bool + Send + Sync + 'static) {
        self.worker.wake.install_native(wake);
    }

    /// Install the Headless validation notification target.
    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn install_waker(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.worker.wake.install(wake);
    }

    /// Reserve progress authority before any fallible GPU recording.
    pub(crate) fn reserve_submission(
        &self,
    ) -> Result<ViewerGpuDeviceProgressPermit<'_>, ViewerGpuDeviceProgressReserveError> {
        self.worker
            .reserve_submission()
            .map(|inner| ViewerGpuDeviceProgressPermit { inner })
    }

    /// Non-blockingly consume one progress observation.
    pub(crate) fn try_observe(&self) -> Option<ViewerGpuDeviceProgressObservation> {
        self.worker.try_observe()
    }

    /// First typed terminal for this device generation.
    pub(crate) fn generation_terminal(&self) -> Option<ViewerGpuDeviceGenerationTerminal> {
        self.worker.health.terminal()
    }

    /// Stop admission and transfer the complete Adapter retirement envelope to
    /// the existing FIFO progress domain without waiting on the caller thread.
    pub(crate) fn retire_device_generation(
        mut self,
        retirement: impl ViewerGpuDeviceGenerationRetirement,
    ) {
        let _ = self.worker.enqueue_generation_retirement(Box::new(retirement));
    }

    /// Transfer the retirement envelope and wait against one caller-owned deadline.
    pub(crate) fn retire_device_generation_until(
        mut self,
        retirement: impl ViewerGpuDeviceGenerationRetirement,
        deadline: Instant,
    ) -> ViewerGpuDeviceProgressShutdownEvidence {
        let handoff = self.worker.enqueue_generation_retirement(Box::new(retirement));
        self.worker.shutdown_until(true, handoff, deadline)
    }
}

impl Drop for ViewerGpuDeviceProgressOwner {
    fn drop(&mut self) {
        // Closing the final sender lets the existing worker drain any already
        // accepted commands. Dropping the JoinHandle detaches that non-UI
        // worker; Adapter Drop never waits or cancels in-flight work.
        self.worker.detach_after_drain();
    }
}

impl<I> ViewerGpuDeviceProgressWorker<I>
where
    I: Send + 'static,
{
    fn spawn<D>(
        thread_name: &str,
        driver: D,
        policy: ViewerGpuDeviceProgressPolicy,
        health: ViewerGpuDeviceGenerationHealth,
        generation_admission: Option<ViewerGpuDeviceGenerationAdmission>,
    ) -> Result<Self, ViewerGpuDeviceProgressStartError>
    where
        D: ViewerGpuDeviceWait<I>,
    {
        let (command_sender, command_receiver) = mpsc::channel();
        let (observation_sender, observation_receiver) = mpsc::channel();
        let (exit_sender, exit_receiver) = mpsc::channel();
        let progress_state = Arc::new(ViewerGpuDeviceProgressState::new());
        let worker_wake = health.wake.clone();
        let worker_health = health.clone();
        let owner_generation_admission = generation_admission.clone();
        let join_handle = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                // Retain pre-reserved generation capacity through the final
                // retirement envelope; teardown never needs fresh admission.
                let panic_health = worker_health.clone();
                let worker_exit = catch_unwind(AssertUnwindSafe(|| {
                    run_viewer_gpu_device_progress_worker(
                        driver,
                        policy,
                        &command_receiver,
                        observation_sender,
                        worker_wake,
                        worker_health,
                    )
                }));
                let exit = match worker_exit {
                    Ok(exit) => exit,
                    Err(panic) => {
                        // The receiver owns any already accepted retirement
                        // envelope. Keep it outside the unwind boundary and
                        // quarantine it, including a racing final handoff.
                        std::mem::forget(command_receiver);
                        let reason = format!(
                            "Viewer GPU progress worker panicked outside its wait boundary: {}",
                            panic_payload_message(panic)
                        );
                        panic_health.mark_progress_failure(None, reason.clone(), Instant::now());
                        panic_health.wake.notify();
                        tracing::error!(%reason);
                        ViewerGpuDeviceProgressExit::WorkerPanicked
                    }
                };
                if !exit.permits_admission_release()
                    && let Some(admission) = generation_admission
                {
                    std::mem::forget(admission);
                }
                let _ = exit_sender.send(exit);
            })
            .map_err(ViewerGpuDeviceProgressStartError::ThreadSpawn)?;
        Ok(Self {
            command_sender: Some(command_sender),
            observation_receiver,
            join_handle: Some(join_handle),
            exit_receiver,
            wake: health.wake.clone(),
            health,
            progress_state,
            generation_admission: owner_generation_admission,
        })
    }

    fn reserve_submission(
        &self,
    ) -> Result<ViewerGpuDeviceProgressWorkerPermit<'_, I>, ViewerGpuDeviceProgressReserveError>
    {
        let Some(sender) = self.command_sender.as_ref() else {
            return Err(ViewerGpuDeviceProgressReserveError::OwnerShutdown);
        };
        let slot = self.progress_state.reserve_slot(&self.health)?;
        Ok(ViewerGpuDeviceProgressWorkerPermit {
            sender: sender.clone(),
            health: self.health.clone(),
            wake: self.wake.clone(),
            callback_observed: Arc::new(AtomicBool::new(false)),
            slot,
            _worker: PhantomData,
        })
    }

    fn try_observe(&self) -> Option<ViewerGpuDeviceProgressObservation> {
        self.observation_receiver.try_recv().ok()
    }

    #[cfg(test)]
    fn shutdown(&mut self) -> Result<(), ViewerGpuDeviceProgressShutdownError> {
        let Some(join_handle) = self.join_handle.take() else {
            return Ok(());
        };
        drop(self.command_sender.take());
        join_handle.join().map_err(|panic| {
            ViewerGpuDeviceProgressShutdownError::WorkerPanicked(panic_payload_message(panic))
        })
    }

    #[cfg(test)]
    fn shutdown_and_wait(
        &mut self,
        retirement_requested: bool,
        retirement_handoff_accepted: bool,
        timeout: Duration,
    ) -> ViewerGpuDeviceProgressShutdownEvidence {
        let deadline = Instant::now().checked_add(timeout).unwrap_or_else(Instant::now);
        self.shutdown_until(retirement_requested, retirement_handoff_accepted, deadline)
    }

    fn shutdown_until(
        &mut self,
        retirement_requested: bool,
        retirement_handoff_accepted: bool,
        deadline: Instant,
    ) -> ViewerGpuDeviceProgressShutdownEvidence {
        use super::owned_worker_lifecycle::OwnedWorkerShutdown;
        self.wake.watch.begin_shutdown();
        drop(self.command_sender.take());
        let (worker_started, worker_shutdown, exit) = match self.join_handle.take() {
            Some(worker) => {
                let exit = self
                    .exit_receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .ok();
                // Exit publication precedes final captured-value destruction.
                // Only the real join can prove termination within this deadline.
                (
                    true,
                    OwnedWorkerShutdown::join_until(worker, deadline),
                    exit,
                )
            }
            None => (false, OwnedWorkerShutdown::NotStarted, None),
        };
        let wake_callbacks = self.wake.watch.shutdown_until(deadline);
        let receipt = match exit {
            Some(ViewerGpuDeviceProgressExit::Retired(receipt)) => Some(receipt),
            _ => None,
        };
        let worker_terminated = matches!(
            worker_shutdown,
            OwnedWorkerShutdown::Terminated
                | OwnedWorkerShutdown::Panicked
                | OwnedWorkerShutdown::PanickedPayloadAbandoned
        );
        let worker_panicked =
            matches!(
                worker_shutdown,
                OwnedWorkerShutdown::Panicked | OwnedWorkerShutdown::PanickedPayloadAbandoned
            ) || matches!(exit, Some(ViewerGpuDeviceProgressExit::WorkerPanicked));
        ViewerGpuDeviceProgressShutdownEvidence {
            worker_shutdown,
            wake_callbacks,
            native_wake_failures: self.wake.native_failures.load(Ordering::Acquire),
            wake_registration_rejections: self.wake.registration_rejections.load(Ordering::Acquire),
            worker_started,
            worker_terminated,
            worker_panicked,
            timed_out: matches!(worker_shutdown, OwnedWorkerShutdown::TimedOutDetached)
                || !wake_callbacks.deadline_met,
            retirement_requested,
            retirement_handoff_accepted,
            retirement_completed: receipt.is_some(),
            renderer_retirement: receipt.and_then(|receipt| receipt.renderer),
            generation_terminal_kind: self.health.terminal().map(|terminal| terminal.kind),
        }
    }
    fn enqueue_generation_retirement(
        &mut self,
        retirement: Box<dyn ViewerGpuDeviceGenerationRetirement>,
    ) -> bool {
        let Some(sender) = self.command_sender.take() else {
            tracing::error!(
                label = retirement.label(),
                "Viewer GPU generation retirement lost its progress admission authority; retaining resources indefinitely"
            );
            self.quarantine_disconnected_retirement(retirement);
            return false;
        };
        let command = ViewerGpuDeviceProgressCommand::RetireDeviceGeneration { retirement };
        if let Err(error) = sender.send(command) {
            self.health.mark_progress_failure(
                None,
                "Viewer GPU progress worker disconnected before generation retirement handoff"
                    .to_owned(),
                Instant::now(),
            );
            tracing::error!(
                "Viewer GPU generation retirement worker disconnected; retaining resources indefinitely"
            );
            let ViewerGpuDeviceProgressCommand::RetireDeviceGeneration { retirement } = error.0
            else {
                unreachable!("retirement handoff sent a non-retirement command")
            };
            self.quarantine_disconnected_retirement(retirement);
            return false;
        }
        drop(sender);
        true
    }

    fn quarantine_disconnected_retirement(
        &mut self,
        retirement: Box<dyn ViewerGpuDeviceGenerationRetirement>,
    ) {
        // The worker-side admission is either still alive or was intentionally
        // quarantined by its panic boundary. Retain this owner-side clone with
        // the envelope as well, so a disconnected worker can never reopen
        // rebuild admission while resources remain unproved.
        if let Some(admission) = self.generation_admission.take() {
            std::mem::forget((retirement, admission));
        } else {
            std::mem::forget(retirement);
        }
    }

    fn detach_after_drain(&mut self) {
        drop(self.command_sender.take());
        drop(self.join_handle.take());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerGpuDeviceWaitStatus {
    TimedOut,
    Satisfied,
}

trait ViewerGpuDeviceWait<I>: Send + 'static {
    fn wait(
        &mut self,
        submission_index: Option<&I>,
        timeout: Duration,
    ) -> Result<ViewerGpuDeviceWaitStatus, String>;
}

struct WgpuViewerGpuDeviceWait {
    device: wgpu::Device,
}

impl ViewerGpuDeviceWait<wgpu::SubmissionIndex> for WgpuViewerGpuDeviceWait {
    fn wait(
        &mut self,
        submission_index: Option<&wgpu::SubmissionIndex>,
        timeout: Duration,
    ) -> Result<ViewerGpuDeviceWaitStatus, String> {
        match self.device.poll(wgpu::PollType::Wait {
            submission_index: submission_index.cloned(),
            timeout: Some(timeout),
        }) {
            Ok(wgpu::PollStatus::QueueEmpty | wgpu::PollStatus::WaitSucceeded) => {
                Ok(ViewerGpuDeviceWaitStatus::Satisfied)
            }
            Ok(wgpu::PollStatus::Poll) => {
                Err("wgpu returned Poll status for a bounded Wait request".to_owned())
            }
            Err(wgpu::PollError::Timeout) => Ok(ViewerGpuDeviceWaitStatus::TimedOut),
            Err(error) => Err(error.to_string()),
        }
    }
}

fn run_viewer_gpu_device_progress_worker<I, D>(
    mut driver: D,
    policy: ViewerGpuDeviceProgressPolicy,
    command_receiver: &mpsc::Receiver<ViewerGpuDeviceProgressCommand<I>>,
    observation_sender: mpsc::Sender<ViewerGpuDeviceProgressObservation>,
    wake: ViewerGpuDeviceProgressWake,
    health: ViewerGpuDeviceGenerationHealth,
) -> ViewerGpuDeviceProgressExit
where
    I: Send + 'static,
    D: ViewerGpuDeviceWait<I>,
{
    // Idle pacing: wgpu invokes `on_submitted_work_done` callbacks only from
    // `Device::poll`. If a submission was registered while the worker had no
    // pending command, the callback would otherwise stay staged forever and
    // its completion notice would never reach the submission lifecycle. Keep
    // polling on a bounded idle cadence so an already-completed submission
    // still releases through its authoritative callback.
    loop {
        let command = match command_receiver.recv_timeout(policy.wait_quantum) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = driver.wait(None, policy.wait_quantum);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            ViewerGpuDeviceProgressCommand::Track {
                submission_id,
                submission_index,
                callback_observed,
                _slot,
            } => drive_viewer_gpu_submission(
                &mut driver,
                policy,
                &observation_sender,
                &wake,
                &health,
                submission_id,
                submission_index,
                callback_observed,
            ),
            ViewerGpuDeviceProgressCommand::DriveRendererCleanup { attempt_id, _slot } => {
                drive_renderer_cleanup(
                    &mut driver,
                    policy,
                    &observation_sender,
                    &wake,
                    &health,
                    attempt_id,
                )
            }
            ViewerGpuDeviceProgressCommand::RetireDeviceGeneration { retirement } => {
                return drive_viewer_gpu_device_generation_retirement(
                    &mut driver,
                    policy,
                    &health,
                    retirement,
                );
            }
        }
    }
    ViewerGpuDeviceProgressExit::DrainedWithoutRetirement
}

#[allow(clippy::too_many_arguments)]
fn drive_viewer_gpu_submission<I, D>(
    driver: &mut D,
    policy: ViewerGpuDeviceProgressPolicy,
    observation_sender: &mpsc::Sender<ViewerGpuDeviceProgressObservation>,
    wake: &ViewerGpuDeviceProgressWake,
    health: &ViewerGpuDeviceGenerationHealth,
    submission_id: ViewerGpuSubmissionId,
    submission_index: I,
    callback_observed: Arc<AtomicBool>,
) where
    D: ViewerGpuDeviceWait<I>,
{
    let mut exact_wait = true;
    let mut terminal_reported = false;
    let mut fence_satisfied = false;

    loop {
        if let Some(terminal) = health.terminal() {
            if !terminal_reported {
                publish_device_failure(
                    observation_sender,
                    wake,
                    submission_id,
                    format_device_generation_terminal(&terminal),
                    terminal.observed_at,
                );
                terminal_reported = true;
            }
            if terminal.wgpu_work_is_terminal() {
                return;
            }
            exact_wait = false;
        }

        let wait_started = Instant::now();
        let callback_was_observed = callback_observed.load(Ordering::Acquire);
        let wait = catch_unwind(AssertUnwindSafe(|| {
            driver.wait(
                (!callback_was_observed && exact_wait).then_some(&submission_index),
                policy.wait_quantum,
            )
        }));
        match wait {
            Ok(Ok(ViewerGpuDeviceWaitStatus::TimedOut)) => {}
            Ok(Ok(ViewerGpuDeviceWaitStatus::Satisfied)) => {
                // The fence is the authoritative GPU completion evidence:
                // wgpu reports `WaitSucceeded` only after the exact submission
                // finished executing. `fence_satisfied` records that fact; the
                // post-poll barrier below publishes it, while a device-loss
                // terminal observed from the same poll still dominates.
                exact_wait = false;
                fence_satisfied = true;
            }
            Ok(Err(reason)) => {
                let terminal =
                    health.mark_progress_failure(Some(submission_id), reason, Instant::now());
                if !terminal_reported {
                    publish_device_failure(
                        observation_sender,
                        wake,
                        submission_id,
                        terminal.reason.clone(),
                        terminal.observed_at,
                    );
                    terminal_reported = true;
                }
                exact_wait = false;
            }
            Err(panic) => {
                let terminal = health.mark_progress_failure(
                    Some(submission_id),
                    format!(
                        "wgpu device progress panicked: {}",
                        panic_payload_message(panic)
                    ),
                    Instant::now(),
                );
                if !terminal_reported {
                    publish_device_failure(
                        observation_sender,
                        wake,
                        submission_id,
                        terminal.reason.clone(),
                        terminal.observed_at,
                    );
                    terminal_reported = true;
                }
                exact_wait = false;
            }
        }
        // wgpu may invoke the work-done callback and the device-lost callback
        // during the same `Device::poll`. The cleanup ticket is only evidence;
        // re-read the generation terminal after poll and let loss dominate.
        if let Some(terminal) = health.terminal() {
            if !terminal_reported {
                publish_device_failure(
                    observation_sender,
                    wake,
                    submission_id,
                    format_device_generation_terminal(&terminal),
                    terminal.observed_at,
                );
                terminal_reported = true;
            }
            if terminal.wgpu_work_is_terminal() {
                return;
            }
        }
        // Native wgpu normally invokes `on_submitted_work_done` while
        // `Device::poll` is on this stack. Observe that release before pacing;
        // otherwise every already-complete frame pays an artificial full
        // wait quantum and 60 fps playback loses almost half its frame budget.
        // The bounded wait's `WaitSucceeded` fence result is also authoritative
        // GPU completion evidence: publish it even when the supplementary
        // callback races or lags this poll (wgpu 30 defers callback delivery),
        // otherwise the completion notice stays stranded behind an unavailable
        // callback and the quarantine deadline revokes the retained output.
        if callback_observed.load(Ordering::Acquire) || fence_satisfied {
            // A progress failure still forbids publication, but this exact
            // post-poll callback lets the Adapter retire its quarantined
            // lifecycle owner. Actual device loss returned above instead.
            publish_wait_satisfied(observation_sender, wake, submission_id);
            return;
        }
        pace_bounded_wait(wait_started, policy.wait_quantum);
    }
}

fn publish_wait_satisfied(
    observation_sender: &mpsc::Sender<ViewerGpuDeviceProgressObservation>,
    wake: &ViewerGpuDeviceProgressWake,
    submission_id: ViewerGpuSubmissionId,
) {
    let _ = observation_sender.send(ViewerGpuDeviceProgressObservation::WaitSatisfied {
        submission_id,
        observed_at: Instant::now(),
    });
    wake.notify();
}

#[allow(clippy::too_many_arguments)]
fn drive_renderer_cleanup<I, D>(
    driver: &mut D,
    policy: ViewerGpuDeviceProgressPolicy,
    observation_sender: &mpsc::Sender<ViewerGpuDeviceProgressObservation>,
    wake: &ViewerGpuDeviceProgressWake,
    health: &ViewerGpuDeviceGenerationHealth,
    attempt_id: ViewerGpuSubmissionId,
) where
    D: ViewerGpuDeviceWait<I>,
{
    let mut terminal_reported = false;

    loop {
        if let Some(terminal) = health.terminal() {
            if !terminal_reported {
                publish_device_failure(
                    observation_sender,
                    wake,
                    attempt_id,
                    format_device_generation_terminal(&terminal),
                    terminal.observed_at,
                );
                terminal_reported = true;
            }
            if terminal.wgpu_work_is_terminal() {
                return;
            }
        }

        let wait_started = Instant::now();
        let wait = catch_unwind(AssertUnwindSafe(|| driver.wait(None, policy.wait_quantum)));
        // As above, loss reported from this poll outranks a simultaneous
        // satisfied return and therefore suppresses cleanup publication.
        if let Some(terminal) = health.terminal() {
            if !terminal_reported {
                publish_device_failure(
                    observation_sender,
                    wake,
                    attempt_id,
                    format_device_generation_terminal(&terminal),
                    terminal.observed_at,
                );
                terminal_reported = true;
            }
            if terminal.wgpu_work_is_terminal() {
                return;
            }
        }
        match wait {
            Ok(Ok(ViewerGpuDeviceWaitStatus::TimedOut)) => {}
            Ok(Ok(ViewerGpuDeviceWaitStatus::Satisfied)) => {
                let _ = observation_sender.send(
                    ViewerGpuDeviceProgressObservation::RendererCleanupSatisfied {
                        attempt_id,
                        observed_at: Instant::now(),
                    },
                );
                wake.notify();
                return;
            }
            Ok(Err(reason)) => {
                let terminal =
                    health.mark_progress_failure(Some(attempt_id), reason, Instant::now());
                if !terminal_reported {
                    publish_device_failure(
                        observation_sender,
                        wake,
                        attempt_id,
                        terminal.reason.clone(),
                        terminal.observed_at,
                    );
                    terminal_reported = true;
                }
            }
            Err(panic) => {
                let terminal = health.mark_progress_failure(
                    Some(attempt_id),
                    format!(
                        "wgpu renderer cleanup progress panicked: {}",
                        panic_payload_message(panic)
                    ),
                    Instant::now(),
                );
                if !terminal_reported {
                    publish_device_failure(
                        observation_sender,
                        wake,
                        attempt_id,
                        terminal.reason.clone(),
                        terminal.observed_at,
                    );
                    terminal_reported = true;
                }
            }
        }
        pace_bounded_wait(wait_started, policy.wait_quantum);
    }
}

fn drive_viewer_gpu_device_generation_retirement<I, D>(
    driver: &mut D,
    policy: ViewerGpuDeviceProgressPolicy,
    health: &ViewerGpuDeviceGenerationHealth,
    mut retirement: Box<dyn ViewerGpuDeviceGenerationRetirement>,
) -> ViewerGpuDeviceProgressExit
where
    D: ViewerGpuDeviceWait<I>,
{
    let mut wgpu_queue_quiesced = false;
    let mut receipt = None;
    loop {
        let terminal_before_wait = health.terminal();
        if terminal_before_wait
            .as_ref()
            .is_some_and(ViewerGpuDeviceGenerationTerminal::wgpu_work_is_terminal)
        {
            wgpu_queue_quiesced = true;
        }

        let wait_started = Instant::now();
        if !wgpu_queue_quiesced {
            let wait = catch_unwind(AssertUnwindSafe(|| driver.wait(None, policy.wait_quantum)));
            match wait {
                Ok(Ok(ViewerGpuDeviceWaitStatus::TimedOut)) => {}
                Ok(Ok(ViewerGpuDeviceWaitStatus::Satisfied)) => {
                    wgpu_queue_quiesced = true;
                }
                Ok(Err(reason)) => {
                    health.mark_progress_failure(None, reason, Instant::now());
                }
                Err(panic) => {
                    health.mark_progress_failure(
                        None,
                        format!(
                            "wgpu generation retirement progress panicked: {}",
                            panic_payload_message(panic)
                        ),
                        Instant::now(),
                    );
                }
            }
        }

        // A device-lost callback may be delivered by the wait above. Concrete
        // loss/destroy invalidates all wgpu work and therefore replaces a
        // successful QueueEmpty fence, but an ordinary progress failure does
        // not. The Adapter retirement envelope still owns independent native
        // copies and must prove those below.
        let terminal = health.terminal();
        if terminal
            .as_ref()
            .is_some_and(ViewerGpuDeviceGenerationTerminal::wgpu_work_is_terminal)
        {
            wgpu_queue_quiesced = true;
        }
        let retirement_ready = catch_unwind(AssertUnwindSafe(|| {
            receipt.or_else(|| retirement.poll_retirement(terminal.as_ref()))
        }));
        match retirement_ready {
            Ok(Some(terminal)) if wgpu_queue_quiesced => {
                return ViewerGpuDeviceProgressExit::Retired(terminal);
            }
            Ok(ready) => receipt = ready,
            Err(panic) => {
                tracing::error!(
                    label = retirement.label(),
                    reason = %panic_payload_message(panic),
                    "Viewer GPU generation retirement panicked; retaining resources indefinitely"
                );
                std::mem::forget(retirement);
                return ViewerGpuDeviceProgressExit::RetainedAfterFailure;
            }
        }
        pace_bounded_wait(wait_started, policy.wait_quantum);
    }
}

fn format_device_generation_terminal(terminal: &ViewerGpuDeviceGenerationTerminal) -> String {
    match terminal.submission_id {
        Some(submission_id) => format!(
            "device generation {:?} terminal after submission attempt {}: {}",
            terminal.kind,
            submission_id.get(),
            terminal.reason
        ),
        None => format!(
            "device generation {:?} terminal outside a Viewer submission: {}",
            terminal.kind, terminal.reason
        ),
    }
}

fn publish_device_failure(
    observation_sender: &mpsc::Sender<ViewerGpuDeviceProgressObservation>,
    wake: &ViewerGpuDeviceProgressWake,
    submission_id: ViewerGpuSubmissionId,
    reason: String,
    observed_at: Instant,
) {
    let _ = observation_sender.send(ViewerGpuDeviceProgressObservation::DevicePollFailed {
        submission_id,
        reason,
        observed_at,
    });
    wake.notify();
}

fn pace_bounded_wait(started_at: Instant, quantum: Duration) {
    let remaining = quantum.saturating_sub(started_at.elapsed());
    if !remaining.is_zero() {
        #[cfg(windows)]
        if wait_with_high_resolution_timer(remaining) {
            return;
        }
        thread::park_timeout(remaining);
    }
}

/// Wait for a short bounded interval without inheriting the coarse Windows
/// scheduler-tick quantization used by condition variables and thread parks.
///
/// This remains a pacing primitive, not a completion authority: callers must
/// re-check their level predicate after it returns. A process-local timer is
/// retained per thread so realtime GPU progress and Headless validation do not
/// create one kernel object per frame.
#[cfg(windows)]
pub(crate) fn wait_with_high_resolution_timer(duration: Duration) -> bool {
    use std::cell::RefCell;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CreateWaitableTimerExW, SetWaitableTimerEx, WaitForSingleObject,
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, INFINITE, TIMER_ALL_ACCESS,
    };

    struct Timer(HANDLE);

    impl Timer {
        fn new() -> Option<Self> {
            let handle = unsafe {
                CreateWaitableTimerExW(
                    ptr::null(),
                    ptr::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS,
                )
            };
            (!handle.is_null()).then_some(Self(handle))
        }

        fn wait(&self, duration: Duration) -> bool {
            let ticks_100ns = duration.as_nanos().div_ceil(100).max(1).min(i64::MAX as u128) as i64;
            let due_time = -ticks_100ns;
            if unsafe {
                SetWaitableTimerEx(self.0, &due_time, 0, None, ptr::null(), ptr::null(), 0)
            } == 0
            {
                return false;
            }
            unsafe { WaitForSingleObject(self.0, INFINITE) == WAIT_OBJECT_0 }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    thread_local! {
        static TIMER: RefCell<Option<Timer>> = RefCell::new(Timer::new());
    }

    TIMER.with(|timer| timer.borrow().as_ref().is_some_and(|timer| timer.wait(duration)))
}

fn panic_payload_message(panic: Box<dyn Any + Send + 'static>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        std::mem::forget(panic);
        "non-string panic payload deliberately retained".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::viewer_gpu_submission::{
        ViewerGpuSubmissionLifecycle, ViewerGpuSubmissionPoll,
    };
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    fn progress_shutdown_complete(evidence: ViewerGpuDeviceProgressShutdownEvidence) -> bool {
        evidence.worker_started
            && evidence.worker_terminated
            && !evidence.worker_panicked
            && !evidence.timed_out
            && (!evidence.retirement_requested
                || (evidence.retirement_handoff_accepted && evidence.retirement_completed))
            && evidence.renderer_retirement.is_none_or(|receipt| receipt.is_healthy())
    }

    #[test]
    fn empty_device_generation_member_reports_no_terminal_and_no_observations() {
        let member = ViewerGpuDeviceGenerationMember::<ViewerGpuDeviceProgressOwner>::empty();
        assert!(member.generation_terminal().is_none());
        assert!(member.try_observe().is_none());
        assert!(matches!(
            member.reserve_submission(),
            Err(ViewerGpuDeviceProgressReserveError::OwnerShutdown)
        ));
    }

    type WaitCallLog = Arc<Mutex<Vec<Option<u64>>>>;

    struct ScriptedWait {
        results: Arc<Mutex<VecDeque<Result<ViewerGpuDeviceWaitStatus, String>>>>,
        calls: WaitCallLog,
    }

    impl ViewerGpuDeviceWait<u64> for ScriptedWait {
        fn wait(
            &mut self,
            submission_index: Option<&u64>,
            _timeout: Duration,
        ) -> Result<ViewerGpuDeviceWaitStatus, String> {
            self.calls.lock().expect("wait call log").push(submission_index.copied());
            self.results
                .lock()
                .expect("scripted wait results")
                .pop_front()
                .unwrap_or(Ok(ViewerGpuDeviceWaitStatus::TimedOut))
        }
    }

    struct CallbackDuringWait {
        callback_observed: Arc<AtomicBool>,
    }

    impl ViewerGpuDeviceWait<u64> for CallbackDuringWait {
        fn wait(
            &mut self,
            _submission_index: Option<&u64>,
            _timeout: Duration,
        ) -> Result<ViewerGpuDeviceWaitStatus, String> {
            self.callback_observed.store(true, Ordering::Release);
            Ok(ViewerGpuDeviceWaitStatus::Satisfied)
        }
    }

    struct CallbackAndDeviceLossDuringWait {
        callback_observed: Arc<AtomicBool>,
        health: ViewerGpuDeviceGenerationHealth,
    }

    impl ViewerGpuDeviceWait<u64> for CallbackAndDeviceLossDuringWait {
        fn wait(
            &mut self,
            _submission_index: Option<&u64>,
            _timeout: Duration,
        ) -> Result<ViewerGpuDeviceWaitStatus, String> {
            // This is the counterexample ordering exposed by wgpu: work-done
            // may run earlier than device-lost in one native poll closure set.
            self.callback_observed.store(true, Ordering::Release);
            self.health.mark_device_lost("device removed in the same poll");
            Ok(ViewerGpuDeviceWaitStatus::Satisfied)
        }
    }

    struct TestRetirement {
        safe_to_release: Arc<AtomicBool>,
        polls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
        require_device_lost: bool,
    }

    impl ViewerGpuDeviceGenerationRetirement for TestRetirement {
        fn label(&self) -> &'static str {
            "test Viewer GPU retirement"
        }

        fn poll_retirement(
            &mut self,
            terminal: Option<&ViewerGpuDeviceGenerationTerminal>,
        ) -> Option<ViewerGpuDeviceGenerationRetirementReceipt> {
            self.polls.fetch_add(1, Ordering::Relaxed);
            let terminal_ready = !self.require_device_lost
                || terminal.is_some_and(ViewerGpuDeviceGenerationTerminal::wgpu_work_is_terminal);
            (terminal_ready && self.safe_to_release.load(Ordering::Acquire))
                .then_some(ViewerGpuDeviceGenerationRetirementReceipt { renderer: None })
        }
    }

    impl Drop for TestRetirement {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Release);
        }
    }

    fn scripted_worker(
        results: impl IntoIterator<Item = Result<ViewerGpuDeviceWaitStatus, String>>,
    ) -> (ViewerGpuDeviceProgressWorker<u64>, WaitCallLog) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let health =
            ViewerGpuDeviceGenerationHealth::for_test(ViewerGpuDeviceProgressWake::default());
        let worker = ViewerGpuDeviceProgressWorker::spawn(
            "mondrian-viewer-gpu-progress-test",
            ScriptedWait {
                results: Arc::new(Mutex::new(results.into_iter().collect())),
                calls: Arc::clone(&calls),
            },
            ViewerGpuDeviceProgressPolicy::new(Duration::from_millis(1)).expect("valid policy"),
            health,
            None,
        )
        .expect("spawn progress worker");
        (worker, calls)
    }

    fn test_submission_id(value: u64) -> ViewerGpuSubmissionId {
        ViewerGpuSubmissionId::for_test(value)
    }

    fn wait_for_calls(calls: &WaitCallLog, minimum: usize) {
        let started = Instant::now();
        while calls.lock().expect("wait call log").len() < minimum
            && started.elapsed() < Duration::from_secs(1)
        {
            thread::yield_now();
        }
        assert!(calls.lock().expect("wait call log").len() >= minimum);
    }

    fn wait_for_atomic(counter: &AtomicUsize, minimum: usize) {
        let started = Instant::now();
        while counter.load(Ordering::Acquire) < minimum
            && started.elapsed() < Duration::from_secs(1)
        {
            thread::yield_now();
        }
        assert!(counter.load(Ordering::Acquire) >= minimum);
    }

    #[test]
    fn zero_wait_quantum_is_rejected() {
        assert_eq!(
            ViewerGpuDeviceProgressPolicy::new(Duration::ZERO),
            Err(ViewerGpuDeviceProgressPolicyError::ZeroWaitQuantum)
        );
    }

    #[test]
    fn device_generation_identities_are_nonzero_and_strictly_monotonic() {
        let first = ViewerGpuDeviceGenerationId::next().expect("first generation identity");
        let second = ViewerGpuDeviceGenerationId::next().expect("second generation identity");

        assert_ne!(first.get(), 0);
        assert_eq!(first.get().checked_add(1), Some(second.get()));
    }

    #[test]
    fn permit_precedes_submit_and_commits_the_exact_index() {
        let (mut worker, calls) = scripted_worker([Ok(ViewerGpuDeviceWaitStatus::TimedOut)]);
        let permit = worker.reserve_submission().expect("reserve progress");
        let completion = permit.completion_signal();
        permit.commit(test_submission_id(7), 41);
        wait_for_calls(&calls, 1);
        completion.mark_observed();
        let observation = worker
            .observation_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("completion observation");
        assert!(matches!(
            observation,
            ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id,
                ..
            } if submission_id == test_submission_id(7)
        ));
        assert_eq!(
            calls.lock().expect("wait call log").first().copied(),
            Some(Some(41))
        );
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn callback_before_worker_progress_still_gets_a_post_callback_device_barrier() {
        let (mut worker, calls) = scripted_worker([Ok(ViewerGpuDeviceWaitStatus::Satisfied)]);
        let permit = worker.reserve_submission().expect("reserve progress");
        let completion = permit.completion_signal();
        completion.mark_observed();
        permit.commit(test_submission_id(8), 43);
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("callback-first observation"),
            ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id,
                ..
            } if submission_id == test_submission_id(8)
        ));
        assert_eq!(
            calls.lock().expect("wait call log").first().copied(),
            Some(None)
        );
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn callback_driven_by_wait_does_not_pay_an_extra_wait_quantum() {
        let callback_observed = Arc::new(AtomicBool::new(false));
        let (observation_sender, observation_receiver) = mpsc::channel();
        let wake = ViewerGpuDeviceProgressWake::default();
        let health = ViewerGpuDeviceGenerationHealth::for_test(wake.clone());
        let wait_quantum = Duration::from_millis(200);
        let started_at = Instant::now();

        drive_viewer_gpu_submission(
            &mut CallbackDuringWait { callback_observed: Arc::clone(&callback_observed) },
            ViewerGpuDeviceProgressPolicy::new(wait_quantum).expect("valid policy"),
            &observation_sender,
            &wake,
            &health,
            test_submission_id(82),
            47,
            callback_observed,
        );

        assert!(
            started_at.elapsed() < wait_quantum / 2,
            "an already-observed callback must not be delayed by pacing"
        );
        assert!(matches!(
            observation_receiver.try_recv(),
            Ok(ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id,
                ..
            }) if submission_id == test_submission_id(82)
        ));
    }

    #[test]
    fn device_loss_from_same_poll_dominates_work_done_callback() {
        let callback_observed = Arc::new(AtomicBool::new(false));
        let (observation_sender, observation_receiver) = mpsc::channel();
        let wake = ViewerGpuDeviceProgressWake::default();
        let health = ViewerGpuDeviceGenerationHealth::for_test(wake.clone());

        drive_viewer_gpu_submission(
            &mut CallbackAndDeviceLossDuringWait {
                callback_observed: Arc::clone(&callback_observed),
                health: health.clone(),
            },
            ViewerGpuDeviceProgressPolicy::new(Duration::from_millis(1)).expect("valid policy"),
            &observation_sender,
            &wake,
            &health,
            test_submission_id(83),
            49,
            callback_observed,
        );

        assert!(matches!(
            observation_receiver.try_recv(),
            Ok(ViewerGpuDeviceProgressObservation::DevicePollFailed {
                submission_id,
                ..
            }) if submission_id == test_submission_id(83)
        ));
        assert!(observation_receiver.try_recv().is_err());
        assert!(health.terminal().is_some_and(|terminal| terminal.wgpu_work_is_terminal()));
    }

    #[test]
    fn device_loss_without_submission_rejects_future_admission() {
        let (mut worker, _) = scripted_worker([]);
        worker.health.mark_device_lost("idle generation lost");
        assert!(matches!(
            worker.reserve_submission(),
            Err(ViewerGpuDeviceProgressReserveError::GenerationTerminal {
                submission_id: None,
                ..
            })
        ));
        worker.shutdown().expect("shutdown idle worker");
    }

    #[test]
    fn explicit_destroy_is_distinct_from_unexpected_device_loss() {
        assert_eq!(
            classify_wgpu_device_loss(wgpu::DeviceLostReason::Destroyed),
            (
                ViewerGpuDeviceGenerationTerminalKind::DeviceDestroyed,
                "destroyed"
            )
        );
        assert_eq!(
            classify_wgpu_device_loss(wgpu::DeviceLostReason::Unknown),
            (ViewerGpuDeviceGenerationTerminalKind::DeviceLost, "lost")
        );
    }

    #[test]
    fn later_device_loss_strengthens_progress_failure_without_rewriting_first_cause() {
        let health =
            ViewerGpuDeviceGenerationHealth::for_test(ViewerGpuDeviceProgressWake::default());
        let first_observed_at = Instant::now();
        health.mark_progress_failure(
            Some(test_submission_id(84)),
            "progress proof failed".to_owned(),
            first_observed_at,
        );
        health.mark_device_lost("device removed later");

        let terminal = health.terminal().expect("terminal generation");
        assert_eq!(terminal.submission_id, Some(test_submission_id(84)));
        assert_eq!(
            terminal.kind,
            ViewerGpuDeviceGenerationTerminalKind::ProgressFailure
        );
        assert_eq!(terminal.reason, "progress proof failed");
        assert_eq!(terminal.observed_at, first_observed_at);
        assert!(terminal.wgpu_work_is_terminal());
    }

    #[test]
    fn pre_submit_renderer_cleanup_has_a_typed_latest_work_barrier() {
        let (mut worker, calls) = scripted_worker([Ok(ViewerGpuDeviceWaitStatus::Satisfied)]);
        let permit = worker.reserve_submission().expect("reserve progress");
        permit.drive_renderer_cleanup(test_submission_id(81));
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("renderer cleanup observation"),
            ViewerGpuDeviceProgressObservation::RendererCleanupSatisfied {
                attempt_id,
                ..
            } if attempt_id == test_submission_id(81)
        ));
        assert_eq!(
            calls.lock().expect("wait call log").first().copied(),
            Some(None)
        );
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn wait_success_does_not_replace_the_exact_callback() {
        type CompletionCallback = Box<dyn FnOnce(u64) + Send + 'static>;

        let now = Instant::now();
        let callback = Arc::new(Mutex::new(None::<CompletionCallback>));
        let mut lifecycle = ViewerGpuSubmissionLifecycle::<String, u64>::new();
        let reservation = lifecycle.reserve().expect("reserve lifecycle");
        let submission_id = reservation.submission_id();
        let (mut worker, calls) = scripted_worker([Ok(ViewerGpuDeviceWaitStatus::Satisfied)]);
        let permit = worker.reserve_submission().expect("reserve progress");
        let completion = permit.completion_signal();
        let callback_slot = Arc::clone(&callback);
        reservation.commit(
            "retained-owner".to_owned(),
            now + Duration::from_secs(1),
            move |registered| {
                *callback_slot.lock().expect("callback slot") = Some(registered);
            },
            move || completion.mark_observed(),
        );
        permit.commit(submission_id, 47);
        wait_for_calls(&calls, 1);
        assert!(matches!(
            lifecycle.poll(now),
            ViewerGpuSubmissionPoll::Pending {
                submission_id: pending,
                quarantined: false,
            } if pending == submission_id
        ));
        callback.lock().expect("callback slot").take().expect("registered callback")(99);
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("callback observation"),
            ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id: observed,
                ..
            } if observed == submission_id
        ));
        assert!(matches!(
            lifecycle.poll(now),
            ViewerGpuSubmissionPoll::Completed(completed)
                if completed.submission_id == submission_id
                    && completed.completion == 99
        ));
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn non_timeout_failure_terminalizes_generation_and_rejects_new_permits() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed_wakes = Arc::clone(&wakes);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let health = ViewerGpuDeviceGenerationHealth::for_test(ViewerGpuDeviceProgressWake::new(
            move || {
                observed_wakes.fetch_add(1, Ordering::Relaxed);
            },
        ));
        let mut worker = ViewerGpuDeviceProgressWorker::spawn(
            "mondrian-viewer-gpu-progress-failure-test",
            ScriptedWait {
                results: Arc::new(Mutex::new(VecDeque::from([Err(
                    "wrong submission index".to_owned()
                )]))),
                calls,
            },
            ViewerGpuDeviceProgressPolicy::new(Duration::from_millis(1)).expect("valid policy"),
            health,
            None,
        )
        .expect("spawn progress worker");
        let permit = worker.reserve_submission().expect("reserve progress");
        let completion = permit.completion_signal();
        permit.commit(test_submission_id(9), 53);
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("failure observation"),
            ViewerGpuDeviceProgressObservation::DevicePollFailed {
                submission_id,
                ..
            } if submission_id == test_submission_id(9)
        ));
        assert!(matches!(
            worker.reserve_submission(),
            Err(ViewerGpuDeviceProgressReserveError::GenerationTerminal {
                submission_id,
                ..
            }) if submission_id == Some(test_submission_id(9))
        ));
        completion.mark_observed();
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("quarantined completion observation"),
            ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id,
                ..
            } if submission_id == test_submission_id(9)
        ));
        let wake_deadline = Instant::now() + Duration::from_secs(1);
        while wakes.load(Ordering::Relaxed) < 3 && Instant::now() < wake_deadline {
            thread::yield_now();
        }
        assert_eq!(wakes.load(Ordering::Relaxed), 3);
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn queued_attempt_receives_generation_failure_under_its_own_identity() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let health =
            ViewerGpuDeviceGenerationHealth::for_test(ViewerGpuDeviceProgressWake::default());
        let mut worker = ViewerGpuDeviceProgressWorker::spawn(
            "mondrian-viewer-gpu-progress-stale-failure-test",
            ScriptedWait {
                results: Arc::new(Mutex::new(VecDeque::from([Err(
                    "device removed".to_owned()
                )]))),
                calls,
            },
            ViewerGpuDeviceProgressPolicy::new(Duration::from_millis(1)).expect("valid policy"),
            health,
            None,
        )
        .expect("spawn progress worker");
        let first = worker.reserve_submission().expect("first permit");
        let first_completion = first.completion_signal();
        let queued = worker.reserve_submission().expect("queued permit");
        let queued_completion = queued.completion_signal();
        first.commit(test_submission_id(91), 67);
        queued.commit(test_submission_id(92), 71);

        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("origin failure"),
            ViewerGpuDeviceProgressObservation::DevicePollFailed {
                submission_id,
                ..
            } if submission_id == test_submission_id(91)
        ));
        first_completion.mark_observed();
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("first quarantined completion"),
            ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id,
                ..
            } if submission_id == test_submission_id(91)
        ));
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("queued generation failure"),
            ViewerGpuDeviceProgressObservation::DevicePollFailed {
                submission_id,
                ..
            } if submission_id == test_submission_id(92)
        ));
        queued_completion.mark_observed();
        assert!(matches!(
            worker
                .observation_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("queued quarantined completion"),
            ViewerGpuDeviceProgressObservation::WaitSatisfied {
                submission_id,
                ..
            } if submission_id == test_submission_id(92)
        ));
        assert_eq!(
            worker.health.terminal().and_then(|terminal| terminal.submission_id),
            Some(test_submission_id(91))
        );
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn progress_capacity_is_reserved_before_submit() {
        let (mut worker, _) = scripted_worker([]);
        let permits = (0..VIEWER_GPU_PROGRESS_SLOT_CAPACITY)
            .map(|_| worker.reserve_submission().expect("bounded permit"))
            .collect::<Vec<_>>();
        assert!(matches!(
            worker.reserve_submission(),
            Err(ViewerGpuDeviceProgressReserveError::Backpressured)
        ));
        drop(permits);
        assert!(worker.reserve_submission().is_ok());
        worker.shutdown().expect("shutdown worker");
    }

    #[test]
    fn teardown_handoff_is_non_blocking_and_retains_owner_until_reaped() {
        let (mut worker, calls) = scripted_worker([
            Ok(ViewerGpuDeviceWaitStatus::TimedOut),
            Ok(ViewerGpuDeviceWaitStatus::Satisfied),
            Ok(ViewerGpuDeviceWaitStatus::Satisfied),
        ]);
        let first = worker.reserve_submission().expect("first permit");
        let first_completion = first.completion_signal();
        first.commit(test_submission_id(10), 59);
        wait_for_calls(&calls, 1);
        let safe_to_release = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let handoff_started = Instant::now();
        let handoff = worker.enqueue_generation_retirement(Box::new(TestRetirement {
            safe_to_release: Arc::clone(&safe_to_release),
            polls: Arc::clone(&polls),
            drops: Arc::clone(&drops),
            require_device_lost: false,
        }));
        assert!(
            handoff_started.elapsed() < Duration::from_millis(100),
            "teardown handoff must not wait for GPU completion"
        );
        assert_eq!(drops.load(Ordering::Acquire), 0);

        first_completion.mark_observed();
        wait_for_atomic(&polls, 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        safe_to_release.store(true, Ordering::Release);
        let evidence = worker.shutdown_and_wait(true, handoff, Duration::from_secs(2));
        assert!(progress_shutdown_complete(evidence));
        assert!(evidence.retirement_completed);
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert_eq!(
            worker.progress_state.active_slots.load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn clean_retirement_requires_a_satisfied_whole_queue_wait() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let results = Arc::new(Mutex::new(VecDeque::from([
            Ok(ViewerGpuDeviceWaitStatus::TimedOut),
            Ok(ViewerGpuDeviceWaitStatus::Satisfied),
        ])));
        let health =
            ViewerGpuDeviceGenerationHealth::for_test(ViewerGpuDeviceProgressWake::default());
        let polls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));

        let completed = drive_viewer_gpu_device_generation_retirement(
            &mut ScriptedWait { results, calls: Arc::clone(&calls) },
            ViewerGpuDeviceProgressPolicy::new(Duration::from_millis(1)).expect("valid policy"),
            &health,
            Box::new(TestRetirement {
                safe_to_release: Arc::new(AtomicBool::new(true)),
                polls: Arc::clone(&polls),
                drops: Arc::clone(&drops),
                require_device_lost: false,
            }),
        );

        assert_eq!(
            completed,
            ViewerGpuDeviceProgressExit::Retired(ViewerGpuDeviceGenerationRetirementReceipt {
                renderer: None
            })
        );
        assert_eq!(
            calls.lock().expect("wait call log").as_slice(),
            &[None, None]
        );
        assert_eq!(polls.load(Ordering::Acquire), 1);
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn wgpu_loss_does_not_replace_independent_native_copy_readiness() {
        let (mut worker, _) = scripted_worker([]);
        worker.health.mark_device_lost("wgpu device removed");
        let native_copy_ready = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let handoff = worker.enqueue_generation_retirement(Box::new(TestRetirement {
            safe_to_release: Arc::clone(&native_copy_ready),
            polls: Arc::clone(&polls),
            drops: Arc::clone(&drops),
            require_device_lost: true,
        }));
        wait_for_atomic(&polls, 1);
        assert_eq!(drops.load(Ordering::Acquire), 0);

        native_copy_ready.store(true, Ordering::Release);
        let evidence = worker.shutdown_and_wait(true, handoff, Duration::from_secs(2));
        assert!(progress_shutdown_complete(evidence));
        assert_eq!(
            evidence.generation_terminal_kind,
            Some(ViewerGpuDeviceGenerationTerminalKind::DeviceLost)
        );
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }

    #[test]
    fn bounded_shutdown_times_out_without_claiming_retirement() {
        let (mut worker, _) = scripted_worker([]);
        let safe_to_release = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(AtomicUsize::new(0));
        let handoff = worker.enqueue_generation_retirement(Box::new(TestRetirement {
            safe_to_release: Arc::clone(&safe_to_release),
            polls: Arc::new(AtomicUsize::new(0)),
            drops: Arc::clone(&drops),
            require_device_lost: false,
        }));

        let evidence =
            worker.shutdown_until(true, handoff, Instant::now() + Duration::from_millis(10));

        assert!(evidence.timed_out);
        assert!(!evidence.worker_terminated);
        assert!(!evidence.retirement_completed);
        assert!(!progress_shutdown_complete(evidence));
        safe_to_release.store(true, Ordering::Release);
        // The caller abandoned its bounded join before the generation ever
        // produced whole-queue completion evidence. Later Adapter readiness
        // alone must not release the quarantined generation.
        thread::yield_now();
        assert_eq!(drops.load(Ordering::Acquire), 0);
    }

    #[test]
    fn device_generation_admission_has_a_hard_upper_bound() {
        let active = AtomicUsize::new(0);
        for _ in 0..MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS {
            assert!(try_reserve_generation_slot(
                &active,
                MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS
            ));
        }
        assert!(!try_reserve_generation_slot(
            &active,
            MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS
        ));
        assert_eq!(
            active.load(Ordering::Acquire),
            MAX_LIVE_VIEWER_GPU_DEVICE_GENERATIONS
        );
    }

    #[test]
    fn disconnected_worker_quarantines_envelope_and_keeps_rebuild_slot_occupied() {
        let active = Box::leak(Box::new(AtomicUsize::new(0)));
        let admission = ViewerGpuDeviceGenerationAdmission::reserve_from(active, 1)
            .expect("reserve isolated generation slot");
        let wake = ViewerGpuDeviceProgressWake::default();
        let health = ViewerGpuDeviceGenerationHealth::for_test(wake.clone());
        let (command_sender, command_receiver) = mpsc::channel();
        drop(command_receiver);
        let (_observation_sender, observation_receiver) = mpsc::channel();
        let (_exit_sender, exit_receiver) = mpsc::channel();
        let mut worker = ViewerGpuDeviceProgressWorker::<u64> {
            command_sender: Some(command_sender),
            observation_receiver,
            join_handle: None,
            exit_receiver,
            wake,
            health,
            progress_state: Arc::new(ViewerGpuDeviceProgressState::new()),
            generation_admission: Some(admission),
        };
        let drops = Arc::new(AtomicUsize::new(0));
        worker.enqueue_generation_retirement(Box::new(TestRetirement {
            safe_to_release: Arc::new(AtomicBool::new(true)),
            polls: Arc::new(AtomicUsize::new(0)),
            drops: Arc::clone(&drops),
            require_device_lost: false,
        }));
        drop(worker);

        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(active.load(Ordering::Acquire), 1);
        assert!(ViewerGpuDeviceGenerationAdmission::reserve_from(active, 1).is_none());
    }

    #[test]
    fn wake_panics_are_isolated() {
        let wake = ViewerGpuDeviceProgressWake::new(|| panic!("wake panic"));
        wake.notify();
        wake.install(|| {});
        wake.notify();
        let receipt = wake.watch.shutdown_until(Instant::now() + Duration::from_secs(2));
        assert!(!receipt.all_resources_released());
        assert_eq!(receipt.invocation_panics, 1);
    }

    #[test]
    fn native_wake_rejection_terminalizes_the_generation() {
        let wake = ViewerGpuDeviceProgressWake::native(|| false);
        let health = ViewerGpuDeviceGenerationHealth::for_test(wake.clone());
        assert!(matches!(
            health.terminal().map(|terminal| terminal.kind),
            Some(ViewerGpuDeviceGenerationTerminalKind::ProgressFailure)
        ));
        assert_eq!(wake.native_failures.load(Ordering::Acquire), 1);
        assert!(wake
            .watch
            .shutdown_until(Instant::now() + Duration::from_secs(2))
            .all_resources_released());
    }

    #[test]
    fn opaque_wake_panic_payload_never_drops_on_the_producer() {
        struct Opaque(Arc<AtomicUsize>);
        impl Drop for Opaque {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let captured = Arc::clone(&drops);
        let wake = ViewerGpuDeviceProgressWake::new(move || {
            std::panic::panic_any(Opaque(Arc::clone(&captured)))
        });
        wake.notify();
        let receipt = wake.watch.shutdown_until(Instant::now() + Duration::from_secs(2));
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(receipt.opaque_payloads_abandoned, 1);
        assert_eq!(receipt.invocation_panics, 1);
        assert!(!receipt.all_resources_released());
    }

    #[test]
    fn wake_capture_destruction_is_owned_off_the_shutdown_thread() {
        struct Capture(mpsc::Sender<thread::ThreadId>);
        impl Drop for Capture {
            fn drop(&mut self) {
                let _ = self.0.send(thread::current().id());
            }
        }
        let (returned, observed) = mpsc::channel();
        let capture = Capture(returned);
        let wake = ViewerGpuDeviceProgressWake::new(move || {
            let _ = &capture;
        });
        let receipt = wake.watch.shutdown_until(Instant::now() + Duration::from_secs(2));
        assert!(receipt.all_resources_released(), "{receipt:?}");
        assert_ne!(
            observed.recv_timeout(Duration::from_secs(1)).expect("capture retired"),
            thread::current().id()
        );
        assert_eq!(receipt.registrations_accepted, 1);
        assert_eq!(receipt.registrations_released, 1);
    }

    #[test]
    fn progress_exit_hint_does_not_extend_the_native_join_deadline() {
        use super::super::owned_worker_lifecycle::OwnedWorkerShutdown;
        let (mut worker, _) = scripted_worker([]);
        worker.shutdown().expect("close scripted worker");
        let (exited, exit_receiver) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (finished, finish_receiver) = mpsc::channel();
        worker.exit_receiver = exit_receiver;
        worker.join_handle = Some(thread::spawn(move || {
            exited
                .send(ViewerGpuDeviceProgressExit::DrainedWithoutRetirement)
                .expect("exit hint");
            let _ = released.recv();
            let _ = finished.send(());
        }));
        let start = Instant::now();
        let receipt = worker.shutdown_until(false, false, start + Duration::from_millis(20));
        // Release before assertions so a test failure cannot strand the worker.
        let _ = release.send(());
        finish_receiver.recv_timeout(Duration::from_secs(2)).expect("tail released");
        assert_eq!(
            receipt.worker_shutdown,
            OwnedWorkerShutdown::TimedOutDetached
        );
        assert!(!receipt.worker_terminated);
        assert!(receipt.timed_out);
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn timed_out_gpu_wake_capture_retains_its_first_dirty_receipt() {
        struct Capture {
            started: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            returned: mpsc::Sender<()>,
        }
        impl Drop for Capture {
            fn drop(&mut self) {
                let _ = self.started.send(());
                let _ = self.release.get_mut().expect("capture gate").recv();
                let _ = self.returned.send(());
            }
        }
        let (started, observe_start) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (returned, observe_return) = mpsc::channel();
        let capture = Capture { started, release: Mutex::new(released), returned };
        let wake = ViewerGpuDeviceProgressWake::new(move || {
            let _ = &capture;
        });
        wake.watch.begin_shutdown();
        observe_start
            .recv_timeout(Duration::from_secs(2))
            .expect("capture destruction started");
        let receipt = wake.watch.shutdown_until(Instant::now() + Duration::from_millis(20));
        let _ = release.send(());
        observe_return.recv_timeout(Duration::from_secs(2)).expect("capture released");
        assert!(!receipt.all_resources_released());
        assert!(!receipt.deadline_met);
        assert_eq!(
            receipt,
            wake.watch.shutdown_until(Instant::now() + Duration::from_secs(2))
        );
    }

    #[test]
    fn normal_command_disconnect_is_not_a_retirement_receipt() {
        let (mut worker, _) = scripted_worker([]);
        let evidence = worker.shutdown_and_wait(false, false, Duration::from_secs(2));
        assert!(progress_shutdown_complete(evidence));
        assert!(!evidence.retirement_completed);
        assert!(evidence.renderer_retirement.is_none());
    }

    struct TerminalRetirement {
        receipt: mondrian_renderer::ViewerGpuRetirementReceipt,
        drops: Arc<AtomicUsize>,
    }

    impl ViewerGpuDeviceGenerationRetirement for TerminalRetirement {
        fn label(&self) -> &'static str {
            "terminal receipt propagation"
        }

        fn poll_retirement(
            &mut self,
            _: Option<&ViewerGpuDeviceGenerationTerminal>,
        ) -> Option<ViewerGpuDeviceGenerationRetirementReceipt> {
            Some(ViewerGpuDeviceGenerationRetirementReceipt { renderer: Some(self.receipt) })
        }
    }

    impl Drop for TerminalRetirement {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Release);
        }
    }

    #[test]
    fn joined_upload_panic_releases_resources_but_fails_qualification() {
        let (mut worker, _) = scripted_worker([Ok(ViewerGpuDeviceWaitStatus::Satisfied)]);
        let drops = Arc::new(AtomicUsize::new(0));
        let receipt = mondrian_renderer::ViewerGpuRetirementReceipt {
            cpu_yuv_upload: mondrian_renderer::ViewerCpuYuvUploadWorkerExit::Panicked,
            native_device_removed: false,
        };
        let handoff = worker.enqueue_generation_retirement(Box::new(TerminalRetirement {
            receipt,
            drops: Arc::clone(&drops),
        }));
        let evidence = worker.shutdown_and_wait(true, handoff, Duration::from_secs(2));
        assert!(evidence.worker_terminated);
        assert!(!evidence.worker_panicked);
        assert!(evidence.retirement_completed);
        assert_eq!(evidence.renderer_retirement, Some(receipt));
        assert_eq!(drops.load(Ordering::Acquire), 1);
        assert!(!progress_shutdown_complete(evidence));
    }

    #[test]
    fn idle_wait_panic_quarantines_an_already_accepted_retirement() {
        struct GatedPanicWait {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }
        impl ViewerGpuDeviceWait<u64> for GatedPanicWait {
            fn wait(
                &mut self,
                _: Option<&u64>,
                _: Duration,
            ) -> Result<ViewerGpuDeviceWaitStatus, String> {
                self.entered.send(()).expect("observe idle wait");
                self.release.recv_timeout(Duration::from_secs(5)).expect("release panic");
                panic!("injected idle progress panic");
            }
        }
        let active = Box::leak(Box::new(AtomicUsize::new(0)));
        let admission = ViewerGpuDeviceGenerationAdmission::reserve_from(active, 1)
            .expect("isolated admission");
        let (entered, entry) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let mut worker = ViewerGpuDeviceProgressWorker::spawn(
            "retirement-idle-panic-test",
            GatedPanicWait { entered, release: gate },
            ViewerGpuDeviceProgressPolicy::new(Duration::from_millis(1)).expect("policy"),
            ViewerGpuDeviceGenerationHealth::for_test(ViewerGpuDeviceProgressWake::default()),
            Some(admission),
        )
        .expect("spawn progress");
        entry.recv_timeout(Duration::from_secs(2)).expect("idle wait entered");
        let drops = Arc::new(AtomicUsize::new(0));
        let handoff = worker.enqueue_generation_retirement(Box::new(TestRetirement {
            safe_to_release: Arc::new(AtomicBool::new(false)),
            polls: Arc::new(AtomicUsize::new(0)),
            drops: Arc::clone(&drops),
            require_device_lost: false,
        }));
        assert!(handoff);
        release.send(()).expect("panic after accepted handoff");
        let evidence = worker.shutdown_and_wait(true, handoff, Duration::from_secs(2));
        assert!(evidence.worker_panicked);
        assert!(!evidence.retirement_completed);
        assert_eq!(evidence.renderer_retirement, None);
        drop(worker);
        assert_eq!(drops.load(Ordering::Acquire), 0);
        assert_eq!(active.load(Ordering::Acquire), 1);
    }
}
