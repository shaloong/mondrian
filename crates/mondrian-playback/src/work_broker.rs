//! Atomic frame-work lifecycle broker.
//!
//! This Module owns admission, queued transport, in-flight execution leases,
//! latest-wins invalidation, preemption, and completion binding under one lock.
//! Payloads, keys, and absolute Adapter deadline values remain opaque. The
//! Broker owns the lowered monotonic deadline used for lifecycle decisions.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{
    FrameDemandIdentity, FrameExecutionCancellation, FrameInFlightDeadlinePolicy,
    FrameRequestBinding, FrameRequestCompletion, FrameRequestResolution, FrameWorkClass,
    FrameWorkDeadline, FrameWorkDeadlineStatus, FrameWorkPriority, FrameWorkResourceScope,
    MonotonicRuntimeClock, MonotonicTimestamp, SystemMonotonicRuntimeClock,
};

/// Stable identity for one dequeued execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FrameExecutionId(u64);

impl FrameExecutionId {
    /// Return the numeric identity used by Adapter evidence.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Semantic worker eligibility lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameWorkerLane {
    /// Single-worker fallback that accepts every class.
    Any,
    /// Worker reserved for playback cursor work.
    Playback,
    /// Worker reserved for interactive scrub work.
    Interactive,
    /// Worker reserved for deterministic still work.
    Still,
    /// Shared worker for interactive and still work.
    NonPlayback,
}

impl FrameWorkerLane {
    /// Return whether this lane directly accepts a semantic work class.
    pub const fn accepts(self, class: FrameWorkClass) -> bool {
        match self {
            Self::Any => true,
            Self::Playback => matches!(class, FrameWorkClass::Playback),
            Self::Interactive => matches!(class, FrameWorkClass::Interactive),
            Self::Still => matches!(class, FrameWorkClass::Still),
            Self::NonPlayback => !matches!(class, FrameWorkClass::Playback),
        }
    }
}

/// One Adapter payload submitted for semantic scheduling.
#[derive(Debug)]
pub struct FrameWorkRequest<K, D, P> {
    /// Opaque semantic frame key.
    pub key: K,
    /// Latest-wins generation.
    pub generation: u64,
    /// Current or speculative priority.
    pub priority: FrameWorkPriority,
    /// Playback, interactive, or still execution semantics.
    pub work_class: FrameWorkClass,
    /// Exact physical resource-ownership scope eligible for work reuse.
    pub resource_scope: FrameWorkResourceScope,
    /// Playback demand identity when demand-backed.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Adapter deadline plus remaining budget lowered by the Broker at admission.
    pub deadline: Option<FrameWorkDeadline<D>>,
    /// Whether an already-running lease is canceled when that deadline expires.
    ///
    /// Queue expiry and completion lateness are unaffected by this policy.
    pub in_flight_deadline_policy: FrameInFlightDeadlinePolicy,
    /// Optional cancellation budget measured from execution dequeue.
    ///
    /// Unlike `deadline`, this budget is independent of presentation timing
    /// and always requests cooperative cancellation when exhausted.
    pub execution_cancellation_budget: Option<Duration>,
    /// Opaque Adapter execution payload.
    pub payload: P,
}

/// Payload-free metadata used to rebind already-owned frame work.
///
/// This request can update a queued payload or compatible execution lease only
/// when the exact semantic key and physical resource scope already exist. It
/// never admits new work and therefore cannot manufacture payload ownership.
#[derive(Debug)]
pub struct FrameWorkBindingRequest<K, D> {
    /// Opaque semantic frame key.
    pub key: K,
    /// Latest-wins generation.
    pub generation: u64,
    /// Current or speculative priority.
    pub priority: FrameWorkPriority,
    /// Playback, interactive, or still execution semantics.
    pub work_class: FrameWorkClass,
    /// Exact physical resource-ownership scope required for reuse.
    pub resource_scope: FrameWorkResourceScope,
    /// Playback demand identity when demand-backed.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Adapter deadline plus remaining budget lowered by the Broker at binding.
    pub deadline: Option<FrameWorkDeadline<D>>,
    /// Deadline-expiry behavior for an already-running compatible lease.
    pub in_flight_deadline_policy: FrameInFlightDeadlinePolicy,
    /// Optional cancellation budget measured from the compatible lease's
    /// original execution dequeue.
    pub execution_cancellation_budget: Option<Duration>,
}

/// Result of atomically submitting frame work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameWorkSubmission<K> {
    /// A new queued execution was created.
    Queued {
        /// Speculative key evicted to preserve bounded capacity.
        evicted_prefetch: Option<K>,
        /// Still key evicted for realtime current work.
        evicted_still: Option<K>,
    },
    /// An existing queued payload and binding were replaced atomically.
    UpdatedQueued {
        /// Whether prefetch was promoted to current.
        priority_promoted: bool,
        /// Whether the semantic work class changed.
        work_class_changed: bool,
        /// Whether the generation changed.
        generation_changed: bool,
    },
    /// Compatible work is already in flight and may bind to this latest request.
    ReusedInFlight,
    /// The request generation was already older than Broker authority.
    DroppedObsoleteGeneration,
    /// Bounded pending/queued capacity could not admit the request.
    DroppedBackpressure,
    /// The priority/class pair violates semantic policy.
    DroppedInvalidClass,
    /// The broker is closed and cannot accept work.
    Closed,
}

/// Result of atomically rebinding already-owned frame work without a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameWorkBindingSubmission {
    /// An existing queued payload was retained while its metadata changed.
    UpdatedQueued {
        /// Whether prefetch was promoted to current.
        priority_promoted: bool,
        /// Whether the semantic work class changed.
        work_class_changed: bool,
        /// Whether the generation changed.
        generation_changed: bool,
    },
    /// A compatible execution lease now owns the latest binding.
    ReusedInFlight,
    /// No same-key, same-scope queued payload or compatible lease exists.
    NeedsPayload,
    /// The request generation was already older than Broker authority.
    DroppedObsoleteGeneration,
    /// The priority/class pair violates semantic policy.
    DroppedInvalidClass,
    /// The broker is closed and cannot update work.
    Closed,
}

/// Dequeued execution lease and opaque Adapter payload.
#[derive(Debug)]
pub struct FrameWorkExecution<K, D, P> {
    /// Unique execution lease identity.
    pub id: FrameExecutionId,
    /// Opaque semantic frame key.
    pub key: K,
    /// Binding generation captured when this execution was dequeued.
    pub generation: u64,
    /// Captured execution priority.
    pub priority: FrameWorkPriority,
    /// Captured semantic work class.
    pub work_class: FrameWorkClass,
    /// Captured physical resource-ownership scope.
    pub resource_scope: FrameWorkResourceScope,
    /// Captured demand identity; final completion must resolve through the broker.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Captured Adapter deadline.
    pub deadline: Option<D>,
    /// Time spent waiting under the latest compatible request binding.
    ///
    /// A metadata-only prefetch-to-current promotion restarts this interval,
    /// so realtime evidence never charges intentional speculative residency to
    /// the later current request.
    pub queue_wait: Duration,
    /// Opaque Adapter execution payload.
    pub payload: P,
}

/// Worker receive outcome.
#[derive(Debug)]
pub enum FrameWorkReceive<K, D, P> {
    /// Work may execute normally.
    Ready(FrameWorkExecution<K, D, P>),
    /// Playback-current work expired while queued and should terminate canceled.
    Expired(FrameWorkExecution<K, D, P>),
}

/// Result of a bounded worker wait.
#[derive(Debug)]
pub enum FrameWorkReceiveWait<K, D, P> {
    /// One execution lease is ready or expired.
    Work(FrameWorkReceive<K, D, P>),
    /// No eligible work arrived before the requested idle interval.
    TimedOut,
    /// An Adapter-requested worker lifecycle checkpoint interrupted the wait.
    ///
    /// No execution lease is created. The worker must finish its local
    /// lifecycle action before receiving work again.
    Interrupted {
        /// Latest lifecycle revision observed under the Broker lock.
        revision: u64,
    },
    /// The broker closed while the worker was waiting.
    Closed,
}

/// Terminal observation from a bounded execution-lifecycle wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameExecutionWaitStatus {
    /// Cooperative cancellation was requested before completion.
    Canceled(crate::FrameExecutionCancellationEvidence),
    /// The worker stamped completion before any cancellation request.
    Completed,
    /// The execution lease no longer exists.
    Missing,
    /// No terminal state was observed within the caller's bound.
    Timeout,
}

/// Presentation binding expired before completion.
///
/// Expiration terminates the binding's publication authority. Queued work is
/// removed, while a matching already-running locality-preserving execution may
/// remain alive without reacquiring that authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredFrameWork<K, D> {
    /// Opaque semantic key.
    pub key: K,
    /// Latest binding removed by expiration.
    pub binding: FrameRequestBinding<D>,
    /// Queued payloads actually removed for this binding.
    pub removed_queued_work: usize,
    /// Matching in-flight attempts retained solely for execution locality.
    pub retained_in_flight_work: usize,
}

/// Stable broker evidence independent of Adapter payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct FrameWorkBrokerDiagnostics {
    /// Latest generation observed.
    pub latest_generation: u64,
    /// Playback demand whose unstarted current work owns queue authority.
    pub active_playback_demand: Option<FrameDemandIdentity>,
    /// Runtime-clock regression episodes clamped to the last observation.
    pub clock_regressions: u64,
    /// Semantic keys awaiting a terminal resolution.
    pub pending_requests: usize,
    /// Payloads waiting for a worker.
    pub queued_work: usize,
    /// Execution leases not yet resolved or abandoned.
    pub in_flight_work: usize,
    /// In-flight leases for which the worker already stamped completion.
    pub in_flight_completed: usize,
    /// In-flight leases whose lifecycle already requests cancellation.
    pub in_flight_cancellation_requested: usize,
    /// Oldest current in-flight lease age in microseconds.
    pub in_flight_max_age_us: u64,
    /// Oldest pending cancellation age among in-flight leases in microseconds.
    pub in_flight_cancellation_max_age_us: u64,
    /// In-flight current execution leases.
    pub in_flight_current: usize,
    /// In-flight speculative execution leases.
    pub in_flight_prefetch: usize,
    /// In-flight playback-class execution leases.
    pub in_flight_playback: usize,
    /// In-flight interactive execution leases.
    pub in_flight_interactive: usize,
    /// In-flight still execution leases.
    pub in_flight_still: usize,
    /// In-flight leases on the unrestricted single-worker lane.
    pub in_flight_any_lane: usize,
    /// In-flight leases on the playback-reserved lane.
    pub in_flight_playback_lane: usize,
    /// In-flight leases on the interactive-scrub lane.
    pub in_flight_interactive_lane: usize,
    /// In-flight leases on the deterministic-still lane.
    pub in_flight_still_lane: usize,
    /// In-flight leases on the shared non-playback lane.
    pub in_flight_non_playback_lane: usize,
    /// Current leases whose recorded lane does not accept their work class.
    pub in_flight_cross_lane_current: usize,
    /// Queued current work.
    pub queued_current: usize,
    /// Queued speculative work.
    pub queued_prefetch: usize,
    /// Queued playback-class work.
    pub queued_playback: usize,
    /// Queued interactive work.
    pub queued_interactive: usize,
    /// Queued still work.
    pub queued_still: usize,
    /// Playback-current work already expired while queued.
    pub queued_expired_playback_current: usize,
    /// All work whose lowered deadline has expired while queued.
    pub queued_expired_work: usize,
    /// Expired playback-current executions returned to workers.
    pub dropped_expired_playback_current: u64,
    /// All expired executions returned to workers.
    pub dropped_expired_work: u64,
    /// Accepted new queued requests.
    pub submitted_queued: u64,
    /// Existing queued requests updated atomically.
    pub submitted_updated_queued: u64,
    /// Requests rebound to compatible in-flight work.
    pub submitted_reused_in_flight: u64,
    /// Existing requests whose semantic class changed.
    pub submitted_work_class_changes: u64,
    /// Requests rejected by bounded capacity.
    pub dropped_backpressure: u64,
    /// Requests rejected by invalid semantic class.
    pub dropped_invalid_class: u64,
    /// Requests rejected because their generation was obsolete.
    pub dropped_obsolete_generation: u64,
    /// Queued prefetch payloads evicted by current work.
    pub evicted_prefetch: u64,
    /// Queued still payloads evicted by realtime work.
    pub evicted_still: u64,
    /// Pending bindings canceled explicitly or by expiration.
    pub canceled_requests: u64,
    /// Queued payloads pruned as obsolete.
    pub pruned_queued: u64,
    /// Older unstarted playback-current payloads superseded by a newer demand.
    pub superseded_queued_playback_current: u64,
    /// Running leases invalidated by a non-locality-preserving generation rotation.
    pub in_flight_generation_invalidations: u64,
    /// Running leases invalidated because their exact pending binding disappeared.
    pub in_flight_binding_invalidations: u64,
    /// Same-generation Playback leases detached after their binding disappeared.
    pub in_flight_binding_locality_detachments: u64,
    /// Execution completions accepted as current.
    pub completed_current: u64,
    /// Execution completions admitted for cache only.
    pub completed_cache_only: u64,
    /// Execution completions rejected as stale.
    pub completed_stale: u64,
    /// Cache-only completions without an eligible publication binding.
    pub completed_cache_only_missing: u64,
    /// Cache-only completions whose semantic class no longer matched.
    pub completed_cache_only_class_mismatch: u64,
    /// Stale completions with no pending binding.
    pub completed_stale_missing: u64,
    /// Stale completions whose semantic class no longer matched.
    pub completed_stale_class_mismatch: u64,
    /// Stale completions superseded by generation or demand binding.
    pub completed_stale_obsolete: u64,
    /// Key-current checks rejected because no pending binding remained.
    pub skipped_missing: u64,
    /// Key-current checks rejected because semantic class changed.
    pub skipped_class_mismatch: u64,
    /// Key-current checks pruned an obsolete generation.
    pub skipped_obsolete: u64,
    /// Whether the broker is closed.
    pub closed: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingBinding<D> {
    binding: FrameRequestBinding<D>,
    requested_at: MonotonicTimestamp,
    deadline_at: Option<MonotonicTimestamp>,
    in_flight_deadline_policy: FrameInFlightDeadlinePolicy,
    execution_cancellation_budget: Option<Duration>,
}

#[derive(Debug)]
struct QueuedWork<K, D, P> {
    request: FrameWorkRequest<K, D, P>,
    deadline_at: Option<MonotonicTimestamp>,
}

#[derive(Debug, Clone)]
struct InFlightWork<K> {
    key: K,
    generation: u64,
    priority: FrameWorkPriority,
    work_class: FrameWorkClass,
    resource_scope: FrameWorkResourceScope,
    worker_lane: Option<FrameWorkerLane>,
    demand_identity: Option<FrameDemandIdentity>,
    started_at: MonotonicTimestamp,
    deadline_at: Option<MonotonicTimestamp>,
    in_flight_deadline_policy: FrameInFlightDeadlinePolicy,
    execution_cancellation_deadline_at: Option<MonotonicTimestamp>,
    completed_at: Option<MonotonicTimestamp>,
    preempted_at: Option<MonotonicTimestamp>,
    invalidated_at: Option<MonotonicTimestamp>,
    /// The presentation binding that admitted this attempt reached a terminal
    /// expiration while execution was retained solely for stateful locality.
    ///
    /// Once detached, this attempt can never bind to a later presentation
    /// demand, even if the semantic key and generation happen to match.
    presentation_binding_expired: bool,
}

struct BrokerState<K, D, P> {
    latest_generation: u64,
    active_playback_demand: Option<FrameDemandIdentity>,
    next_execution_id: u64,
    pending: HashMap<K, PendingBinding<D>>,
    queue: VecDeque<QueuedWork<K, D, P>>,
    in_flight: HashMap<FrameExecutionId, InFlightWork<K>>,
    last_observed_at: MonotonicTimestamp,
    clock_regression_active: bool,
    closed_at: Option<MonotonicTimestamp>,
    worker_lifecycle_revision: u64,
    metrics: FrameWorkBrokerMetrics,
}

#[derive(Default)]
struct FrameWorkBrokerMetrics {
    clock_regressions: u64,
    dropped_expired_work: u64,
    dropped_expired_playback_current: u64,
    submitted_queued: u64,
    submitted_updated_queued: u64,
    submitted_reused_in_flight: u64,
    submitted_work_class_changes: u64,
    dropped_backpressure: u64,
    dropped_invalid_class: u64,
    dropped_obsolete_generation: u64,
    evicted_prefetch: u64,
    evicted_still: u64,
    canceled_requests: u64,
    pruned_queued: u64,
    superseded_queued_playback_current: u64,
    in_flight_generation_invalidations: u64,
    in_flight_binding_invalidations: u64,
    in_flight_binding_locality_detachments: u64,
    completed_current: u64,
    completed_cache_only: u64,
    completed_stale: u64,
    completed_cache_only_missing: u64,
    completed_cache_only_class_mismatch: u64,
    completed_stale_missing: u64,
    completed_stale_class_mismatch: u64,
    completed_stale_obsolete: u64,
    skipped_missing: u64,
    skipped_class_mismatch: u64,
    skipped_obsolete: u64,
}

struct BrokerShared<K, D, P> {
    state: Mutex<BrokerState<K, D, P>>,
    changed: Condvar,
    clock: Arc<dyn MonotonicRuntimeClock>,
    max_pending: usize,
    max_queued: usize,
}

/// Thread-safe deep Module owning one complete frame-work lifecycle.
pub struct FrameWorkBroker<K, D, P> {
    shared: Arc<BrokerShared<K, D, P>>,
}

impl<K, D, P> Clone for FrameWorkBroker<K, D, P> {
    fn clone(&self) -> Self {
        Self { shared: Arc::clone(&self.shared) }
    }
}

impl<K, D, P> FrameWorkBroker<K, D, P>
where
    K: Clone + Eq + Hash,
    D: Copy + PartialEq,
{
    /// Construct a broker with strict nonzero pending and queued budgets.
    pub fn new(max_pending: usize, max_queued: usize) -> Self {
        Self::new_with_clock(
            max_pending,
            max_queued,
            SystemMonotonicRuntimeClock::default(),
        )
    }

    /// Construct a broker with an explicit production or Headless clock Adapter.
    pub fn new_with_clock<C>(max_pending: usize, max_queued: usize, clock: C) -> Self
    where
        C: MonotonicRuntimeClock,
    {
        Self {
            shared: Arc::new(BrokerShared {
                state: Mutex::new(BrokerState {
                    latest_generation: 0,
                    active_playback_demand: None,
                    next_execution_id: 1,
                    pending: HashMap::new(),
                    queue: VecDeque::new(),
                    in_flight: HashMap::new(),
                    last_observed_at: MonotonicTimestamp::ZERO,
                    clock_regression_active: false,
                    closed_at: None,
                    worker_lifecycle_revision: 0,
                    metrics: FrameWorkBrokerMetrics::default(),
                }),
                changed: Condvar::new(),
                clock: Arc::new(clock),
                max_pending: max_pending.max(1),
                max_queued: max_queued.max(1),
            }),
        }
    }

    /// Begin a new latest-wins generation.
    pub fn begin_generation(&self) -> u64 {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        for execution in state.in_flight.values_mut() {
            execution.presentation_binding_expired = false;
        }
        state.latest_generation = state.latest_generation.saturating_add(1);
        let queued_before = state.queue.len();
        prune_obsolete_locked(&mut state);
        let pruned = queued_before.saturating_sub(state.queue.len());
        state.metrics.pruned_queued = state.metrics.pruned_queued.saturating_add(pruned as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
        state.latest_generation
    }

    /// Begin a new generation while preserving bounded Playback decode locality.
    ///
    /// Running leases lose presentation authority before old bindings are
    /// pruned, so their results can only resolve as stale/cache-only. Queued
    /// Playback work that explicitly preserves decoder locality is rebound to
    /// the new generation: its semantic media key and Playback Epoch remain
    /// valid, and discarding the bounded forward queue would force the decoder
    /// to reopen or seek after a Viewer-only rotation.
    ///
    /// This seam is reserved for Viewer-only changes whose source, Playback
    /// Epoch, and authored semantics remain identical. Seek, source authoring,
    /// Project authoring, and lifecycle changes must use
    /// [`Self::begin_generation`].
    pub fn begin_generation_preserving_playback_locality(&self) -> u64 {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        for execution in state.in_flight.values_mut() {
            if execution.work_class == FrameWorkClass::Playback
                && execution.in_flight_deadline_policy
                    == FrameInFlightDeadlinePolicy::FinishForLocality
                && execution.completed_at.is_none()
                && execution.preempted_at.is_none()
                && execution.invalidated_at.is_none()
            {
                execution.presentation_binding_expired = true;
            }
        }
        state.latest_generation = state.latest_generation.saturating_add(1);
        let latest_generation = state.latest_generation;
        let mut rebound_bindings = Vec::new();
        for queued in &mut state.queue {
            if queued.request.work_class == FrameWorkClass::Playback
                && queued.request.in_flight_deadline_policy
                    == FrameInFlightDeadlinePolicy::FinishForLocality
            {
                queued.request.generation = latest_generation;
                rebound_bindings.push((queued.request.key.clone(), binding_for(&queued.request)));
            }
        }
        for (key, binding) in rebound_bindings {
            if let Some(pending) = state.pending.get_mut(&key) {
                pending.binding = binding;
            }
        }
        let queued_before = state.queue.len();
        prune_obsolete_locked(&mut state);
        let pruned = queued_before.saturating_sub(state.queue.len());
        state.metrics.pruned_queued = state.metrics.pruned_queued.saturating_add(pruned as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
        state.latest_generation
    }

    /// Begin a new generation while preserving only running Playback locality.
    ///
    /// This transition expires publication authority for an already-running
    /// [`FrameInFlightDeadlinePolicy::FinishForLocality`] Playback lease, but
    /// deliberately does not rebind queued work. It is the exact boundary for
    /// representation changes such as adaptive Preview resolution: finishing
    /// one bounded decode keeps the worker-owned demux/codec Session warm,
    /// while pruning old-representation prefetch lets the new current frame
    /// enter the lane immediately.
    ///
    /// Source, Playback Epoch, authored semantics, and lifecycle changes must
    /// use [`Self::begin_generation`]. Viewer-only changes whose queued media
    /// identities remain valid should use
    /// [`Self::begin_generation_preserving_playback_locality`].
    pub fn begin_generation_preserving_in_flight_playback_locality(&self) -> u64 {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        for execution in state.in_flight.values_mut() {
            if execution.work_class == FrameWorkClass::Playback
                && execution.in_flight_deadline_policy
                    == FrameInFlightDeadlinePolicy::FinishForLocality
                && execution.completed_at.is_none()
                && execution.preempted_at.is_none()
                && execution.invalidated_at.is_none()
            {
                execution.presentation_binding_expired = true;
            }
        }
        state.latest_generation = state.latest_generation.saturating_add(1);
        let queued_before = state.queue.len();
        prune_obsolete_locked(&mut state);
        let pruned = queued_before.saturating_sub(state.queue.len());
        state.metrics.pruned_queued = state.metrics.pruned_queued.saturating_add(pruned as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
        state.latest_generation
    }

    /// Observe an externally allocated generation and prune older queued bindings.
    pub fn prune_before(&self, generation: u64) -> usize {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        state.latest_generation = state.latest_generation.max(generation);
        let before = state.queue.len();
        prune_obsolete_locked(&mut state);
        refresh_in_flight_invalidations_locked(&mut state, now);
        let pruned = before.saturating_sub(state.queue.len());
        state.metrics.pruned_queued = state.metrics.pruned_queued.saturating_add(pruned as u64);
        self.shared.changed.notify_all();
        pruned
    }

    /// Atomically admit, queue, update, or bind one request to in-flight work.
    pub fn submit(&self, mut request: FrameWorkRequest<K, D, P>) -> FrameWorkSubmission<K> {
        let mut state = lock_state(&self.shared.state);
        if state.closed_at.is_some() {
            return FrameWorkSubmission::Closed;
        }
        if !priority_accepts(request.priority, request.work_class) {
            bump(&mut state.metrics.dropped_invalid_class);
            return FrameWorkSubmission::DroppedInvalidClass;
        }
        if request.generation < state.latest_generation {
            bump(&mut state.metrics.dropped_obsolete_generation);
            return FrameWorkSubmission::DroppedObsoleteGeneration;
        }
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let deadline_at = lowered_deadline_at(request.deadline, now);

        if let Some(previous) = state.pending.get(&request.key).copied() {
            let previous_binding = previous.binding;
            let requested_priority = request.priority;
            let requested_class = request.work_class;
            let requested_deadline_policy = request.in_flight_deadline_policy;
            let requested_execution_budget = request.execution_cancellation_budget;
            request.priority = promote_priority(previous_binding.priority, requested_priority);
            request.work_class = promote_class(
                previous_binding.priority,
                previous_binding.work_class,
                requested_priority,
                requested_class,
            );
            request.in_flight_deadline_policy = promote_in_flight_deadline_policy(
                previous_binding.priority,
                previous.in_flight_deadline_policy,
                requested_priority,
                requested_deadline_policy,
            );
            request.execution_cancellation_budget = promote_execution_cancellation_budget(
                previous_binding.priority,
                previous.execution_cancellation_budget,
                requested_priority,
                requested_execution_budget,
            );
            let binding = binding_for(&request);
            restore_same_generation_playback_demand_binding_locked(
                &mut state,
                &request.key,
                binding,
            );
            if let Some(index) =
                state.queue.iter().position(|queued| queued.request.key == request.key)
            {
                let priority_promoted = previous_binding.priority != binding.priority;
                let work_class_changed = previous_binding.work_class != binding.work_class;
                let generation_changed = previous_binding.generation != binding.generation;
                state.pending.insert(
                    request.key.clone(),
                    PendingBinding {
                        binding,
                        requested_at: if previous_binding != binding {
                            now
                        } else {
                            previous.requested_at
                        },
                        deadline_at,
                        in_flight_deadline_policy: request.in_flight_deadline_policy,
                        execution_cancellation_budget: request.execution_cancellation_budget,
                    },
                );
                state.queue[index] = QueuedWork { request, deadline_at };
                refresh_in_flight_invalidations_locked(&mut state, now);
                bump(&mut state.metrics.submitted_updated_queued);
                if work_class_changed {
                    bump(&mut state.metrics.submitted_work_class_changes);
                }
                self.shared.changed.notify_all();
                return FrameWorkSubmission::UpdatedQueued {
                    priority_promoted,
                    work_class_changed,
                    generation_changed,
                };
            }

            let compatible_in_flight = state
                .in_flight
                .values()
                .any(|execution| execution_matches_binding(execution, &request.key, binding));
            if previous_binding.work_class == binding.work_class && compatible_in_flight {
                state.pending.insert(
                    request.key.clone(),
                    PendingBinding {
                        binding,
                        requested_at: if previous_binding != binding {
                            now
                        } else {
                            previous.requested_at
                        },
                        deadline_at,
                        in_flight_deadline_policy: request.in_flight_deadline_policy,
                        execution_cancellation_budget: request.execution_cancellation_budget,
                    },
                );
                refresh_in_flight_invalidations_locked(&mut state, now);
                bump(&mut state.metrics.submitted_reused_in_flight);
                self.shared.changed.notify_all();
                return FrameWorkSubmission::ReusedInFlight;
            }

            let Some(eviction) = plan_queue_eviction(&state, self.shared.max_queued, &request)
            else {
                bump(&mut state.metrics.dropped_backpressure);
                return FrameWorkSubmission::DroppedBackpressure;
            };
            let (evicted_prefetch, evicted_still) = apply_eviction(&mut state, eviction, now);
            state.pending.insert(
                request.key.clone(),
                PendingBinding {
                    binding,
                    requested_at: now,
                    deadline_at,
                    in_flight_deadline_policy: request.in_flight_deadline_policy,
                    execution_cancellation_budget: request.execution_cancellation_budget,
                },
            );
            if previous_binding.work_class != binding.work_class {
                bump(&mut state.metrics.submitted_work_class_changes);
            }
            state.queue.push_back(QueuedWork { request, deadline_at });
            refresh_in_flight_invalidations_locked(&mut state, now);
            bump(&mut state.metrics.submitted_queued);
            self.shared.changed.notify_all();
            return FrameWorkSubmission::Queued { evicted_prefetch, evicted_still };
        }

        prune_obsolete_locked(&mut state);
        refresh_in_flight_invalidations_locked(&mut state, now);
        let Some(eviction) = plan_new_admission_eviction(
            &state,
            self.shared.max_pending,
            self.shared.max_queued,
            &request,
        ) else {
            bump(&mut state.metrics.dropped_backpressure);
            return FrameWorkSubmission::DroppedBackpressure;
        };
        let (evicted_prefetch, evicted_still) = apply_eviction(&mut state, eviction, now);
        state.pending.insert(
            request.key.clone(),
            PendingBinding {
                binding: binding_for(&request),
                requested_at: now,
                deadline_at,
                in_flight_deadline_policy: request.in_flight_deadline_policy,
                execution_cancellation_budget: request.execution_cancellation_budget,
            },
        );
        state.queue.push_back(QueuedWork { request, deadline_at });
        refresh_in_flight_invalidations_locked(&mut state, now);
        bump(&mut state.metrics.submitted_queued);
        self.shared.changed.notify_all();
        FrameWorkSubmission::Queued { evicted_prefetch, evicted_still }
    }

    /// Atomically update already-owned work without replacing its payload.
    ///
    /// The update succeeds only for a same-key, same-resource-scope queued
    /// payload or compatible in-flight lease. [`FrameWorkBindingSubmission::NeedsPayload`]
    /// leaves the Broker unchanged and tells the Adapter to acquire physical
    /// ownership before using [`Self::submit`].
    pub fn bind_existing(
        &self,
        mut request: FrameWorkBindingRequest<K, D>,
    ) -> FrameWorkBindingSubmission {
        let mut state = lock_state(&self.shared.state);
        if state.closed_at.is_some() {
            return FrameWorkBindingSubmission::Closed;
        }
        if !priority_accepts(request.priority, request.work_class) {
            bump(&mut state.metrics.dropped_invalid_class);
            return FrameWorkBindingSubmission::DroppedInvalidClass;
        }
        if request.generation < state.latest_generation {
            bump(&mut state.metrics.dropped_obsolete_generation);
            return FrameWorkBindingSubmission::DroppedObsoleteGeneration;
        }
        let Some(previous) = state.pending.get(&request.key).copied() else {
            return FrameWorkBindingSubmission::NeedsPayload;
        };
        if previous.binding.resource_scope != request.resource_scope {
            return FrameWorkBindingSubmission::NeedsPayload;
        }

        let previous_binding = previous.binding;
        let requested_priority = request.priority;
        let requested_class = request.work_class;
        let requested_deadline_policy = request.in_flight_deadline_policy;
        let requested_execution_budget = request.execution_cancellation_budget;
        request.priority = promote_priority(previous_binding.priority, requested_priority);
        request.work_class = promote_class(
            previous_binding.priority,
            previous_binding.work_class,
            requested_priority,
            requested_class,
        );
        request.in_flight_deadline_policy = promote_in_flight_deadline_policy(
            previous_binding.priority,
            previous.in_flight_deadline_policy,
            requested_priority,
            requested_deadline_policy,
        );
        request.execution_cancellation_budget = promote_execution_cancellation_budget(
            previous_binding.priority,
            previous.execution_cancellation_budget,
            requested_priority,
            requested_execution_budget,
        );
        let binding = binding_for_metadata(
            request.generation,
            request.priority,
            request.work_class,
            request.resource_scope,
            request.demand_identity,
            request.deadline,
        );
        let queued_index = state.queue.iter().position(|queued| {
            queued.request.key == request.key
                && queued.request.resource_scope == request.resource_scope
        });
        let compatible_in_flight = state
            .in_flight
            .values()
            .any(|execution| execution_matches_binding(execution, &request.key, binding));
        if queued_index.is_none() && !compatible_in_flight {
            return FrameWorkBindingSubmission::NeedsPayload;
        }

        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let deadline_at = lowered_deadline_at(request.deadline, now);
        let priority_promoted = previous_binding.priority != binding.priority;
        let work_class_changed = previous_binding.work_class != binding.work_class;
        let generation_changed = previous_binding.generation != binding.generation;
        state.pending.insert(
            request.key.clone(),
            PendingBinding {
                binding,
                requested_at: if previous_binding != binding {
                    now
                } else {
                    previous.requested_at
                },
                deadline_at,
                in_flight_deadline_policy: request.in_flight_deadline_policy,
                execution_cancellation_budget: request.execution_cancellation_budget,
            },
        );

        if let Some(index) = queued_index {
            let queued = &mut state.queue[index];
            queued.request.generation = request.generation;
            queued.request.priority = request.priority;
            queued.request.work_class = request.work_class;
            queued.request.resource_scope = request.resource_scope;
            queued.request.demand_identity = request.demand_identity;
            queued.request.deadline = request.deadline;
            queued.request.in_flight_deadline_policy = request.in_flight_deadline_policy;
            queued.request.execution_cancellation_budget = request.execution_cancellation_budget;
            queued.deadline_at = deadline_at;
            refresh_in_flight_invalidations_locked(&mut state, now);
            bump(&mut state.metrics.submitted_updated_queued);
            if work_class_changed {
                bump(&mut state.metrics.submitted_work_class_changes);
            }
            self.shared.changed.notify_all();
            FrameWorkBindingSubmission::UpdatedQueued {
                priority_promoted,
                work_class_changed,
                generation_changed,
            }
        } else {
            refresh_in_flight_invalidations_locked(&mut state, now);
            bump(&mut state.metrics.submitted_reused_in_flight);
            self.shared.changed.notify_all();
            FrameWorkBindingSubmission::ReusedInFlight
        }
    }

    /// Block until eligible work or closure, then create one execution lease.
    pub fn receive(&self, lane: FrameWorkerLane) -> Option<FrameWorkReceive<K, D, P>> {
        let mut state = lock_state(&self.shared.state);
        loop {
            let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
            if let Some(work) = dequeue_work_locked(&mut state, lane, now) {
                return Some(work);
            }
            if state.closed_at.is_some() {
                return None;
            }
            if let Some(wait_for) = next_non_playback_current_failover_wait(&state, lane, now) {
                let (next_state, _) = wait_state_timeout(&self.shared.changed, state, wait_for);
                state = next_state;
            } else {
                state = wait_state(&self.shared.changed, state);
            }
        }
    }

    /// Wait for eligible work while exposing an idle lifecycle boundary.
    ///
    /// Adapters use the timeout to release access-pattern-local decoder
    /// sessions without polling the broker or weakening its queue authority.
    pub fn receive_timeout(
        &self,
        lane: FrameWorkerLane,
        timeout: Duration,
    ) -> FrameWorkReceiveWait<K, D, P> {
        let wait_started = Instant::now();
        let mut state = lock_state(&self.shared.state);
        loop {
            let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
            if let Some(work) = dequeue_work_locked(&mut state, lane, now) {
                return FrameWorkReceiveWait::Work(work);
            }
            if state.closed_at.is_some() {
                return FrameWorkReceiveWait::Closed;
            }
            let remaining = timeout.saturating_sub(wait_started.elapsed());
            if remaining.is_zero() {
                return FrameWorkReceiveWait::TimedOut;
            }
            let failover_wait =
                next_non_playback_current_failover_wait(&state, lane, now).unwrap_or(remaining);
            let wait_for = remaining.min(failover_wait);
            let (next_state, timed_out) = wait_state_timeout(&self.shared.changed, state, wait_for);
            state = next_state;
            if timed_out && wait_for == remaining && wait_started.elapsed() >= timeout {
                return FrameWorkReceiveWait::TimedOut;
            }
        }
    }

    /// Wait for work, timeout, closure, or a newer worker-lifecycle revision.
    ///
    /// The revision and condition variable share the Broker lock, preventing
    /// the lost-wakeup race that an external atomic flag plus `notify_all`
    /// would permit between a worker's predicate check and its wait.
    pub fn receive_timeout_after_lifecycle_revision(
        &self,
        lane: FrameWorkerLane,
        timeout: Duration,
        observed_revision: u64,
    ) -> FrameWorkReceiveWait<K, D, P> {
        let wait_started = Instant::now();
        let mut state = lock_state(&self.shared.state);
        loop {
            if state.worker_lifecycle_revision != observed_revision {
                return FrameWorkReceiveWait::Interrupted {
                    revision: state.worker_lifecycle_revision,
                };
            }
            let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
            if let Some(work) = dequeue_work_locked(&mut state, lane, now) {
                return FrameWorkReceiveWait::Work(work);
            }
            if state.closed_at.is_some() {
                return FrameWorkReceiveWait::Closed;
            }
            let remaining = timeout.saturating_sub(wait_started.elapsed());
            if remaining.is_zero() {
                return FrameWorkReceiveWait::TimedOut;
            }
            let failover_wait =
                next_non_playback_current_failover_wait(&state, lane, now).unwrap_or(remaining);
            let wait_for = remaining.min(failover_wait);
            let (next_state, timed_out) = wait_state_timeout(&self.shared.changed, state, wait_for);
            state = next_state;
            if timed_out && wait_for == remaining && wait_started.elapsed() >= timeout {
                return FrameWorkReceiveWait::TimedOut;
            }
        }
    }

    /// Current worker-lifecycle revision for a newly attached receiver.
    pub fn worker_lifecycle_revision(&self) -> u64 {
        lock_state(&self.shared.state).worker_lifecycle_revision
    }

    /// Publish a worker-lifecycle boundary and wake every bounded receiver.
    pub fn interrupt_worker_waits(&self) -> u64 {
        let mut state = lock_state(&self.shared.state);
        state.worker_lifecycle_revision = state.worker_lifecycle_revision.saturating_add(1);
        let revision = state.worker_lifecycle_revision;
        self.shared.changed.notify_all();
        revision
    }

    /// Decide atomically whether an execution must stop for lifecycle or
    /// preemption reasons.
    pub fn execution_cancellation(
        &self,
        id: FrameExecutionId,
    ) -> Option<FrameExecutionCancellation> {
        self.execution_cancellation_evidence(id).map(|evidence| evidence.cancellation)
    }

    /// Decide cancellation and sample execution age under one lifecycle lock.
    pub fn execution_cancellation_evidence(
        &self,
        id: FrameExecutionId,
    ) -> Option<crate::FrameExecutionCancellationEvidence> {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        execution_cancellation_evidence_locked(&state, id, now)
    }

    /// Wait for cancellation, completion, disappearance, or the caller's timeout.
    ///
    /// The predicate and condition variable share the Broker lifecycle lock,
    /// so an event published immediately before or during the wait cannot be
    /// lost. Execution and presentation deadlines also bound the sleep.
    pub fn wait_for_execution_terminal_state(
        &self,
        id: FrameExecutionId,
        timeout: Duration,
    ) -> FrameExecutionWaitStatus {
        let wait_started = Instant::now();
        let mut state = lock_state(&self.shared.state);
        loop {
            let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
            refresh_in_flight_invalidations_locked(&mut state, now);
            let Some(execution) = state.in_flight.get(&id) else {
                return FrameExecutionWaitStatus::Missing;
            };
            if let Some(evidence) = execution_cancellation_evidence_locked(&state, id, now) {
                let cancellation_precedes_completion =
                    execution.completed_at.is_none_or(|completed_at| {
                        evidence.cancellation.request_age().is_some_and(|request_age| {
                            request_age >= elapsed_since(now, completed_at)
                        })
                    });
                if cancellation_precedes_completion {
                    return FrameExecutionWaitStatus::Canceled(evidence);
                }
            }
            if execution.completed_at.is_some() {
                return FrameExecutionWaitStatus::Completed;
            }
            let remaining = timeout.saturating_sub(wait_started.elapsed());
            if remaining.is_zero() {
                return FrameExecutionWaitStatus::Timeout;
            }
            let cancellation_wait =
                next_execution_cancellation_wait(execution, now).unwrap_or(remaining);
            let wait_for = remaining.min(cancellation_wait);
            let (next_state, _) = wait_state_timeout(&self.shared.changed, state, wait_for);
            state = next_state;
        }
    }

    /// Record the worker-return instant without resolving its latest binding.
    ///
    /// Completion is stamped once so later UI/event-loop latency cannot turn
    /// on-time work into a false miss. The lease remains in flight until
    /// [`Self::resolve_execution`] atomically evaluates the latest binding.
    pub fn mark_execution_completed(&self, id: FrameExecutionId) -> bool {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let Some(execution) = state.in_flight.get_mut(&id) else {
            return false;
        };
        if execution.completed_at.is_none() {
            execution.completed_at = Some(now);
        }
        self.shared.changed.notify_all();
        true
    }

    /// Resolve one execution lease atomically against the latest binding.
    pub fn resolve_execution(
        &self,
        id: FrameExecutionId,
        reusable: bool,
    ) -> FrameRequestResolution<D> {
        let mut state = lock_state(&self.shared.state);
        let Some(mut execution) = state.in_flight.remove(&id) else {
            return FrameRequestResolution {
                completion: FrameRequestCompletion::Stale,
                binding: None,
                deadline: FrameWorkDeadlineStatus::NotApplicable,
            };
        };
        let key = execution.key.clone();
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        if execution.completed_at.is_none() {
            execution.completed_at = Some(now);
        }
        let deadline =
            deadline_status(execution.deadline_at, execution.completed_at.unwrap_or(now));
        let cause = completion_cause(&state, &execution, reusable);
        let resolution = resolve_locked(&mut state, execution, reusable, deadline);
        remove_orphaned_pending_binding_locked(&mut state, &key);
        refresh_in_flight_invalidations_locked(&mut state, now);
        record_completion(&mut state.metrics, resolution.completion, cause);
        self.shared.changed.notify_all();
        resolution
    }

    /// Resolve synchronous or externally executed work that never held a worker lease.
    pub fn resolve_unleased(
        &self,
        key: K,
        generation: u64,
        work_class: FrameWorkClass,
        resource_scope: FrameWorkResourceScope,
        demand_identity: Option<FrameDemandIdentity>,
        reusable: bool,
    ) -> FrameRequestResolution<D> {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let execution_key = key.clone();
        let mut execution = InFlightWork {
            key,
            generation,
            priority: FrameWorkPriority::Current,
            work_class,
            resource_scope,
            worker_lane: None,
            demand_identity,
            started_at: now,
            deadline_at: None,
            in_flight_deadline_policy: FrameInFlightDeadlinePolicy::Cancel,
            execution_cancellation_deadline_at: None,
            completed_at: Some(now),
            preempted_at: None,
            invalidated_at: None,
            presentation_binding_expired: false,
        };
        if let Some(current) =
            current_pending_binding(state.latest_generation, &state.pending, &execution)
        {
            execution.deadline_at = current.deadline_at;
        }
        let deadline = deadline_status(execution.deadline_at, now);
        let cause = completion_cause(&state, &execution, reusable);
        let resolution = resolve_locked(&mut state, execution, reusable, deadline);
        remove_orphaned_pending_binding_locked(&mut state, &execution_key);
        refresh_in_flight_invalidations_locked(&mut state, now);
        record_completion(&mut state.metrics, resolution.completion, cause);
        self.shared.changed.notify_all();
        resolution
    }

    /// Abandon a lease whose result cannot reach the completion Adapter.
    pub fn abandon_execution(&self, id: FrameExecutionId) -> bool {
        let mut state = lock_state(&self.shared.state);
        let removed = state.in_flight.remove(&id);
        if let Some(execution) = &removed {
            remove_orphaned_pending_binding_locked(&mut state, &execution.key);
            self.shared.changed.notify_all();
        }
        removed.is_some()
    }

    /// Fail one execution and atomically consume the latest compatible binding.
    ///
    /// A failed execution has no reusable value, so it must not pass through
    /// [`Self::resolve_execution`]. Returning the latest binding gives the
    /// domain Adapter exact terminal-delivery authority, including a demand
    /// rebound to compatible in-flight work. `None` means the execution was
    /// already gone or superseded; in that case no terminal delivery is legal.
    pub fn fail_execution(&self, id: FrameExecutionId) -> Option<FrameRequestBinding<D>> {
        let mut state = lock_state(&self.shared.state);
        let execution = state.in_flight.remove(&id)?;
        let preserve_preempted_fallback =
            execution.preempted_at.is_some() && has_preempted_fallback(&state, &execution);
        let exact_binding =
            exact_pending_binding(state.latest_generation, &state.pending, &execution)
                .map(|pending| pending.binding);
        let superseded_playback_binding = exact_binding.is_some_and(|binding| {
            playback_current_binding_is_superseded(state.active_playback_demand, binding)
        });
        let binding = if execution.presentation_binding_expired
            || preserve_preempted_fallback
            || superseded_playback_binding
        {
            None
        } else {
            exact_binding
        };
        if binding.is_some() || superseded_playback_binding {
            state.pending.remove(&execution.key);
        }
        let inactive_orphan = state.pending.get(&execution.key).is_some_and(|pending| {
            playback_current_binding_is_superseded(state.active_playback_demand, pending.binding)
        }) && !state
            .queue
            .iter()
            .any(|queued| queued.request.key == execution.key)
            && !state.in_flight.values().any(|candidate| candidate.key == execution.key);
        if inactive_orphan {
            state.pending.remove(&execution.key);
        }
        remove_orphaned_pending_binding_locked(&mut state, &execution.key);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
        binding
    }

    /// Cancel one key across pending and queued state; in-flight leases become stale.
    pub fn cancel_key(&self, key: &K) -> usize {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let pending = usize::from(state.pending.remove(key).is_some());
        let before = state.queue.len();
        state.queue.retain(|queued| &queued.request.key != key);
        for execution in state.in_flight.values_mut().filter(|execution| &execution.key == key) {
            execution.presentation_binding_expired = false;
            execution.invalidated_at.get_or_insert(now);
        }
        let queued = before.saturating_sub(state.queue.len());
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(pending as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        if pending > 0 || queued > 0 {
            self.shared.changed.notify_all();
        }
        queued
    }

    /// Cancel all pending/queued work and start a new generation.
    pub fn cancel_all(&self) -> (u64, usize) {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let canceled = state.pending.len() as u64;
        let queued = state.queue.len();
        state.pending.clear();
        state.queue.clear();
        for execution in state.in_flight.values_mut() {
            execution.presentation_binding_expired = false;
        }
        state.active_playback_demand = None;
        state.latest_generation = state.latest_generation.saturating_add(1);
        refresh_in_flight_invalidations_locked(&mut state, now);
        state.metrics.canceled_requests = state.metrics.canceled_requests.saturating_add(canceled);
        self.shared.changed.notify_all();
        (state.latest_generation, queued)
    }

    /// Remove obsolete pending/queued work under the same lifecycle lock.
    pub fn prune_obsolete(&self) -> usize {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let before = state.queue.len();
        prune_obsolete_locked(&mut state);
        refresh_in_flight_invalidations_locked(&mut state, now);
        let pruned = before.saturating_sub(state.queue.len());
        state.metrics.pruned_queued = state.metrics.pruned_queued.saturating_add(pruned as u64);
        if pruned > 0 {
            self.shared.changed.notify_all();
        }
        pruned
    }

    /// Synchronize the one active Playback demand and discard older unstarted current work.
    ///
    /// All media keys carrying `active` remain eligible so a multi-layer
    /// current frame is never collapsed to one payload. Prefetch,
    /// interactive/still work, and work without Playback demand identity are
    /// outside this authority. A superseded binding already represented by a
    /// compatible in-flight lease is retained only as a locality/completion
    /// seam; [`Self::resolve_execution`] prevents that old identity from
    /// regaining current publication authority.
    pub fn synchronize_playback_current_demand(&self, active: FrameDemandIdentity) -> usize {
        let mut state = lock_state(&self.shared.state);
        state.active_playback_demand = Some(active);
        let latest_generation = state.latest_generation;
        let BrokerState { pending, in_flight, .. } = &mut *state;
        for execution in in_flight.values_mut() {
            let rebound_to_active_demand = pending.get(&execution.key).is_some_and(|pending| {
                pending.binding.generation >= latest_generation
                    && pending.binding.work_class == execution.work_class
                    && pending.binding.resource_scope == execution.resource_scope
                    && pending.binding.demand_identity == Some(active)
            });
            if execution.priority == FrameWorkPriority::Current
                && execution.work_class == FrameWorkClass::Playback
                && execution.demand_identity.is_some_and(|identity| identity != active)
                && !rebound_to_active_demand
                && execution.in_flight_deadline_policy
                    == FrameInFlightDeadlinePolicy::FinishForLocality
                && execution.completed_at.is_none()
                && execution.preempted_at.is_none()
                && execution.invalidated_at.is_none()
            {
                execution.presentation_binding_expired = true;
            }
        }
        let superseded_keys = state
            .queue
            .iter()
            .filter(|queued| {
                queued.request.priority == FrameWorkPriority::Current
                    && queued.request.work_class == FrameWorkClass::Playback
                    && queued.request.demand_identity.is_some_and(|identity| identity != active)
            })
            .map(|queued| queued.request.key.clone())
            .collect::<Vec<_>>();
        state.queue.retain(|queued| {
            !(queued.request.priority == FrameWorkPriority::Current
                && queued.request.work_class == FrameWorkClass::Playback
                && queued.request.demand_identity.is_some_and(|identity| identity != active))
        });
        let mut removed_pending = 0usize;
        for key in &superseded_keys {
            let Some(pending) = state.pending.get(key).copied() else {
                continue;
            };
            if !playback_current_binding_is_superseded(Some(active), pending.binding) {
                continue;
            }
            let compatible_in_flight = state.in_flight.values().any(|execution| {
                &execution.key == key
                    && execution.work_class == pending.binding.work_class
                    && execution.resource_scope == pending.binding.resource_scope
            });
            if !compatible_in_flight {
                state.pending.remove(key);
                removed_pending = removed_pending.saturating_add(1);
            }
        }
        let removed = superseded_keys.len();
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(removed_pending as u64);
        state.metrics.superseded_queued_playback_current =
            state.metrics.superseded_queued_playback_current.saturating_add(removed as u64);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
        removed
    }

    /// Expire latest Playback-current bindings older than `max_age`.
    ///
    /// Interactive scrub work is deliberately excluded. It has latest-wins
    /// cancellation through generations, but no presentation deadline and may
    /// legitimately spend longer than a playback stall window opening a GOP
    /// or building source-session evidence.
    pub fn expire_playback_current_older_than(
        &self,
        max_age: Duration,
    ) -> Vec<ExpiredFrameWork<K, D>> {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let latest = state.latest_generation;
        let mut expired = state
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending.binding.priority == FrameWorkPriority::Current
                    && pending.binding.work_class == FrameWorkClass::Playback
                    && pending.binding.generation >= latest
                    && elapsed_since(now, pending.requested_at) >= max_age
            })
            .map(|(key, pending)| ExpiredFrameWork {
                key: key.clone(),
                binding: pending.binding,
                removed_queued_work: 0,
                retained_in_flight_work: 0,
            })
            .collect::<Vec<_>>();
        for request in &mut expired {
            state.pending.remove(&request.key);
            let queued_before = state.queue.len();
            state.queue.retain(|queued| queued.request.key != request.key);
            request.removed_queued_work = queued_before.saturating_sub(state.queue.len());
            let mut retained_in_flight_work = 0usize;
            for execution in state.in_flight.values_mut().filter(|execution| {
                execution.key == request.key
                    && execution.generation == request.binding.generation
                    && execution.generation >= latest
                    && execution.work_class == request.binding.work_class
                    && execution.resource_scope == request.binding.resource_scope
                    && execution.in_flight_deadline_policy
                        == FrameInFlightDeadlinePolicy::FinishForLocality
                    && execution.invalidated_at.is_none()
                    && execution.preempted_at.is_none()
            }) {
                execution.presentation_binding_expired = true;
                retained_in_flight_work = retained_in_flight_work.saturating_add(1);
            }
            request.retained_in_flight_work = retained_in_flight_work;
        }
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(expired.len() as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        if !expired.is_empty() {
            self.shared.changed.notify_all();
        }
        expired
    }

    /// Close the broker, clear pending/queued work, and wake every worker.
    pub fn close(&self) {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        if state.closed_at.is_none() {
            state.closed_at = Some(now);
        }
        state.active_playback_demand = None;
        state.pending.clear();
        state.queue.clear();
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
    }

    /// Return whether one semantic key still owns a pending binding.
    pub fn has_pending_key(&self, key: &K) -> bool {
        lock_state(&self.shared.state).pending.contains_key(key)
    }

    /// Return whether one exact latest binding still has queued or in-flight ownership.
    ///
    /// This is the level predicate for an Adapter that atomically reused work
    /// through [`Self::bind_existing`]. Unlike aggregate diagnostics, it cannot
    /// be kept alive by unrelated work, and unlike [`Self::has_pending_key`],
    /// it refuses an orphaned binding whose physical execution owner vanished.
    pub fn binding_has_execution_owner(
        &self,
        key: &K,
        generation: u64,
        work_class: FrameWorkClass,
        resource_scope: FrameWorkResourceScope,
        demand_identity: Option<FrameDemandIdentity>,
    ) -> bool {
        let state = lock_state(&self.shared.state);
        let Some(pending) = state.pending.get(key) else {
            return false;
        };
        let binding = pending.binding;
        if binding.generation != generation
            || binding.generation < state.latest_generation
            || binding.work_class != work_class
            || binding.resource_scope != resource_scope
            || binding.demand_identity != demand_identity
        {
            return false;
        }

        pending_binding_has_execution_owner_locked(&state, key, binding)
    }

    /// Snapshot every semantic key with a live latest-generation binding.
    ///
    /// Payload ownership remains inside the Broker. Coordinators use this
    /// read-only identity projection to reserve downstream residency before
    /// admitting more speculative work.
    pub fn pending_keys(&self) -> Vec<K> {
        let state = lock_state(&self.shared.state);
        state
            .pending
            .iter()
            .filter(|(_, pending)| pending.binding.generation >= state.latest_generation)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Snapshot every key that still owns queued or in-flight execution work.
    ///
    /// Unlike [`Self::pending_keys`], this retains canceled or superseded
    /// in-flight attempts until their worker lease actually returns. Resource
    /// coordinators use this projection to avoid releasing physical execution
    /// reservations merely because publication authority was canceled.
    pub fn active_keys(&self) -> Vec<K> {
        let state = lock_state(&self.shared.state);
        let mut keys = state.pending.keys().cloned().collect::<Vec<_>>();
        for execution in state.in_flight.values() {
            if !keys.contains(&execution.key) {
                keys.push(execution.key.clone());
            }
        }
        keys
    }

    /// Cancel one queued speculative key that has no in-flight execution.
    ///
    /// This is the safe preemption seam for a visible current request that
    /// needs to reclaim an outstanding speculative resource reservation.
    /// In-flight work is never reported as reclaimable.
    pub fn cancel_one_queued_prefetch(&self) -> Option<K> {
        let mut state = lock_state(&self.shared.state);
        let index = state.queue.iter().position(|queued| {
            queued.request.priority == FrameWorkPriority::Prefetch
                && !state.in_flight.values().any(|execution| execution.key == queued.request.key)
        })?;
        let queued = state.queue.remove(index)?;
        let key = queued.request.key;
        let removed_pending = state.pending.remove(&key).is_some();
        if removed_pending {
            state.metrics.canceled_requests = state.metrics.canceled_requests.saturating_add(1);
            state.metrics.evicted_prefetch = state.metrics.evicted_prefetch.saturating_add(1);
        }
        self.shared.changed.notify_all();
        Some(key)
    }

    /// Request cooperative cancellation from at most one in-flight Prefetch.
    ///
    /// This Seam exists for a visible Current request whose physical resource
    /// reservation is blocked after queued Prefetch reclamation. It marks only
    /// a live, not-yet-completed, not-already-preempted lease. Ownership and
    /// resource charges remain with the worker until normal completion/failure
    /// resolution drops its payload; completed work remains completion-pump
    /// responsibility.
    pub fn request_one_in_flight_prefetch_preemption(&self) -> bool {
        let mut state = lock_state(&self.shared.state);
        if state.closed_at.is_some() {
            return false;
        }
        let candidate = state
            .in_flight
            .iter()
            .filter(|(_, execution)| {
                execution.priority == FrameWorkPriority::Prefetch
                    && execution.completed_at.is_none()
                    && execution.preempted_at.is_none()
            })
            .min_by_key(|(id, execution)| (execution.started_at, id.get()))
            .map(|(id, _)| *id);
        let Some(candidate) = candidate else {
            return false;
        };
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let Some(execution) = state.in_flight.get_mut(&candidate) else {
            return false;
        };
        execution.preempted_at = Some(now.max(execution.started_at));
        self.shared.changed.notify_all();
        true
    }

    /// Return whether one key/generation/class/scope remains compatible with latest work.
    pub fn key_current(
        &self,
        key: &K,
        generation: u64,
        work_class: FrameWorkClass,
        resource_scope: FrameWorkResourceScope,
    ) -> bool {
        let mut state = lock_state(&self.shared.state);
        let Some(pending) = state.pending.get(key).copied() else {
            bump(&mut state.metrics.skipped_missing);
            return false;
        };
        if pending.binding.work_class != work_class
            || pending.binding.resource_scope != resource_scope
        {
            bump(&mut state.metrics.skipped_class_mismatch);
            return false;
        }
        if pending.binding.generation >= generation
            && pending.binding.generation >= state.latest_generation
        {
            return true;
        }
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        remove_key_locked(&mut state, key, now);
        refresh_in_flight_invalidations_locked(&mut state, now);
        bump(&mut state.metrics.skipped_obsolete);
        self.shared.changed.notify_all();
        false
    }

    /// Return whether current work other than the protected key/scope is pending.
    pub fn has_other_current_key(
        &self,
        key: &K,
        resource_scope: FrameWorkResourceScope,
        realtime_only: bool,
    ) -> bool {
        let state = lock_state(&self.shared.state);
        state.pending.iter().any(|(pending_key, pending)| {
            (pending_key != key || pending.binding.resource_scope != resource_scope)
                && pending.binding.priority == FrameWorkPriority::Current
                && (!realtime_only || pending.binding.work_class != FrameWorkClass::Still)
                && pending.binding.generation >= state.latest_generation
        })
    }

    /// Return latest still keys outside one protected key/scope that realtime work may preempt.
    pub fn pending_still_except(
        &self,
        protected_key: &K,
        protected_scope: FrameWorkResourceScope,
    ) -> Vec<K> {
        let state = lock_state(&self.shared.state);
        state
            .pending
            .iter()
            .filter(|(key, pending)| {
                (*key != protected_key || pending.binding.resource_scope != protected_scope)
                    && pending.binding.priority == FrameWorkPriority::Current
                    && pending.binding.work_class == FrameWorkClass::Still
                    && pending.binding.generation >= state.latest_generation
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Cancel one still key/scope only while it remains eligible for realtime preemption.
    pub fn cancel_preempted_still(&self, key: &K, resource_scope: FrameWorkResourceScope) -> bool {
        let mut state = lock_state(&self.shared.state);
        let eligible = state.pending.get(key).is_some_and(|pending| {
            pending.binding.priority == FrameWorkPriority::Current
                && pending.binding.work_class == FrameWorkClass::Still
                && pending.binding.resource_scope == resource_scope
                && pending.binding.generation >= state.latest_generation
        });
        if !eligible {
            return false;
        }
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        remove_key_locked(&mut state, key, now);
        refresh_in_flight_invalidations_locked(&mut state, now);
        bump(&mut state.metrics.evicted_still);
        self.shared.changed.notify_all();
        true
    }

    /// Return stable lifecycle and queue evidence.
    pub fn diagnostics(&self) -> FrameWorkBrokerDiagnostics {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let mut diagnostics = FrameWorkBrokerDiagnostics {
            latest_generation: state.latest_generation,
            active_playback_demand: state.active_playback_demand,
            clock_regressions: state.metrics.clock_regressions,
            pending_requests: state.pending.len(),
            queued_work: state.queue.len(),
            in_flight_work: state.in_flight.len(),
            dropped_expired_work: state.metrics.dropped_expired_work,
            dropped_expired_playback_current: state.metrics.dropped_expired_playback_current,
            submitted_queued: state.metrics.submitted_queued,
            submitted_updated_queued: state.metrics.submitted_updated_queued,
            submitted_reused_in_flight: state.metrics.submitted_reused_in_flight,
            submitted_work_class_changes: state.metrics.submitted_work_class_changes,
            dropped_backpressure: state.metrics.dropped_backpressure,
            dropped_invalid_class: state.metrics.dropped_invalid_class,
            dropped_obsolete_generation: state.metrics.dropped_obsolete_generation,
            evicted_prefetch: state.metrics.evicted_prefetch,
            evicted_still: state.metrics.evicted_still,
            canceled_requests: state.metrics.canceled_requests,
            pruned_queued: state.metrics.pruned_queued,
            superseded_queued_playback_current: state.metrics.superseded_queued_playback_current,
            in_flight_generation_invalidations: state.metrics.in_flight_generation_invalidations,
            in_flight_binding_invalidations: state.metrics.in_flight_binding_invalidations,
            in_flight_binding_locality_detachments: state
                .metrics
                .in_flight_binding_locality_detachments,
            completed_current: state.metrics.completed_current,
            completed_cache_only: state.metrics.completed_cache_only,
            completed_stale: state.metrics.completed_stale,
            completed_cache_only_missing: state.metrics.completed_cache_only_missing,
            completed_cache_only_class_mismatch: state.metrics.completed_cache_only_class_mismatch,
            completed_stale_missing: state.metrics.completed_stale_missing,
            completed_stale_class_mismatch: state.metrics.completed_stale_class_mismatch,
            completed_stale_obsolete: state.metrics.completed_stale_obsolete,
            skipped_missing: state.metrics.skipped_missing,
            skipped_class_mismatch: state.metrics.skipped_class_mismatch,
            skipped_obsolete: state.metrics.skipped_obsolete,
            closed: state.closed_at.is_some(),
            ..FrameWorkBrokerDiagnostics::default()
        };
        for queued in &state.queue {
            match queued.request.priority {
                FrameWorkPriority::Current => diagnostics.queued_current += 1,
                FrameWorkPriority::Prefetch => diagnostics.queued_prefetch += 1,
            }
            match queued.request.work_class {
                FrameWorkClass::Playback => diagnostics.queued_playback += 1,
                FrameWorkClass::Interactive => diagnostics.queued_interactive += 1,
                FrameWorkClass::Still => diagnostics.queued_still += 1,
            }
            if queued.request.priority == FrameWorkPriority::Current
                && queued.request.work_class == FrameWorkClass::Playback
                && deadline_expired(queued.deadline_at, now)
            {
                diagnostics.queued_expired_playback_current += 1;
            }
            if deadline_expired(queued.deadline_at, now) {
                diagnostics.queued_expired_work += 1;
            }
        }
        for execution in state.in_flight.values() {
            diagnostics.in_flight_completed += usize::from(execution.completed_at.is_some());
            diagnostics.in_flight_max_age_us = diagnostics
                .in_flight_max_age_us
                .max(duration_micros(elapsed_since(now, execution.started_at)));
            let lifecycle_cancel_at = [
                state.closed_at,
                current_pending_binding(state.latest_generation, &state.pending, execution)
                    .is_none()
                    .then_some(execution.invalidated_at)
                    .flatten(),
                execution.preempted_at,
                (execution.in_flight_deadline_policy == FrameInFlightDeadlinePolicy::Cancel)
                    .then(|| execution.deadline_at.filter(|deadline| *deadline <= now))
                    .flatten(),
                execution.execution_cancellation_deadline_at.filter(|deadline| *deadline <= now),
            ]
            .into_iter()
            .flatten()
            .min();
            if let Some(cancel_at) = lifecycle_cancel_at {
                diagnostics.in_flight_cancellation_requested += 1;
                diagnostics.in_flight_cancellation_max_age_us = diagnostics
                    .in_flight_cancellation_max_age_us
                    .max(duration_micros(elapsed_since(now, cancel_at)));
            }
            match execution.priority {
                FrameWorkPriority::Current => diagnostics.in_flight_current += 1,
                FrameWorkPriority::Prefetch => diagnostics.in_flight_prefetch += 1,
            }
            match execution.work_class {
                FrameWorkClass::Playback => diagnostics.in_flight_playback += 1,
                FrameWorkClass::Interactive => diagnostics.in_flight_interactive += 1,
                FrameWorkClass::Still => diagnostics.in_flight_still += 1,
            }
            if let Some(lane) = execution.worker_lane {
                match lane {
                    FrameWorkerLane::Any => diagnostics.in_flight_any_lane += 1,
                    FrameWorkerLane::Playback => diagnostics.in_flight_playback_lane += 1,
                    FrameWorkerLane::Interactive => diagnostics.in_flight_interactive_lane += 1,
                    FrameWorkerLane::Still => diagnostics.in_flight_still_lane += 1,
                    FrameWorkerLane::NonPlayback => {
                        diagnostics.in_flight_non_playback_lane += 1;
                    }
                }
                if execution.priority == FrameWorkPriority::Current
                    && !lane.accepts(execution.work_class)
                {
                    diagnostics.in_flight_cross_lane_current += 1;
                }
            }
        }
        diagnostics
    }
}

fn binding_for<K, D: Copy, P>(request: &FrameWorkRequest<K, D, P>) -> FrameRequestBinding<D> {
    binding_for_metadata(
        request.generation,
        request.priority,
        request.work_class,
        request.resource_scope,
        request.demand_identity,
        request.deadline,
    )
}

fn binding_for_metadata<D: Copy>(
    generation: u64,
    priority: FrameWorkPriority,
    work_class: FrameWorkClass,
    resource_scope: FrameWorkResourceScope,
    demand_identity: Option<FrameDemandIdentity>,
    deadline: Option<FrameWorkDeadline<D>>,
) -> FrameRequestBinding<D> {
    FrameRequestBinding {
        generation,
        priority,
        work_class,
        resource_scope,
        demand_identity,
        deadline: deadline.map(FrameWorkDeadline::adapter_deadline),
    }
}

fn playback_current_binding_is_superseded<D>(
    active: Option<FrameDemandIdentity>,
    binding: FrameRequestBinding<D>,
) -> bool {
    binding.priority == FrameWorkPriority::Current
        && binding.work_class == FrameWorkClass::Playback
        && binding
            .demand_identity
            .zip(active)
            .is_some_and(|(identity, active)| identity != active)
}

fn execution_matches_binding<K, D>(
    execution: &InFlightWork<K>,
    key: &K,
    binding: FrameRequestBinding<D>,
) -> bool
where
    K: Eq,
{
    &execution.key == key
        && execution.work_class == binding.work_class
        && execution.resource_scope == binding.resource_scope
        && execution.generation == binding.generation
        && execution.demand_identity == binding.demand_identity
        && !execution.presentation_binding_expired
        && execution.preempted_at.is_none()
}

fn restore_same_generation_playback_demand_binding_locked<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    key: &K,
    binding: FrameRequestBinding<D>,
) where
    K: Eq,
    D: Copy,
{
    let active_demand = state.active_playback_demand;
    if binding.priority != FrameWorkPriority::Current
        || binding.work_class != FrameWorkClass::Playback
        || binding.demand_identity != active_demand
    {
        return;
    }
    for execution in state.in_flight.values_mut().filter(|execution| {
        &execution.key == key
            && execution.generation == binding.generation
            && execution.work_class == binding.work_class
            && execution.resource_scope == binding.resource_scope
            && execution.demand_identity != binding.demand_identity
            && execution.in_flight_deadline_policy == FrameInFlightDeadlinePolicy::FinishForLocality
            && execution.completed_at.is_none()
            && execution.preempted_at.is_none()
            && execution.invalidated_at.is_none()
    }) {
        // Demand synchronization detaches an old identity before pruning its
        // queued siblings, keeping the decoder lease alive but cache-only. A
        // later same-generation request for the exact media key may legally
        // bind that reusable result to the active demand. Generation rotation
        // and age expiry cannot enter here: the former changes generation,
        // while the latter retains the same demand identity.
        execution.presentation_binding_expired = false;
    }
}

fn pending_binding_has_execution_owner_locked<K, D, P>(
    state: &BrokerState<K, D, P>,
    key: &K,
    binding: FrameRequestBinding<D>,
) -> bool
where
    K: Eq,
    D: Copy,
{
    state.queue.iter().any(|queued| {
        &queued.request.key == key
            && queued.request.generation == binding.generation
            && queued.request.work_class == binding.work_class
            && queued.request.resource_scope == binding.resource_scope
            && queued.request.demand_identity == binding.demand_identity
    }) || state
        .in_flight
        .values()
        .any(|execution| execution_matches_binding(execution, key, binding))
}

/// Enforce that every pending publication binding retains one physical owner.
fn remove_orphaned_pending_binding_locked<K, D, P>(state: &mut BrokerState<K, D, P>, key: &K)
where
    K: Eq + Hash,
    D: Copy,
{
    let orphaned = state.pending.get(key).is_some_and(|pending| {
        !pending_binding_has_execution_owner_locked(state, key, pending.binding)
    });
    if orphaned {
        state.pending.remove(key);
    }
}

fn lowered_deadline_at<D: Copy>(
    deadline: Option<FrameWorkDeadline<D>>,
    now: MonotonicTimestamp,
) -> Option<MonotonicTimestamp> {
    deadline.map(|deadline| now.saturating_add(deadline.remaining_at_admission()))
}

fn deadline_expired(deadline_at: Option<MonotonicTimestamp>, now: MonotonicTimestamp) -> bool {
    deadline_at.is_some_and(|deadline| deadline <= now)
}

fn priority_accepts(priority: FrameWorkPriority, class: FrameWorkClass) -> bool {
    priority == FrameWorkPriority::Current || class == FrameWorkClass::Playback
}

fn promote_priority(
    existing: FrameWorkPriority,
    requested: FrameWorkPriority,
) -> FrameWorkPriority {
    if existing == FrameWorkPriority::Current || requested == FrameWorkPriority::Current {
        FrameWorkPriority::Current
    } else {
        FrameWorkPriority::Prefetch
    }
}

fn promote_in_flight_deadline_policy(
    existing_priority: FrameWorkPriority,
    existing: FrameInFlightDeadlinePolicy,
    requested_priority: FrameWorkPriority,
    requested: FrameInFlightDeadlinePolicy,
) -> FrameInFlightDeadlinePolicy {
    if existing_priority == FrameWorkPriority::Current
        && requested_priority == FrameWorkPriority::Prefetch
    {
        existing
    } else {
        requested
    }
}

fn promote_execution_cancellation_budget(
    existing_priority: FrameWorkPriority,
    existing: Option<Duration>,
    requested_priority: FrameWorkPriority,
    requested: Option<Duration>,
) -> Option<Duration> {
    if existing_priority == FrameWorkPriority::Current
        && requested_priority == FrameWorkPriority::Prefetch
    {
        existing
    } else {
        requested
    }
}

fn promote_class(
    existing_priority: FrameWorkPriority,
    existing_class: FrameWorkClass,
    requested_priority: FrameWorkPriority,
    requested_class: FrameWorkClass,
) -> FrameWorkClass {
    if requested_priority == FrameWorkPriority::Current {
        requested_class
    } else if existing_priority == FrameWorkPriority::Current {
        existing_class
    } else {
        requested_class
    }
}

enum Evicted<K> {
    Prefetch(K),
    Still(K),
    None,
}

fn plan_new_admission_eviction<K, D, P>(
    state: &BrokerState<K, D, P>,
    max_pending: usize,
    max_queued: usize,
    request: &FrameWorkRequest<K, D, P>,
) -> Option<Evicted<K>>
where
    K: Clone + Eq + Hash,
{
    let proactive_still = (request.priority == FrameWorkPriority::Current
        && request.work_class != FrameWorkClass::Still)
        .then(|| {
            state
                .queue
                .iter()
                .find(|queued| {
                    queued.request.priority == FrameWorkPriority::Current
                        && queued.request.work_class == FrameWorkClass::Still
                })
                .map(|queued| Evicted::Still(queued.request.key.clone()))
        })
        .flatten();
    let pending_full = state.pending.len() >= max_pending;
    let queue_full = state.queue.len() >= max_queued;
    let planned = if let Some(eviction) = proactive_still {
        eviction
    } else if !pending_full && !queue_full {
        Evicted::None
    } else if request.priority != FrameWorkPriority::Current {
        return None;
    } else if queue_full {
        queue_eviction_candidate(state, request)?
    } else {
        pending_eviction_candidate(state, request)?
    };
    let removes_pending = !matches!(planned, Evicted::None);
    let removes_queued = match &planned {
        Evicted::Prefetch(key) | Evicted::Still(key) => {
            state.queue.iter().any(|queued| &queued.request.key == key)
        }
        Evicted::None => false,
    };
    if state.pending.len().saturating_sub(usize::from(removes_pending)) >= max_pending
        || state.queue.len().saturating_sub(usize::from(removes_queued)) >= max_queued
    {
        None
    } else {
        Some(planned)
    }
}

fn plan_queue_eviction<K, D, P>(
    state: &BrokerState<K, D, P>,
    max_queued: usize,
    request: &FrameWorkRequest<K, D, P>,
) -> Option<Evicted<K>>
where
    K: Clone + Eq + Hash,
{
    if state.queue.len() < max_queued {
        Some(Evicted::None)
    } else if request.priority == FrameWorkPriority::Current {
        queue_eviction_candidate(state, request)
    } else {
        None
    }
}

fn queue_eviction_candidate<K, D, P>(
    state: &BrokerState<K, D, P>,
    request: &FrameWorkRequest<K, D, P>,
) -> Option<Evicted<K>>
where
    K: Clone,
{
    if let Some(queued) = state
        .queue
        .iter()
        .find(|queued| queued.request.priority == FrameWorkPriority::Prefetch)
    {
        return Some(Evicted::Prefetch(queued.request.key.clone()));
    }
    if request.work_class != FrameWorkClass::Still
        && let Some(queued) = state
            .queue
            .iter()
            .find(|queued| queued.request.work_class == FrameWorkClass::Still)
    {
        return Some(Evicted::Still(queued.request.key.clone()));
    }
    None
}

fn pending_eviction_candidate<K, D, P>(
    state: &BrokerState<K, D, P>,
    request: &FrameWorkRequest<K, D, P>,
) -> Option<Evicted<K>>
where
    K: Clone + Eq + Hash,
{
    if let Some(key) = state
        .pending
        .iter()
        .find(|(_, pending)| pending.binding.priority == FrameWorkPriority::Prefetch)
        .map(|(key, _)| key.clone())
    {
        return Some(Evicted::Prefetch(key));
    }
    if request.work_class != FrameWorkClass::Still {
        return state
            .pending
            .iter()
            .find(|(_, pending)| pending.binding.work_class == FrameWorkClass::Still)
            .map(|(key, _)| Evicted::Still(key.clone()));
    }
    None
}

fn apply_eviction<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    eviction: Evicted<K>,
    now: MonotonicTimestamp,
) -> (Option<K>, Option<K>)
where
    K: Clone + Eq + Hash,
{
    match eviction {
        Evicted::Prefetch(key) => {
            remove_key_locked(state, &key, now);
            bump(&mut state.metrics.evicted_prefetch);
            (Some(key), None)
        }
        Evicted::Still(key) => {
            remove_key_locked(state, &key, now);
            bump(&mut state.metrics.evicted_still);
            (None, Some(key))
        }
        Evicted::None => (None, None),
    }
}

fn remove_key_locked<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    key: &K,
    invalidated_at: MonotonicTimestamp,
) where
    K: Eq + Hash,
{
    state.pending.remove(key);
    state.queue.retain(|queued| &queued.request.key != key);
    for execution in state.in_flight.values_mut().filter(|execution| &execution.key == key) {
        execution.presentation_binding_expired = false;
        execution.invalidated_at.get_or_insert(invalidated_at);
    }
}

fn prune_obsolete_locked<K, D, P>(state: &mut BrokerState<K, D, P>) {
    let latest = state.latest_generation;
    state.pending.retain(|_, pending| pending.binding.generation >= latest);
    state.queue.retain(|queued| queued.request.generation >= latest);
}

fn current_pending_binding<'a, K, D>(
    latest_generation: u64,
    pending: &'a HashMap<K, PendingBinding<D>>,
    execution: &InFlightWork<K>,
) -> Option<&'a PendingBinding<D>>
where
    K: Eq + Hash,
{
    pending.get(&execution.key).filter(|pending| {
        pending.binding.work_class == execution.work_class
            && pending.binding.resource_scope == execution.resource_scope
            && pending.binding.generation >= execution.generation
            && pending.binding.generation >= latest_generation
    })
}

fn exact_pending_binding<'a, K, D>(
    latest_generation: u64,
    pending: &'a HashMap<K, PendingBinding<D>>,
    execution: &InFlightWork<K>,
) -> Option<&'a PendingBinding<D>>
where
    K: Eq + Hash,
{
    pending.get(&execution.key).filter(|pending| {
        pending.binding.work_class == execution.work_class
            && pending.binding.resource_scope == execution.resource_scope
            && pending.binding.generation == execution.generation
            && pending.binding.demand_identity == execution.demand_identity
            && pending.binding.generation >= latest_generation
    })
}

fn has_preempted_fallback<K, D, P>(
    state: &BrokerState<K, D, P>,
    execution: &InFlightWork<K>,
) -> bool
where
    K: Eq + Hash,
{
    let Some(pending) = state.pending.get(&execution.key) else {
        return false;
    };
    let queued_fallback = state.queue.iter().any(|queued| {
        queued.request.key == execution.key
            && queued.request.work_class == pending.binding.work_class
            && queued.request.resource_scope == pending.binding.resource_scope
            && queued.request.generation == pending.binding.generation
            && queued.request.demand_identity == pending.binding.demand_identity
    });
    queued_fallback
        || state.in_flight.values().any(|fallback| {
            fallback.key == execution.key
                && fallback.preempted_at.is_none()
                && !fallback.presentation_binding_expired
                && exact_pending_binding(state.latest_generation, &state.pending, fallback)
                    .is_some()
        })
}

fn oldest_other_current_request<K, D>(
    latest_generation: u64,
    pending: &HashMap<K, PendingBinding<D>>,
    execution: &InFlightWork<K>,
    realtime_only: bool,
    preserve_playback_prefetch: bool,
) -> Option<MonotonicTimestamp>
where
    K: Eq,
{
    pending
        .iter()
        .filter(|(key, pending)| {
            (*key != &execution.key || pending.binding.resource_scope != execution.resource_scope)
                && pending.binding.priority == FrameWorkPriority::Current
                && (!realtime_only || pending.binding.work_class != FrameWorkClass::Still)
                && !(preserve_playback_prefetch
                    && pending.binding.work_class == FrameWorkClass::Playback)
                && pending.binding.generation >= latest_generation
        })
        .map(|(_, pending)| pending.requested_at)
        .min()
}

#[derive(Clone, Copy)]
enum ExecutionCancellationCause {
    Superseded,
    PrefetchPreemptedByCurrent,
    StillPreemptedByRealtimeCurrent,
    DeadlineExpired,
    ExecutionBudgetExpired,
}

#[derive(Clone, Copy)]
struct ExecutionCancellationCandidate {
    requested_at: MonotonicTimestamp,
    cause: ExecutionCancellationCause,
}

impl ExecutionCancellationCandidate {
    fn into_cancellation(self, now: MonotonicTimestamp) -> FrameExecutionCancellation {
        let age = elapsed_since(now, self.requested_at);
        match self.cause {
            ExecutionCancellationCause::Superseded => {
                FrameExecutionCancellation::Superseded { age: Some(age) }
            }
            ExecutionCancellationCause::PrefetchPreemptedByCurrent => {
                FrameExecutionCancellation::PrefetchPreemptedByCurrent { request_age: age }
            }
            ExecutionCancellationCause::StillPreemptedByRealtimeCurrent => {
                FrameExecutionCancellation::StillPreemptedByRealtimeCurrent { request_age: age }
            }
            ExecutionCancellationCause::DeadlineExpired => {
                FrameExecutionCancellation::DeadlineExpired { age }
            }
            ExecutionCancellationCause::ExecutionBudgetExpired => {
                FrameExecutionCancellation::ExecutionBudgetExpired { age }
            }
        }
    }
}

fn earlier_cancellation(
    current: Option<ExecutionCancellationCandidate>,
    candidate: ExecutionCancellationCandidate,
) -> Option<ExecutionCancellationCandidate> {
    match current {
        Some(current) if current.requested_at <= candidate.requested_at => Some(current),
        _ => Some(candidate),
    }
}

fn execution_cancellation_evidence_locked<K, D, P>(
    state: &BrokerState<K, D, P>,
    id: FrameExecutionId,
    now: MonotonicTimestamp,
) -> Option<crate::FrameExecutionCancellationEvidence>
where
    K: Eq + Hash,
{
    if let Some(closed_at) = state.closed_at {
        let execution_age = state.in_flight.get(&id).map_or(Duration::ZERO, |execution| {
            elapsed_since(now, execution.started_at)
        });
        return Some(crate::FrameExecutionCancellationEvidence {
            cancellation: FrameExecutionCancellation::BrokerClosed {
                age: elapsed_since(now, closed_at),
            },
            execution_age,
        });
    }
    let Some(execution) = state.in_flight.get(&id) else {
        return Some(crate::FrameExecutionCancellationEvidence {
            cancellation: FrameExecutionCancellation::Superseded { age: None },
            execution_age: Duration::ZERO,
        });
    };
    let execution_age = elapsed_since(now, execution.started_at);
    let mut candidate = None;
    let publication_binding_missing = execution.presentation_binding_expired
        || current_pending_binding(state.latest_generation, &state.pending, execution).is_none();
    if publication_binding_missing {
        if let Some(invalidated_at) = execution.invalidated_at {
            candidate = Some(ExecutionCancellationCandidate {
                requested_at: invalidated_at,
                cause: ExecutionCancellationCause::Superseded,
            });
        } else if !execution.presentation_binding_expired {
            return Some(crate::FrameExecutionCancellationEvidence {
                cancellation: FrameExecutionCancellation::Superseded { age: None },
                execution_age,
            });
        }
    }
    if let Some(requested_at) = execution.preempted_at {
        let cause = if execution.priority == FrameWorkPriority::Prefetch {
            ExecutionCancellationCause::PrefetchPreemptedByCurrent
        } else {
            ExecutionCancellationCause::StillPreemptedByRealtimeCurrent
        };
        candidate = earlier_cancellation(
            candidate,
            ExecutionCancellationCandidate { requested_at, cause },
        );
    }
    if execution.in_flight_deadline_policy == FrameInFlightDeadlinePolicy::Cancel
        && let Some(deadline_at) = execution.deadline_at.filter(|deadline| *deadline <= now)
    {
        candidate = earlier_cancellation(
            candidate,
            ExecutionCancellationCandidate {
                requested_at: deadline_at,
                cause: ExecutionCancellationCause::DeadlineExpired,
            },
        );
    }
    if let Some(deadline_at) =
        execution.execution_cancellation_deadline_at.filter(|deadline| *deadline <= now)
    {
        candidate = earlier_cancellation(
            candidate,
            ExecutionCancellationCandidate {
                requested_at: deadline_at,
                cause: ExecutionCancellationCause::ExecutionBudgetExpired,
            },
        );
    }
    candidate.map(|candidate| crate::FrameExecutionCancellationEvidence {
        cancellation: candidate.into_cancellation(now),
        execution_age,
    })
}

fn next_execution_cancellation_wait<K>(
    execution: &InFlightWork<K>,
    now: MonotonicTimestamp,
) -> Option<Duration> {
    let presentation_deadline = (execution.in_flight_deadline_policy
        == FrameInFlightDeadlinePolicy::Cancel)
        .then_some(execution.deadline_at)
        .flatten();
    presentation_deadline
        .into_iter()
        .chain(execution.execution_cancellation_deadline_at)
        .map(|deadline| {
            deadline.duration_since_origin().saturating_sub(now.duration_since_origin())
        })
        .min()
}

fn dequeue_work_locked<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    lane: FrameWorkerLane,
    now: MonotonicTimestamp,
) -> Option<FrameWorkReceive<K, D, P>>
where
    K: Clone + Eq + Hash,
    D: Copy,
{
    refresh_in_flight_invalidations_locked(state, now);
    let allow_current_playback_failover = non_playback_current_failover_allowed(state, lane, now);
    let index =
        next_work_index_with_failover(&state.queue, lane, now, allow_current_playback_failover)?;
    let queued = state.queue.remove(index)?;
    let queue_wait = state.pending.get(&queued.request.key).map_or(Duration::ZERO, |pending| {
        elapsed_since(now, pending.requested_at)
    });
    let request = queued.request;
    let id = FrameExecutionId(state.next_execution_id);
    state.next_execution_id = state.next_execution_id.saturating_add(1);
    state.in_flight.insert(
        id,
        InFlightWork {
            key: request.key.clone(),
            generation: request.generation,
            priority: request.priority,
            work_class: request.work_class,
            resource_scope: request.resource_scope,
            worker_lane: Some(lane),
            demand_identity: request.demand_identity,
            started_at: now,
            deadline_at: queued.deadline_at,
            in_flight_deadline_policy: request.in_flight_deadline_policy,
            execution_cancellation_deadline_at: request
                .execution_cancellation_budget
                .map(|budget| now.saturating_add(budget)),
            completed_at: None,
            preempted_at: None,
            invalidated_at: None,
            presentation_binding_expired: false,
        },
    );
    refresh_in_flight_invalidations_locked(state, now);
    let expired = deadline_expired(queued.deadline_at, now);
    if expired {
        bump(&mut state.metrics.dropped_expired_work);
        if request.priority == FrameWorkPriority::Current
            && request.work_class == FrameWorkClass::Playback
        {
            bump(&mut state.metrics.dropped_expired_playback_current);
        }
    }
    let execution = FrameWorkExecution {
        id,
        key: request.key,
        generation: request.generation,
        priority: request.priority,
        work_class: request.work_class,
        resource_scope: request.resource_scope,
        demand_identity: request.demand_identity,
        deadline: request.deadline.map(FrameWorkDeadline::adapter_deadline),
        queue_wait,
        payload: request.payload,
    };
    Some(if expired {
        FrameWorkReceive::Expired(execution)
    } else {
        FrameWorkReceive::Ready(execution)
    })
}

fn refresh_in_flight_invalidations_locked<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    now: MonotonicTimestamp,
) where
    K: Eq + Hash,
{
    let BrokerState { latest_generation, pending, in_flight, metrics, .. } = state;
    for execution in in_flight.values_mut() {
        if execution.presentation_binding_expired {
            let retained_playback_locality = execution.work_class == FrameWorkClass::Playback
                && execution.in_flight_deadline_policy
                    == FrameInFlightDeadlinePolicy::FinishForLocality;
            if execution.generation < *latest_generation
                && !retained_playback_locality
                && execution.invalidated_at.is_none()
            {
                execution.invalidated_at = Some(now);
            }
        } else if let Some(current) =
            current_pending_binding(*latest_generation, pending, execution)
        {
            execution.deadline_at = current.deadline_at;
            execution.in_flight_deadline_policy = current.in_flight_deadline_policy;
            execution.execution_cancellation_deadline_at = current
                .execution_cancellation_budget
                .map(|budget| execution.started_at.saturating_add(budget));
            execution.invalidated_at = None;
        } else if execution.invalidated_at.is_none() {
            let retain_same_generation_playback_locality = execution.generation
                >= *latest_generation
                && execution.work_class == FrameWorkClass::Playback
                && execution.in_flight_deadline_policy
                    == FrameInFlightDeadlinePolicy::FinishForLocality;
            if retain_same_generation_playback_locality {
                // The execution remains useful only for its stateful decoder
                // locality and resolves without publication authority.
                execution.presentation_binding_expired = true;
                bump(&mut metrics.in_flight_binding_locality_detachments);
            } else {
                execution.invalidated_at = Some(now);
                if execution.generation < *latest_generation {
                    bump(&mut metrics.in_flight_generation_invalidations);
                } else {
                    bump(&mut metrics.in_flight_binding_invalidations);
                }
            }
        }
        let preempted_at = if execution.priority == FrameWorkPriority::Prefetch {
            oldest_other_current_request(
                *latest_generation,
                pending,
                execution,
                false,
                execution.work_class == FrameWorkClass::Playback,
            )
        } else if execution.work_class == FrameWorkClass::Still {
            oldest_other_current_request(*latest_generation, pending, execution, true, false)
        } else {
            None
        };
        if let Some(preempted_at) = preempted_at {
            // An execution-specific stop request cannot predate the lease it
            // acts on. A current binding may legitimately remain pending for
            // presentation while a later prefetch lease begins; clamp that
            // inherited request time to lease start instead of manufacturing
            // a negative request-to-logical-cancellation interval.
            let preempted_at = preempted_at.max(execution.started_at);
            execution.preempted_at = Some(
                execution
                    .preempted_at
                    .map_or(preempted_at, |existing| existing.min(preempted_at)),
            );
        }
    }
}

#[cfg(test)]
fn next_work_index<K, D, P>(
    queue: &VecDeque<QueuedWork<K, D, P>>,
    lane: FrameWorkerLane,
    now: MonotonicTimestamp,
) -> Option<usize> {
    next_work_index_with_failover(queue, lane, now, false)
}

fn next_work_index_with_failover<K, D, P>(
    queue: &VecDeque<QueuedWork<K, D, P>>,
    lane: FrameWorkerLane,
    now: MonotonicTimestamp,
    allow_current_playback_failover: bool,
) -> Option<usize> {
    queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| {
            queued.request.priority == FrameWorkPriority::Current
                && (lane.accepts(queued.request.work_class)
                    || (allow_current_playback_failover
                        && queued.request.work_class == FrameWorkClass::Playback))
        })
        .min_by_key(|(_, queued)| {
            let authorized_playback_failover = allow_current_playback_failover
                && queued.request.work_class == FrameWorkClass::Playback
                && !lane.accepts(queued.request.work_class);
            (
                !authorized_playback_failover,
                deadline_expired(queued.deadline_at, now),
                work_rank(queued.request.work_class),
            )
        })
        .map(|(index, _)| index)
        .or_else(|| {
            queue
                .iter()
                .enumerate()
                .filter(|(_, queued)| {
                    queued.request.priority == FrameWorkPriority::Prefetch
                        && lane.accepts(queued.request.work_class)
                })
                .min_by_key(|(_, queued)| deadline_expired(queued.deadline_at, now))
                .map(|(index, _)| index)
        })
}

fn non_playback_current_failover_allowed<K, D, P>(
    state: &BrokerState<K, D, P>,
    lane: FrameWorkerLane,
    now: MonotonicTimestamp,
) -> bool
where
    K: Eq + Hash,
{
    if lane != FrameWorkerLane::NonPlayback {
        return false;
    }
    let failover_already_running = state.in_flight.values().any(|execution| {
        execution.worker_lane == Some(FrameWorkerLane::NonPlayback)
            && execution.work_class == FrameWorkClass::Playback
            && execution.completed_at.is_none()
    });
    if failover_already_running {
        return false;
    }
    state.in_flight.iter().any(|(id, execution)| {
        execution.worker_lane == Some(FrameWorkerLane::Playback)
            && execution.work_class == FrameWorkClass::Playback
            && execution.completed_at.is_none()
            && execution_cancellation_evidence_locked(state, *id, now).is_some()
    })
}

fn next_non_playback_current_failover_wait<K, D, P>(
    state: &BrokerState<K, D, P>,
    lane: FrameWorkerLane,
    now: MonotonicTimestamp,
) -> Option<Duration>
where
    K: Eq + Hash,
{
    if lane != FrameWorkerLane::NonPlayback
        || !state.queue.iter().any(|queued| {
            queued.request.priority == FrameWorkPriority::Current
                && queued.request.work_class == FrameWorkClass::Playback
        })
        || state.in_flight.values().any(|execution| {
            execution.worker_lane == Some(FrameWorkerLane::NonPlayback)
                && execution.work_class == FrameWorkClass::Playback
                && execution.completed_at.is_none()
        })
    {
        return None;
    }

    state
        .in_flight
        .iter()
        .filter(|(_, execution)| {
            execution.worker_lane == Some(FrameWorkerLane::Playback)
                && execution.work_class == FrameWorkClass::Playback
                && execution.completed_at.is_none()
        })
        .filter_map(|(id, execution)| {
            if execution_cancellation_evidence_locked(state, *id, now).is_some() {
                Some(Duration::ZERO)
            } else {
                next_execution_cancellation_wait(execution, now)
            }
        })
        .min()
}

fn work_rank(class: FrameWorkClass) -> u8 {
    match class {
        FrameWorkClass::Interactive => 0,
        FrameWorkClass::Playback => 1,
        FrameWorkClass::Still => 2,
    }
}

#[derive(Clone, Copy)]
enum CompletionCause {
    Current,
    Missing,
    ClassMismatch,
    Obsolete,
}

fn completion_cause<K, D, P>(
    state: &BrokerState<K, D, P>,
    execution: &InFlightWork<K>,
    reusable: bool,
) -> CompletionCause
where
    K: Eq + Hash,
    D: Copy,
{
    if execution_budget_expired_before_completion(execution) {
        return CompletionCause::Obsolete;
    }
    if execution.preempted_at.is_some() {
        return CompletionCause::Obsolete;
    }
    if execution.presentation_binding_expired {
        return CompletionCause::Missing;
    }
    let Some(pending) = state.pending.get(&execution.key) else {
        return CompletionCause::Missing;
    };
    if pending.binding.work_class != execution.work_class {
        return CompletionCause::ClassMismatch;
    }
    if pending.binding.resource_scope != execution.resource_scope {
        return CompletionCause::Obsolete;
    }
    if playback_current_binding_is_superseded(state.active_playback_demand, pending.binding) {
        return CompletionCause::Obsolete;
    }
    let changed = pending.binding.generation != execution.generation
        || pending.binding.demand_identity != execution.demand_identity;
    if (changed && !reusable) || pending.binding.generation < state.latest_generation {
        CompletionCause::Obsolete
    } else {
        CompletionCause::Current
    }
}

fn record_completion(
    metrics: &mut FrameWorkBrokerMetrics,
    completion: FrameRequestCompletion,
    cause: CompletionCause,
) {
    match completion {
        FrameRequestCompletion::Current => bump(&mut metrics.completed_current),
        FrameRequestCompletion::CacheOnly => {
            bump(&mut metrics.completed_cache_only);
            match cause {
                CompletionCause::Missing => bump(&mut metrics.completed_cache_only_missing),
                CompletionCause::ClassMismatch => {
                    bump(&mut metrics.completed_cache_only_class_mismatch);
                }
                CompletionCause::Current | CompletionCause::Obsolete => {}
            }
        }
        FrameRequestCompletion::Stale => {
            bump(&mut metrics.completed_stale);
            match cause {
                CompletionCause::Missing => bump(&mut metrics.completed_stale_missing),
                CompletionCause::ClassMismatch => {
                    bump(&mut metrics.completed_stale_class_mismatch);
                }
                CompletionCause::Obsolete => bump(&mut metrics.completed_stale_obsolete),
                CompletionCause::Current => {}
            }
        }
    }
}

fn resolve_locked<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    execution: InFlightWork<K>,
    reusable: bool,
    deadline: FrameWorkDeadlineStatus,
) -> FrameRequestResolution<D>
where
    K: Eq + Hash,
    D: Copy,
{
    if execution_budget_expired_before_completion(&execution) {
        if exact_pending_binding(state.latest_generation, &state.pending, &execution).is_some()
            && !has_preempted_fallback(state, &execution)
        {
            state.pending.remove(&execution.key);
        }
        return FrameRequestResolution {
            completion: FrameRequestCompletion::Stale,
            binding: None,
            deadline,
        };
    }
    if execution.preempted_at.is_some() {
        if !has_preempted_fallback(state, &execution) {
            state.pending.remove(&execution.key);
        }
        return FrameRequestResolution {
            completion: FrameRequestCompletion::Stale,
            binding: None,
            deadline,
        };
    }
    if execution.presentation_binding_expired {
        return FrameRequestResolution {
            // Only explicit locality-preserving seams set this flag. The
            // presentation binding is permanently detached, but a reusable
            // payload still has exact key/resource identity and may populate
            // its domain cache across a Viewer-only generation rotation.
            completion: FrameRequestCompletion::CacheOnly,
            binding: None,
            deadline,
        };
    }
    let Some(pending) = state.pending.get(&execution.key).copied() else {
        return FrameRequestResolution {
            completion: if execution.generation >= state.latest_generation {
                FrameRequestCompletion::CacheOnly
            } else {
                FrameRequestCompletion::Stale
            },
            binding: None,
            deadline,
        };
    };
    if pending.binding.work_class != execution.work_class {
        return FrameRequestResolution {
            completion: if execution.generation >= state.latest_generation {
                FrameRequestCompletion::CacheOnly
            } else {
                FrameRequestCompletion::Stale
            },
            binding: None,
            deadline,
        };
    }
    if pending.binding.resource_scope != execution.resource_scope {
        return FrameRequestResolution {
            completion: if execution.generation >= state.latest_generation {
                FrameRequestCompletion::CacheOnly
            } else {
                FrameRequestCompletion::Stale
            },
            binding: None,
            deadline,
        };
    }
    if playback_current_binding_is_superseded(state.active_playback_demand, pending.binding) {
        state.pending.remove(&execution.key);
        state.queue.retain(|queued| queued.request.key != execution.key);
        return FrameRequestResolution {
            completion: if execution.generation >= state.latest_generation {
                FrameRequestCompletion::CacheOnly
            } else {
                FrameRequestCompletion::Stale
            },
            binding: None,
            deadline,
        };
    }
    let changed = pending.binding.generation != execution.generation
        || pending.binding.demand_identity != execution.demand_identity;
    if changed && !reusable {
        return FrameRequestResolution {
            completion: FrameRequestCompletion::Stale,
            binding: None,
            deadline,
        };
    }
    state.pending.remove(&execution.key);
    state.queue.retain(|queued| queued.request.key != execution.key);
    if pending.binding.generation >= state.latest_generation
        || execution.generation >= state.latest_generation
    {
        FrameRequestResolution {
            completion: FrameRequestCompletion::Current,
            binding: Some(pending.binding),
            deadline,
        }
    } else {
        FrameRequestResolution {
            completion: FrameRequestCompletion::Stale,
            binding: None,
            deadline,
        }
    }
}

fn execution_budget_expired_before_completion<K>(execution: &InFlightWork<K>) -> bool {
    execution
        .execution_cancellation_deadline_at
        .zip(execution.completed_at)
        .is_some_and(|(deadline_at, completed_at)| deadline_at <= completed_at)
}

fn deadline_status(
    deadline_at: Option<MonotonicTimestamp>,
    completed_at: MonotonicTimestamp,
) -> FrameWorkDeadlineStatus {
    let Some(deadline_at) = deadline_at else {
        return FrameWorkDeadlineStatus::NotApplicable;
    };
    if completed_at < deadline_at {
        FrameWorkDeadlineStatus::OnTime
    } else {
        FrameWorkDeadlineStatus::Missed { late_by: elapsed_since(completed_at, deadline_at) }
    }
}

fn bump(value: &mut u64) {
    *value = value.saturating_add(1);
}

fn observe_now_locked<K, D, P>(
    clock: &dyn MonotonicRuntimeClock,
    state: &mut BrokerState<K, D, P>,
) -> MonotonicTimestamp {
    let sampled = clock.now();
    if sampled < state.last_observed_at {
        if !state.clock_regression_active {
            bump(&mut state.metrics.clock_regressions);
            state.clock_regression_active = true;
        }
        state.last_observed_at
    } else {
        state.clock_regression_active = false;
        state.last_observed_at = sampled;
        sampled
    }
}

fn elapsed_since(now: MonotonicTimestamp, earlier: MonotonicTimestamp) -> Duration {
    now.duration_since_origin().saturating_sub(earlier.duration_since_origin())
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn lock_state<K, D, P>(
    state: &Mutex<BrokerState<K, D, P>>,
) -> MutexGuard<'_, BrokerState<K, D, P>> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state<'a, K, D, P>(
    changed: &Condvar,
    state: MutexGuard<'a, BrokerState<K, D, P>>,
) -> MutexGuard<'a, BrokerState<K, D, P>> {
    changed.wait(state).unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_state_timeout<'a, K, D, P>(
    changed: &Condvar,
    state: MutexGuard<'a, BrokerState<K, D, P>>,
    timeout: Duration,
) -> (MutexGuard<'a, BrokerState<K, D, P>>, bool) {
    match changed.wait_timeout(state, timeout) {
        Ok((state, result)) => (state, result.timed_out()),
        Err(poisoned) => {
            let (state, result) = poisoned.into_inner();
            (state, result.timed_out())
        }
    }
}

#[cfg(test)]
#[path = "work_broker/tests.rs"]
mod tests;
