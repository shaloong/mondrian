//! Proxy demand identity, priority queues, and terminal publication.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mondrian_core::types::{AssetId, ProjectId};
use mondrian_core::{
    ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionTerminalDisposition,
    ExecutionTerminalEvidence,
};
use mondrian_media::{
    MediaFileFingerprint, ProxyCodec, ProxyColorContract, ProxyConfig, ProxyGenerationOutcome,
    ProxyPublicationFailureKind, ProxyResolution, ProxyStatus,
};
use parking_lot::{Condvar, Mutex};

use super::backend::ProxyGenerationBackend;
use super::{
    ProxyGenerationDiagnostics, ProxyGenerationFailure, ProxyGenerationFailureReason,
    ProxyGenerationOrigin, ProxyGenerationRequestOutcome, ProxyGenerationTerminalRecord,
    PROXY_FAILURE_CAPACITY, PROXY_FOREGROUND_BURST, PROXY_PENDING_CAPACITY,
    PROXY_TERMINAL_CAPACITY,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ProxyGenerationKey {
    pub(super) asset_id: AssetId,
    pub(super) source_path: PathBuf,
    pub(super) source_fingerprint: MediaFileFingerprint,
    resolution: ProxyResolution,
    codec: ProxyCodec,
    crf: u8,
    cache_dir: PathBuf,
    color: ProxyColorContract,
}

#[derive(Debug, Clone)]
pub(super) struct ProxyGenerationRequest {
    pub(super) key: ProxyGenerationKey,
    pub(super) config: ProxyConfig,
    pub(super) color: ProxyColorContract,
}

impl ProxyGenerationRequest {
    pub(super) fn new(
        asset_id: AssetId,
        source_path: PathBuf,
        source_fingerprint: MediaFileFingerprint,
        config: ProxyConfig,
        color: ProxyColorContract,
    ) -> Result<Self, ProxyGenerationFailure> {
        if !config.cache_dir.is_absolute() {
            return Err(ProxyGenerationFailure::new(
                ProxyGenerationFailureReason::InvalidProxyContract,
                format!(
                    "proxy admission requires an absolute frozen cache root: {}",
                    config.cache_dir.display()
                ),
            ));
        }
        let key = ProxyGenerationKey {
            asset_id,
            source_path,
            source_fingerprint,
            resolution: config.resolution,
            codec: config.codec,
            crf: config.crf.min(51),
            cache_dir: config.cache_dir.clone(),
            color,
        };
        Ok(Self { key, config, color })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyAttemptPhase {
    Queued,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunningResourceYieldScope {
    None,
    AutomaticOnly,
    All,
}

#[derive(Debug, Clone)]
struct PendingProxyAttempt {
    request: ProxyGenerationRequest,
    generation: u64,
    origin: ProxyGenerationOrigin,
    cancellation: ExecutionCancellationToken,
    phase: ProxyAttemptPhase,
    queue_revision: u64,
    resource_yield_requested: bool,
}

#[derive(Debug, Clone, Copy)]
struct QueueEntry {
    attempt_id: u64,
    revision: u64,
}

#[derive(Debug, Clone)]
struct RetainedProxyFailure {
    failure: ProxyGenerationFailure,
}

fn is_publication_quarantine(failure: &ProxyGenerationFailure) -> bool {
    failure.publication.as_ref().is_some_and(|publication| {
        matches!(
            publication.kind,
            ProxyPublicationFailureKind::DurabilityUnconfirmed
                | ProxyPublicationFailureKind::NamespaceIndeterminate
        )
    })
}

#[derive(Default)]
pub(super) struct ProxyGenerationCounters {
    pub(super) admissions: u64,
    pub(super) deduplications: u64,
    pub(super) promotions: u64,
    pub(super) fresh_hits: u64,
    pub(super) completions: u64,
    pub(super) failures: u64,
    pub(super) cancellations: u64,
    pub(super) resource_yields: u64,
    pub(super) superseded: u64,
    pub(super) rejections: u64,
}

pub(super) struct ProxyGenerationState {
    project_id: Option<ProjectId>,
    pub(super) generation: u64,
    next_attempt_id: u64,
    pending: HashMap<u64, PendingProxyAttempt>,
    active_by_key: HashMap<ProxyGenerationKey, u64>,
    user_queue: VecDeque<QueueEntry>,
    recovery_queue: VecDeque<QueueEntry>,
    import_queue: VecDeque<QueueEntry>,
    foreground_streak: usize,
    pub(super) dispatch_enabled: bool,
    pub(super) automatic_dispatch_enabled: bool,
    pub(super) dispatch_parallelism: usize,
    running: usize,
    running_by_cache_root: HashMap<PathBuf, usize>,
    failures: HashMap<ProxyGenerationKey, RetainedProxyFailure>,
    failure_lru: VecDeque<ProxyGenerationKey>,
    terminal_records: VecDeque<ProxyGenerationTerminalRecord>,
    latest_terminal_sequence: u64,
    pub(super) counters: ProxyGenerationCounters,
}

impl Default for ProxyGenerationState {
    fn default() -> Self {
        Self {
            project_id: None,
            generation: 1,
            next_attempt_id: 1,
            pending: HashMap::new(),
            active_by_key: HashMap::new(),
            user_queue: VecDeque::new(),
            recovery_queue: VecDeque::new(),
            import_queue: VecDeque::new(),
            foreground_streak: 0,
            dispatch_enabled: true,
            automatic_dispatch_enabled: true,
            dispatch_parallelism: usize::MAX,
            running: 0,
            running_by_cache_root: HashMap::new(),
            failures: HashMap::new(),
            failure_lru: VecDeque::new(),
            terminal_records: VecDeque::new(),
            latest_terminal_sequence: 0,
            counters: ProxyGenerationCounters::default(),
        }
    }
}

impl ProxyGenerationState {
    pub(super) fn remove_failure(&mut self, key: &ProxyGenerationKey) {
        self.failures.remove(key);
        self.failure_lru.retain(|candidate| candidate != key);
    }

    fn allocate_attempt_id(&mut self) -> u64 {
        let attempt_id = self.next_attempt_id;
        self.next_attempt_id = self.next_attempt_id.saturating_add(1).max(1);
        attempt_id
    }

    fn allocate_terminal_sequence(&mut self) -> u64 {
        self.latest_terminal_sequence = self.latest_terminal_sequence.saturating_add(1).max(1);
        self.latest_terminal_sequence
    }

    fn enqueue(&mut self, attempt_id: u64, origin: ProxyGenerationOrigin, revision: u64) {
        let entry = QueueEntry { attempt_id, revision };
        match origin {
            ProxyGenerationOrigin::User => self.user_queue.push_back(entry),
            ProxyGenerationOrigin::PlaybackRecovery => self.recovery_queue.push_back(entry),
            ProxyGenerationOrigin::Import => self.import_queue.push_back(entry),
        }
    }

    fn pop_next(&mut self) -> Option<WorkerProxyAttempt> {
        if !self.dispatch_enabled || self.running >= self.dispatch_parallelism {
            return None;
        }
        let entry = if !self.automatic_dispatch_enabled {
            self.pop_eligible(ProxyGenerationOrigin::User)
        } else {
            let background_due =
                !self.import_queue.is_empty() && self.foreground_streak >= PROXY_FOREGROUND_BURST;
            if background_due {
                self.pop_eligible(ProxyGenerationOrigin::Import)
                    .or_else(|| self.pop_eligible(ProxyGenerationOrigin::User))
                    .or_else(|| self.pop_eligible(ProxyGenerationOrigin::PlaybackRecovery))
            } else {
                self.pop_eligible(ProxyGenerationOrigin::User)
                    .or_else(|| self.pop_eligible(ProxyGenerationOrigin::PlaybackRecovery))
                    .or_else(|| self.pop_eligible(ProxyGenerationOrigin::Import))
            }
        }?;
        let pending = self.pending.get_mut(&entry.attempt_id)?;
        pending.phase = ProxyAttemptPhase::Running;
        let request = pending.request.clone();
        let generation = pending.generation;
        let cancellation = pending.cancellation.clone();
        let origin = pending.origin;
        self.running = self.running.saturating_add(1);
        let active =
            self.running_by_cache_root.entry(request.config.cache_dir.clone()).or_default();
        *active = active.saturating_add(1);
        if origin == ProxyGenerationOrigin::Import {
            self.foreground_streak = 0;
        } else {
            self.foreground_streak = self.foreground_streak.saturating_add(1);
        }
        Some(WorkerProxyAttempt {
            attempt_id: entry.attempt_id,
            request,
            generation,
            cancellation,
        })
    }

    fn pop_eligible(&mut self, origin: ProxyGenerationOrigin) -> Option<QueueEntry> {
        let queue = match origin {
            ProxyGenerationOrigin::User => &mut self.user_queue,
            ProxyGenerationOrigin::PlaybackRecovery => &mut self.recovery_queue,
            ProxyGenerationOrigin::Import => &mut self.import_queue,
        };
        let candidates = queue.len();
        for _ in 0..candidates {
            let entry = queue.pop_front()?;
            let Some(pending) = self.pending.get(&entry.attempt_id) else {
                continue;
            };
            if pending.phase != ProxyAttemptPhase::Queued
                || pending.queue_revision != entry.revision
                || pending.origin != origin
            {
                continue;
            }
            let active = self
                .running_by_cache_root
                .get(&pending.request.config.cache_dir)
                .copied()
                .unwrap_or(0);
            let limit = usize::from(pending.request.config.concurrent_jobs.max(1));
            if active < limit {
                return Some(entry);
            }
            queue.push_back(entry);
        }
        None
    }
}

pub(super) struct ProxyGenerationInner {
    pub(super) state: Mutex<ProxyGenerationState>,
    pub(super) available: Condvar,
    pub(super) backend: Arc<dyn ProxyGenerationBackend>,
    pub(super) diagnostics_revision: AtomicU64,
    pub(super) model_revision: AtomicU64,
    pub(super) shutdown: AtomicBool,
}

impl ProxyGenerationInner {
    pub(super) fn mark_diagnostics_changed(&self) {
        self.diagnostics_revision.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn mark_model_changed(&self) {
        self.diagnostics_revision.fetch_add(1, Ordering::AcqRel);
        self.model_revision.fetch_add(1, Ordering::AcqRel);
    }
}

struct WorkerProxyAttempt {
    attempt_id: u64,
    request: ProxyGenerationRequest,
    generation: u64,
    cancellation: ExecutionCancellationToken,
}

pub(super) fn request_admission(
    state: &mut ProxyGenerationState,
    request: ProxyGenerationRequest,
    origin: ProxyGenerationOrigin,
    prior_status: ProxyStatus,
) -> ProxyGenerationRequestOutcome {
    if let Some(outcome) = preflight_request(state, &request.key, origin) {
        return outcome;
    }

    if state.pending.len() >= PROXY_PENDING_CAPACITY {
        let failure = ProxyGenerationFailure::new(
            ProxyGenerationFailureReason::AdmissionRejected,
            "proxy generation demand capacity is exhausted",
        );
        let attempt_id = state.allocate_attempt_id();
        state.counters.rejections = state.counters.rejections.saturating_add(1);
        push_terminal(
            state,
            attempt_id,
            &request.key,
            state.generation,
            origin,
            ExecutionTerminalDisposition::Rejected,
            Duration::ZERO,
            false,
            Some(&failure),
            None,
        );
        return ProxyGenerationRequestOutcome::Failed(failure);
    }

    let attempt_id = state.allocate_attempt_id();
    let generation = state.generation;
    let pending = PendingProxyAttempt {
        request: request.clone(),
        generation,
        origin,
        cancellation: ExecutionCancellationToken::new(),
        phase: ProxyAttemptPhase::Queued,
        queue_revision: 1,
        resource_yield_requested: false,
    };
    state.pending.insert(attempt_id, pending);
    state.active_by_key.insert(request.key, attempt_id);
    state.enqueue(attempt_id, origin, 1);
    state.counters.admissions = state.counters.admissions.saturating_add(1);
    ProxyGenerationRequestOutcome::Admitted { prior_status }
}

pub(super) fn request_running_resource_yield(
    state: &mut ProxyGenerationState,
    scope: RunningResourceYieldScope,
) -> bool {
    if scope == RunningResourceYieldScope::None {
        return false;
    }
    let mut changed = false;
    for pending in state.pending.values_mut() {
        let origin_is_eligible = match scope {
            RunningResourceYieldScope::None => false,
            RunningResourceYieldScope::AutomaticOnly => {
                pending.origin != ProxyGenerationOrigin::User
            }
            RunningResourceYieldScope::All => true,
        };
        if pending.phase == ProxyAttemptPhase::Running
            && origin_is_eligible
            && !pending.resource_yield_requested
        {
            pending.resource_yield_requested = true;
            pending.cancellation.cancel();
            changed = true;
        }
    }
    changed
}

pub(super) fn preflight_request(
    state: &mut ProxyGenerationState,
    key: &ProxyGenerationKey,
    origin: ProxyGenerationOrigin,
) -> Option<ProxyGenerationRequestOutcome> {
    if let Some(attempt_id) = state.active_by_key.get(key).copied() {
        state.counters.deduplications = state.counters.deduplications.saturating_add(1);
        let mut promoted = false;
        let mut queued_promotion = None;
        if let Some(pending) = state.pending.get_mut(&attempt_id)
            && origin.rank() < pending.origin.rank()
        {
            pending.origin = origin;
            if pending.phase == ProxyAttemptPhase::Queued {
                pending.queue_revision = pending.queue_revision.saturating_add(1).max(1);
                queued_promotion = Some((pending.origin, pending.queue_revision));
            }
            promoted = true;
        }
        if let Some((promoted_origin, revision)) = queued_promotion {
            state.enqueue(attempt_id, promoted_origin, revision);
        }
        if promoted {
            state.counters.promotions = state.counters.promotions.saturating_add(1);
        }
        return Some(ProxyGenerationRequestOutcome::Deduplicated { promoted });
    }

    if let Some(retained) = state.failures.get(key) {
        if retained
            .failure
            .publication
            .as_ref()
            .is_some_and(|failure| failure.kind == ProxyPublicationFailureKind::BeforeNamespace)
        {
            state.remove_failure(key);
            return None;
        }
        if is_publication_quarantine(&retained.failure) {
            return Some(ProxyGenerationRequestOutcome::RetainedFailure(
                retained.failure.clone(),
            ));
        }
        if origin != ProxyGenerationOrigin::User {
            return Some(ProxyGenerationRequestOutcome::RetainedFailure(
                retained.failure.clone(),
            ));
        }
        state.remove_failure(key);
    }
    None
}

pub(super) fn bind_project_generation(
    state: &mut ProxyGenerationState,
    project_id: Option<ProjectId>,
) -> bool {
    if state.project_id.is_none() && project_id.is_none() {
        return false;
    }
    state.project_id = project_id;
    state.generation = state.generation.saturating_add(1).max(1);
    state.active_by_key.clear();
    state.user_queue.clear();
    state.recovery_queue.clear();
    state.import_queue.clear();
    state
        .failures
        .retain(|_, retained| is_publication_quarantine(&retained.failure));
    let quarantined_keys = state.failures.keys().cloned().collect::<Vec<_>>();
    state.failure_lru.retain(|key| quarantined_keys.contains(key));

    let queued: Vec<_> = state
        .pending
        .iter()
        .filter(|(_, pending)| pending.phase == ProxyAttemptPhase::Queued)
        .map(|(attempt_id, pending)| (*attempt_id, pending.clone()))
        .collect();
    for (attempt_id, pending) in queued {
        pending.cancellation.cancel();
        state.pending.remove(&attempt_id);
        state.counters.cancellations = state.counters.cancellations.saturating_add(1);
        push_terminal(
            state,
            attempt_id,
            &pending.request.key,
            pending.generation,
            pending.origin,
            ExecutionTerminalDisposition::Canceled,
            Duration::ZERO,
            false,
            None,
            None,
        );
    }
    for pending in state.pending.values() {
        pending.cancellation.cancel();
    }
    true
}

pub(super) fn cancel_for_shutdown(state: &mut ProxyGenerationState) {
    for pending in state.pending.values() {
        pending.cancellation.cancel();
    }
}

#[cfg(any(test, feature = "validation"))]
pub(super) fn clear_after_workers_terminated(state: &mut ProxyGenerationState) {
    state.pending.clear();
    state.active_by_key.clear();
    state.user_queue.clear();
    state.recovery_queue.clear();
    state.import_queue.clear();
    state.running = 0;
    state.running_by_cache_root.clear();
}

pub(super) fn record_immediate_failure(
    state: &mut ProxyGenerationState,
    key: Option<ProxyGenerationKey>,
    asset_id: AssetId,
    fingerprint: MediaFileFingerprint,
    origin: ProxyGenerationOrigin,
    failure: ProxyGenerationFailure,
) {
    let attempt_id = state.allocate_attempt_id();
    state.counters.failures = state.counters.failures.saturating_add(1);
    if let Some(key) = key {
        retain_failure(state, key, failure.clone());
    }
    let terminal_sequence = state.allocate_terminal_sequence();
    state.terminal_records.push_back(ProxyGenerationTerminalRecord {
        terminal_sequence,
        evidence: ExecutionTerminalEvidence {
            generation: state.generation,
            priority: origin.priority(),
            disposition: ExecutionTerminalDisposition::Failed,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        },
        attempt_id,
        asset_id,
        source_fingerprint: fingerprint,
        origin,
        elapsed: Duration::ZERO,
        executed: false,
        failure: Some(failure.reason),
        failure_detail: Some(failure.detail),
        publication_failure: failure.publication.map(|publication| *publication),
        publication: None,
    });
    trim_terminals(state);
}

pub(super) fn proxy_generation_worker(
    inner: Arc<ProxyGenerationInner>,
    runtime: tokio::runtime::Runtime,
) {
    loop {
        let attempt = {
            let mut state = inner.state.lock();
            loop {
                if inner.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Some(attempt) = state.pop_next() {
                    break attempt;
                }
                inner.available.wait(&mut state);
            }
        };
        inner.mark_model_changed();
        let started = Instant::now();
        let result =
            inner.backend.execute(&runtime, &attempt.request, attempt.cancellation.clone());
        publish_worker_result(&inner, attempt, result, started.elapsed());
    }
}

fn publish_worker_result(
    inner: &ProxyGenerationInner,
    attempt: WorkerProxyAttempt,
    result: Result<ProxyGenerationOutcome, ProxyGenerationFailure>,
    elapsed: Duration,
) {
    let mut state = inner.state.lock();
    let Some(mut pending) = state.pending.remove(&attempt.attempt_id) else {
        return;
    };
    state.running = state.running.saturating_sub(1);
    let cache_root = pending.request.config.cache_dir.clone();
    if let Some(active) = state.running_by_cache_root.get_mut(&cache_root) {
        *active = active.saturating_sub(1);
        if *active == 0 {
            state.running_by_cache_root.remove(&cache_root);
        }
    }
    let current = state.generation == attempt.generation
        && state.active_by_key.get(&pending.request.key) == Some(&attempt.attempt_id);
    let resource_yield_completed = current
        && pending.resource_yield_requested
        && matches!(&result, Ok(ProxyGenerationOutcome::Canceled))
        && !inner.shutdown.load(Ordering::Acquire);
    if resource_yield_completed {
        pending.phase = ProxyAttemptPhase::Queued;
        pending.cancellation = ExecutionCancellationToken::new();
        pending.resource_yield_requested = false;
        pending.queue_revision = pending.queue_revision.saturating_add(1).max(1);
        let origin = pending.origin;
        let revision = pending.queue_revision;
        state.pending.insert(attempt.attempt_id, pending);
        state.enqueue(attempt.attempt_id, origin, revision);
        state.counters.resource_yields = state.counters.resource_yields.saturating_add(1);
        drop(state);
        inner.mark_diagnostics_changed();
        inner.available.notify_all();
        return;
    }
    if current {
        state.active_by_key.remove(&pending.request.key);
    }

    let publication = match &result {
        Ok(ProxyGenerationOutcome::Completed(evidence)) => Some(evidence.clone()),
        _ => None,
    };
    let (disposition, terminal_failure) = match result {
        Ok(ProxyGenerationOutcome::Canceled) => {
            state.counters.cancellations = state.counters.cancellations.saturating_add(1);
            (ExecutionTerminalDisposition::Canceled, None)
        }
        Ok(ProxyGenerationOutcome::Completed(_) | ProxyGenerationOutcome::Reused(_)) if current => {
            state.counters.completions = state.counters.completions.saturating_add(1);
            state.remove_failure(&pending.request.key);
            (ExecutionTerminalDisposition::Completed, None)
        }
        Ok(ProxyGenerationOutcome::Completed(_) | ProxyGenerationOutcome::Reused(_)) => {
            state.counters.superseded = state.counters.superseded.saturating_add(1);
            (ExecutionTerminalDisposition::Superseded, None)
        }
        Err(failure) if current => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            retain_failure(&mut state, pending.request.key.clone(), failure.clone());
            (ExecutionTerminalDisposition::Failed, Some(failure))
        }
        Err(_) => {
            state.counters.superseded = state.counters.superseded.saturating_add(1);
            (ExecutionTerminalDisposition::Superseded, None)
        }
    };
    push_terminal(
        &mut state,
        attempt.attempt_id,
        &pending.request.key,
        pending.generation,
        pending.origin,
        disposition,
        elapsed,
        true,
        terminal_failure.as_ref(),
        publication,
    );
    drop(state);
    inner.mark_model_changed();
    inner.available.notify_all();
}

fn retain_failure(
    state: &mut ProxyGenerationState,
    key: ProxyGenerationKey,
    failure: ProxyGenerationFailure,
) {
    tracing::warn!(
        target: "mondrian::proxy",
        asset_id = %key.asset_id,
        path = %key.source_path.display(),
        reason = failure.reason.code(),
        detail = %failure.detail,
        "proxy generation attempt failed"
    );
    state.failures.insert(key.clone(), RetainedProxyFailure { failure });
    state.failure_lru.retain(|candidate| candidate != &key);
    state.failure_lru.push_front(key);
    while state.failure_lru.len() > PROXY_FAILURE_CAPACITY {
        if let Some(evicted) = state.failure_lru.pop_back() {
            state.failures.remove(&evicted);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_terminal(
    state: &mut ProxyGenerationState,
    attempt_id: u64,
    key: &ProxyGenerationKey,
    generation: u64,
    origin: ProxyGenerationOrigin,
    disposition: ExecutionTerminalDisposition,
    elapsed: Duration,
    executed: bool,
    failure: Option<&ProxyGenerationFailure>,
    publication: Option<mondrian_media::ProxyPublicationEvidence>,
) {
    let terminal_sequence = state.allocate_terminal_sequence();
    state.terminal_records.push_back(ProxyGenerationTerminalRecord {
        terminal_sequence,
        evidence: ExecutionTerminalEvidence {
            generation,
            priority: origin.priority(),
            disposition,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        },
        attempt_id,
        asset_id: key.asset_id,
        source_fingerprint: key.source_fingerprint,
        origin,
        elapsed,
        executed,
        failure: failure.map(|failure| failure.reason),
        failure_detail: failure.map(|failure| failure.detail.clone()),
        publication_failure: failure.and_then(|failure| failure.publication.as_deref()).cloned(),
        publication,
    });
    trim_terminals(state);
}

fn trim_terminals(state: &mut ProxyGenerationState) {
    while state.terminal_records.len() > PROXY_TERMINAL_CAPACITY {
        state.terminal_records.pop_front();
    }
}

pub(super) fn diagnostics_snapshot(state: &ProxyGenerationState) -> ProxyGenerationDiagnostics {
    ProxyGenerationDiagnostics {
        revision: 0,
        generation: state.generation,
        dispatch_enabled: state.dispatch_enabled,
        automatic_dispatch_enabled: state.automatic_dispatch_enabled,
        dispatch_parallelism: state.dispatch_parallelism,
        worker_startup_attempted: false,
        requested_workers: 0,
        started_workers: 0,
        worker_unexpectedly_exited: false,
        queued: state
            .pending
            .values()
            .filter(|pending| pending.phase == ProxyAttemptPhase::Queued)
            .count(),
        queued_user: state
            .pending
            .values()
            .filter(|pending| {
                pending.phase == ProxyAttemptPhase::Queued
                    && pending.origin == ProxyGenerationOrigin::User
            })
            .count(),
        running: state.running,
        yielding: state
            .pending
            .values()
            .filter(|pending| {
                pending.phase == ProxyAttemptPhase::Running && pending.resource_yield_requested
            })
            .count(),
        running_user: state
            .pending
            .values()
            .filter(|pending| {
                pending.phase == ProxyAttemptPhase::Running
                    && pending.origin == ProxyGenerationOrigin::User
            })
            .count(),
        retained_failures: state.failures.len(),
        admissions: state.counters.admissions,
        deduplications: state.counters.deduplications,
        promotions: state.counters.promotions,
        fresh_hits: state.counters.fresh_hits,
        completions: state.counters.completions,
        failures: state.counters.failures,
        cancellations: state.counters.cancellations,
        resource_yields: state.counters.resource_yields,
        superseded: state.counters.superseded,
        rejections: state.counters.rejections,
        oldest_terminal_sequence: state
            .terminal_records
            .front()
            .map(|record| record.terminal_sequence),
        latest_terminal_sequence: state.latest_terminal_sequence,
        terminal_records: state.terminal_records.iter().cloned().collect(),
    }
}

pub(super) fn terminal_delta_snapshot(
    state: &ProxyGenerationState,
    cursor: u64,
) -> super::ProxyGenerationTerminalDelta {
    let oldest_terminal_sequence =
        state.terminal_records.front().map(|record| record.terminal_sequence);
    let retention_gap = cursor < state.latest_terminal_sequence
        && oldest_terminal_sequence.is_some_and(|oldest| oldest > cursor.saturating_add(1));
    super::ProxyGenerationTerminalDelta {
        generation: state.generation,
        next_cursor: state.latest_terminal_sequence.max(cursor),
        retention_gap,
        records: state
            .terminal_records
            .iter()
            .filter(|record| record.terminal_sequence > cursor)
            .cloned()
            .collect(),
    }
}
