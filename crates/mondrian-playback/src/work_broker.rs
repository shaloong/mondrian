//! Atomic frame-work lifecycle broker.
//!
//! This Module owns admission, queued transport, in-flight execution leases,
//! latest-wins invalidation, preemption, and completion binding under one lock.
//! Payloads, keys, and absolute Adapter deadline values remain opaque. The
//! Broker owns the lowered monotonic deadline used for lifecycle decisions.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::{
    FrameDemandIdentity, FrameExecutionCancellation, FrameRequestBinding, FrameRequestCompletion,
    FrameRequestResolution, FrameWorkClass, FrameWorkDeadline, FrameWorkDeadlineStatus,
    FrameWorkPriority, MonotonicRuntimeClock, MonotonicTimestamp, SystemMonotonicRuntimeClock,
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
    /// Playback demand identity when demand-backed.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Adapter deadline plus remaining budget lowered by the Broker at admission.
    pub deadline: Option<FrameWorkDeadline<D>>,
    /// Opaque Adapter execution payload.
    pub payload: P,
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
    /// Bounded pending/queued capacity could not admit the request.
    DroppedBackpressure,
    /// The priority/class pair violates semantic policy.
    DroppedInvalidClass,
    /// The broker is closed and cannot accept work.
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
    /// Captured demand identity; final completion must resolve through the broker.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Captured Adapter deadline.
    pub deadline: Option<D>,
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

/// Binding canceled by residency expiration before completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredFrameWork<K, D> {
    /// Opaque semantic key.
    pub key: K,
    /// Latest binding removed by expiration.
    pub binding: FrameRequestBinding<D>,
}

/// Stable broker evidence independent of Adapter payloads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct FrameWorkBrokerDiagnostics {
    /// Latest generation observed.
    pub latest_generation: u64,
    /// Runtime-clock regression episodes clamped to the last observation.
    pub clock_regressions: u64,
    /// Semantic keys awaiting a terminal resolution.
    pub pending_requests: usize,
    /// Payloads waiting for a worker.
    pub queued_work: usize,
    /// Execution leases not yet resolved or abandoned.
    pub in_flight_work: usize,
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
    /// Execution completions accepted as current.
    pub completed_current: u64,
    /// Execution completions admitted for cache only.
    pub completed_cache_only: u64,
    /// Execution completions rejected as stale.
    pub completed_stale: u64,
    /// Cache-only completions with no pending binding.
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
    worker_lane: Option<FrameWorkerLane>,
    demand_identity: Option<FrameDemandIdentity>,
    deadline_at: Option<MonotonicTimestamp>,
    completed_at: Option<MonotonicTimestamp>,
    preempted_at: Option<MonotonicTimestamp>,
    invalidated_at: Option<MonotonicTimestamp>,
}

struct BrokerState<K, D, P> {
    latest_generation: u64,
    next_execution_id: u64,
    pending: HashMap<K, PendingBinding<D>>,
    queue: VecDeque<QueuedWork<K, D, P>>,
    in_flight: HashMap<FrameExecutionId, InFlightWork<K>>,
    last_observed_at: MonotonicTimestamp,
    clock_regression_active: bool,
    closed_at: Option<MonotonicTimestamp>,
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
                    next_execution_id: 1,
                    pending: HashMap::new(),
                    queue: VecDeque::new(),
                    in_flight: HashMap::new(),
                    last_observed_at: MonotonicTimestamp::ZERO,
                    clock_regression_active: false,
                    closed_at: None,
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
        state.latest_generation = state.latest_generation.saturating_add(1);
        refresh_in_flight_invalidations_locked(&mut state, now);
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
            bump(&mut state.metrics.dropped_backpressure);
            bump(&mut state.metrics.dropped_obsolete_generation);
            return FrameWorkSubmission::DroppedBackpressure;
        }
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let deadline_at = lowered_deadline_at(request.deadline, now);

        if let Some(previous) = state.pending.get(&request.key).copied() {
            let previous_binding = previous.binding;
            let requested_priority = request.priority;
            let requested_class = request.work_class;
            request.priority = promote_priority(previous_binding.priority, requested_priority);
            request.work_class = promote_class(
                previous_binding.priority,
                previous_binding.work_class,
                requested_priority,
                requested_class,
            );
            let binding = binding_for(&request);
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

            if previous_binding.work_class == binding.work_class {
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
                    },
                );
                refresh_in_flight_invalidations_locked(&mut state, now);
                bump(&mut state.metrics.submitted_reused_in_flight);
                return FrameWorkSubmission::ReusedInFlight;
            }

            let Some(eviction) = plan_queue_eviction(&state, self.shared.max_queued, &request)
            else {
                bump(&mut state.metrics.dropped_backpressure);
                return FrameWorkSubmission::DroppedBackpressure;
            };
            let (evicted_prefetch, evicted_still) = apply_eviction(&mut state, eviction);
            state.pending.insert(
                request.key.clone(),
                PendingBinding { binding, requested_at: now, deadline_at },
            );
            bump(&mut state.metrics.submitted_work_class_changes);
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
        let (evicted_prefetch, evicted_still) = apply_eviction(&mut state, eviction);
        state.pending.insert(
            request.key.clone(),
            PendingBinding {
                binding: binding_for(&request),
                requested_at: now,
                deadline_at,
            },
        );
        state.queue.push_back(QueuedWork { request, deadline_at });
        refresh_in_flight_invalidations_locked(&mut state, now);
        bump(&mut state.metrics.submitted_queued);
        self.shared.changed.notify_all();
        FrameWorkSubmission::Queued { evicted_prefetch, evicted_still }
    }

    /// Block until eligible work or closure, then create one execution lease.
    pub fn receive(&self, lane: FrameWorkerLane) -> Option<FrameWorkReceive<K, D, P>> {
        let mut state = lock_state(&self.shared.state);
        loop {
            let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
            if let Some(index) = next_work_index(&state.queue, lane, now) {
                let queued = state.queue.remove(index)?;
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
                        worker_lane: Some(lane),
                        demand_identity: request.demand_identity,
                        deadline_at: queued.deadline_at,
                        completed_at: None,
                        preempted_at: None,
                        invalidated_at: None,
                    },
                );
                refresh_in_flight_invalidations_locked(&mut state, now);
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
                    demand_identity: request.demand_identity,
                    deadline: request.deadline.map(FrameWorkDeadline::adapter_deadline),
                    payload: request.payload,
                };
                return Some(if expired {
                    FrameWorkReceive::Expired(execution)
                } else {
                    FrameWorkReceive::Ready(execution)
                });
            }
            if state.closed_at.is_some() {
                return None;
            }
            state = wait_state(&self.shared.changed, state);
        }
    }

    /// Decide atomically whether an execution must stop for lifecycle or
    /// preemption reasons.
    pub fn execution_cancellation(
        &self,
        id: FrameExecutionId,
    ) -> Option<FrameExecutionCancellation> {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        if let Some(closed_at) = state.closed_at {
            return Some(FrameExecutionCancellation::BrokerClosed {
                age: elapsed_since(now, closed_at),
            });
        }
        let Some(execution) = state.in_flight.get(&id) else {
            return Some(FrameExecutionCancellation::Superseded { age: None });
        };
        let mut candidate = None;
        if current_pending_binding(state.latest_generation, &state.pending, execution).is_none() {
            let Some(invalidated_at) = execution.invalidated_at else {
                return Some(FrameExecutionCancellation::Superseded { age: None });
            };
            candidate = Some(ExecutionCancellationCandidate {
                requested_at: invalidated_at,
                cause: ExecutionCancellationCause::Superseded,
            });
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
        if let Some(deadline_at) = execution.deadline_at.filter(|deadline| *deadline <= now) {
            candidate = earlier_cancellation(
                candidate,
                ExecutionCancellationCandidate {
                    requested_at: deadline_at,
                    cause: ExecutionCancellationCause::DeadlineExpired,
                },
            );
        }
        candidate.map(|candidate| candidate.into_cancellation(now))
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
        true
    }

    /// Resolve one execution lease atomically against the latest binding.
    pub fn resolve_execution(
        &self,
        id: FrameExecutionId,
        reusable: bool,
    ) -> FrameRequestResolution<D> {
        let mut state = lock_state(&self.shared.state);
        let Some(execution) = state.in_flight.remove(&id) else {
            return FrameRequestResolution {
                completion: FrameRequestCompletion::Stale,
                binding: None,
                deadline: FrameWorkDeadlineStatus::NotApplicable,
            };
        };
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let deadline =
            deadline_status(execution.deadline_at, execution.completed_at.unwrap_or(now));
        let cause = completion_cause(&state, &execution, reusable);
        let resolution = resolve_locked(&mut state, execution, reusable, deadline);
        refresh_in_flight_invalidations_locked(&mut state, now);
        record_completion(&mut state.metrics, resolution.completion, cause);
        resolution
    }

    /// Resolve synchronous or externally executed work that never held a worker lease.
    pub fn resolve_unleased(
        &self,
        key: K,
        generation: u64,
        work_class: FrameWorkClass,
        demand_identity: Option<FrameDemandIdentity>,
        reusable: bool,
    ) -> FrameRequestResolution<D> {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let mut execution = InFlightWork {
            key,
            generation,
            priority: FrameWorkPriority::Current,
            work_class,
            worker_lane: None,
            demand_identity,
            deadline_at: None,
            completed_at: Some(now),
            preempted_at: None,
            invalidated_at: None,
        };
        if let Some(current) =
            current_pending_binding(state.latest_generation, &state.pending, &execution)
        {
            execution.deadline_at = current.deadline_at;
        }
        let deadline = deadline_status(execution.deadline_at, now);
        let cause = completion_cause(&state, &execution, reusable);
        let resolution = resolve_locked(&mut state, execution, reusable, deadline);
        refresh_in_flight_invalidations_locked(&mut state, now);
        record_completion(&mut state.metrics, resolution.completion, cause);
        resolution
    }

    /// Abandon a lease whose result cannot reach the completion Adapter.
    pub fn abandon_execution(&self, id: FrameExecutionId) -> bool {
        lock_state(&self.shared.state).in_flight.remove(&id).is_some()
    }

    /// Cancel one key across pending and queued state; in-flight leases become stale.
    pub fn cancel_key(&self, key: &K) -> usize {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let pending = usize::from(state.pending.remove(key).is_some());
        let before = state.queue.len();
        state.queue.retain(|queued| &queued.request.key != key);
        let queued = before.saturating_sub(state.queue.len());
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(pending as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        if queued > 0 {
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
        pruned
    }

    /// Expire latest realtime current bindings older than `max_age`.
    pub fn expire_realtime_current_older_than(
        &self,
        max_age: Duration,
    ) -> Vec<ExpiredFrameWork<K, D>> {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let latest = state.latest_generation;
        let expired = state
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending.binding.priority == FrameWorkPriority::Current
                    && pending.binding.work_class != FrameWorkClass::Still
                    && pending.binding.generation >= latest
                    && elapsed_since(now, pending.requested_at) >= max_age
            })
            .map(|(key, pending)| ExpiredFrameWork { key: key.clone(), binding: pending.binding })
            .collect::<Vec<_>>();
        for request in &expired {
            state.pending.remove(&request.key);
        }
        state
            .queue
            .retain(|queued| !expired.iter().any(|item| item.key == queued.request.key));
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(expired.len() as u64);
        refresh_in_flight_invalidations_locked(&mut state, now);
        expired
    }

    /// Close the broker, clear pending/queued work, and wake every worker.
    pub fn close(&self) {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        if state.closed_at.is_none() {
            state.closed_at = Some(now);
        }
        state.pending.clear();
        state.queue.clear();
        refresh_in_flight_invalidations_locked(&mut state, now);
        self.shared.changed.notify_all();
    }

    /// Return whether one semantic key still owns a pending binding.
    pub fn has_pending_key(&self, key: &K) -> bool {
        lock_state(&self.shared.state).pending.contains_key(key)
    }

    /// Return whether one key/generation/class remains compatible with latest work.
    pub fn key_current(&self, key: &K, generation: u64, work_class: FrameWorkClass) -> bool {
        let mut state = lock_state(&self.shared.state);
        let Some(pending) = state.pending.get(key).copied() else {
            bump(&mut state.metrics.skipped_missing);
            return false;
        };
        if pending.binding.work_class != work_class {
            bump(&mut state.metrics.skipped_class_mismatch);
            return false;
        }
        if pending.binding.generation >= generation
            && pending.binding.generation >= state.latest_generation
        {
            return true;
        }
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        remove_key_locked(&mut state, key);
        refresh_in_flight_invalidations_locked(&mut state, now);
        bump(&mut state.metrics.skipped_obsolete);
        false
    }

    /// Return whether current work other than `key` is pending.
    pub fn has_other_current_key(&self, key: &K, realtime_only: bool) -> bool {
        let state = lock_state(&self.shared.state);
        state.pending.iter().any(|(pending_key, pending)| {
            pending_key != key
                && pending.binding.priority == FrameWorkPriority::Current
                && (!realtime_only || pending.binding.work_class != FrameWorkClass::Still)
                && pending.binding.generation >= state.latest_generation
        })
    }

    /// Return latest still keys that realtime current work may preempt.
    pub fn pending_still_except(&self, protected_key: &K) -> Vec<K> {
        let state = lock_state(&self.shared.state);
        state
            .pending
            .iter()
            .filter(|(key, pending)| {
                *key != protected_key
                    && pending.binding.priority == FrameWorkPriority::Current
                    && pending.binding.work_class == FrameWorkClass::Still
                    && pending.binding.generation >= state.latest_generation
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Cancel one still key only while it remains eligible for realtime preemption.
    pub fn cancel_preempted_still(&self, key: &K) -> bool {
        let mut state = lock_state(&self.shared.state);
        let eligible = state.pending.get(key).is_some_and(|pending| {
            pending.binding.priority == FrameWorkPriority::Current
                && pending.binding.work_class == FrameWorkClass::Still
                && pending.binding.generation >= state.latest_generation
        });
        if !eligible {
            return false;
        }
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        remove_key_locked(&mut state, key);
        refresh_in_flight_invalidations_locked(&mut state, now);
        bump(&mut state.metrics.evicted_still);
        true
    }

    /// Return stable lifecycle and queue evidence.
    pub fn diagnostics(&self) -> FrameWorkBrokerDiagnostics {
        let mut state = lock_state(&self.shared.state);
        let now = observe_now_locked(self.shared.clock.as_ref(), &mut state);
        let mut diagnostics = FrameWorkBrokerDiagnostics {
            latest_generation: state.latest_generation,
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
    FrameRequestBinding {
        generation: request.generation,
        priority: request.priority,
        work_class: request.work_class,
        demand_identity: request.demand_identity,
        deadline: request.deadline.map(FrameWorkDeadline::adapter_deadline),
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
    if request.work_class != FrameWorkClass::Still {
        if let Some(queued) = state
            .queue
            .iter()
            .find(|queued| queued.request.work_class == FrameWorkClass::Still)
        {
            return Some(Evicted::Still(queued.request.key.clone()));
        }
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
) -> (Option<K>, Option<K>)
where
    K: Clone + Eq + Hash,
{
    match eviction {
        Evicted::Prefetch(key) => {
            remove_key_locked(state, &key);
            bump(&mut state.metrics.evicted_prefetch);
            (Some(key), None)
        }
        Evicted::Still(key) => {
            remove_key_locked(state, &key);
            bump(&mut state.metrics.evicted_still);
            (None, Some(key))
        }
        Evicted::None => (None, None),
    }
}

fn remove_key_locked<K, D, P>(state: &mut BrokerState<K, D, P>, key: &K)
where
    K: Eq + Hash,
{
    state.pending.remove(key);
    state.queue.retain(|queued| &queued.request.key != key);
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
            && pending.binding.generation >= execution.generation
            && pending.binding.generation >= latest_generation
    })
}

fn oldest_other_current_request<K, D>(
    latest_generation: u64,
    pending: &HashMap<K, PendingBinding<D>>,
    execution: &InFlightWork<K>,
    realtime_only: bool,
) -> Option<MonotonicTimestamp>
where
    K: Eq,
{
    pending
        .iter()
        .filter(|(key, pending)| {
            *key != &execution.key
                && pending.binding.priority == FrameWorkPriority::Current
                && (!realtime_only || pending.binding.work_class != FrameWorkClass::Still)
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

fn refresh_in_flight_invalidations_locked<K, D, P>(
    state: &mut BrokerState<K, D, P>,
    now: MonotonicTimestamp,
) where
    K: Eq + Hash,
{
    let BrokerState { latest_generation, pending, in_flight, .. } = state;
    for execution in in_flight.values_mut() {
        if let Some(current) = current_pending_binding(*latest_generation, pending, execution) {
            execution.deadline_at = current.deadline_at;
            execution.invalidated_at = None;
        } else if execution.invalidated_at.is_none() {
            execution.invalidated_at = Some(now);
        }
        let preempted_at = if execution.priority == FrameWorkPriority::Prefetch {
            oldest_other_current_request(*latest_generation, pending, execution, false)
        } else if execution.work_class == FrameWorkClass::Still {
            oldest_other_current_request(*latest_generation, pending, execution, true)
        } else {
            None
        };
        if let Some(preempted_at) = preempted_at {
            execution.preempted_at = Some(
                execution
                    .preempted_at
                    .map_or(preempted_at, |existing| existing.min(preempted_at)),
            );
        }
    }
}

fn next_work_index<K, D, P>(
    queue: &VecDeque<QueuedWork<K, D, P>>,
    lane: FrameWorkerLane,
    now: MonotonicTimestamp,
) -> Option<usize> {
    queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| {
            queued.request.priority == FrameWorkPriority::Current
                && lane.accepts(queued.request.work_class)
        })
        .min_by_key(|(_, queued)| {
            (
                deadline_expired(queued.deadline_at, now),
                !lane.accepts(queued.request.work_class),
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
    let Some(pending) = state.pending.get(&execution.key) else {
        return CompletionCause::Missing;
    };
    if pending.binding.work_class != execution.work_class {
        return CompletionCause::ClassMismatch;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Clone)]
    struct ManualRuntimeClock {
        now_nanos: Arc<AtomicU64>,
    }

    impl ManualRuntimeClock {
        fn at(duration: Duration) -> Self {
            Self {
                now_nanos: Arc::new(AtomicU64::new(duration_nanos(duration))),
            }
        }

        fn set(&self, duration: Duration) {
            self.now_nanos.store(duration_nanos(duration), Ordering::Release);
        }
    }

    impl MonotonicRuntimeClock for ManualRuntimeClock {
        fn now(&self) -> MonotonicTimestamp {
            MonotonicTimestamp::from_duration(Duration::from_nanos(
                self.now_nanos.load(Ordering::Acquire),
            ))
        }
    }

    fn duration_nanos(duration: Duration) -> u64 {
        u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
    }

    fn request(
        key: u64,
        generation: u64,
        class: FrameWorkClass,
    ) -> FrameWorkRequest<u64, u64, u64> {
        FrameWorkRequest {
            key,
            generation,
            priority: FrameWorkPriority::Current,
            work_class: class,
            demand_identity: None,
            deadline: None,
            payload: key,
        }
    }

    #[test]
    fn submit_dequeue_and_completion_share_one_lifecycle() {
        let broker = FrameWorkBroker::new(4, 4);
        let generation = broker.begin_generation();
        assert!(matches!(
            broker.submit(request(1, generation, FrameWorkClass::Playback)),
            FrameWorkSubmission::Queued { .. }
        ));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(broker.execution_cancellation(execution.id), None);
        let resolution = broker.resolve_execution(execution.id, true);
        assert_eq!(resolution.completion, FrameRequestCompletion::Current);
        let diagnostics = broker.diagnostics();
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.queued_work, 0);
        assert_eq!(diagnostics.in_flight_work, 0);
    }

    #[test]
    fn current_still_work_preserves_decoder_lane_affinity() {
        let queue = VecDeque::from([QueuedWork {
            request: request(1, 1, FrameWorkClass::Still),
            deadline_at: None,
        }]);

        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Playback, MonotonicTimestamp::ZERO),
            None
        );
        assert_eq!(
            next_work_index(
                &queue,
                FrameWorkerLane::Interactive,
                MonotonicTimestamp::ZERO
            ),
            None
        );
        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Still, MonotonicTimestamp::ZERO),
            Some(0)
        );
        assert_eq!(
            next_work_index(
                &queue,
                FrameWorkerLane::NonPlayback,
                MonotonicTimestamp::ZERO
            ),
            Some(0)
        );
        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Any, MonotonicTimestamp::ZERO),
            Some(0)
        );
    }

    #[test]
    fn same_class_rerequest_reuses_in_flight_and_rebinds_completion() {
        let broker = FrameWorkBroker::new(2, 2);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        let latest = broker.begin_generation();
        let mut latest_request = request(1, latest, FrameWorkClass::Playback);
        latest_request.deadline = Some(FrameWorkDeadline::from_remaining(
            200,
            Duration::from_secs(1),
        ));
        assert_eq!(
            broker.submit(latest_request),
            FrameWorkSubmission::ReusedInFlight
        );
        let resolution = broker.resolve_execution(execution.id, true);
        assert_eq!(resolution.completion, FrameRequestCompletion::Current);
        assert_eq!(resolution.binding.expect("binding").deadline, Some(200));
    }

    #[test]
    fn execution_cancellation_tracks_obsolescence_and_rebinding() {
        let broker = FrameWorkBroker::new(2, 2);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(broker.execution_cancellation(execution.id), None);

        let latest = broker.begin_generation();
        assert!(matches!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::Superseded { age: Some(_) })
        ));
        assert_eq!(
            broker.submit(request(1, latest, FrameWorkClass::Playback)),
            FrameWorkSubmission::ReusedInFlight
        );
        assert_eq!(broker.execution_cancellation(execution.id), None);

        broker.cancel_all();
        assert!(matches!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::Superseded { age: Some(_) })
        ));
    }

    #[test]
    fn injected_clock_controls_cancellation_age_and_reports_regression() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };

        clock.set(Duration::from_millis(20));
        broker.begin_generation();
        clock.set(Duration::from_millis(27));
        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::Superseded { age: Some(Duration::from_millis(7)) })
        );

        clock.set(Duration::from_millis(25));
        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::Superseded { age: Some(Duration::from_millis(7)) })
        );
        assert_eq!(broker.diagnostics().clock_regressions, 1);
    }

    #[test]
    fn injected_clock_makes_realtime_expiration_exact() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(100));
        let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Interactive));

        clock.set(Duration::from_millis(149));
        assert!(broker.expire_realtime_current_older_than(Duration::from_millis(50)).is_empty());
        clock.set(Duration::from_millis(150));
        let expired = broker.expire_realtime_current_older_than(Duration::from_millis(50));

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].key, 1);
    }

    #[test]
    fn closing_broker_cancels_every_in_flight_execution() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };

        clock.set(Duration::from_millis(20));
        broker.close();
        clock.set(Duration::from_millis(27));

        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::BrokerClosed { age: Duration::from_millis(7) })
        );

        clock.set(Duration::from_millis(30));
        broker.close();
        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::BrokerClosed { age: Duration::from_millis(10) })
        );
    }

    #[test]
    fn still_preemption_age_starts_at_competing_request_admission() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Still));
        let execution = match broker.receive(FrameWorkerLane::Still) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(broker.execution_cancellation(execution.id), None);

        clock.set(Duration::from_millis(30));
        broker.submit(request(2, generation, FrameWorkClass::Playback));
        clock.set(Duration::from_millis(35));
        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(
                FrameExecutionCancellation::StillPreemptedByRealtimeCurrent {
                    request_age: Duration::from_millis(5)
                }
            )
        );
    }

    #[test]
    fn speculative_rerequest_cannot_change_current_interactive_work_class() {
        let broker = FrameWorkBroker::new(2, 2);
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Interactive));

        let mut speculative = request(1, generation, FrameWorkClass::Playback);
        speculative.priority = FrameWorkPriority::Prefetch;
        assert!(matches!(
            broker.submit(speculative),
            FrameWorkSubmission::UpdatedQueued {
                priority_promoted: false,
                work_class_changed: false,
                generation_changed: false,
            }
        ));

        let execution = match broker.receive(FrameWorkerLane::Interactive) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(execution.priority, FrameWorkPriority::Current);
        assert_eq!(execution.work_class, FrameWorkClass::Interactive);
    }

    #[test]
    fn class_change_queues_replacement_and_invalidates_old_execution() {
        let broker = FrameWorkBroker::new(2, 2);
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Interactive));
        let old = match broker.receive(FrameWorkerLane::Interactive) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert!(matches!(
            broker.submit(request(1, generation, FrameWorkClass::Playback)),
            FrameWorkSubmission::Queued { .. }
        ));
        assert!(matches!(
            broker.execution_cancellation(old.id),
            Some(FrameExecutionCancellation::Superseded { .. })
        ));
        assert_eq!(
            broker.resolve_execution(old.id, true).completion,
            FrameRequestCompletion::CacheOnly
        );
        assert_eq!(broker.diagnostics().queued_work, 1);
    }

    #[test]
    fn canceled_old_execution_leaves_new_binding_pending() {
        let broker = FrameWorkBroker::new(2, 2);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        let latest = broker.begin_generation();
        broker.submit(request(1, latest, FrameWorkClass::Playback));
        assert_eq!(
            broker.resolve_execution(execution.id, false).completion,
            FrameRequestCompletion::Stale
        );
        assert_eq!(broker.diagnostics().pending_requests, 1);
    }

    #[test]
    fn playback_deadline_expires_at_dequeue_without_losing_lease_identity() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
        let generation = broker.begin_generation();
        let mut request = request(1, generation, FrameWorkClass::Playback);
        request.deadline = Some(FrameWorkDeadline::from_remaining(
            100,
            Duration::from_millis(20),
        ));
        broker.submit(request);
        clock.set(Duration::from_millis(29));
        assert_eq!(broker.diagnostics().queued_expired_work, 0);
        clock.set(Duration::from_millis(30));
        assert_eq!(broker.diagnostics().queued_expired_work, 1);
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Expired(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(execution.key, 1);
        assert_eq!(execution.deadline, Some(100));
        assert_eq!(broker.diagnostics().dropped_expired_work, 1);
        assert_eq!(broker.diagnostics().dropped_expired_playback_current, 1);
        assert_eq!(
            broker.resolve_execution(execution.id, false).completion,
            FrameRequestCompletion::Current
        );
    }

    #[test]
    fn expired_prefetch_uses_generic_deadline_evidence() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
        let generation = broker.begin_generation();
        let mut request = request(1, generation, FrameWorkClass::Playback);
        request.priority = FrameWorkPriority::Prefetch;
        request.deadline = Some(FrameWorkDeadline::from_remaining(
            100,
            Duration::from_millis(20),
        ));
        broker.submit(request);
        clock.set(Duration::from_millis(30));

        assert!(matches!(
            broker.receive(FrameWorkerLane::Playback),
            Some(FrameWorkReceive::Expired(_))
        ));
        let diagnostics = broker.diagnostics();
        assert_eq!(diagnostics.dropped_expired_work, 1);
        assert_eq!(diagnostics.dropped_expired_playback_current, 0);
    }

    #[test]
    fn worker_completion_stamp_prevents_poll_delay_false_miss() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
        let generation = broker.begin_generation();
        let mut request = request(1, generation, FrameWorkClass::Playback);
        request.deadline = Some(FrameWorkDeadline::from_remaining(
            100,
            Duration::from_millis(20),
        ));
        broker.submit(request);
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };

        clock.set(Duration::from_millis(29));
        assert!(broker.mark_execution_completed(execution.id));
        clock.set(Duration::from_millis(40));
        let resolution = broker.resolve_execution(execution.id, true);

        assert_eq!(resolution.deadline, FrameWorkDeadlineStatus::OnTime);
    }

    #[test]
    fn in_flight_deadline_rebind_uses_latest_binding_and_exact_age() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(1, 1, clock.clone());
        let first = broker.begin_generation();
        let mut first_request = request(1, first, FrameWorkClass::Playback);
        first_request.deadline = Some(FrameWorkDeadline::from_remaining(
            100,
            Duration::from_millis(40),
        ));
        broker.submit(first_request);
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };

        clock.set(Duration::from_millis(20));
        let latest = broker.begin_generation();
        let mut rebound = request(1, latest, FrameWorkClass::Playback);
        rebound.deadline = Some(FrameWorkDeadline::from_remaining(
            200,
            Duration::from_millis(100),
        ));
        assert_eq!(broker.submit(rebound), FrameWorkSubmission::ReusedInFlight);

        clock.set(Duration::from_millis(50));
        assert_eq!(broker.execution_cancellation(execution.id), None);
        clock.set(Duration::from_millis(125));
        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::DeadlineExpired { age: Duration::from_millis(5) })
        );
        let resolution = broker.resolve_execution(execution.id, true);
        assert_eq!(
            resolution.binding.expect("latest binding").deadline,
            Some(200)
        );
    }

    #[test]
    fn earliest_deadline_wins_over_later_preemption_request() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
        let generation = broker.begin_generation();
        let mut prefetch = request(1, generation, FrameWorkClass::Playback);
        prefetch.priority = FrameWorkPriority::Prefetch;
        prefetch.deadline = Some(FrameWorkDeadline::from_remaining(
            100,
            Duration::from_millis(20),
        ));
        broker.submit(prefetch);
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };

        clock.set(Duration::from_millis(35));
        broker.submit(request(2, generation, FrameWorkClass::Playback));
        clock.set(Duration::from_millis(40));

        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::DeadlineExpired { age: Duration::from_millis(10) })
        );
    }

    #[test]
    fn earliest_preemption_survives_later_generation_invalidation() {
        let clock = ManualRuntimeClock::at(Duration::from_millis(10));
        let broker = FrameWorkBroker::new_with_clock(2, 2, clock.clone());
        let generation = broker.begin_generation();
        let mut prefetch = request(1, generation, FrameWorkClass::Playback);
        prefetch.priority = FrameWorkPriority::Prefetch;
        broker.submit(prefetch);
        let execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };

        clock.set(Duration::from_millis(20));
        broker.submit(request(2, generation, FrameWorkClass::Playback));
        clock.set(Duration::from_millis(30));
        broker.begin_generation();
        clock.set(Duration::from_millis(40));

        assert_eq!(
            broker.execution_cancellation(execution.id),
            Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent {
                request_age: Duration::from_millis(20),
            })
        );
    }

    #[test]
    fn cancellation_and_pruning_cover_pending_and_queue_atomically() {
        let broker = FrameWorkBroker::new(4, 4);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let latest = broker.begin_generation();
        assert_eq!(broker.prune_obsolete(), 1);
        broker.submit(request(2, latest, FrameWorkClass::Playback));
        assert_eq!(broker.cancel_key(&2), 1);
        let diagnostics = broker.diagnostics();
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.queued_work, 0);
    }

    #[test]
    fn diagnostics_derive_worker_lane_residency_from_execution_leases() {
        let broker = FrameWorkBroker::new(5, 5);
        let generation = broker.begin_generation();
        let cases = [
            (1, FrameWorkClass::Playback, FrameWorkerLane::Playback),
            (2, FrameWorkClass::Interactive, FrameWorkerLane::Interactive),
            (3, FrameWorkClass::Still, FrameWorkerLane::Still),
            (4, FrameWorkClass::Interactive, FrameWorkerLane::NonPlayback),
            (5, FrameWorkClass::Playback, FrameWorkerLane::Any),
        ];
        let mut execution_ids = Vec::with_capacity(cases.len());

        for (key, work_class, worker_lane) in cases {
            assert!(matches!(
                broker.submit(request(key, generation, work_class)),
                FrameWorkSubmission::Queued { .. }
            ));
            let execution = match broker.receive(worker_lane) {
                Some(FrameWorkReceive::Ready(execution)) => execution,
                other => panic!("unexpected receive for {worker_lane:?}: {other:?}"),
            };
            execution_ids.push(execution.id);
        }

        let diagnostics = broker.diagnostics();
        assert_eq!(diagnostics.in_flight_work, 5);
        assert_eq!(diagnostics.in_flight_current, 5);
        assert_eq!(diagnostics.in_flight_playback, 2);
        assert_eq!(diagnostics.in_flight_interactive, 2);
        assert_eq!(diagnostics.in_flight_still, 1);
        assert_eq!(diagnostics.in_flight_any_lane, 1);
        assert_eq!(diagnostics.in_flight_playback_lane, 1);
        assert_eq!(diagnostics.in_flight_interactive_lane, 1);
        assert_eq!(diagnostics.in_flight_still_lane, 1);
        assert_eq!(diagnostics.in_flight_non_playback_lane, 1);
        assert_eq!(diagnostics.in_flight_cross_lane_current, 0);

        for execution_id in execution_ids {
            assert!(broker.abandon_execution(execution_id));
        }
        let diagnostics = broker.diagnostics();
        assert_eq!(diagnostics.in_flight_work, 0);
        assert_eq!(diagnostics.in_flight_any_lane, 0);
        assert_eq!(diagnostics.in_flight_playback_lane, 0);
        assert_eq!(diagnostics.in_flight_interactive_lane, 0);
        assert_eq!(diagnostics.in_flight_still_lane, 0);
        assert_eq!(diagnostics.in_flight_non_playback_lane, 0);
        assert_eq!(diagnostics.in_flight_cross_lane_current, 0);
    }

    #[test]
    fn failed_dual_capacity_admission_preserves_every_existing_binding() {
        let broker = FrameWorkBroker::new(2, 1);
        let generation = broker.begin_generation();
        let mut in_flight_prefetch = request(1, generation, FrameWorkClass::Playback);
        in_flight_prefetch.priority = FrameWorkPriority::Prefetch;
        broker.submit(in_flight_prefetch);
        let prefetch_execution = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        broker.submit(request(2, generation, FrameWorkClass::Playback));

        assert_eq!(
            broker.submit(request(3, generation, FrameWorkClass::Playback)),
            FrameWorkSubmission::DroppedBackpressure
        );
        assert!(broker.has_pending_key(&1));
        assert!(broker.has_pending_key(&2));
        assert!(matches!(
            broker.execution_cancellation(prefetch_execution.id),
            Some(FrameExecutionCancellation::PrefetchPreemptedByCurrent { .. })
        ));
        let queued = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(queued.key, 2);
    }

    #[test]
    fn dual_capacity_admission_prefers_one_queued_eviction_that_opens_both_windows() {
        let broker = FrameWorkBroker::new(2, 1);
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Playback));
        let _current = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        let mut queued_prefetch = request(2, generation, FrameWorkClass::Playback);
        queued_prefetch.priority = FrameWorkPriority::Prefetch;
        broker.submit(queued_prefetch);

        assert_eq!(
            broker.submit(request(3, generation, FrameWorkClass::Playback)),
            FrameWorkSubmission::Queued { evicted_prefetch: Some(2), evicted_still: None }
        );
        assert!(!broker.has_pending_key(&2));
        assert!(broker.has_pending_key(&3));
    }

    #[test]
    fn failed_class_change_keeps_original_in_flight_binding_current() {
        let broker = FrameWorkBroker::new(2, 1);
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Playback));
        let original = match broker.receive(FrameWorkerLane::Playback) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        broker.submit(request(2, generation, FrameWorkClass::Playback));

        assert_eq!(
            broker.submit(request(1, generation, FrameWorkClass::Interactive)),
            FrameWorkSubmission::DroppedBackpressure
        );
        assert_eq!(broker.execution_cancellation(original.id), None);
        assert_eq!(broker.diagnostics().pending_requests, 2);
    }
}
