//! Instance-owned bounded media-import execution implementation.
//!
//! Import is an explicit user workflow. The Module therefore admits bounded
//! batches even while product resource policy has paused dispatch, retains
//! them until execution can resume, and publishes results only for the exact
//! Project generation that admitted them. It owns no UI models.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_assets::AssetLibrary;
use mondrian_core::types::{AssetId, ProjectId};
use mondrian_core::{
    ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence,
};
use parking_lot::{Condvar, Mutex};

use super::asset_adapter::{AssetLibraryMediaImportBackend, MediaImportPreparedCandidate};
use super::{
    MediaImportBatchId, MediaImportDiagnostics, MediaImportFailureReason, MediaImportTerminalRecord,
};

const MEDIA_IMPORT_BATCH_CAPACITY: usize = 16;
const MEDIA_IMPORT_FILES_PER_BATCH_CAPACITY: usize = 512;
const MEDIA_IMPORT_OUTSTANDING_FILE_CAPACITY: usize = 1_024;
const MEDIA_IMPORT_RESULT_CAPACITY: usize = MEDIA_IMPORT_OUTSTANDING_FILE_CAPACITY;
const MEDIA_IMPORT_TERMINAL_CAPACITY: usize = 512;
const MEDIA_IMPORT_MAX_WORKERS: usize = 2;
const MEDIA_IMPORT_SHUTDOWN_GRACE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MediaImportAdmissionError {
    ProjectUnavailable,
    WorkerUnavailable,
    EmptyBatch,
    BatchCapacityExceeded { capacity: usize },
    BatchTooLarge { files: usize, capacity: usize },
    FileCapacityExceeded { files: usize, available: usize },
}

impl std::fmt::Display for MediaImportAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProjectUnavailable => write!(formatter, "媒体导入没有绑定活动项目"),
            Self::WorkerUnavailable => write!(formatter, "媒体导入 worker 不可用"),
            Self::EmptyBatch => write!(formatter, "媒体导入批次不包含文件"),
            Self::BatchCapacityExceeded { capacity } => {
                write!(formatter, "媒体导入批次容量已满（上限 {capacity}）")
            }
            Self::BatchTooLarge { files, capacity } => {
                write!(
                    formatter,
                    "单个媒体导入批次包含 {files} 个文件，超过上限 {capacity}"
                )
            }
            Self::FileCapacityExceeded { files, available } => write!(
                formatter,
                "媒体导入需要 {files} 个文件槽位，但当前仅剩 {available} 个"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct MediaImportAdmission {
    pub(super) batch_id: u64,
    pub(super) generation: u64,
    pub(super) total: usize,
}

#[derive(Debug)]
pub(super) enum MediaImportWorkerOutcome {
    Prepared(Box<MediaImportPreparedCandidate>),
    Failed(String),
    Canceled,
}

#[derive(Debug)]
pub(super) enum MediaImportPublicationOutcome {
    Imported(AssetId),
    Failed(String),
    Canceled,
}

#[derive(Debug)]
struct MediaImportWorkerResult {
    batch_id: u64,
    generation: u64,
    path: PathBuf,
    elapsed: Duration,
    outcome: MediaImportWorkerOutcome,
}

#[derive(Debug)]
pub(super) struct MediaImportPublication {
    pub(super) batch_id: u64,
    pub(super) generation: u64,
    pub(super) path: PathBuf,
    pub(super) evidence: ExecutionTerminalEvidence,
    pub(super) outcome: MediaImportPublicationOutcome,
}

#[derive(Clone)]
struct MediaImportJob {
    batch_id: u64,
    generation: u64,
    folder_id: Option<String>,
    path: PathBuf,
    cancellation: ExecutionCancellationToken,
}

struct MediaImportBatchRuntime {
    generation: u64,
    remaining: usize,
    cancellation: ExecutionCancellationToken,
}

#[derive(Default)]
struct MediaImportCounters {
    admissions: u64,
    rejections: u64,
    imported_files: u64,
    failed_files: u64,
    canceled_files: u64,
    superseded_files: u64,
}

struct MediaImportExecutionState {
    project_id: Option<ProjectId>,
    generation: u64,
    next_batch_id: u64,
    dispatch_enabled: bool,
    dispatch_parallelism: usize,
    queue: VecDeque<MediaImportJob>,
    batches: HashMap<u64, MediaImportBatchRuntime>,
    running_files: usize,
    outstanding_files: usize,
    counters: MediaImportCounters,
    terminal_records: VecDeque<MediaImportTerminalRecord>,
}

impl Default for MediaImportExecutionState {
    fn default() -> Self {
        Self {
            project_id: None,
            generation: 1,
            next_batch_id: 1,
            dispatch_enabled: true,
            dispatch_parallelism: 1,
            queue: VecDeque::new(),
            batches: HashMap::new(),
            running_files: 0,
            outstanding_files: 0,
            counters: MediaImportCounters::default(),
            terminal_records: VecDeque::new(),
        }
    }
}

struct MediaImportExecutionInner {
    state: Mutex<MediaImportExecutionState>,
    available: Condvar,
    shutdown: AtomicBool,
    diagnostics_revision: AtomicU64,
    model_revision: AtomicU64,
    preparer: Arc<dyn MediaImportPreparationBackend>,
    result_tx: mpsc::SyncSender<MediaImportWorkerResult>,
}

impl MediaImportExecutionInner {
    fn mark_diagnostics_changed(&self) {
        self.diagnostics_revision.fetch_add(1, Ordering::AcqRel);
    }

    fn mark_model_changed(&self) {
        self.diagnostics_revision.fetch_add(1, Ordering::AcqRel);
        self.model_revision.fetch_add(1, Ordering::AcqRel);
    }
}

pub(super) trait MediaImportPreparationBackend: Send + Sync {
    /// Prepare immutable source metadata without reading or writing Project,
    /// Asset Library, authoring, or UI state.
    ///
    /// Implementations should observe cancellation before and after any
    /// blocking foreign-library call. The call itself need not be
    /// interruptible; workers have no commit authority, so shutdown may safely
    /// detach a late preparation after the bounded grace period.
    fn prepare(
        &self,
        path: &Path,
        folder_id: Option<&str>,
        cancellation: &ExecutionCancellationToken,
    ) -> MediaImportWorkerOutcome;
}

pub(super) trait MediaImportCommitBackend: Send + Sync {
    fn commit(
        &self,
        library: &AssetLibrary,
        candidate: MediaImportPreparedCandidate,
    ) -> MediaImportPublicationOutcome;
}

trait MediaImportBackend: MediaImportPreparationBackend + MediaImportCommitBackend {}

impl<T> MediaImportBackend for T where T: MediaImportPreparationBackend + MediaImportCommitBackend {}

pub(in super::super) struct MediaImportExecution {
    inner: Arc<MediaImportExecutionInner>,
    committer: Arc<dyn MediaImportCommitBackend>,
    /// Serializes irreversible Asset Library publication with Project
    /// rebinding and user cancellation without holding the execution-state
    /// lock across SQLite I/O.
    publication_gate: Mutex<()>,
    results: Mutex<mpsc::Receiver<MediaImportWorkerResult>>,
    worker_handles: Vec<JoinHandle<()>>,
    observed_model_revision: AtomicU64,
}

impl MediaImportExecution {
    pub(in super::super) fn new() -> Self {
        Self::with_backend(
            media_import_worker_count(),
            Arc::new(AssetLibraryMediaImportBackend),
        )
    }

    fn with_backend<B>(worker_count: usize, backend: Arc<B>) -> Self
    where
        B: MediaImportBackend + 'static,
    {
        let preparer: Arc<dyn MediaImportPreparationBackend> = backend.clone();
        let committer: Arc<dyn MediaImportCommitBackend> = backend;
        let (result_tx, result_rx) = mpsc::sync_channel(MEDIA_IMPORT_RESULT_CAPACITY);
        let inner = Arc::new(MediaImportExecutionInner {
            state: Mutex::new(MediaImportExecutionState {
                dispatch_parallelism: worker_count.max(1),
                ..MediaImportExecutionState::default()
            }),
            available: Condvar::new(),
            shutdown: AtomicBool::new(false),
            diagnostics_revision: AtomicU64::new(0),
            model_revision: AtomicU64::new(0),
            preparer,
            result_tx,
        });
        let mut worker_handles = Vec::new();
        for index in 0..worker_count {
            let worker_inner = Arc::clone(&inner);
            match std::thread::Builder::new()
                .name(format!("mondrian-media-import-{index}"))
                .spawn(move || media_import_worker(worker_inner))
            {
                Ok(handle) => worker_handles.push(handle),
                Err(error) => {
                    tracing::error!(
                        worker_index = index,
                        %error,
                        "failed to start media import worker"
                    );
                }
            }
        }
        Self {
            inner,
            committer,
            publication_gate: Mutex::new(()),
            results: Mutex::new(result_rx),
            worker_handles,
            observed_model_revision: AtomicU64::new(0),
        }
    }

    pub(in super::super) fn bind_project(&self, project_id: Option<ProjectId>) {
        let _publication = self.publication_gate.lock();
        let mut state = self.inner.state.lock();
        if state.project_id.is_none() && project_id.is_none() {
            return;
        }
        state.project_id = project_id;
        state.generation = next_nonzero_counter(state.generation);
        for batch in state.batches.values() {
            batch.cancellation.cancel();
        }
        let retired: Vec<_> = state.queue.drain(..).collect();
        for job in retired {
            state.outstanding_files = state.outstanding_files.saturating_sub(1);
            state.counters.canceled_files = state.counters.canceled_files.saturating_add(1);
            push_import_terminal(
                &mut state,
                job.batch_id,
                Some(job.path),
                job.generation,
                ExecutionTerminalDisposition::Canceled,
                Duration::ZERO,
                Some(MediaImportFailureReason::Canceled),
            );
        }
        state.batches.clear();
        self.inner.mark_model_changed();
        drop(state);
        self.inner.available.notify_all();
    }

    pub(in super::super) fn set_resource_policy(
        &self,
        dispatch_enabled: bool,
        max_parallelism: usize,
    ) {
        let mut state = self.inner.state.lock();
        let max_parallelism = max_parallelism.max(1).min(self.worker_handles.len().max(1));
        if state.dispatch_enabled == dispatch_enabled
            && state.dispatch_parallelism == max_parallelism
        {
            return;
        }
        state.dispatch_enabled = dispatch_enabled;
        state.dispatch_parallelism = max_parallelism;
        self.inner.mark_diagnostics_changed();
        drop(state);
        self.inner.available.notify_all();
    }

    pub(super) fn admit_batch(
        &self,
        paths: Vec<PathBuf>,
        folder_id: Option<String>,
    ) -> std::result::Result<MediaImportAdmission, MediaImportAdmissionError> {
        let mut state = self.inner.state.lock();
        if state.project_id.is_none() {
            return Err(reject_import_admission(
                &self.inner,
                &mut state,
                MediaImportAdmissionError::ProjectUnavailable,
            ));
        }
        if self.worker_handles.is_empty() {
            return Err(reject_import_admission(
                &self.inner,
                &mut state,
                MediaImportAdmissionError::WorkerUnavailable,
            ));
        }
        if paths.is_empty() {
            return Err(reject_import_admission(
                &self.inner,
                &mut state,
                MediaImportAdmissionError::EmptyBatch,
            ));
        }
        if paths.len() > MEDIA_IMPORT_FILES_PER_BATCH_CAPACITY {
            return Err(reject_import_admission(
                &self.inner,
                &mut state,
                MediaImportAdmissionError::BatchTooLarge {
                    files: paths.len(),
                    capacity: MEDIA_IMPORT_FILES_PER_BATCH_CAPACITY,
                },
            ));
        }
        if state.batches.len() >= MEDIA_IMPORT_BATCH_CAPACITY {
            return Err(reject_import_admission(
                &self.inner,
                &mut state,
                MediaImportAdmissionError::BatchCapacityExceeded {
                    capacity: MEDIA_IMPORT_BATCH_CAPACITY,
                },
            ));
        }
        let available =
            MEDIA_IMPORT_OUTSTANDING_FILE_CAPACITY.saturating_sub(state.outstanding_files);
        if paths.len() > available {
            return Err(reject_import_admission(
                &self.inner,
                &mut state,
                MediaImportAdmissionError::FileCapacityExceeded { files: paths.len(), available },
            ));
        }

        let batch_id = allocate_batch_id(&mut state);
        let generation = state.generation;
        let cancellation = ExecutionCancellationToken::new();
        let total = paths.len();
        state.batches.insert(
            batch_id,
            MediaImportBatchRuntime {
                generation,
                remaining: total,
                cancellation: cancellation.clone(),
            },
        );
        for path in paths {
            state.queue.push_back(MediaImportJob {
                batch_id,
                generation,
                folder_id: folder_id.clone(),
                path,
                cancellation: cancellation.clone(),
            });
        }
        state.outstanding_files = state.outstanding_files.saturating_add(total);
        state.counters.admissions = state.counters.admissions.saturating_add(1);
        self.inner.mark_model_changed();
        drop(state);
        self.inner.available.notify_all();
        Ok(MediaImportAdmission { batch_id, generation, total })
    }

    pub(super) fn cancel_batch(&self, batch_id: MediaImportBatchId) -> bool {
        let _publication = self.publication_gate.lock();
        let batch_id = batch_id.get();
        let mut state = self.inner.state.lock();
        let Some(batch) = state.batches.get(&batch_id) else {
            return false;
        };
        batch.cancellation.cancel();
        let retired: Vec<_> =
            state.queue.iter().filter(|job| job.batch_id == batch_id).cloned().collect();
        state.queue.retain(|job| job.batch_id != batch_id);
        for job in &retired {
            state.outstanding_files = state.outstanding_files.saturating_sub(1);
            state.counters.canceled_files = state.counters.canceled_files.saturating_add(1);
            push_import_terminal(
                &mut state,
                batch_id,
                Some(job.path.clone()),
                job.generation,
                ExecutionTerminalDisposition::Canceled,
                Duration::ZERO,
                Some(MediaImportFailureReason::Canceled),
            );
        }
        if let Some(batch) = state.batches.get_mut(&batch_id) {
            batch.remaining = batch.remaining.saturating_sub(retired.len());
            if batch.remaining == 0 {
                state.batches.remove(&batch_id);
            }
        }
        self.inner.mark_model_changed();
        drop(state);
        self.inner.available.notify_all();
        true
    }

    pub(super) fn poll_results(
        &self,
        library: Option<&AssetLibrary>,
        limit: usize,
    ) -> Vec<MediaImportPublication> {
        let mut publications = Vec::new();
        let results = self.results.lock();
        for _ in 0..limit {
            let result = match results.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            // Publication is the only irreversible phase. Serialize it with
            // Project rebinding and cancellation, but release execution state
            // before touching SQLite so workers, diagnostics, and resource
            // policy remain responsive on slow storage.
            let _publication = self.publication_gate.lock();
            let mut state = self.inner.state.lock();
            state.outstanding_files = state.outstanding_files.saturating_sub(1);
            let current = state.generation == result.generation
                && state
                    .batches
                    .get(&result.batch_id)
                    .is_some_and(|batch| batch.generation == result.generation);
            let canceled = current
                && state
                    .batches
                    .get(&result.batch_id)
                    .is_some_and(|batch| batch.cancellation.is_canceled());
            let (outcome, disposition, failure) = if !current {
                state.counters.superseded_files = state.counters.superseded_files.saturating_add(1);
                (
                    MediaImportPublicationOutcome::Canceled,
                    ExecutionTerminalDisposition::Superseded,
                    None,
                )
            } else if canceled {
                state.counters.canceled_files = state.counters.canceled_files.saturating_add(1);
                (
                    MediaImportPublicationOutcome::Canceled,
                    ExecutionTerminalDisposition::Canceled,
                    Some(MediaImportFailureReason::Canceled),
                )
            } else {
                match result.outcome {
                    MediaImportWorkerOutcome::Prepared(candidate) => {
                        drop(state);
                        let committed = match library {
                            Some(library) => self.committer.commit(library, *candidate),
                            None => MediaImportPublicationOutcome::Failed(
                                "素材库在导入提交前已断开".to_owned(),
                            ),
                        };
                        state = self.inner.state.lock();
                        match committed {
                            MediaImportPublicationOutcome::Imported(asset_id) => {
                                state.counters.imported_files =
                                    state.counters.imported_files.saturating_add(1);
                                (
                                    MediaImportPublicationOutcome::Imported(asset_id),
                                    ExecutionTerminalDisposition::Completed,
                                    None,
                                )
                            }
                            MediaImportPublicationOutcome::Failed(error) => {
                                state.counters.failed_files =
                                    state.counters.failed_files.saturating_add(1);
                                (
                                    MediaImportPublicationOutcome::Failed(error),
                                    ExecutionTerminalDisposition::Failed,
                                    Some(MediaImportFailureReason::ImportFailed),
                                )
                            }
                            MediaImportPublicationOutcome::Canceled => {
                                state.counters.canceled_files =
                                    state.counters.canceled_files.saturating_add(1);
                                (
                                    MediaImportPublicationOutcome::Canceled,
                                    ExecutionTerminalDisposition::Canceled,
                                    Some(MediaImportFailureReason::Canceled),
                                )
                            }
                        }
                    }
                    MediaImportWorkerOutcome::Failed(error) => {
                        state.counters.failed_files = state.counters.failed_files.saturating_add(1);
                        (
                            MediaImportPublicationOutcome::Failed(error),
                            ExecutionTerminalDisposition::Failed,
                            Some(MediaImportFailureReason::ImportFailed),
                        )
                    }
                    MediaImportWorkerOutcome::Canceled => {
                        state.counters.canceled_files =
                            state.counters.canceled_files.saturating_add(1);
                        (
                            MediaImportPublicationOutcome::Canceled,
                            ExecutionTerminalDisposition::Canceled,
                            Some(MediaImportFailureReason::Canceled),
                        )
                    }
                }
            };
            if current {
                if let Some(batch) = state.batches.get_mut(&result.batch_id) {
                    batch.remaining = batch.remaining.saturating_sub(1);
                    if batch.remaining == 0 {
                        state.batches.remove(&result.batch_id);
                    }
                }
            }
            push_import_terminal(
                &mut state,
                result.batch_id,
                Some(result.path.clone()),
                result.generation,
                disposition,
                result.elapsed,
                failure,
            );
            publications.push(MediaImportPublication {
                batch_id: result.batch_id,
                generation: result.generation,
                path: result.path,
                evidence: ExecutionTerminalEvidence {
                    generation: result.generation,
                    priority: ExecutionPriority::UserInitiated,
                    disposition,
                    deadline: ExecutionDeadlineStatus::NotApplicable,
                },
                outcome,
            });
        }
        drop(results);
        if !publications.is_empty() {
            self.inner.mark_model_changed();
        }
        publications
    }

    pub(super) fn poll_model_changed(&self) -> bool {
        let revision = self.inner.model_revision.load(Ordering::Acquire);
        self.observed_model_revision.swap(revision, Ordering::AcqRel) != revision
    }

    pub(in super::super) fn diagnostics(&self) -> MediaImportDiagnostics {
        let state = self.inner.state.lock();
        let mut active_batch_ids =
            state.batches.keys().copied().map(MediaImportBatchId).collect::<Vec<_>>();
        active_batch_ids.sort_unstable_by_key(|batch_id| batch_id.get());
        MediaImportDiagnostics {
            diagnostics_revision: self.inner.diagnostics_revision.load(Ordering::Acquire),
            model_revision: self.inner.model_revision.load(Ordering::Acquire),
            generation: state.generation,
            dispatch_enabled: state.dispatch_enabled,
            dispatch_parallelism: state.dispatch_parallelism,
            active_batches: state.batches.len(),
            active_batch_ids,
            queued_files: state.queue.len(),
            running_files: state.running_files,
            outstanding_files: state.batches.values().map(|batch| batch.remaining).sum(),
            transport_occupied_files: state.outstanding_files,
            admissions: state.counters.admissions,
            rejections: state.counters.rejections,
            imported_files: state.counters.imported_files,
            failed_files: state.counters.failed_files,
            canceled_files: state.counters.canceled_files,
            superseded_files: state.counters.superseded_files,
            terminal_records: state.terminal_records.iter().cloned().collect(),
        }
    }
}

impl Default for MediaImportExecution {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MediaImportExecution {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Release);
        let mut state = self.inner.state.lock();
        for batch in state.batches.values() {
            batch.cancellation.cancel();
        }
        state.queue.clear();
        drop(state);
        self.inner.available.notify_all();
        let deadline = Instant::now() + MEDIA_IMPORT_SHUTDOWN_GRACE;
        let mut handles = self.worker_handles.drain(..).collect::<Vec<_>>();
        while !handles.is_empty() && Instant::now() < deadline {
            let mut index = handles.len();
            while index > 0 {
                index -= 1;
                if handles[index].is_finished() {
                    let handle = handles.swap_remove(index);
                    if handle.join().is_err() {
                        tracing::error!("media import worker panicked during shutdown");
                    }
                }
            }
            if !handles.is_empty() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        if !handles.is_empty() {
            tracing::warn!(
                worker_count = handles.len(),
                "media import preparation exceeded bounded shutdown grace; detached workers retain no Asset Library commit authority"
            );
        }
    }
}

fn reject_import_admission(
    inner: &MediaImportExecutionInner,
    state: &mut MediaImportExecutionState,
    error: MediaImportAdmissionError,
) -> MediaImportAdmissionError {
    let batch_id = allocate_batch_id(state);
    state.counters.rejections = state.counters.rejections.saturating_add(1);
    push_import_terminal(
        state,
        batch_id,
        None,
        state.generation,
        ExecutionTerminalDisposition::Rejected,
        Duration::ZERO,
        Some(MediaImportFailureReason::AdmissionRejected),
    );
    inner.mark_model_changed();
    error
}

fn media_import_worker(inner: Arc<MediaImportExecutionInner>) {
    loop {
        let job = {
            let mut state = inner.state.lock();
            loop {
                if inner.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if state.dispatch_enabled && state.running_files < state.dispatch_parallelism {
                    if let Some(job) = state.queue.pop_front() {
                        state.running_files = state.running_files.saturating_add(1);
                        inner.mark_diagnostics_changed();
                        break job;
                    }
                }
                inner.available.wait(&mut state);
            }
        };
        let started = Instant::now();
        let outcome = if job.cancellation.is_canceled() {
            MediaImportWorkerOutcome::Canceled
        } else {
            inner.preparer.prepare(&job.path, job.folder_id.as_deref(), &job.cancellation)
        };
        let result = MediaImportWorkerResult {
            batch_id: job.batch_id,
            generation: job.generation,
            path: job.path,
            elapsed: started.elapsed(),
            outcome,
        };
        {
            let mut state = inner.state.lock();
            state.running_files = state.running_files.saturating_sub(1);
            inner.mark_diagnostics_changed();
        }
        // Admission caps total unconsumed results to this channel's capacity,
        // so a live App consumer cannot deadlock worker shutdown here.
        if inner.result_tx.send(result).is_err() {
            return;
        }
        inner.mark_model_changed();
        inner.available.notify_all();
    }
}

fn push_import_terminal(
    state: &mut MediaImportExecutionState,
    batch_id: u64,
    path: Option<PathBuf>,
    generation: u64,
    disposition: ExecutionTerminalDisposition,
    elapsed: Duration,
    failure: Option<MediaImportFailureReason>,
) {
    state.terminal_records.push_back(MediaImportTerminalRecord {
        evidence: ExecutionTerminalEvidence {
            generation,
            priority: ExecutionPriority::UserInitiated,
            disposition,
            deadline: ExecutionDeadlineStatus::NotApplicable,
        },
        batch_id,
        path,
        elapsed,
        failure,
    });
    while state.terminal_records.len() > MEDIA_IMPORT_TERMINAL_CAPACITY {
        state.terminal_records.pop_front();
    }
}

fn next_nonzero_counter(value: u64) -> u64 {
    value.wrapping_add(1).max(1)
}

fn allocate_batch_id(state: &mut MediaImportExecutionState) -> u64 {
    // At most `MEDIA_IMPORT_BATCH_CAPACITY` identities are live, so a free
    // non-zero identity exists within this bounded search even across wrap.
    loop {
        let candidate = state.next_batch_id.max(1);
        state.next_batch_id = next_nonzero_counter(candidate);
        if !state.batches.contains_key(&candidate) {
            return candidate;
        }
    }
}

fn media_import_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .saturating_sub(2)
        .clamp(1, MEDIA_IMPORT_MAX_WORKERS)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
