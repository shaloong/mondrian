//! Atomic frame-work lifecycle broker.
//!
//! This Module owns admission, queued transport, in-flight execution leases,
//! latest-wins invalidation, preemption, and completion binding under one lock.
//! Payloads, keys, and deadline values remain opaque Adapter data.

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::{
    FrameDemandIdentity, FrameRequestBinding, FrameRequestCompletion, FrameRequestResolution,
    FrameWorkClass, FrameWorkPriority,
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
    /// Adapter deadline carried without clock comparison by the Module.
    pub deadline: Option<D>,
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
    /// Expired playback-current executions returned to workers.
    pub dropped_expired_playback_current: u64,
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
    requested_at: Instant,
}

#[derive(Debug)]
struct QueuedWork<K, D, P> {
    request: FrameWorkRequest<K, D, P>,
}

#[derive(Debug, Clone)]
struct InFlightWork<K> {
    key: K,
    generation: u64,
    priority: FrameWorkPriority,
    work_class: FrameWorkClass,
    demand_identity: Option<FrameDemandIdentity>,
}

struct BrokerState<K, D, P> {
    latest_generation: u64,
    next_execution_id: u64,
    pending: HashMap<K, PendingBinding<D>>,
    queue: VecDeque<QueuedWork<K, D, P>>,
    in_flight: HashMap<FrameExecutionId, InFlightWork<K>>,
    closed: bool,
    metrics: FrameWorkBrokerMetrics,
}

#[derive(Default)]
struct FrameWorkBrokerMetrics {
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
        Self {
            shared: Arc::new(BrokerShared {
                state: Mutex::new(BrokerState {
                    latest_generation: 0,
                    next_execution_id: 1,
                    pending: HashMap::new(),
                    queue: VecDeque::new(),
                    in_flight: HashMap::new(),
                    closed: false,
                    metrics: FrameWorkBrokerMetrics::default(),
                }),
                changed: Condvar::new(),
                max_pending: max_pending.max(1),
                max_queued: max_queued.max(1),
            }),
        }
    }

    /// Begin a new latest-wins generation.
    pub fn begin_generation(&self) -> u64 {
        let mut state = lock_state(&self.shared.state);
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.latest_generation
    }

    /// Observe an externally allocated generation and prune older queued bindings.
    pub fn prune_before(&self, generation: u64) -> usize {
        let mut state = lock_state(&self.shared.state);
        state.latest_generation = state.latest_generation.max(generation);
        let before = state.queue.len();
        prune_obsolete_locked(&mut state);
        let pruned = before.saturating_sub(state.queue.len());
        state.metrics.pruned_queued = state.metrics.pruned_queued.saturating_add(pruned as u64);
        pruned
    }

    /// Atomically admit, queue, update, or bind one request to in-flight work.
    pub fn submit(&self, mut request: FrameWorkRequest<K, D, P>) -> FrameWorkSubmission<K> {
        let mut state = lock_state(&self.shared.state);
        if state.closed {
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
                            Instant::now()
                        } else {
                            previous.requested_at
                        },
                    },
                );
                state.queue[index] = QueuedWork { request };
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
                            Instant::now()
                        } else {
                            previous.requested_at
                        },
                    },
                );
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
                PendingBinding { binding, requested_at: Instant::now() },
            );
            bump(&mut state.metrics.submitted_work_class_changes);
            state.queue.push_back(QueuedWork { request });
            bump(&mut state.metrics.submitted_queued);
            self.shared.changed.notify_all();
            return FrameWorkSubmission::Queued { evicted_prefetch, evicted_still };
        }

        prune_obsolete_locked(&mut state);
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
                requested_at: Instant::now(),
            },
        );
        state.queue.push_back(QueuedWork { request });
        bump(&mut state.metrics.submitted_queued);
        self.shared.changed.notify_all();
        FrameWorkSubmission::Queued { evicted_prefetch, evicted_still }
    }

    /// Block until eligible work or closure, then create one execution lease.
    pub fn receive(
        &self,
        lane: FrameWorkerLane,
        deadline_expired: impl Fn(Option<D>) -> bool,
    ) -> Option<FrameWorkReceive<K, D, P>> {
        let mut state = lock_state(&self.shared.state);
        loop {
            if let Some(index) = next_work_index(&state.queue, lane, &deadline_expired) {
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
                        demand_identity: request.demand_identity,
                    },
                );
                let expired = request.priority == FrameWorkPriority::Current
                    && request.work_class == FrameWorkClass::Playback
                    && deadline_expired(request.deadline);
                if expired {
                    bump(&mut state.metrics.dropped_expired_playback_current);
                }
                let execution = FrameWorkExecution {
                    id,
                    key: request.key,
                    generation: request.generation,
                    priority: request.priority,
                    work_class: request.work_class,
                    demand_identity: request.demand_identity,
                    deadline: request.deadline,
                    payload: request.payload,
                };
                return Some(if expired {
                    FrameWorkReceive::Expired(execution)
                } else {
                    FrameWorkReceive::Ready(execution)
                });
            }
            if state.closed {
                return None;
            }
            state = wait_state(&self.shared.changed, state);
        }
    }

    /// Return whether an execution lease remains compatible with latest work.
    pub fn execution_current(&self, id: FrameExecutionId) -> bool {
        let state = lock_state(&self.shared.state);
        let Some(execution) = state.in_flight.get(&id) else {
            return false;
        };
        state.pending.get(&execution.key).is_some_and(|pending| {
            pending.binding.work_class == execution.work_class
                && pending.binding.generation >= execution.generation
                && pending.binding.generation >= state.latest_generation
        })
    }

    /// Return whether other latest current work is pending.
    pub fn has_other_current(&self, id: FrameExecutionId, realtime_only: bool) -> bool {
        let state = lock_state(&self.shared.state);
        let Some(execution) = state.in_flight.get(&id) else {
            return false;
        };
        state.pending.iter().any(|(key, pending)| {
            key != &execution.key
                && pending.binding.priority == FrameWorkPriority::Current
                && (!realtime_only || pending.binding.work_class != FrameWorkClass::Still)
                && pending.binding.generation >= state.latest_generation
        })
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
            };
        };
        let cause = completion_cause(&state, &execution, reusable);
        let resolution = resolve_locked(&mut state, execution, reusable);
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
        let execution = InFlightWork {
            key,
            generation,
            priority: FrameWorkPriority::Current,
            work_class,
            demand_identity,
        };
        let cause = completion_cause(&state, &execution, reusable);
        let resolution = resolve_locked(&mut state, execution, reusable);
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
        let pending = usize::from(state.pending.remove(key).is_some());
        let before = state.queue.len();
        state.queue.retain(|queued| &queued.request.key != key);
        let queued = before.saturating_sub(state.queue.len());
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(pending as u64);
        if queued > 0 {
            self.shared.changed.notify_all();
        }
        queued
    }

    /// Cancel all pending/queued work and start a new generation.
    pub fn cancel_all(&self) -> (u64, usize) {
        let mut state = lock_state(&self.shared.state);
        let canceled = state.pending.len() as u64;
        let queued = state.queue.len();
        state.pending.clear();
        state.queue.clear();
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.metrics.canceled_requests = state.metrics.canceled_requests.saturating_add(canceled);
        self.shared.changed.notify_all();
        (state.latest_generation, queued)
    }

    /// Remove obsolete pending/queued work under the same lifecycle lock.
    pub fn prune_obsolete(&self) -> usize {
        let mut state = lock_state(&self.shared.state);
        let before = state.queue.len();
        prune_obsolete_locked(&mut state);
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
        let now = Instant::now();
        let latest = state.latest_generation;
        let expired = state
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending.binding.priority == FrameWorkPriority::Current
                    && pending.binding.work_class != FrameWorkClass::Still
                    && pending.binding.generation >= latest
                    && now.saturating_duration_since(pending.requested_at) >= max_age
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
        expired
    }

    /// Close the broker, clear pending/queued work, and wake every worker.
    pub fn close(&self) {
        let mut state = lock_state(&self.shared.state);
        state.closed = true;
        state.pending.clear();
        state.queue.clear();
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
        remove_key_locked(&mut state, key);
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
        remove_key_locked(&mut state, key);
        bump(&mut state.metrics.evicted_still);
        true
    }

    /// Return stable lifecycle and queue evidence.
    pub fn diagnostics(
        &self,
        deadline_expired: impl Fn(Option<D>) -> bool,
    ) -> FrameWorkBrokerDiagnostics {
        let state = lock_state(&self.shared.state);
        let mut diagnostics = FrameWorkBrokerDiagnostics {
            latest_generation: state.latest_generation,
            pending_requests: state.pending.len(),
            queued_work: state.queue.len(),
            in_flight_work: state.in_flight.len(),
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
            closed: state.closed,
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
                && deadline_expired(queued.request.deadline)
            {
                diagnostics.queued_expired_playback_current += 1;
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
        deadline: request.deadline,
    }
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

fn next_work_index<K, D, P>(
    queue: &VecDeque<QueuedWork<K, D, P>>,
    lane: FrameWorkerLane,
    deadline_expired: &impl Fn(Option<D>) -> bool,
) -> Option<usize>
where
    D: Copy,
{
    queue
        .iter()
        .enumerate()
        .filter(|(_, queued)| {
            queued.request.priority == FrameWorkPriority::Current
                && lane.accepts(queued.request.work_class)
        })
        .min_by_key(|(_, queued)| {
            (
                deadline_expired(queued.request.deadline),
                !lane.accepts(queued.request.work_class),
                work_rank(queued.request.work_class),
            )
        })
        .map(|(index, _)| index)
        .or_else(|| {
            queue.iter().position(|queued| {
                queued.request.priority == FrameWorkPriority::Prefetch
                    && lane.accepts(queued.request.work_class)
            })
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
        };
    }
    let changed = pending.binding.generation != execution.generation
        || pending.binding.demand_identity != execution.demand_identity;
    if changed && !reusable {
        return FrameRequestResolution {
            completion: FrameRequestCompletion::Stale,
            binding: None,
        };
    }
    state.pending.remove(&execution.key);
    if pending.binding.generation >= state.latest_generation
        || execution.generation >= state.latest_generation
    {
        FrameRequestResolution {
            completion: FrameRequestCompletion::Current,
            binding: Some(pending.binding),
        }
    } else {
        FrameRequestResolution {
            completion: FrameRequestCompletion::Stale,
            binding: None,
        }
    }
}

fn bump(value: &mut u64) {
    *value = value.saturating_add(1);
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
            deadline: Some(100),
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
        let execution = match broker.receive(FrameWorkerLane::Playback, |_| false) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert!(broker.execution_current(execution.id));
        let resolution = broker.resolve_execution(execution.id, true);
        assert_eq!(resolution.completion, FrameRequestCompletion::Current);
        let diagnostics = broker.diagnostics(|_| false);
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.queued_work, 0);
        assert_eq!(diagnostics.in_flight_work, 0);
    }

    #[test]
    fn current_still_work_preserves_decoder_lane_affinity() {
        let queue = VecDeque::from([QueuedWork { request: request(1, 1, FrameWorkClass::Still) }]);

        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Playback, &|_| false),
            None
        );
        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Interactive, &|_| false),
            None
        );
        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Still, &|_| false),
            Some(0)
        );
        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::NonPlayback, &|_| false),
            Some(0)
        );
        assert_eq!(
            next_work_index(&queue, FrameWorkerLane::Any, &|_| false),
            Some(0)
        );
    }

    #[test]
    fn same_class_rerequest_reuses_in_flight_and_rebinds_completion() {
        let broker = FrameWorkBroker::new(2, 2);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback, |_| false) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        let latest = broker.begin_generation();
        let mut latest_request = request(1, latest, FrameWorkClass::Playback);
        latest_request.deadline = Some(200);
        assert_eq!(
            broker.submit(latest_request),
            FrameWorkSubmission::ReusedInFlight
        );
        let resolution = broker.resolve_execution(execution.id, true);
        assert_eq!(resolution.completion, FrameRequestCompletion::Current);
        assert_eq!(resolution.binding.expect("binding").deadline, Some(200));
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

        let execution = match broker.receive(FrameWorkerLane::Interactive, |_| false) {
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
        let old = match broker.receive(FrameWorkerLane::Interactive, |_| false) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert!(matches!(
            broker.submit(request(1, generation, FrameWorkClass::Playback)),
            FrameWorkSubmission::Queued { .. }
        ));
        assert!(!broker.execution_current(old.id));
        assert_eq!(
            broker.resolve_execution(old.id, true).completion,
            FrameRequestCompletion::CacheOnly
        );
        assert_eq!(broker.diagnostics(|_| false).queued_work, 1);
    }

    #[test]
    fn canceled_old_execution_leaves_new_binding_pending() {
        let broker = FrameWorkBroker::new(2, 2);
        let first = broker.begin_generation();
        broker.submit(request(1, first, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback, |_| false) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        let latest = broker.begin_generation();
        broker.submit(request(1, latest, FrameWorkClass::Playback));
        assert_eq!(
            broker.resolve_execution(execution.id, false).completion,
            FrameRequestCompletion::Stale
        );
        assert_eq!(broker.diagnostics(|_| false).pending_requests, 1);
    }

    #[test]
    fn playback_deadline_expires_at_dequeue_without_losing_lease_identity() {
        let broker = FrameWorkBroker::new(1, 1);
        let generation = broker.begin_generation();
        broker.submit(request(1, generation, FrameWorkClass::Playback));
        let execution = match broker.receive(FrameWorkerLane::Playback, |deadline| {
            deadline.is_some_and(|value| value <= 100)
        }) {
            Some(FrameWorkReceive::Expired(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        assert_eq!(execution.key, 1);
        assert_eq!(
            broker.resolve_execution(execution.id, false).completion,
            FrameRequestCompletion::Current
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
        let diagnostics = broker.diagnostics(|_| false);
        assert_eq!(diagnostics.pending_requests, 0);
        assert_eq!(diagnostics.queued_work, 0);
    }

    #[test]
    fn failed_dual_capacity_admission_preserves_every_existing_binding() {
        let broker = FrameWorkBroker::new(2, 1);
        let generation = broker.begin_generation();
        let mut in_flight_prefetch = request(1, generation, FrameWorkClass::Playback);
        in_flight_prefetch.priority = FrameWorkPriority::Prefetch;
        broker.submit(in_flight_prefetch);
        let prefetch_execution = match broker.receive(FrameWorkerLane::Playback, |_| false) {
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
        assert!(broker.execution_current(prefetch_execution.id));
        let queued = match broker.receive(FrameWorkerLane::Playback, |_| false) {
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
        let _current = match broker.receive(FrameWorkerLane::Playback, |_| false) {
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
        let original = match broker.receive(FrameWorkerLane::Playback, |_| false) {
            Some(FrameWorkReceive::Ready(execution)) => execution,
            other => panic!("unexpected receive: {other:?}"),
        };
        broker.submit(request(2, generation, FrameWorkClass::Playback));

        assert_eq!(
            broker.submit(request(1, generation, FrameWorkClass::Interactive)),
            FrameWorkSubmission::DroppedBackpressure
        );
        assert!(broker.execution_current(original.id));
        assert_eq!(broker.diagnostics(|_| false).pending_requests, 2);
    }
}
