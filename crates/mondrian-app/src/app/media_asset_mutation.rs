//! Bounded two-phase execution for existing file-Asset mutations.
//!
//! Relink, audio Component refresh, and audio Component rebind all require a
//! physical media probe. The probe Adapter runs on this Module's single
//! ordered worker. Only the app event-loop poll owns commit authority, after
//! rechecking Project generation and cancellation.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_assets::{AssetLibrary, AssetMediaProbeCandidate};
use mondrian_core::events::AppEvent;
use mondrian_core::types::{AssetId, AudioSourceComponentId, ProjectId};
#[cfg(test)]
use mondrian_core::MediaFileFingerprint;
use mondrian_core::{
    ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence, MondrianError, Result,
};
use parking_lot::{Condvar, Mutex};

#[cfg(any(test, feature = "validation"))]
use super::endurance_shutdown::{join_workers_until, EnduranceWorkerShutdownEvidence};
use super::AppState;

const MEDIA_ASSET_MUTATION_CAPACITY: usize = 64;
const MEDIA_ASSET_MUTATION_RESULT_CAPACITY: usize = MEDIA_ASSET_MUTATION_CAPACITY + 1;
const MEDIA_ASSET_MUTATION_MAX_RESULTS_PER_POLL: usize = 16;
const MEDIA_ASSET_MUTATION_TERMINAL_CAPACITY: usize = 128;
const MEDIA_ASSET_MUTATION_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

/// Existing file-Asset author intent requiring a fresh physical media probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAssetMutationKind {
    /// Replace one file Asset's source and probe facts.
    Relink,
    /// Refresh probe facts and reconcile newly discovered audio streams.
    RefreshAudioComponents,
    /// Refresh probe facts and explicitly retarget one logical Component.
    RebindAudioComponent {
        /// Stable logical Component being retargeted.
        component_id: AudioSourceComponentId,
        /// Absolute probed stream index selected by the author.
        stream_index: u32,
    },
}

impl MediaAssetMutationKind {
    fn step_id(self) -> &'static str {
        match self {
            Self::Relink => "relink_asset",
            Self::RefreshAudioComponents => "assets_refresh_audio_components",
            Self::RebindAudioComponent { .. } => "assets_rebind_audio_component",
        }
    }
}

/// One immutable terminal attempt published by the mutation Module.
#[derive(Debug, Clone)]
pub struct MediaAssetMutationTerminalRecord {
    /// Instance-local operation identity.
    pub operation_id: u64,
    /// Asset targeted by this attempt.
    pub asset_id: AssetId,
    /// Semantic author intent.
    pub kind: MediaAssetMutationKind,
    /// Shared generation and terminal disposition evidence.
    pub evidence: ExecutionTerminalEvidence,
    /// Worker preparation duration.
    pub elapsed: Duration,
    /// Structured-domain error rendered for diagnostics, when present.
    pub detail: Option<String>,
}

/// Bounded operational snapshot for Headless gates and product diagnostics.
#[derive(Debug, Clone, Default)]
pub struct MediaAssetMutationDiagnostics {
    /// Product-observable operation-state revision.
    ///
    /// Admission, execution-phase, publication, terminal, and Project-generation
    /// changes advance this value. Resource-policy-only changes do not.
    pub operation_revision: u64,
    /// Effective resource-policy revision.
    ///
    /// This advances only when the effective dispatch policy changes and is
    /// diagnostic evidence, not a product-model invalidation signal.
    pub policy_revision: u64,
    /// Current Project execution generation.
    pub generation: u64,
    /// Whether a queued request may enter the probe worker.
    pub dispatch_enabled: bool,
    /// Requests waiting for the ordered worker.
    pub queued: usize,
    /// Requests currently preparing; this is at most one.
    pub running: usize,
    /// Admitted requests without a drained terminal result.
    pub outstanding: usize,
    /// Physical queue, running worker, and unpublished-result occupancy.
    pub transport_occupied: usize,
    /// Whether the configured worker thread was created.
    pub worker_available: bool,
    /// Whether the worker exited before shutdown was requested.
    pub worker_unexpectedly_exited: bool,
    /// Successfully admitted operations.
    pub admissions: u64,
    /// Admission failures.
    pub rejections: u64,
    /// Successfully completed mutations.
    pub completions: u64,
    /// Failed preparation or publication operations.
    pub failures: u64,
    /// Cooperatively canceled operations.
    pub cancellations: u64,
    /// Operations retired by a newer Project generation.
    pub superseded: u64,
    /// Bounded terminal evidence in publication order.
    pub terminals: Vec<MediaAssetMutationTerminalRecord>,
}

#[derive(Debug, Clone)]
struct MediaAssetMutationRequest {
    operation_id: u64,
    generation: u64,
    asset_id: AssetId,
    asset_name: String,
    source_path: PathBuf,
    kind: MediaAssetMutationKind,
    cancellation: ExecutionCancellationToken,
}

#[derive(Debug)]
struct PreparedMediaAssetMutation {
    request: MediaAssetMutationRequest,
    candidate: AssetMediaProbeCandidate,
}

#[derive(Debug)]
enum MediaAssetMutationWorkerOutcome {
    Prepared(Box<PreparedMediaAssetMutation>),
    Failed {
        request: MediaAssetMutationRequest,
        reason: String,
    },
    Canceled(MediaAssetMutationRequest),
}

impl MediaAssetMutationWorkerOutcome {
    fn request(&self) -> &MediaAssetMutationRequest {
        match self {
            Self::Prepared(prepared) => &prepared.request,
            Self::Failed { request, .. } | Self::Canceled(request) => request,
        }
    }
}

#[derive(Debug)]
struct MediaAssetMutationWorkerResult {
    outcome: MediaAssetMutationWorkerOutcome,
    elapsed: Duration,
}

#[derive(Debug)]
struct MediaAssetMutationPublication {
    request: MediaAssetMutationRequest,
    evidence: ExecutionTerminalEvidence,
    elapsed: Duration,
    result: std::result::Result<(), String>,
}

struct ActiveMediaAssetMutation {
    generation: u64,
    asset_id: AssetId,
    kind: MediaAssetMutationKind,
    cancellation: ExecutionCancellationToken,
}

struct MediaAssetMutationState {
    operation_revision: u64,
    policy_revision: u64,
    project_id: Option<ProjectId>,
    generation: u64,
    next_operation_id: u64,
    dispatch_enabled: bool,
    queue: VecDeque<MediaAssetMutationRequest>,
    active: HashMap<u64, ActiveMediaAssetMutation>,
    transport_occupied: usize,
    running_operation_id: Option<u64>,
    retired_worker_operations: VecDeque<u64>,
    terminals: VecDeque<MediaAssetMutationTerminalRecord>,
    counters: MediaAssetMutationCounters,
}

#[derive(Default)]
struct MediaAssetMutationCounters {
    admissions: u64,
    rejections: u64,
    completions: u64,
    failures: u64,
    cancellations: u64,
    superseded: u64,
}

impl Default for MediaAssetMutationState {
    fn default() -> Self {
        Self {
            operation_revision: 0,
            policy_revision: 0,
            project_id: None,
            generation: 1,
            next_operation_id: 1,
            dispatch_enabled: true,
            queue: VecDeque::new(),
            active: HashMap::new(),
            transport_occupied: 0,
            running_operation_id: None,
            retired_worker_operations: VecDeque::new(),
            terminals: VecDeque::new(),
            counters: MediaAssetMutationCounters::default(),
        }
    }
}

struct MediaAssetMutationInner {
    state: Mutex<MediaAssetMutationState>,
    available: Condvar,
    shutdown: AtomicBool,
    probe_helper_executable: Option<PathBuf>,
    result_tx: mpsc::SyncSender<MediaAssetMutationWorkerResult>,
}

pub(crate) struct MediaAssetMutationExecution {
    inner: Arc<MediaAssetMutationInner>,
    results: Mutex<mpsc::Receiver<MediaAssetMutationWorkerResult>>,
    worker: Option<JoinHandle<()>>,
    observed_operation_revision: AtomicU64,
}

impl MediaAssetMutationExecution {
    pub(crate) fn new() -> Self {
        let (result_tx, result_rx) = mpsc::sync_channel(MEDIA_ASSET_MUTATION_RESULT_CAPACITY);
        let inner = Arc::new(MediaAssetMutationInner {
            state: Mutex::new(MediaAssetMutationState::default()),
            available: Condvar::new(),
            shutdown: AtomicBool::new(false),
            probe_helper_executable: super::packaged_worker::discover_media_probe_worker(),
            result_tx,
        });
        let worker_inner = Arc::clone(&inner);
        let worker = std::thread::Builder::new()
            .name("mondrian-media-asset-mutation".to_owned())
            .spawn(move || media_asset_mutation_worker(worker_inner))
            .map_err(|error| {
                tracing::error!(%error, "failed to start media Asset mutation worker");
                error
            })
            .ok();
        Self {
            inner,
            results: Mutex::new(result_rx),
            worker,
            observed_operation_revision: AtomicU64::new(0),
        }
    }

    pub(crate) fn bind_project(&self, project_id: Option<ProjectId>) {
        let mut state = self.inner.state.lock();
        if state.project_id.is_none() && project_id.is_none() {
            return;
        }
        state.project_id = project_id;
        state.generation = next_nonzero_counter(state.generation);
        let retired = state
            .active
            .iter()
            .map(|(operation_id, active)| {
                (
                    *operation_id,
                    active.generation,
                    active.asset_id,
                    active.kind,
                    active.cancellation.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (operation_id, generation, asset_id, kind, cancellation) in retired {
            cancellation.cancel();
            push_terminal(
                &mut state,
                MediaAssetMutationTerminalRecord {
                    operation_id,
                    asset_id,
                    kind,
                    evidence: ExecutionTerminalEvidence {
                        generation,
                        priority: ExecutionPriority::UserInitiated,
                        disposition: ExecutionTerminalDisposition::Superseded,
                        deadline: ExecutionDeadlineStatus::NotApplicable,
                    },
                    elapsed: Duration::ZERO,
                    detail: Some("Project generation changed before commit".to_owned()),
                },
            );
        }
        if let Some(operation_id) = state.running_operation_id {
            if !state.retired_worker_operations.contains(&operation_id) {
                state.retired_worker_operations.push_back(operation_id);
            }
            while state.retired_worker_operations.len() > MEDIA_ASSET_MUTATION_CAPACITY {
                state.retired_worker_operations.pop_front();
            }
        }
        state.transport_occupied = state.transport_occupied.saturating_sub(state.queue.len());
        state.queue.clear();
        state.active.clear();
        mark_operation_changed(&mut state);
        drop(state);
        let results = self.results.lock();
        let mut drained_operation_ids = Vec::new();
        while let Ok(result) = results.try_recv() {
            drained_operation_ids.push(result.outcome.request().operation_id);
        }
        drop(results);
        if !drained_operation_ids.is_empty() {
            let mut state = self.inner.state.lock();
            state
                .retired_worker_operations
                .retain(|operation_id| !drained_operation_ids.contains(operation_id));
        }
        self.inner.available.notify_all();
    }

    pub(crate) fn set_resource_policy(&self, dispatch_enabled: bool) {
        let mut state = self.inner.state.lock();
        if state.dispatch_enabled == dispatch_enabled {
            return;
        }
        state.dispatch_enabled = dispatch_enabled;
        mark_policy_changed(&mut state);
        drop(state);
        self.inner.available.notify_all();
    }

    fn admit(
        &self,
        asset_id: AssetId,
        asset_name: String,
        source_path: PathBuf,
        kind: MediaAssetMutationKind,
    ) -> Result<u64> {
        let mut state = self.inner.state.lock();
        if state.project_id.is_none() {
            return Err(reject_admission(&mut state, kind, "素材库未连接"));
        }
        if self.worker.is_none() {
            return Err(reject_admission(
                &mut state,
                kind,
                "媒体素材任务 worker 不可用",
            ));
        }
        if state.active.values().any(|active| active.asset_id == asset_id) {
            return Err(reject_admission(
                &mut state,
                kind,
                format!("素材 {asset_id} 已有一个尚未提交的媒体任务"),
            ));
        }
        if state.active.len() >= MEDIA_ASSET_MUTATION_CAPACITY {
            return Err(reject_admission(
                &mut state,
                kind,
                format!("媒体素材任务队列已满（上限 {MEDIA_ASSET_MUTATION_CAPACITY}）"),
            ));
        }

        let operation_id = allocate_operation_id(&mut state);
        let generation = state.generation;
        let cancellation = ExecutionCancellationToken::new();
        let request = MediaAssetMutationRequest {
            operation_id,
            generation,
            asset_id,
            asset_name,
            source_path,
            kind,
            cancellation: cancellation.clone(),
        };
        state.active.insert(
            operation_id,
            ActiveMediaAssetMutation { generation, asset_id, kind, cancellation },
        );
        state.queue.push_back(request);
        state.transport_occupied = state.transport_occupied.saturating_add(1);
        state.counters.admissions = state.counters.admissions.saturating_add(1);
        mark_operation_changed(&mut state);
        drop(state);
        self.inner.available.notify_one();
        Ok(operation_id)
    }

    fn poll_results(
        &self,
        library: Option<&AssetLibrary>,
        max_results: usize,
    ) -> Vec<MediaAssetMutationPublication> {
        let results = self.results.lock();
        let mut publications = Vec::new();
        for _ in 0..max_results {
            let result = match results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            let request = result.outcome.request().clone();
            let mut state = self.inner.state.lock();
            state.transport_occupied = state.transport_occupied.saturating_sub(1);
            let current = state.active.get(&request.operation_id).is_some_and(|active| {
                active.generation == request.generation
                    && request.generation == state.generation
                    && !active.cancellation.is_canceled()
            });
            if !current
                && let Some(position) = state
                    .retired_worker_operations
                    .iter()
                    .position(|operation_id| *operation_id == request.operation_id)
            {
                state.retired_worker_operations.remove(position);
                continue;
            }
            if current {
                state.active.remove(&request.operation_id);
            }

            let (disposition, publication_result, detail) = if !current {
                (
                    ExecutionTerminalDisposition::Superseded,
                    Err("Project generation 已变化，结果未提交".to_owned()),
                    None,
                )
            } else {
                match result.outcome {
                    MediaAssetMutationWorkerOutcome::Prepared(prepared) => match library {
                        Some(library) => match commit_prepared_mutation(library, *prepared) {
                            Ok(()) => (ExecutionTerminalDisposition::Completed, Ok(()), None),
                            Err(error) => {
                                let reason = error.to_string();
                                (
                                    ExecutionTerminalDisposition::Failed,
                                    Err(reason.clone()),
                                    Some(reason),
                                )
                            }
                        },
                        None => {
                            let reason = "素材库在提交前已断开".to_owned();
                            (
                                ExecutionTerminalDisposition::Failed,
                                Err(reason.clone()),
                                Some(reason),
                            )
                        }
                    },
                    MediaAssetMutationWorkerOutcome::Failed { reason, .. } => (
                        ExecutionTerminalDisposition::Failed,
                        Err(reason.clone()),
                        Some(reason),
                    ),
                    MediaAssetMutationWorkerOutcome::Canceled(_) => (
                        ExecutionTerminalDisposition::Canceled,
                        Err("媒体素材任务已取消".to_owned()),
                        None,
                    ),
                }
            };
            let evidence = ExecutionTerminalEvidence {
                generation: request.generation,
                priority: ExecutionPriority::UserInitiated,
                disposition,
                deadline: ExecutionDeadlineStatus::NotApplicable,
            };
            push_terminal(
                &mut state,
                MediaAssetMutationTerminalRecord {
                    operation_id: request.operation_id,
                    asset_id: request.asset_id,
                    kind: request.kind,
                    evidence,
                    elapsed: result.elapsed,
                    detail,
                },
            );
            mark_operation_changed(&mut state);
            publications.push(MediaAssetMutationPublication {
                request,
                evidence,
                elapsed: result.elapsed,
                result: publication_result,
            });
        }
        drop(results);
        publications
    }

    pub(crate) fn poll_changed(&self) -> bool {
        let revision = self.inner.state.lock().operation_revision;
        self.observed_operation_revision.swap(revision, Ordering::AcqRel) != revision
    }

    pub(crate) fn diagnostics(&self) -> MediaAssetMutationDiagnostics {
        let state = self.inner.state.lock();
        MediaAssetMutationDiagnostics {
            operation_revision: state.operation_revision,
            policy_revision: state.policy_revision,
            generation: state.generation,
            dispatch_enabled: state.dispatch_enabled,
            queued: state.queue.len(),
            running: usize::from(state.running_operation_id.is_some()),
            outstanding: state.active.len(),
            transport_occupied: state.transport_occupied,
            worker_available: self.worker.is_some(),
            worker_unexpectedly_exited: self.worker.as_ref().is_some_and(JoinHandle::is_finished)
                && !self.inner.shutdown.load(Ordering::Acquire),
            admissions: state.counters.admissions,
            rejections: state.counters.rejections,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            superseded: state.counters.superseded,
            terminals: state.terminals.iter().cloned().collect(),
        }
    }

    pub(crate) fn begin_endurance_shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        let mut state = self.inner.state.lock();
        for active in state.active.values() {
            active.cancellation.cancel();
        }
        state.transport_occupied = state.transport_occupied.saturating_sub(state.queue.len());
        state.queue.clear();
        drop(state);
        self.inner.available.notify_all();
    }

    #[cfg(any(test, feature = "validation"))]
    pub(crate) fn finish_endurance_shutdown(
        &mut self,
        deadline: Instant,
    ) -> EnduranceWorkerShutdownEvidence {
        self.begin_endurance_shutdown();
        let mut workers = self.worker.take().into_iter().collect::<Vec<_>>();
        let join = join_workers_until(&mut workers, deadline);
        let mut state = self.inner.state.lock();
        if join.all_workers_returned_normally() {
            for result in self.results.get_mut().try_iter() {
                let request = result.outcome.request().clone();
                let (disposition, detail) = match result.outcome {
                    MediaAssetMutationWorkerOutcome::Prepared(_) => (
                        ExecutionTerminalDisposition::Canceled,
                        Some("App shutdown consumed an unpublished prepared mutation".to_owned()),
                    ),
                    MediaAssetMutationWorkerOutcome::Failed { reason, .. } => {
                        (ExecutionTerminalDisposition::Failed, Some(reason))
                    }
                    MediaAssetMutationWorkerOutcome::Canceled(_) => {
                        (ExecutionTerminalDisposition::Canceled, None)
                    }
                };
                push_terminal(
                    &mut state,
                    MediaAssetMutationTerminalRecord {
                        operation_id: request.operation_id,
                        asset_id: request.asset_id,
                        kind: request.kind,
                        evidence: ExecutionTerminalEvidence {
                            generation: request.generation,
                            priority: ExecutionPriority::UserInitiated,
                            disposition,
                            deadline: ExecutionDeadlineStatus::NotApplicable,
                        },
                        elapsed: result.elapsed,
                        detail,
                    },
                );
            }
            state.queue.clear();
            state.active.clear();
            state.running_operation_id = None;
            state.retired_worker_operations.clear();
            state.transport_occupied = 0;
        }
        let cumulative_failures = state.counters.failures.saturating_add(state.counters.rejections);
        let queued = state.queue.len();
        let running = usize::from(state.running_operation_id.is_some());
        let owned = state.active.len();
        EnduranceWorkerShutdownEvidence::from_join(
            1,
            true,
            join,
            queued,
            running,
            owned,
            cumulative_failures,
        )
    }
}

impl Default for MediaAssetMutationExecution {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MediaAssetMutationExecution {
    fn drop(&mut self) {
        self.begin_endurance_shutdown();

        let Some(worker) = self.worker.take() else {
            return;
        };
        let deadline = Instant::now() + MEDIA_ASSET_MUTATION_SHUTDOWN_GRACE;
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        if worker.is_finished() {
            if worker.join().is_err() {
                tracing::error!("media Asset mutation worker panicked during shutdown");
            }
        } else {
            tracing::warn!(
                "supervised media Asset Probe Helper settlement exceeded bounded shutdown grace; detached parent worker retains no Asset Library commit authority"
            );
        }
    }
}

fn media_asset_mutation_worker(inner: Arc<MediaAssetMutationInner>) {
    loop {
        let request = {
            let mut state = inner.state.lock();
            loop {
                if inner.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if state.dispatch_enabled {
                    let Some(request) = state.queue.pop_front() else {
                        inner.available.wait(&mut state);
                        continue;
                    };
                    state.running_operation_id = Some(request.operation_id);
                    mark_operation_changed(&mut state);
                    break request;
                }
                inner.available.wait(&mut state);
            }
        };
        let started = Instant::now();
        let outcome =
            prepare_media_asset_mutation(request, inner.probe_helper_executable.as_deref());
        let operation_id = outcome.request().operation_id;
        let disconnected = inner
            .result_tx
            .send(MediaAssetMutationWorkerResult { outcome, elapsed: started.elapsed() })
            .is_err();
        {
            let mut state = inner.state.lock();
            if state.running_operation_id == Some(operation_id) {
                state.running_operation_id = None;
                mark_operation_changed(&mut state);
            }
        }
        if disconnected {
            let mut state = inner.state.lock();
            state.transport_occupied = state.transport_occupied.saturating_sub(1);
            return;
        }
    }
}

fn prepare_media_asset_mutation(
    request: MediaAssetMutationRequest,
    helper_executable: Option<&Path>,
) -> MediaAssetMutationWorkerOutcome {
    if request.cancellation.is_canceled() {
        return MediaAssetMutationWorkerOutcome::Canceled(request);
    }
    let Some(helper_executable) = helper_executable else {
        return MediaAssetMutationWorkerOutcome::Failed {
            request,
            reason: "未找到与当前产品运行时匹配的媒体 Probe Helper".to_owned(),
        };
    };
    match prepare_probe_candidate(
        helper_executable,
        &request.source_path,
        &request.cancellation,
    ) {
        Ok(_) if request.cancellation.is_canceled() => {
            MediaAssetMutationWorkerOutcome::Canceled(request)
        }
        Ok(candidate) => {
            MediaAssetMutationWorkerOutcome::Prepared(Box::new(PreparedMediaAssetMutation {
                request,
                candidate,
            }))
        }
        Err(error) if error.is_canceled() => MediaAssetMutationWorkerOutcome::Canceled(request),
        Err(error) => {
            MediaAssetMutationWorkerOutcome::Failed { request, reason: error.to_string() }
        }
    }
}

fn prepare_probe_candidate(
    helper_executable: &Path,
    path: &Path,
    cancellation: &ExecutionCancellationToken,
) -> std::result::Result<AssetMediaProbeCandidate, mondrian_media::IsolatedMediaProbeError> {
    let prepared = mondrian_media::prepare_media_probe_isolated(
        helper_executable,
        path,
        cancellation,
        Instant::now() + super::packaged_worker::MEDIA_PROBE_TIMEOUT,
    )?;
    AssetMediaProbeCandidate::new(
        prepared.canonical_path,
        prepared.source_fingerprint,
        prepared.probe,
    )
    .map_err(
        |error| mondrian_media::IsolatedMediaProbeError::SnapshotRejected {
            detail: format!("isolated probe produced an invalid Asset candidate: {error}"),
        },
    )
}

fn commit_prepared_mutation(
    library: &AssetLibrary,
    prepared: PreparedMediaAssetMutation,
) -> Result<()> {
    match prepared.request.kind {
        MediaAssetMutationKind::Relink => {
            library.commit_relink_probe(prepared.request.asset_id, prepared.candidate)
        }
        MediaAssetMutationKind::RefreshAudioComponents => library.commit_audio_component_probe(
            prepared.request.asset_id,
            prepared.candidate,
            None,
        ),
        MediaAssetMutationKind::RebindAudioComponent { component_id, stream_index } => library
            .commit_audio_component_probe(
                prepared.request.asset_id,
                prepared.candidate,
                Some((component_id, stream_index)),
            ),
    }
}

fn push_terminal(state: &mut MediaAssetMutationState, terminal: MediaAssetMutationTerminalRecord) {
    match terminal.evidence.disposition {
        ExecutionTerminalDisposition::Completed => {
            state.counters.completions = state.counters.completions.saturating_add(1);
        }
        ExecutionTerminalDisposition::Failed | ExecutionTerminalDisposition::Rejected => {
            state.counters.failures = state.counters.failures.saturating_add(1);
        }
        ExecutionTerminalDisposition::Canceled => {
            state.counters.cancellations = state.counters.cancellations.saturating_add(1);
        }
        ExecutionTerminalDisposition::Superseded => {
            state.counters.superseded = state.counters.superseded.saturating_add(1);
        }
    }
    state.terminals.push_back(terminal);
    while state.terminals.len() > MEDIA_ASSET_MUTATION_TERMINAL_CAPACITY {
        state.terminals.pop_front();
    }
}

fn reject_admission(
    state: &mut MediaAssetMutationState,
    kind: MediaAssetMutationKind,
    reason: impl Into<String>,
) -> MondrianError {
    state.counters.rejections = state.counters.rejections.saturating_add(1);
    workflow_error(kind, reason)
}

fn workflow_error(kind: MediaAssetMutationKind, reason: impl Into<String>) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: kind.step_id().to_owned(),
        reason: reason.into(),
    }
}

fn next_nonzero_counter(value: u64) -> u64 {
    value.wrapping_add(1).max(1)
}

fn mark_operation_changed(state: &mut MediaAssetMutationState) {
    state.operation_revision = next_nonzero_counter(state.operation_revision);
}

fn mark_policy_changed(state: &mut MediaAssetMutationState) {
    state.policy_revision = next_nonzero_counter(state.policy_revision);
}

fn allocate_operation_id(state: &mut MediaAssetMutationState) -> u64 {
    // Active work is strictly bounded, so a free non-zero identity exists
    // within one more probe than the active capacity even across u64 wrap.
    loop {
        let candidate = state.next_operation_id.max(1);
        state.next_operation_id = next_nonzero_counter(candidate);
        if !state.active.contains_key(&candidate) {
            return candidate;
        }
    }
}

impl AppState {
    pub(crate) fn request_media_asset_mutation(
        &mut self,
        asset_id: AssetId,
        path: Option<PathBuf>,
        kind: MediaAssetMutationKind,
    ) -> Result<u64> {
        let library = self
            .asset_library_handle()
            .ok_or_else(|| workflow_error(kind, "素材库未连接"))?;
        let asset = library
            .get_asset(asset_id)?
            .ok_or_else(|| MondrianError::AssetNotFound { asset_id: asset_id.to_string() })?;
        if kind == MediaAssetMutationKind::Relink && asset.file_path().is_none() {
            return Err(workflow_error(
                kind,
                "只有文件素材可以重新链接；生成素材与远程素材拥有不同的 Source Interface",
            ));
        }
        let source_path = path
            .or_else(|| asset.file_path().map(Path::to_path_buf))
            .ok_or_else(|| workflow_error(kind, "生成素材或远程素材没有可探测的文件源"))?;
        let operation_id =
            self.media_asset_mutations.admit(asset_id, asset.name, source_path, kind)?;
        let _ = self.refresh_internal_execution_resource_decision();
        Ok(operation_id)
    }

    /// Drain prepared Asset mutations through the generation-safe commit Seam.
    pub fn poll_media_asset_mutations(&mut self) -> bool {
        let library = self.asset_library_handle();
        let publications = self.media_asset_mutations.poll_results(
            library.as_deref(),
            MEDIA_ASSET_MUTATION_MAX_RESULTS_PER_POLL,
        );
        let mut changed = self.media_asset_mutations.poll_changed();
        for publication in publications {
            changed = true;
            self.apply_media_asset_mutation(publication);
        }
        if changed {
            let _ = self.refresh_internal_execution_resource_decision();
        }
        changed
    }

    /// Snapshot bounded existing-Asset mutation execution and terminal evidence.
    pub fn media_asset_mutation_diagnostics(&self) -> MediaAssetMutationDiagnostics {
        self.media_asset_mutations.diagnostics()
    }

    fn apply_media_asset_mutation(&mut self, publication: MediaAssetMutationPublication) {
        if publication.evidence.disposition == ExecutionTerminalDisposition::Superseded {
            return;
        }
        let request = publication.request;
        match publication.result {
            Ok(()) => {
                // Asset mutations advance the SQLite media-binding revision
                // without necessarily changing the Project author generation.
                // Rotate the speculative binding first so an old completed
                // warmup cannot suppress or outlive the new physical source.
                self.synchronize_audio_idle_warmup_binding();
                match request.kind {
                    MediaAssetMutationKind::Relink => {
                        self.reconcile_audio_after_committed_authoring_change("media_asset_relink");
                        self.set_status_hint(
                            format!(
                                "已重新链接素材：{} → {}",
                                request.asset_name,
                                request.source_path.display()
                            ),
                            false,
                        );
                    }
                    MediaAssetMutationKind::RefreshAudioComponents => {
                        self.reconcile_audio_after_committed_authoring_change(
                            "media_asset_refresh_components",
                        );
                        self.set_status_hint(
                            format!("已刷新 {} 的音频流候选", request.asset_name),
                            false,
                        );
                    }
                    MediaAssetMutationKind::RebindAudioComponent { stream_index, .. } => {
                        self.reconcile_audio_after_committed_authoring_change(
                            "media_asset_interpretation",
                        );
                        self.set_status_hint(
                            format!(
                                "已将 {} 的音频 Component 映射到流 #{}",
                                request.asset_name, stream_index
                            ),
                            false,
                        );
                    }
                }
                self.event_bus.publish(AppEvent::AssetLibraryReloaded);
            }
            Err(reason) => {
                tracing::warn!(
                    target: "mondrian::asset_mutation",
                    operation_id = request.operation_id,
                    asset_id = %request.asset_id,
                    kind = ?request.kind,
                    elapsed_ms = publication.elapsed.as_millis(),
                    %reason,
                    "media Asset mutation failed"
                );
                let label = match request.kind {
                    MediaAssetMutationKind::Relink => "重新链接素材",
                    MediaAssetMutationKind::RefreshAudioComponents => "音频 Component 探测",
                    MediaAssetMutationKind::RebindAudioComponent { .. } => "音频 Component 重绑定",
                };
                self.set_status_hint(format!("{label}失败：{reason}"), true);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_root(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "mondrian-media-asset-mutation-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create fixture root");
        root
    }

    #[test]
    fn unbound_execution_rejects_admission_without_starting_probe() {
        let execution = MediaAssetMutationExecution::new();
        let error = execution
            .admit(
                AssetId::new(),
                "asset".to_owned(),
                PathBuf::from("missing.wav"),
                MediaAssetMutationKind::Relink,
            )
            .expect_err("unbound mutation must fail");
        assert!(matches!(
            error,
            MondrianError::WorkflowStepFailed { step_id, .. } if step_id == "relink_asset"
        ));
        assert_eq!(execution.diagnostics().outstanding, 0);
    }

    #[test]
    fn project_rotation_cancels_queued_commit_authority() {
        let execution = MediaAssetMutationExecution::new();
        execution.bind_project(Some(ProjectId::new()));
        let _ = execution
            .admit(
                AssetId::new(),
                "asset".to_owned(),
                PathBuf::from("missing.wav"),
                MediaAssetMutationKind::Relink,
            )
            .expect("admit");
        execution.bind_project(Some(ProjectId::new()));
        let diagnostics = execution.diagnostics();
        assert_eq!(diagnostics.outstanding, 0);
        assert_eq!(diagnostics.terminals.len(), 1);
        assert_eq!(
            diagnostics.terminals[0].evidence.disposition,
            ExecutionTerminalDisposition::Superseded
        );
    }

    #[test]
    fn resource_policy_revision_is_diagnostic_only() {
        let execution = MediaAssetMutationExecution::new();
        assert!(!execution.poll_changed());
        let initial = execution.diagnostics();

        execution.set_resource_policy(false);

        let paused = execution.diagnostics();
        assert!(!paused.dispatch_enabled);
        assert_eq!(paused.operation_revision, initial.operation_revision);
        assert_eq!(paused.policy_revision, initial.policy_revision + 1);
        assert!(!execution.poll_changed());

        execution.set_resource_policy(false);
        let unchanged = execution.diagnostics();
        assert_eq!(unchanged.operation_revision, paused.operation_revision);
        assert_eq!(unchanged.policy_revision, paused.policy_revision);
        assert!(!execution.poll_changed());
    }

    #[test]
    fn operation_execution_publication_and_terminal_remain_observable() {
        let root = unique_root("observable");
        let execution = MediaAssetMutationExecution::new();
        assert!(!execution.poll_changed());
        execution.bind_project(Some(ProjectId::new()));
        assert!(execution.poll_changed());
        execution.set_resource_policy(false);
        assert!(!execution.poll_changed());

        execution
            .admit(
                AssetId::new(),
                "missing asset".to_owned(),
                root.join("missing.wav"),
                MediaAssetMutationKind::Relink,
            )
            .expect("admit paused mutation");
        assert!(execution.poll_changed());

        execution.set_resource_policy(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let diagnostics = execution.diagnostics();
            if diagnostics.queued == 0 && diagnostics.running == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "mutation worker did not publish its outcome"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(execution.poll_changed());

        let publications = execution.poll_results(None, 1);
        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].evidence.disposition,
            ExecutionTerminalDisposition::Failed
        );
        assert!(execution.poll_changed());
        let terminal = execution.diagnostics();
        assert_eq!(terminal.outstanding, 0);
        assert_eq!(terminal.terminals.len(), 1);
        assert_eq!(
            terminal.terminals[0].evidence.disposition,
            ExecutionTerminalDisposition::Failed
        );

        drop(execution);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paused_execution_retains_work_until_resource_policy_resumes() {
        let execution = MediaAssetMutationExecution::new();
        execution.bind_project(Some(ProjectId::new()));
        execution.set_resource_policy(false);
        execution
            .admit(
                AssetId::new(),
                "asset".to_owned(),
                PathBuf::from("missing.wav"),
                MediaAssetMutationKind::Relink,
            )
            .expect("admit paused mutation");
        std::thread::sleep(Duration::from_millis(20));
        let paused = execution.diagnostics();
        assert!(!paused.dispatch_enabled);
        assert_eq!(paused.queued, 1);
        assert_eq!(paused.running, 0);

        execution.set_resource_policy(true);
        let deadline = Instant::now() + Duration::from_secs(2);
        while execution.diagnostics().queued != 0 {
            assert!(Instant::now() < deadline, "mutation worker did not resume");
            std::thread::sleep(Duration::from_millis(2));
        }
        let resumed = execution.diagnostics();
        assert!(resumed.dispatch_enabled);
        assert_eq!(resumed.queued, 0);
    }

    #[test]
    fn one_asset_cannot_bind_two_uncommitted_source_intents() {
        let execution = MediaAssetMutationExecution::new();
        execution.bind_project(Some(ProjectId::new()));
        execution.set_resource_policy(false);
        let asset_id = AssetId::new();
        execution
            .admit(
                asset_id,
                "asset".to_owned(),
                PathBuf::from("first.wav"),
                MediaAssetMutationKind::Relink,
            )
            .expect("first intent");

        let error = execution
            .admit(
                asset_id,
                "asset".to_owned(),
                PathBuf::from("stale.wav"),
                MediaAssetMutationKind::RefreshAudioComponents,
            )
            .expect_err("second uncommitted intent must be rejected");

        assert!(error.to_string().contains("尚未提交"));
        assert_eq!(execution.diagnostics().outstanding, 1);
    }

    #[test]
    fn project_generation_rotates_at_counter_exhaustion() {
        let execution = MediaAssetMutationExecution::new();
        execution.inner.state.lock().generation = u64::MAX;

        execution.bind_project(Some(ProjectId::new()));

        assert_eq!(execution.diagnostics().generation, 1);
    }

    #[test]
    fn late_prepared_candidate_is_superseded_before_asset_library_commit() {
        let root = unique_root("superseded");
        let source_path = root.join("replacement.bin");
        std::fs::write(&source_path, [7_u8]).expect("write source");
        let source_path = source_path.canonicalize().expect("canonical source");
        let fingerprint = MediaFileFingerprint::capture(&source_path);
        let candidate = AssetMediaProbeCandidate::new(
            source_path.clone(),
            fingerprint,
            mondrian_core::MediaProbeSnapshot {
                duration: Duration::ZERO,
                file_size: 1,
                container: "unknown".to_owned(),
                video_streams: Vec::new(),
                audio_streams: Vec::new(),
                has_video: false,
                has_audio: false,
            },
        )
        .expect("candidate");
        let execution = MediaAssetMutationExecution::new();
        execution.bind_project(Some(ProjectId::new()));
        let generation = execution.diagnostics().generation;
        let cancellation = ExecutionCancellationToken::new();
        let request = MediaAssetMutationRequest {
            operation_id: 91,
            generation,
            asset_id: AssetId::new(),
            asset_name: "late".to_owned(),
            source_path,
            kind: MediaAssetMutationKind::Relink,
            cancellation: cancellation.clone(),
        };
        {
            let mut state = execution.inner.state.lock();
            state.active.insert(
                request.operation_id,
                ActiveMediaAssetMutation {
                    generation,
                    asset_id: request.asset_id,
                    kind: request.kind,
                    cancellation,
                },
            );
            state.running_operation_id = Some(request.operation_id);
        }
        execution.bind_project(Some(ProjectId::new()));
        execution
            .inner
            .result_tx
            .send(MediaAssetMutationWorkerResult {
                outcome: MediaAssetMutationWorkerOutcome::Prepared(Box::new(
                    PreparedMediaAssetMutation { request, candidate },
                )),
                elapsed: Duration::from_millis(2),
            })
            .expect("inject late worker result");
        execution.inner.state.lock().running_operation_id = None;
        let library = AssetLibrary::open(root.join("library")).expect("library");
        let publications = execution.poll_results(Some(&library), 1);

        assert!(publications.is_empty());
        assert!(library.list_assets().expect("list assets").is_empty());
        assert_eq!(execution.diagnostics().terminals.len(), 1);
        assert_eq!(
            execution.diagnostics().terminals.last().expect("terminal").evidence.disposition,
            ExecutionTerminalDisposition::Superseded
        );

        drop(execution);
        let _ = std::fs::remove_dir_all(root);
    }
}
