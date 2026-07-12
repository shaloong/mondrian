//! Bounded, latest-wins scheduling policy for frame-producing Adapters.
//!
//! The scheduler owns semantic admission and completion policy. It deliberately
//! does not know about codecs, files, GPU resources, worker threads, or UI
//! gestures. Adapters lower those concerns to [`FrameWorkClass`] and carry their
//! own opaque request key through this Module.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::FrameDemandIdentity;

/// Semantic class of frame-producing work at the Playback seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameWorkClass {
    /// Current playback cursor or forward playback prefetch.
    Playback,
    /// Latest-wins interactive playhead movement, jog, or shuttle work.
    Interactive,
    /// Deterministic one-off still extraction without realtime privilege.
    Still,
}

impl FrameWorkClass {
    const fn is_realtime(self) -> bool {
        !matches!(self, Self::Still)
    }
}

/// Admission priority for frame-producing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameWorkPriority {
    /// Speculative work that must yield to visible work.
    Prefetch,
    /// Work required for the current visible position.
    Current,
}

impl FrameWorkPriority {
    const fn promoted_with(self, requested: Self) -> Self {
        match (self, requested) {
            (Self::Current, _) | (_, Self::Current) => Self::Current,
            (Self::Prefetch, Self::Prefetch) => Self::Prefetch,
        }
    }

    const fn accepts(self, class: FrameWorkClass) -> bool {
        matches!(self, Self::Current) || matches!(class, FrameWorkClass::Playback)
    }
}

/// Result of admitting one request into a [`FrameRequestScheduler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameRequestAdmission<K> {
    /// The request was admitted; lower-priority work may have been evicted.
    Scheduled {
        /// Speculative request evicted to make bounded room.
        evicted_prefetch: Option<K>,
        /// Still request evicted in favor of realtime current work.
        evicted_still: Option<K>,
    },
    /// The same key was already pending and was updated in place.
    AlreadyPending {
        /// Whether the semantic work class changed.
        work_class_changed: bool,
    },
    /// The bounded pending window could not admit this work.
    DroppedBackpressure,
    /// The priority/class pair violated policy, such as still prefetch.
    DroppedInvalidClass,
}

/// Freshness disposition for completed Adapter work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRequestCompletion {
    /// The completion satisfies the current request.
    Current,
    /// The completion may populate a cache but must not be presented as current.
    CacheOnly,
    /// The completion is obsolete and must not affect visible state.
    Stale,
}

impl FrameRequestCompletion {
    /// Return whether this completion may satisfy visible current-frame work.
    pub const fn is_current(self) -> bool {
        matches!(self, Self::Current)
    }

    /// Return whether an Adapter may retain the result in a semantic cache.
    pub const fn should_cache(self) -> bool {
        matches!(self, Self::Current | Self::CacheOnly)
    }
}

/// One realtime current request canceled after exceeding its residency limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredFrameRequest<K, D> {
    /// Opaque Adapter request key.
    pub key: K,
    /// Semantic class originally admitted for the request.
    pub work_class: FrameWorkClass,
    /// Playback demand identity, when this work was demand-backed.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Adapter-owned deadline in the same clock domain supplied at admission.
    pub deadline: Option<D>,
}

/// Latest semantic binding attached to one admitted request key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequestBinding<D> {
    /// Latest-wins generation that owns this binding.
    pub generation: u64,
    /// Current or speculative admission priority.
    pub priority: FrameWorkPriority,
    /// Semantic work class used for worker eligibility.
    pub work_class: FrameWorkClass,
    /// Playback demand identity, when the request is demand-backed.
    pub demand_identity: Option<FrameDemandIdentity>,
    /// Adapter-owned deadline; the Playback Module carries but never compares it.
    pub deadline: Option<D>,
}

/// Atomic completion decision plus the binding it is allowed to satisfy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRequestResolution<D> {
    /// Visibility/cache freshness classification.
    pub completion: FrameRequestCompletion,
    /// Exact latest binding satisfied by a current reusable completion.
    pub binding: Option<FrameRequestBinding<D>>,
}

#[derive(Debug, Clone, Copy)]
struct PendingFrameRequest<D> {
    generation: u64,
    priority: FrameWorkPriority,
    work_class: FrameWorkClass,
    requested_at: Instant,
    demand_identity: Option<FrameDemandIdentity>,
    deadline: Option<D>,
}

#[derive(Debug, Default)]
struct FrameRequestSchedulerState<K, D> {
    latest_generation: u64,
    pending: HashMap<K, PendingFrameRequest<D>>,
    metrics: FrameRequestSchedulerMetrics,
}

#[derive(Debug, Clone, Copy, Default)]
struct FrameRequestSchedulerMetrics {
    scheduled_requests: u64,
    already_pending_requests: u64,
    already_pending_class_changes: u64,
    dropped_backpressure_requests: u64,
    dropped_invalid_class_requests: u64,
    dropped_obsolete_generation_requests: u64,
    dropped_pending_window_requests: u64,
    skipped_work: u64,
    skipped_missing_pending: u64,
    skipped_class_mismatch: u64,
    skipped_obsolete_generation: u64,
    completed_current: u64,
    completed_cache_only: u64,
    completed_cache_only_missing_pending: u64,
    completed_cache_only_class_mismatch: u64,
    completed_stale: u64,
    completed_stale_missing_pending: u64,
    completed_stale_class_mismatch: u64,
    completed_stale_obsolete_generation: u64,
    canceled_requests: u64,
    pruned_obsolete_requests: u64,
    evicted_prefetch_requests: u64,
    evicted_still_requests: u64,
}

/// Stable scheduler evidence, independent of any concrete presentation Adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct FrameRequestSchedulerDiagnostics {
    /// Latest semantic generation observed by the scheduler.
    pub latest_generation: u64,
    /// Requests currently admitted and not terminal.
    pub pending_requests: usize,
    /// Newly admitted requests.
    pub scheduled_requests: u64,
    /// Requests that updated an existing key.
    pub already_pending_requests: u64,
    /// Existing requests whose semantic work class changed.
    pub already_pending_class_changes: u64,
    /// Requests rejected by generation or bounded-window pressure.
    pub dropped_backpressure_requests: u64,
    /// Requests rejected because their priority/class pair was invalid.
    pub dropped_invalid_class_requests: u64,
    /// Requests rejected because their generation was obsolete.
    pub dropped_obsolete_generation_requests: u64,
    /// Requests rejected because no eligible bounded-window room existed.
    pub dropped_pending_window_requests: u64,
    /// Worker attempts skipped because they were no longer eligible.
    pub skipped_work: u64,
    /// Worker attempts skipped because no pending request remained.
    pub skipped_missing_pending: u64,
    /// Worker attempts skipped because the work class no longer matched.
    pub skipped_class_mismatch: u64,
    /// Worker attempts skipped because their generation was obsolete.
    pub skipped_obsolete_generation: u64,
    /// Completions accepted as current.
    pub completed_current: u64,
    /// Completions allowed to populate cache only.
    pub completed_cache_only: u64,
    /// Cache-only completions with no pending request.
    pub completed_cache_only_missing_pending: u64,
    /// Cache-only completions whose work class no longer matched.
    pub completed_cache_only_class_mismatch: u64,
    /// Completions rejected as stale.
    pub completed_stale: u64,
    /// Stale completions with no pending request.
    pub completed_stale_missing_pending: u64,
    /// Stale completions whose work class no longer matched.
    pub completed_stale_class_mismatch: u64,
    /// Stale completions from an obsolete generation.
    pub completed_stale_obsolete_generation: u64,
    /// Pending requests explicitly canceled or expired.
    pub canceled_requests: u64,
    /// Obsolete requests removed during generation pruning.
    pub pruned_obsolete_requests: u64,
    /// Prefetch requests evicted for current work.
    pub evicted_prefetch_requests: u64,
    /// Still requests evicted for realtime current work.
    pub evicted_still_requests: u64,
}

/// Thread-safe bounded scheduler shared by presentation and decode Adapters.
///
/// `K` is owned by the Adapter and is intentionally opaque to playback policy.
pub struct FrameRequestScheduler<K, D = ()> {
    state: Arc<Mutex<FrameRequestSchedulerState<K, D>>>,
    max_pending: usize,
}

impl<K, D> Clone for FrameRequestScheduler<K, D> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
            max_pending: self.max_pending,
        }
    }
}

impl<K, D> FrameRequestScheduler<K, D>
where
    K: Clone + Eq + Hash,
    D: Copy + PartialEq,
{
    /// Construct a scheduler with a strict nonzero pending-work budget.
    pub fn with_max_pending(max_pending: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(FrameRequestSchedulerState {
                latest_generation: 0,
                pending: HashMap::new(),
                metrics: FrameRequestSchedulerMetrics::default(),
            })),
            max_pending: max_pending.max(1),
        }
    }

    /// Begin a new latest-wins generation.
    pub fn begin_generation(&self) -> u64 {
        let mut state = lock_state(&self.state);
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.latest_generation
    }

    /// Admit or update one request with optional Playback demand identity.
    pub fn request(
        &self,
        key: K,
        generation: u64,
        priority: FrameWorkPriority,
        work_class: FrameWorkClass,
        demand_identity: Option<FrameDemandIdentity>,
        deadline: Option<D>,
    ) -> FrameRequestAdmission<K> {
        let mut state = lock_state(&self.state);
        if !priority.accepts(work_class) {
            bump(&mut state.metrics.dropped_invalid_class_requests);
            return FrameRequestAdmission::DroppedInvalidClass;
        }
        if generation < state.latest_generation {
            bump(&mut state.metrics.dropped_backpressure_requests);
            bump(&mut state.metrics.dropped_obsolete_generation_requests);
            return FrameRequestAdmission::DroppedBackpressure;
        }
        if let Some(pending) = state.pending.get_mut(&key) {
            let previous_class = pending.work_class;
            let promoted_class =
                promoted_work_class(pending.priority, pending.work_class, priority, work_class);
            let changed = pending.generation != generation
                || previous_class != promoted_class
                || pending.demand_identity != demand_identity
                || pending.deadline != deadline;
            pending.work_class = promoted_class;
            pending.generation = generation;
            pending.priority = pending.priority.promoted_with(priority);
            pending.demand_identity = demand_identity;
            pending.deadline = deadline;
            if changed {
                pending.requested_at = Instant::now();
            }
            let class_changed = previous_class != pending.work_class;
            prune_obsolete_locked(&mut state);
            bump(&mut state.metrics.already_pending_requests);
            if class_changed {
                bump(&mut state.metrics.already_pending_class_changes);
            }
            return FrameRequestAdmission::AlreadyPending { work_class_changed: class_changed };
        }

        prune_obsolete_locked(&mut state);
        let mut evicted_prefetch = None;
        let mut evicted_still = None;
        if state.pending.len() >= self.max_pending {
            if priority == FrameWorkPriority::Current {
                if let Some(evicted) = find_key(&state.pending, |pending| {
                    pending.priority == FrameWorkPriority::Prefetch
                }) {
                    state.pending.remove(&evicted);
                    evicted_prefetch = Some(evicted);
                    bump(&mut state.metrics.evicted_prefetch_requests);
                } else if work_class.is_realtime() {
                    if let Some(evicted) = find_key(&state.pending, |pending| {
                        pending.priority == FrameWorkPriority::Current
                            && pending.work_class == FrameWorkClass::Still
                    }) {
                        state.pending.remove(&evicted);
                        evicted_still = Some(evicted);
                        bump(&mut state.metrics.evicted_still_requests);
                    } else {
                        record_window_drop(&mut state.metrics);
                        return FrameRequestAdmission::DroppedBackpressure;
                    }
                } else {
                    record_window_drop(&mut state.metrics);
                    return FrameRequestAdmission::DroppedBackpressure;
                }
            } else {
                record_window_drop(&mut state.metrics);
                return FrameRequestAdmission::DroppedBackpressure;
            }
        }

        state.pending.insert(
            key,
            PendingFrameRequest {
                generation,
                priority,
                work_class,
                requested_at: Instant::now(),
                demand_identity,
                deadline,
            },
        );
        bump(&mut state.metrics.scheduled_requests);
        FrameRequestAdmission::Scheduled { evicted_prefetch, evicted_still }
    }

    /// Return whether a worker may begin the current semantic request.
    pub fn should_execute(&self, key: &K, work_class: FrameWorkClass) -> bool {
        let mut state = lock_state(&self.state);
        let Some(pending) = state.pending.get(key).copied() else {
            bump(&mut state.metrics.skipped_work);
            bump(&mut state.metrics.skipped_missing_pending);
            return false;
        };
        if pending.work_class != work_class {
            bump(&mut state.metrics.skipped_work);
            bump(&mut state.metrics.skipped_class_mismatch);
            return false;
        }
        if pending.generation >= state.latest_generation {
            return true;
        }
        state.pending.remove(key);
        bump(&mut state.metrics.skipped_work);
        bump(&mut state.metrics.skipped_obsolete_generation);
        false
    }

    /// Check whether in-flight execution still matches current admitted work.
    pub fn is_execution_current(
        &self,
        key: &K,
        generation: u64,
        work_class: FrameWorkClass,
    ) -> bool {
        let state = lock_state(&self.state);
        state.pending.get(key).is_some_and(|pending| {
            pending.work_class == work_class
                && pending.generation >= generation
                && pending.generation >= state.latest_generation
        })
    }

    /// Return whether other current work is pending in the latest generation.
    pub fn has_other_current(&self, key: &K, realtime_only: bool) -> bool {
        let state = lock_state(&self.state);
        state.pending.iter().any(|(pending_key, pending)| {
            pending_key != key
                && pending.priority == FrameWorkPriority::Current
                && (!realtime_only || pending.work_class.is_realtime())
                && pending.generation >= state.latest_generation
        })
    }

    /// Atomically resolve one completion against the latest binding.
    ///
    /// A reusable result (decoded frame or deterministic failure for the same
    /// opaque key) may satisfy a newer binding. A canceled execution cannot:
    /// it leaves the newer request pending instead of consuming its ownership.
    pub fn resolve_completion(
        &self,
        key: &K,
        result_generation: u64,
        work_class: FrameWorkClass,
        result_demand_identity: Option<FrameDemandIdentity>,
        reusable: bool,
    ) -> FrameRequestResolution<D> {
        let mut state = lock_state(&self.state);
        let result_is_latest = result_generation >= state.latest_generation;
        let Some(pending) = state.pending.get(key).copied() else {
            if result_is_latest {
                bump(&mut state.metrics.completed_cache_only);
                bump(&mut state.metrics.completed_cache_only_missing_pending);
                return FrameRequestResolution {
                    completion: FrameRequestCompletion::CacheOnly,
                    binding: None,
                };
            }
            bump(&mut state.metrics.completed_stale);
            bump(&mut state.metrics.completed_stale_missing_pending);
            return FrameRequestResolution {
                completion: FrameRequestCompletion::Stale,
                binding: None,
            };
        };
        if pending.work_class != work_class {
            if result_is_latest {
                bump(&mut state.metrics.completed_cache_only);
                bump(&mut state.metrics.completed_cache_only_class_mismatch);
                return FrameRequestResolution {
                    completion: FrameRequestCompletion::CacheOnly,
                    binding: None,
                };
            }
            bump(&mut state.metrics.completed_stale);
            bump(&mut state.metrics.completed_stale_class_mismatch);
            return FrameRequestResolution {
                completion: FrameRequestCompletion::Stale,
                binding: None,
            };
        }
        let binding_changed = pending.generation != result_generation
            || pending.demand_identity != result_demand_identity;
        if binding_changed && !reusable {
            bump(&mut state.metrics.completed_stale);
            bump(&mut state.metrics.completed_stale_obsolete_generation);
            return FrameRequestResolution {
                completion: FrameRequestCompletion::Stale,
                binding: None,
            };
        }
        state.pending.remove(key);
        if pending.generation >= state.latest_generation || result_is_latest {
            bump(&mut state.metrics.completed_current);
            FrameRequestResolution {
                completion: FrameRequestCompletion::Current,
                binding: Some(FrameRequestBinding {
                    generation: pending.generation,
                    priority: pending.priority,
                    work_class: pending.work_class,
                    demand_identity: pending.demand_identity,
                    deadline: pending.deadline,
                }),
            }
        } else {
            bump(&mut state.metrics.completed_stale);
            bump(&mut state.metrics.completed_stale_obsolete_generation);
            FrameRequestResolution {
                completion: FrameRequestCompletion::Stale,
                binding: None,
            }
        }
    }

    /// Cancel one pending key.
    pub fn cancel(&self, key: &K) {
        let mut state = lock_state(&self.state);
        if state.pending.remove(key).is_some() {
            bump(&mut state.metrics.canceled_requests);
        }
    }

    /// Expire latest-generation realtime current work older than `max_age`.
    pub fn expire_realtime_current_older_than(
        &self,
        max_age: Duration,
    ) -> Vec<ExpiredFrameRequest<K, D>> {
        let mut state = lock_state(&self.state);
        let now = Instant::now();
        let latest = state.latest_generation;
        let expired = state
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending.priority == FrameWorkPriority::Current
                    && pending.work_class.is_realtime()
                    && pending.generation >= latest
                    && now.saturating_duration_since(pending.requested_at) >= max_age
            })
            .map(|(key, pending)| ExpiredFrameRequest {
                key: key.clone(),
                work_class: pending.work_class,
                demand_identity: pending.demand_identity,
                deadline: pending.deadline,
            })
            .collect::<Vec<_>>();
        for request in &expired {
            state.pending.remove(&request.key);
        }
        state.metrics.canceled_requests =
            state.metrics.canceled_requests.saturating_add(expired.len() as u64);
        expired
    }

    /// Return latest-generation still keys eligible for realtime preemption.
    pub fn pending_still_except(&self, protected_key: &K) -> Vec<K> {
        let state = lock_state(&self.state);
        state
            .pending
            .iter()
            .filter(|(key, pending)| {
                *key != protected_key
                    && pending.priority == FrameWorkPriority::Current
                    && pending.work_class == FrameWorkClass::Still
                    && pending.generation >= state.latest_generation
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Cancel a still request only if it remains eligible for realtime preemption.
    pub fn cancel_preempted_still(&self, key: &K) -> bool {
        let mut state = lock_state(&self.state);
        let eligible = state.pending.get(key).is_some_and(|pending| {
            pending.priority == FrameWorkPriority::Current
                && pending.work_class == FrameWorkClass::Still
                && pending.generation >= state.latest_generation
        });
        if !eligible {
            return false;
        }
        state.pending.remove(key);
        bump(&mut state.metrics.evicted_still_requests);
        true
    }

    /// Cancel every pending request and invalidate in-flight work.
    pub fn cancel_all(&self) -> u64 {
        let mut state = lock_state(&self.state);
        let canceled = state.pending.len() as u64;
        state.pending.clear();
        state.latest_generation = state.latest_generation.saturating_add(1);
        state.metrics.canceled_requests = state.metrics.canceled_requests.saturating_add(canceled);
        state.latest_generation
    }

    /// Remove requests older than the latest semantic generation.
    pub fn prune_obsolete(&self) {
        prune_obsolete_locked(&mut lock_state(&self.state));
    }

    /// Return stable point-in-time scheduling evidence.
    pub fn diagnostics(&self) -> FrameRequestSchedulerDiagnostics {
        let state = lock_state(&self.state);
        let m = state.metrics;
        FrameRequestSchedulerDiagnostics {
            latest_generation: state.latest_generation,
            pending_requests: state.pending.len(),
            scheduled_requests: m.scheduled_requests,
            already_pending_requests: m.already_pending_requests,
            already_pending_class_changes: m.already_pending_class_changes,
            dropped_backpressure_requests: m.dropped_backpressure_requests,
            dropped_invalid_class_requests: m.dropped_invalid_class_requests,
            dropped_obsolete_generation_requests: m.dropped_obsolete_generation_requests,
            dropped_pending_window_requests: m.dropped_pending_window_requests,
            skipped_work: m.skipped_work,
            skipped_missing_pending: m.skipped_missing_pending,
            skipped_class_mismatch: m.skipped_class_mismatch,
            skipped_obsolete_generation: m.skipped_obsolete_generation,
            completed_current: m.completed_current,
            completed_cache_only: m.completed_cache_only,
            completed_cache_only_missing_pending: m.completed_cache_only_missing_pending,
            completed_cache_only_class_mismatch: m.completed_cache_only_class_mismatch,
            completed_stale: m.completed_stale,
            completed_stale_missing_pending: m.completed_stale_missing_pending,
            completed_stale_class_mismatch: m.completed_stale_class_mismatch,
            completed_stale_obsolete_generation: m.completed_stale_obsolete_generation,
            canceled_requests: m.canceled_requests,
            pruned_obsolete_requests: m.pruned_obsolete_requests,
            evicted_prefetch_requests: m.evicted_prefetch_requests,
            evicted_still_requests: m.evicted_still_requests,
        }
    }

    /// Return the number of currently admitted requests.
    pub fn pending_len(&self) -> usize {
        lock_state(&self.state).pending.len()
    }

    /// Return whether one opaque key is currently pending.
    pub fn has_pending_key(&self, key: &K) -> bool {
        lock_state(&self.state).pending.contains_key(key)
    }
}

fn promoted_work_class(
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

fn find_key<K, D>(
    pending: &HashMap<K, PendingFrameRequest<D>>,
    predicate: impl Fn(&PendingFrameRequest<D>) -> bool,
) -> Option<K>
where
    K: Clone + Eq + Hash,
{
    pending.iter().find(|(_, value)| predicate(value)).map(|(key, _)| key.clone())
}

fn prune_obsolete_locked<K, D>(state: &mut FrameRequestSchedulerState<K, D>) {
    let latest = state.latest_generation;
    let before = state.pending.len();
    state.pending.retain(|_, pending| pending.generation >= latest);
    state.metrics.pruned_obsolete_requests = state
        .metrics
        .pruned_obsolete_requests
        .saturating_add(before.saturating_sub(state.pending.len()) as u64);
}

fn record_window_drop(metrics: &mut FrameRequestSchedulerMetrics) {
    bump(&mut metrics.dropped_backpressure_requests);
    bump(&mut metrics.dropped_pending_window_requests);
}

fn bump(value: &mut u64) {
    *value = value.saturating_add(1);
}

fn lock_state<K, D>(
    state: &Mutex<FrameRequestSchedulerState<K, D>>,
) -> MutexGuard<'_, FrameRequestSchedulerState<K, D>> {
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_wins_is_bounded_across_one_hundred_headless_seeks() {
        let scheduler = FrameRequestScheduler::<u64, ()>::with_max_pending(4);
        let mut completions = Vec::new();

        for key in 0_u64..100 {
            let generation = scheduler.begin_generation();
            assert!(matches!(
                scheduler.request(
                    key,
                    generation,
                    FrameWorkPriority::Current,
                    FrameWorkClass::Interactive,
                    None,
                    None,
                ),
                FrameRequestAdmission::Scheduled { .. }
            ));
            scheduler.prune_obsolete();
            assert!(scheduler.pending_len() <= 1);
            if key > 0 {
                completions.push(scheduler.resolve_completion(
                    &(key - 1),
                    generation - 1,
                    FrameWorkClass::Interactive,
                    None,
                    true,
                ));
            }
        }

        assert!(completions
            .iter()
            .all(|result| result.completion == FrameRequestCompletion::Stale));
        assert_eq!(scheduler.pending_len(), 1);
        assert!(scheduler.has_pending_key(&99));
    }

    #[test]
    fn headless_and_window_adapters_share_identical_admission_semantics() {
        fn drive(adapter_key: &str) -> (FrameRequestAdmission<String>, FrameRequestCompletion) {
            let scheduler = FrameRequestScheduler::<String, ()>::with_max_pending(1);
            let generation = scheduler.begin_generation();
            let key = adapter_key.to_owned();
            let admission = scheduler.request(
                key.clone(),
                generation,
                FrameWorkPriority::Current,
                FrameWorkClass::Playback,
                None,
                None,
            );
            let completion = scheduler
                .resolve_completion(&key, generation, FrameWorkClass::Playback, None, true)
                .completion;
            (admission, completion)
        }

        let window = drive("window-frame");
        let headless = drive("headless-frame");
        assert!(matches!(window.0, FrameRequestAdmission::Scheduled { .. }));
        assert!(matches!(
            headless.0,
            FrameRequestAdmission::Scheduled { .. }
        ));
        assert_eq!(window.1, headless.1);
    }

    #[test]
    fn realtime_current_preempts_still_but_still_never_preempts_realtime() {
        let scheduler = FrameRequestScheduler::<i32, ()>::with_max_pending(1);
        let generation = scheduler.begin_generation();
        assert!(matches!(
            scheduler.request(
                1,
                generation,
                FrameWorkPriority::Current,
                FrameWorkClass::Still,
                None,
                None,
            ),
            FrameRequestAdmission::Scheduled { .. }
        ));
        assert_eq!(
            scheduler.request(
                2,
                generation,
                FrameWorkPriority::Current,
                FrameWorkClass::Interactive,
                None,
                None,
            ),
            FrameRequestAdmission::Scheduled { evicted_prefetch: None, evicted_still: Some(1) }
        );
        assert_eq!(
            scheduler.request(
                3,
                generation,
                FrameWorkPriority::Current,
                FrameWorkClass::Still,
                None,
                None,
            ),
            FrameRequestAdmission::DroppedBackpressure
        );
    }

    #[test]
    fn prefetch_is_playback_only_and_yields_to_current_work() {
        let scheduler = FrameRequestScheduler::<i32, ()>::with_max_pending(1);
        let generation = scheduler.begin_generation();
        assert_eq!(
            scheduler.request(
                1,
                generation,
                FrameWorkPriority::Prefetch,
                FrameWorkClass::Still,
                None,
                None,
            ),
            FrameRequestAdmission::DroppedInvalidClass
        );
        assert!(matches!(
            scheduler.request(
                2,
                generation,
                FrameWorkPriority::Prefetch,
                FrameWorkClass::Playback,
                None,
                None,
            ),
            FrameRequestAdmission::Scheduled { .. }
        ));
        assert_eq!(
            scheduler.request(
                3,
                generation,
                FrameWorkPriority::Current,
                FrameWorkClass::Playback,
                None,
                None,
            ),
            FrameRequestAdmission::Scheduled { evicted_prefetch: Some(2), evicted_still: None }
        );
    }

    #[test]
    fn reusable_same_key_completion_rebinds_to_latest_demand_atomically() {
        let scheduler = FrameRequestScheduler::<u64, u64>::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let old_identity = FrameDemandIdentity {
            epoch: crate::PlaybackEpoch(1),
            quality_revision: 0,
            sequence: crate::FrameDemandSequence(1),
            target_frame: 42,
        };
        let latest_identity = FrameDemandIdentity {
            epoch: crate::PlaybackEpoch(2),
            quality_revision: 0,
            sequence: crate::FrameDemandSequence(2),
            target_frame: 42,
        };
        scheduler.request(
            7,
            generation,
            FrameWorkPriority::Current,
            FrameWorkClass::Playback,
            Some(old_identity),
            Some(100),
        );
        let latest_generation = scheduler.begin_generation();
        scheduler.request(
            7,
            latest_generation,
            FrameWorkPriority::Current,
            FrameWorkClass::Playback,
            Some(latest_identity),
            Some(200),
        );

        let resolution = scheduler.resolve_completion(
            &7,
            generation,
            FrameWorkClass::Playback,
            Some(old_identity),
            true,
        );

        assert_eq!(resolution.completion, FrameRequestCompletion::Current);
        let binding = resolution.binding.expect("latest binding");
        assert_eq!(binding.generation, latest_generation);
        assert_eq!(binding.demand_identity, Some(latest_identity));
        assert_eq!(binding.deadline, Some(200));
        assert!(!scheduler.has_pending_key(&7));
    }

    #[test]
    fn canceled_old_execution_cannot_consume_newer_same_key_binding() {
        let scheduler = FrameRequestScheduler::<u64, u64>::with_max_pending(1);
        let generation = scheduler.begin_generation();
        let old_identity = FrameDemandIdentity {
            epoch: crate::PlaybackEpoch(1),
            quality_revision: 0,
            sequence: crate::FrameDemandSequence(1),
            target_frame: 42,
        };
        let latest_identity = FrameDemandIdentity {
            epoch: crate::PlaybackEpoch(2),
            quality_revision: 0,
            sequence: crate::FrameDemandSequence(2),
            target_frame: 42,
        };
        scheduler.request(
            7,
            generation,
            FrameWorkPriority::Current,
            FrameWorkClass::Playback,
            Some(old_identity),
            Some(100),
        );
        scheduler.request(
            7,
            generation,
            FrameWorkPriority::Current,
            FrameWorkClass::Playback,
            Some(latest_identity),
            Some(200),
        );

        let resolution = scheduler.resolve_completion(
            &7,
            generation,
            FrameWorkClass::Playback,
            Some(old_identity),
            false,
        );

        assert_eq!(resolution.completion, FrameRequestCompletion::Stale);
        assert!(resolution.binding.is_none());
        assert!(scheduler.has_pending_key(&7));
    }
}
