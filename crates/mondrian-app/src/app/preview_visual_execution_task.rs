//! UI-independent, broker-owned Preview visual execution.
//!
//! This Module owns one bounded [`FrameWorkBroker`] and one dedicated serial
//! worker. The worker prepares expensive CPU visual work but deliberately does
//! not complete the frame-work lease. A successful lease crosses the App poll,
//! Viewer recording, queue submission, and actual GPU completion seams. Only
//! the Adapter that observes GPU completion may finalize the lease.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mondrian_core::{
    ExecutionDeadlineStatus, ExecutionPriority, ExecutionTerminalDisposition,
    ExecutionTerminalEvidence,
};
use mondrian_effects::HeterogeneousCpuExecutionStopReason;
#[cfg(test)]
use mondrian_playback::FrameWorkBrokerDiagnostics;
use mondrian_playback::{
    FrameDemandIdentity, FrameExecutionCancellation, FrameExecutionCancellationEvidence,
    FrameExecutionId, FrameRequestBinding, FrameRequestCompletion, FrameRequestResolution,
    FrameWorkBroker, FrameWorkClass, FrameWorkDeadline, FrameWorkPriority, FrameWorkReceive,
    FrameWorkRequest, FrameWorkSubmission, FrameWorkerLane, MediaFrameProtectionLease,
    MonotonicRuntimeClock, MonotonicTimestamp, PlaybackEpoch,
};
use mondrian_renderer::{
    HeterogeneousCpuPrefixBatchError, HeterogeneousCpuPrefixBatchExecutor,
    HeterogeneousCpuPrefixBatchOutput, HeterogeneousCpuPrefixBatchRequest,
    HeterogeneousGpuResourceGrant,
};

use super::preview_work_notification::PreviewWorkNotifier;
use super::preview_worker_lifecycle::PreviewOwnedWorkerShutdown;

const DEFAULT_MAX_PENDING: usize = 2;
const DEFAULT_MAX_QUEUED: usize = 2;
const DEFAULT_RESULT_CAPACITY: usize = 2;
const MAX_WORKER_PANIC_DETAIL_BYTES: usize = 512;
const RESULT_BACKPRESSURE_POLL: Duration = Duration::from_millis(1);

/// Complete identity of one logical visual artifact.
///
/// Execution ids identify attempts only. Cache and stale-result decisions must
/// retain all 256 semantic bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct VisualExecutionTaskKey([u8; 32]);

impl VisualExecutionTaskKey {
    /// Construct a key from a complete, domain-separated semantic fingerprint.
    pub(crate) const fn from_complete_semantic_fingerprint(fingerprint: [u8; 32]) -> Self {
        Self(fingerprint)
    }

    /// Return the complete semantic fingerprint.
    #[cfg(test)]
    pub(crate) const fn semantic_fingerprint(self) -> [u8; 32] {
        self.0
    }
}

/// Heavy renderer work carried opaquely by the visual frame-work Broker.
#[derive(Debug, Clone)]
enum VisualExecutionWork {
    HeterogeneousCpuPrefixBatch {
        request: HeterogeneousCpuPrefixBatchRequest,
        gpu_grant: HeterogeneousGpuResourceGrant,
        media_residency_protections: Vec<MediaFrameProtectionLease>,
    },
}

/// Opaque payload owned by the independent visual frame-work Broker.
///
/// The Playback epoch belongs to the product request identity, while the
/// generation, priority, class, demand, and deadline live in
/// [`FrameWorkRequest`] and remain authoritative in the Broker.
#[derive(Debug, Clone)]
pub(crate) struct VisualExecutionTaskPayload {
    epoch: PlaybackEpoch,
    work: VisualExecutionWork,
    #[cfg(test)]
    control: VisualExecutionTestControl,
}

impl VisualExecutionTaskPayload {
    /// Construct one atomic heterogeneous CPU-prefix task.
    pub(crate) fn heterogeneous_cpu_prefix_batch(
        epoch: PlaybackEpoch,
        request: HeterogeneousCpuPrefixBatchRequest,
        gpu_grant: HeterogeneousGpuResourceGrant,
        media_residency_protections: Vec<MediaFrameProtectionLease>,
    ) -> Self {
        Self {
            epoch,
            work: VisualExecutionWork::HeterogeneousCpuPrefixBatch {
                request,
                gpu_grant,
                media_residency_protections,
            },
            #[cfg(test)]
            control: VisualExecutionTestControl::default(),
        }
    }

    /// Playback Session captured by this immutable task.
    pub(crate) const fn epoch(&self) -> PlaybackEpoch {
        self.epoch
    }

    #[cfg(test)]
    fn with_gate(mut self, gate: VisualExecutionTestGate) -> Self {
        self.control.gate = Some(gate);
        self
    }

    #[cfg(test)]
    fn with_panic_before_execution(mut self) -> Self {
        self.control.panic_before_execution = true;
        self
    }

    #[cfg(test)]
    fn apply_test_control(&self) {
        if let Some(gate) = &self.control.gate {
            gate.worker_wait();
        }
        if self.control.panic_before_execution {
            panic!("test-only Preview visual worker panic before renderer execution");
        }
    }
}

/// Immutable admission intent converted directly into a domain Broker request.
#[derive(Debug, Clone)]
pub(crate) struct VisualExecutionAdmission {
    key: VisualExecutionTaskKey,
    generation: u64,
    priority: FrameWorkPriority,
    work_class: FrameWorkClass,
    demand_identity: Option<FrameDemandIdentity>,
    deadline: Option<FrameWorkDeadline<MonotonicTimestamp>>,
    payload: VisualExecutionTaskPayload,
}

impl VisualExecutionAdmission {
    /// Bind one visual payload to exact Broker scheduling semantics.
    pub(crate) fn new(
        key: VisualExecutionTaskKey,
        generation: u64,
        priority: FrameWorkPriority,
        work_class: FrameWorkClass,
        demand_identity: Option<FrameDemandIdentity>,
        deadline: Option<FrameWorkDeadline<MonotonicTimestamp>>,
        payload: VisualExecutionTaskPayload,
    ) -> Self {
        Self {
            key,
            generation,
            priority,
            work_class,
            demand_identity,
            deadline,
            payload,
        }
    }

    /// Latest-wins generation.
    #[cfg(test)]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    fn into_frame_work_request(
        self,
    ) -> FrameWorkRequest<VisualExecutionTaskKey, MonotonicTimestamp, VisualExecutionTaskPayload>
    {
        FrameWorkRequest {
            key: self.key,
            generation: self.generation,
            priority: self.priority,
            work_class: self.work_class,
            worker_affinity: None,
            resource_scope: mondrian_playback::FrameWorkResourceScope::Shared,
            demand_identity: self.demand_identity,
            deadline: self.deadline,
            in_flight_deadline_policy: mondrian_playback::FrameInFlightDeadlinePolicy::Cancel,
            execution_cancellation_budget: None,
            payload: self.payload,
        }
    }
}

/// Successful heavy preparation awaiting its exact GPU continuation.
#[derive(Debug)]
pub(crate) enum VisualExecutionTaskOutput {
    /// Atomic addressed CPU completions in deterministic request order.
    HeterogeneousCpuPrefixBatch {
        output: HeterogeneousCpuPrefixBatchOutput,
        gpu_grant: HeterogeneousGpuResourceGrant,
        media_residency_protections: Vec<MediaFrameProtectionLease>,
    },
}

impl VisualExecutionTaskOutput {
    /// Consume an atomic heterogeneous CPU-prefix batch and the GPU grant
    /// frozen by the same resource decision at admission.
    pub(crate) fn into_heterogeneous_cpu_prefix_batch(
        self,
    ) -> (
        HeterogeneousCpuPrefixBatchOutput,
        HeterogeneousGpuResourceGrant,
        Vec<MediaFrameProtectionLease>,
    ) {
        match self {
            Self::HeterogeneousCpuPrefixBatch {
                output,
                gpu_grant,
                media_residency_protections,
            } => (output, gpu_grant, media_residency_protections),
        }
    }
}

/// Why a worker attempt terminated before it could hand off a GPU lease.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VisualExecutionTaskFailure {
    /// Broker lifecycle, preemption, or deadline evidence stopped execution.
    #[error("visual execution stopped by Broker lifecycle: {evidence:?}")]
    BrokerCanceled {
        /// Atomic Broker-clock cancellation sample.
        evidence: FrameExecutionCancellationEvidence,
    },
    /// The execution id disappeared from the owning Broker.
    #[error("visual execution lease disappeared before renderer execution completed")]
    LeaseUnavailable,
    /// Renderer-owned atomic batch preparation or execution failed.
    #[error(transparent)]
    RendererBatch(#[from] HeterogeneousCpuPrefixBatchError),
    /// A task panicked inside the worker ownership boundary.
    #[error("Preview visual execution task panicked: {detail}")]
    WorkerPanicked {
        /// Bounded panic payload suitable for diagnostics.
        detail: String,
    },
}

impl VisualExecutionTaskFailure {
    fn is_broker_cancellation(&self) -> bool {
        matches!(self, Self::BrokerCanceled { .. })
    }
}

#[derive(Debug, Clone, Copy)]
struct VisualExecutionIdentity {
    key: VisualExecutionTaskKey,
    execution_id: FrameExecutionId,
    epoch: PlaybackEpoch,
    generation: u64,
    work_class: FrameWorkClass,
}

/// Successful CPU-prefix result whose Broker execution lease is still active.
///
/// Dropping this value without moving and finalizing its lease retires only
/// this exact execution and its exact binding. A different binding's queued
/// fallback remains independently executable.
#[derive(Debug)]
pub(crate) struct VisualExecutionPrefixReady {
    identity: VisualExecutionIdentity,
    output: VisualExecutionTaskOutput,
    lease: VisualExecutionLease,
}

impl VisualExecutionPrefixReady {
    /// Complete logical artifact identity.
    pub(crate) const fn key(&self) -> VisualExecutionTaskKey {
        self.identity.key
    }

    /// Playback Session captured at admission.
    pub(crate) const fn epoch(&self) -> PlaybackEpoch {
        self.identity.epoch
    }

    /// Generation captured when the worker dequeued this attempt.
    pub(crate) const fn generation(&self) -> u64 {
        self.identity.generation
    }

    /// Move the prepared output and active lease into Viewer execution.
    pub(crate) fn into_parts(self) -> (VisualExecutionTaskOutput, VisualExecutionLease) {
        (self.output, self.lease)
    }
}

/// True worker failure after its Broker lease has been cleaned up.
#[derive(Debug)]
pub(crate) struct VisualExecutionFailed {
    identity: VisualExecutionIdentity,
    terminal_binding: Option<FrameRequestBinding<MonotonicTimestamp>>,
    terminal_evidence: Option<ExecutionTerminalEvidence>,
    failure: VisualExecutionTaskFailure,
}

impl VisualExecutionFailed {
    /// Complete logical artifact identity.
    pub(crate) const fn key(&self) -> VisualExecutionTaskKey {
        self.identity.key
    }

    /// Playback Session captured at admission.
    pub(crate) const fn epoch(&self) -> PlaybackEpoch {
        self.identity.epoch
    }

    /// Generation captured at dequeue.
    pub(crate) const fn generation(&self) -> u64 {
        match self.terminal_binding {
            Some(binding) => binding.generation,
            None => self.identity.generation,
        }
    }

    /// Playback demand captured by the failed execution, if any.
    ///
    /// This is consumed only by the production result pump to issue one exact
    /// `Late` or `Failed` terminal delivery after the Broker lease has been
    /// cleaned up.
    pub(crate) const fn demand_identity(&self) -> Option<FrameDemandIdentity> {
        match self.terminal_binding {
            Some(binding) => binding.demand_identity,
            None => None,
        }
    }

    /// Cross-domain evidence for a true renderer/panic failure.
    ///
    /// Broker cancellation, deadline, and freshness remain Broker evidence and
    /// therefore return `None`.
    pub(crate) const fn terminal_evidence(&self) -> Option<ExecutionTerminalEvidence> {
        self.terminal_evidence
    }

    /// Whether this failure atomically consumed a latest compatible binding.
    pub(crate) const fn owns_terminal_binding(&self) -> bool {
        self.terminal_binding.is_some()
    }

    /// Exact domain failure.
    pub(crate) const fn failure(&self) -> &VisualExecutionTaskFailure {
        &self.failure
    }
}

/// One result emitted by the dedicated CPU preparation worker.
#[derive(Debug)]
pub(crate) enum VisualExecutionTaskResult {
    /// CPU work succeeded; completion authority remains inside the lease.
    PrefixReady(VisualExecutionPrefixReady),
    /// CPU preparation truly terminated and no lease remains active.
    Failed(VisualExecutionFailed),
}

/// Result of a nonblocking result-channel poll.
#[derive(Debug)]
pub(crate) enum VisualExecutionTaskPoll {
    /// One move-only worker result is ready.
    ///
    /// The result is boxed at the polling boundary so the empty and
    /// disconnected control states remain compact without copying or
    /// widening ownership of the result's execution lease.
    Result(Box<VisualExecutionTaskResult>),
    /// No result is currently available.
    Empty,
    /// The worker publication channel is permanently disconnected.
    Disconnected,
}

/// Exact bounded residency for the visual Broker and worker handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisualExecutionTaskConfig {
    max_pending: usize,
    max_queued: usize,
    result_capacity: usize,
}

impl VisualExecutionTaskConfig {
    /// Construct exact Broker pending/queued and worker-result capacities.
    pub(crate) const fn new(max_pending: usize, max_queued: usize, result_capacity: usize) -> Self {
        Self { max_pending, max_queued, result_capacity }
    }
}

impl Default for VisualExecutionTaskConfig {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_PENDING,
            DEFAULT_MAX_QUEUED,
            DEFAULT_RESULT_CAPACITY,
        )
    }
}

/// Failure to construct the independent visual worker.
#[derive(Debug, thiserror::Error)]
pub(crate) enum VisualExecutionTaskStartError {
    /// Every bounded Broker and result capacity must be nonzero.
    #[error(
        "visual execution capacities must be nonzero, got pending={max_pending}, \
         queued={max_queued}, results={result_capacity}"
    )]
    InvalidCapacity {
        /// Rejected pending-binding capacity.
        max_pending: usize,
        /// Rejected queued-payload capacity.
        max_queued: usize,
        /// Rejected result capacity.
        result_capacity: usize,
    },
    /// The operating system rejected the dedicated worker thread.
    #[error("failed to start Preview visual execution worker: {0}")]
    WorkerStart(#[source] std::io::Error),
}

type VisualExecutionBroker =
    FrameWorkBroker<VisualExecutionTaskKey, MonotonicTimestamp, VisualExecutionTaskPayload>;

/// Move-only authority spanning CPU prefix through real GPU completion.
#[must_use = "dropping an unfinished visual lease abandons and cleans its Broker binding"]
pub(crate) struct VisualExecutionLease {
    broker: VisualExecutionBroker,
    identity: VisualExecutionIdentity,
    active: bool,
}

impl fmt::Debug for VisualExecutionLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VisualExecutionLease")
            .field("key", &self.identity.key)
            .field("execution_id", &self.identity.execution_id)
            .field("epoch", &self.identity.epoch)
            .field("generation", &self.identity.generation)
            .field("active", &self.active)
            .finish()
    }
}

impl VisualExecutionLease {
    fn new(broker: VisualExecutionBroker, identity: VisualExecutionIdentity) -> Self {
        Self { broker, identity, active: true }
    }

    /// Complete semantic artifact key used by Broker regression tests.
    #[cfg(test)]
    pub(crate) const fn key(&self) -> VisualExecutionTaskKey {
        self.identity.key
    }

    /// Playback Session captured at admission.
    pub(crate) const fn epoch(&self) -> PlaybackEpoch {
        self.identity.epoch
    }

    /// Generation captured when CPU execution started.
    pub(crate) const fn generation(&self) -> u64 {
        self.identity.generation
    }

    /// Finalize only after the exact GPU submission has truly completed.
    ///
    /// `reusable` states whether the completed artifact may satisfy a compatible
    /// cross-binding request and atomically retire its queued fallback. The
    /// Broker records the completion instant and resolves latest freshness and
    /// deadline at this seam.
    pub(crate) fn finalize_gpu(mut self, reusable: bool) -> VisualExecutionGpuFinalization {
        let completion_recorded = self.broker.mark_execution_completed(self.identity.execution_id);
        let resolution = self.broker.resolve_execution(self.identity.execution_id, reusable);
        self.active = false;
        VisualExecutionGpuFinalization {
            identity: self.identity,
            completion_recorded,
            resolution,
        }
    }

    /// Fail a GPU continuation before completion and atomically consume only
    /// the latest compatible terminal binding.
    pub(crate) fn fail_gpu(mut self) -> VisualExecutionGpuFailure {
        let terminal_binding = self.broker.fail_execution(self.identity.execution_id);
        self.active = false;
        VisualExecutionGpuFailure { identity: self.identity, terminal_binding }
    }

    fn cleanup_unfinished(&mut self) {
        if !self.active {
            return;
        }
        // Failure is non-reusable. The Broker consumes only an exact binding;
        // a cross-binding fallback remains pending and queued.
        let _ = self.broker.fail_execution(self.identity.execution_id);
        self.active = false;
    }
}

/// Broker result for a GPU continuation that produced no value.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VisualExecutionGpuFailure {
    identity: VisualExecutionIdentity,
    terminal_binding: Option<FrameRequestBinding<MonotonicTimestamp>>,
}

impl VisualExecutionGpuFailure {
    /// Exact execution generation, updated to a compatible rebound when one
    /// still owned terminal authority.
    pub(crate) const fn generation(self) -> u64 {
        match self.terminal_binding {
            Some(binding) => binding.generation,
            None => self.identity.generation,
        }
    }

    /// Playback Session captured by the actual execution payload.
    pub(crate) const fn epoch(self) -> PlaybackEpoch {
        self.identity.epoch
    }

    /// Latest compatible class that owned terminal authority.
    pub(crate) const fn work_class(self) -> Option<FrameWorkClass> {
        match self.terminal_binding {
            Some(binding) => Some(binding.work_class),
            None => None,
        }
    }

    /// Latest exact demand rebound to this execution, if any.
    pub(crate) const fn demand_identity(self) -> Option<FrameDemandIdentity> {
        match self.terminal_binding {
            Some(binding) => binding.demand_identity,
            None => None,
        }
    }
}

impl Drop for VisualExecutionLease {
    fn drop(&mut self) {
        self.cleanup_unfinished();
    }
}

/// Broker resolution sampled at the real GPU completion boundary.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VisualExecutionGpuFinalization {
    identity: VisualExecutionIdentity,
    completion_recorded: bool,
    resolution: FrameRequestResolution<MonotonicTimestamp>,
}

impl VisualExecutionGpuFinalization {
    /// Whether the Broker accepted the actual GPU completion timestamp.
    pub(crate) const fn completion_recorded(self) -> bool {
        self.completion_recorded
    }

    /// Atomic freshness and deadline resolution.
    #[cfg(test)]
    pub(crate) const fn resolution(self) -> FrameRequestResolution<MonotonicTimestamp> {
        self.resolution
    }

    /// Broker-owned semantic class captured by the completed execution.
    pub(crate) const fn work_class(self) -> FrameWorkClass {
        self.identity.work_class
    }

    /// Atomic deadline classification sampled at real GPU completion.
    pub(crate) const fn deadline_status(self) -> mondrian_playback::FrameWorkDeadlineStatus {
        self.resolution.deadline
    }

    /// Playback demand that this exact current completion was admitted to
    /// satisfy, if the binding still owns one.
    pub(crate) const fn demand_identity(self) -> Option<FrameDemandIdentity> {
        match self.resolution.binding {
            Some(binding) => binding.demand_identity,
            None => None,
        }
    }

    /// Only an exact current completion may enter visible publication.
    ///
    /// A Broker resolution can be both `Current` and deadline-missed. Such a
    /// result terminates a matching Playback demand as `Late`; it cannot
    /// become visible merely because no newer semantic binding exists.
    pub(crate) const fn may_publish_current(self) -> bool {
        self.completion_recorded
            && matches!(self.resolution.completion, FrameRequestCompletion::Current)
            && !self.resolution.deadline.is_missed()
    }

    /// Current and CacheOnly artifacts may enter semantic caches.
    pub(crate) const fn may_cache(self) -> bool {
        self.completion_recorded
            && self.resolution.completion.should_cache()
            && !self.resolution.deadline.is_missed()
    }
}

/// Dedicated, bounded, UI-independent Preview visual execution owner.
pub(crate) struct VisualExecutionTask {
    broker: VisualExecutionBroker,
    clock: SharedVisualExecutionClock,
    results: Option<mpsc::Receiver<VisualExecutionTaskResult>>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct SharedVisualExecutionClock(Arc<dyn MonotonicRuntimeClock>);

impl MonotonicRuntimeClock for SharedVisualExecutionClock {
    fn now(&self) -> MonotonicTimestamp {
        self.0.now()
    }
}

impl VisualExecutionTask {
    /// Start one dedicated serial worker with an explicit Broker clock.
    #[cfg(test)]
    pub(crate) fn new<C>(
        clock: C,
        config: VisualExecutionTaskConfig,
    ) -> Result<Self, VisualExecutionTaskStartError>
    where
        C: MonotonicRuntimeClock,
    {
        Self::new_with_notifier(clock, config, PreviewWorkNotifier::default())
    }

    /// Start one worker publishing completion wakes into a shared Runtime watch.
    pub(crate) fn new_with_notifier<C>(
        clock: C,
        config: VisualExecutionTaskConfig,
        work_notifier: PreviewWorkNotifier,
    ) -> Result<Self, VisualExecutionTaskStartError>
    where
        C: MonotonicRuntimeClock,
    {
        if config.max_pending == 0 || config.max_queued == 0 || config.result_capacity == 0 {
            return Err(VisualExecutionTaskStartError::InvalidCapacity {
                max_pending: config.max_pending,
                max_queued: config.max_queued,
                result_capacity: config.result_capacity,
            });
        }
        let clock = SharedVisualExecutionClock(Arc::new(clock));
        let broker =
            FrameWorkBroker::new_with_clock(config.max_pending, config.max_queued, clock.clone());
        let worker_broker = broker.clone();
        let (result_sender, result_receiver) = mpsc::sync_channel(config.result_capacity);
        let worker = thread::Builder::new()
            .name("mondrian-preview-visual-execution".to_owned())
            .spawn(move || visual_execution_worker(worker_broker, result_sender, work_notifier))
            .map_err(VisualExecutionTaskStartError::WorkerStart)?;
        Ok(Self {
            broker,
            clock,
            results: Some(result_receiver),
            worker: Some(worker),
        })
    }

    /// Atomically submit using the exact domain Broker result semantics.
    pub(crate) fn submit(
        &self,
        admission: VisualExecutionAdmission,
    ) -> FrameWorkSubmission<VisualExecutionTaskKey> {
        self.broker.submit(admission.into_frame_work_request())
    }

    /// Project one absolute App Adapter deadline into the exact monotonic
    /// origin owned by this visual Broker.
    pub(crate) fn project_adapter_deadline(
        &self,
        adapter_deadline: Instant,
        sampled_at: Instant,
    ) -> FrameWorkDeadline<MonotonicTimestamp> {
        let remaining = adapter_deadline.saturating_duration_since(sampled_at);
        let broker_now = self.clock.now();
        FrameWorkDeadline::from_remaining(broker_now.saturating_add(remaining), remaining)
    }

    /// Begin a new Broker-owned latest-wins generation.
    ///
    /// Callers using an external generation domain may instead use
    /// [`Self::prune_before`]. Keeping rotation separate from submission is
    /// essential: the Broker must be able to rebind compatible in-flight work
    /// after a generation change.
    #[cfg(test)]
    pub(crate) fn begin_generation(&self) -> u64 {
        self.broker.begin_generation()
    }

    /// Cancel one semantic artifact across pending, queued, and in-flight work.
    #[cfg(test)]
    pub(crate) fn cancel_key(&self, key: &VisualExecutionTaskKey) -> usize {
        self.broker.cancel_key(key)
    }

    /// Observe an external generation and prune obsolete queued work.
    pub(crate) fn prune_before(&self, generation: u64) -> usize {
        self.broker.prune_before(generation)
    }

    /// Snapshot authoritative Broker lifecycle diagnostics.
    #[cfg(test)]
    pub(crate) fn diagnostics(&self) -> FrameWorkBrokerDiagnostics {
        self.broker.diagnostics()
    }

    /// Poll at most one worker result without blocking the caller.
    pub(crate) fn try_poll(&self) -> VisualExecutionTaskPoll {
        let Some(results) = self.results.as_ref() else {
            return VisualExecutionTaskPoll::Disconnected;
        };
        match results.try_recv() {
            Ok(result) => VisualExecutionTaskPoll::Result(Box::new(result)),
            Err(mpsc::TryRecvError::Empty) => VisualExecutionTaskPoll::Empty,
            Err(mpsc::TryRecvError::Disconnected) => VisualExecutionTaskPoll::Disconnected,
        }
    }

    /// Close admission after the sole execution worker becomes unavailable.
    ///
    /// Pending and in-flight Broker authority is invalidated atomically; any
    /// move-only result already owned by the foreground remains responsible
    /// for its own exact lease cleanup.
    pub(crate) fn close_after_worker_disconnect(&self) {
        self.broker.close();
    }

    pub(crate) fn shutdown_and_wait(mut self) -> PreviewOwnedWorkerShutdown {
        self.stop_worker()
    }

    pub(crate) fn shutdown_until(mut self, deadline: Instant) -> PreviewOwnedWorkerShutdown {
        self.stop_worker_until(deadline)
    }

    pub(crate) fn begin_shutdown(&mut self) {
        self.results.take();
        self.broker.close();
    }

    fn stop_worker(&mut self) -> PreviewOwnedWorkerShutdown {
        self.begin_shutdown();
        self.worker.take().map_or(
            PreviewOwnedWorkerShutdown::NotStarted,
            PreviewOwnedWorkerShutdown::join,
        )
    }

    fn stop_worker_until(&mut self, deadline: Instant) -> PreviewOwnedWorkerShutdown {
        self.begin_shutdown();
        self.worker.take().map_or(PreviewOwnedWorkerShutdown::NotStarted, |worker| {
            PreviewOwnedWorkerShutdown::join_until(worker, deadline)
        })
    }
}

impl Drop for VisualExecutionTask {
    fn drop(&mut self) {
        match self.stop_worker_until(Instant::now()) {
            PreviewOwnedWorkerShutdown::Panicked
            | PreviewOwnedWorkerShutdown::PanickedPayloadAbandoned => {
                tracing::warn!("Preview visual execution worker panicked during shutdown");
            }
            PreviewOwnedWorkerShutdown::CurrentThreadSkipped => {
                tracing::warn!("Preview visual execution shutdown detached its current worker");
            }
            PreviewOwnedWorkerShutdown::TimedOutDetached => {
                tracing::warn!("Preview visual execution Drop detached its active worker");
            }
            PreviewOwnedWorkerShutdown::NotStarted | PreviewOwnedWorkerShutdown::Terminated => {}
        }
    }
}

fn visual_execution_worker(
    broker: VisualExecutionBroker,
    results: mpsc::SyncSender<VisualExecutionTaskResult>,
    work_notifier: PreviewWorkNotifier,
) {
    let _exit_notification = work_notifier.worker_exit_notification();
    let mut executor = HeterogeneousCpuPrefixBatchExecutor::default();
    while let Some(received) = broker.receive(FrameWorkerLane::Any) {
        let (execution, expired) = match received {
            FrameWorkReceive::Ready(execution) => (execution, false),
            FrameWorkReceive::Expired(execution) => (execution, true),
        };
        let identity = VisualExecutionIdentity {
            key: execution.key,
            execution_id: execution.id,
            epoch: execution.payload.epoch(),
            generation: execution.generation,
            work_class: execution.work_class,
        };
        let lease = VisualExecutionLease::new(broker.clone(), identity);
        let outcome = if expired {
            Err(broker
                .execution_cancellation_evidence(identity.execution_id)
                .map_or(VisualExecutionTaskFailure::LeaseUnavailable, |evidence| {
                    VisualExecutionTaskFailure::BrokerCanceled { evidence }
                }))
        } else {
            catch_unwind(AssertUnwindSafe(|| {
                #[cfg(test)]
                execution.payload.apply_test_control();
                execute_visual_payload(execution.payload, identity, &broker, &mut executor)
            }))
            .unwrap_or_else(|payload| {
                // No mutable renderer/effect residency survives a task panic.
                executor = HeterogeneousCpuPrefixBatchExecutor::default();
                Err(VisualExecutionTaskFailure::WorkerPanicked {
                    detail: bounded_panic_detail(payload),
                })
            })
        };
        let result = match outcome {
            Ok(output) => VisualExecutionTaskResult::PrefixReady(VisualExecutionPrefixReady {
                identity,
                output,
                lease,
            }),
            Err(failure) => {
                let gpu_failure = lease.fail_gpu();
                let terminal_binding = gpu_failure.terminal_binding;
                let terminal_evidence = terminal_failure_evidence(terminal_binding, &failure);
                VisualExecutionTaskResult::Failed(VisualExecutionFailed {
                    identity,
                    terminal_binding,
                    terminal_evidence,
                    failure,
                })
            }
        };
        if !publish_with_bounded_backpressure(&results, result) {
            break;
        }
        work_notifier.result_became_pollable();
    }
}

fn execute_visual_payload(
    payload: VisualExecutionTaskPayload,
    identity: VisualExecutionIdentity,
    broker: &VisualExecutionBroker,
    executor: &mut HeterogeneousCpuPrefixBatchExecutor,
) -> Result<VisualExecutionTaskOutput, VisualExecutionTaskFailure> {
    match payload.work {
        VisualExecutionWork::HeterogeneousCpuPrefixBatch {
            request,
            gpu_grant,
            media_residency_protections,
        } => {
            let mut observed_cancellation = None;
            let output = executor.execute(request, identity.generation, || {
                broker.execution_cancellation_evidence(identity.execution_id).map(|evidence| {
                    observed_cancellation = Some(evidence);
                    effect_stop_reason(evidence.cancellation)
                })
            });
            match output {
                Ok(output) => Ok(VisualExecutionTaskOutput::HeterogeneousCpuPrefixBatch {
                    output,
                    gpu_grant,
                    media_residency_protections,
                }),
                Err(HeterogeneousCpuPrefixBatchError::Stopped { .. }) => Err(observed_cancellation
                    .map_or(VisualExecutionTaskFailure::LeaseUnavailable, |evidence| {
                        VisualExecutionTaskFailure::BrokerCanceled { evidence }
                    })),
                Err(error) => Err(VisualExecutionTaskFailure::RendererBatch(error)),
            }
        }
    }
}

fn effect_stop_reason(
    cancellation: FrameExecutionCancellation,
) -> HeterogeneousCpuExecutionStopReason {
    match cancellation {
        FrameExecutionCancellation::DeadlineExpired { .. }
        | FrameExecutionCancellation::ExecutionBudgetExpired { .. } => {
            HeterogeneousCpuExecutionStopReason::DeadlineExpired
        }
        FrameExecutionCancellation::BrokerClosed { .. }
        | FrameExecutionCancellation::Superseded { .. }
        | FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. }
        | FrameExecutionCancellation::StillPreemptedByRealtimeCurrent { .. } => {
            HeterogeneousCpuExecutionStopReason::Canceled
        }
    }
}

fn terminal_failure_evidence(
    terminal_binding: Option<FrameRequestBinding<MonotonicTimestamp>>,
    failure: &VisualExecutionTaskFailure,
) -> Option<ExecutionTerminalEvidence> {
    let binding = terminal_binding?;
    (!failure.is_broker_cancellation()).then_some(ExecutionTerminalEvidence {
        generation: binding.generation,
        priority: terminal_priority(binding.priority, binding.work_class),
        disposition: ExecutionTerminalDisposition::Failed,
        // Deadline and freshness are exclusively Broker-owned. This evidence
        // only says that renderer work truly failed.
        deadline: ExecutionDeadlineStatus::NotApplicable,
    })
}

fn terminal_priority(priority: FrameWorkPriority, work_class: FrameWorkClass) -> ExecutionPriority {
    match (priority, work_class) {
        (FrameWorkPriority::Current, FrameWorkClass::Playback) => ExecutionPriority::Realtime,
        (FrameWorkPriority::Current, FrameWorkClass::Interactive | FrameWorkClass::Still) => {
            ExecutionPriority::UserInitiated
        }
        (FrameWorkPriority::Prefetch, _) => ExecutionPriority::Background,
    }
}

fn bounded_panic_detail(payload: Box<dyn std::any::Any + Send>) -> String {
    let detail = if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    };
    truncate_utf8_detail(detail, MAX_WORKER_PANIC_DETAIL_BYTES)
}

fn truncate_utf8_detail(mut detail: String, max_bytes: usize) -> String {
    if detail.len() <= max_bytes {
        return detail;
    }
    const SUFFIX: &str = "...";
    let append_suffix = max_bytes >= SUFFIX.len();
    let mut end = if append_suffix {
        max_bytes - SUFFIX.len()
    } else {
        max_bytes
    }
    .min(detail.len());
    while !detail.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    detail.truncate(end);
    if append_suffix {
        detail.push_str(SUFFIX);
    }
    detail
}

fn publish_with_bounded_backpressure(
    sender: &mpsc::SyncSender<VisualExecutionTaskResult>,
    mut result: VisualExecutionTaskResult,
) -> bool {
    loop {
        match sender.try_send(result) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(returned)) => {
                result = returned;
                thread::park_timeout(RESULT_BACKPRESSURE_POLL);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
struct VisualExecutionTestControl {
    gate: Option<VisualExecutionTestGate>,
    panic_before_execution: bool,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct VisualExecutionTestGate {
    entered: std::sync::Arc<std::sync::atomic::AtomicBool>,
    released: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(test)]
impl VisualExecutionTestGate {
    fn new() -> Self {
        Self {
            entered: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            released: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    fn worker_wait(&self) {
        use std::sync::atomic::Ordering;

        self.entered.store(true, Ordering::Release);
        while !self.released.load(Ordering::Acquire) {
            thread::park_timeout(Duration::from_millis(1));
        }
    }

    fn wait_until_entered(&self) {
        use std::sync::atomic::Ordering;
        use std::time::Instant;

        let started = Instant::now();
        while !self.entered.load(Ordering::Acquire) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "visual test worker did not enter its gate"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn release(&self) {
        use std::sync::atomic::Ordering;

        self.released.store(true, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/preview_visual_execution_task.rs"]
mod tests;
