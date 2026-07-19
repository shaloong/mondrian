//! Bounded offline export admission, lifecycle, cancellation, and evidence.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use mondrian_core::{
    ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence, JobId,
};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};

use crate::preset::ExportConfig;

use super::{ExportExecutor, ExportJobDiagnostics, JobExecutionResult};

/// Maximum number of admitted jobs that may be pending or executing.
pub const EXPORT_IN_FLIGHT_CAPACITY: usize = 64;
/// Maximum number of lightweight terminal snapshots retained without user cleanup.
pub const EXPORT_TERMINAL_HISTORY_CAPACITY: usize = 256;
const EXPORT_FAILURE_DETAIL_CHARS: usize = 4_096;

/// Coarse production phase for one offline export attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportProgressPhase {
    /// Validate the frozen snapshot and prepare resources.
    Preparing,
    /// Render timeline video frames.
    Rendering,
    /// Encode or mux media.
    Encoding,
    /// Validate the complete temporary deliverable.
    Validating,
    /// Atomically publish the validated deliverable.
    Publishing,
}

/// Phase-specific progress units; values are never presented as fake frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum ExportProgressDetail {
    /// The phase has no meaningful discrete unit count.
    None,
    /// Timeline frames rendered into the encoder input.
    Frames { completed: u64, total: u64 },
    /// Encoded source-media time reported by the encoder.
    MediaTimeMicros { completed: u64, total: u64 },
}

/// Monotonic progress snapshot for one export attempt.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExportProgress {
    /// Current execution phase.
    pub phase: ExportProgressPhase,
    /// Normalized whole-job completion estimate.
    pub fraction: f32,
    /// Optional truthful units for this phase.
    pub detail: ExportProgressDetail,
}

impl ExportProgress {
    pub(crate) const fn preparing(fraction: f32) -> Self {
        Self {
            phase: ExportProgressPhase::Preparing,
            fraction,
            detail: ExportProgressDetail::None,
        }
    }

    pub(crate) const fn rendering(fraction: f32, completed: u64, total: u64) -> Self {
        Self {
            phase: ExportProgressPhase::Rendering,
            fraction,
            detail: ExportProgressDetail::Frames { completed, total },
        }
    }

    pub(crate) const fn encoding(fraction: f32) -> Self {
        Self {
            phase: ExportProgressPhase::Encoding,
            fraction,
            detail: ExportProgressDetail::None,
        }
    }

    pub(crate) const fn validating(fraction: f32) -> Self {
        Self {
            phase: ExportProgressPhase::Validating,
            fraction,
            detail: ExportProgressDetail::None,
        }
    }

    pub(crate) const fn publishing(fraction: f32) -> Self {
        Self {
            phase: ExportProgressPhase::Publishing,
            fraction,
            detail: ExportProgressDetail::None,
        }
    }

    fn normalized(mut self, previous: Self) -> Self {
        self.fraction = if self.fraction.is_finite() {
            self.fraction.clamp(previous.fraction, 1.0)
        } else {
            previous.fraction
        };
        self.detail = normalized_progress_detail(self, previous);
        self
    }
}

fn normalized_progress_detail(
    current: ExportProgress,
    previous: ExportProgress,
) -> ExportProgressDetail {
    let sanitized = match (current.phase, current.detail) {
        (ExportProgressPhase::Rendering, ExportProgressDetail::Frames { completed, total })
            if total > 0 =>
        {
            ExportProgressDetail::Frames { completed: completed.min(total), total }
        }
        (
            ExportProgressPhase::Encoding,
            ExportProgressDetail::MediaTimeMicros { completed, total },
        ) if total > 0 => {
            ExportProgressDetail::MediaTimeMicros { completed: completed.min(total), total }
        }
        (_, ExportProgressDetail::None) => ExportProgressDetail::None,
        _ => ExportProgressDetail::None,
    };
    if current.phase != previous.phase {
        return sanitized;
    }
    match (previous.detail, sanitized) {
        (
            ExportProgressDetail::Frames {
                completed: previous_completed,
                total: previous_total,
            },
            ExportProgressDetail::Frames { completed, total },
        ) if previous_total == total => ExportProgressDetail::Frames {
            completed: completed.max(previous_completed),
            total,
        },
        (
            ExportProgressDetail::MediaTimeMicros {
                completed: previous_completed,
                total: previous_total,
            },
            ExportProgressDetail::MediaTimeMicros { completed, total },
        ) if previous_total == total => ExportProgressDetail::MediaTimeMicros {
            completed: completed.max(previous_completed),
            total,
        },
        (ExportProgressDetail::None, sanitized) => sanitized,
        (previous, _) => previous,
    }
}

impl ExportProgressPhase {
    const fn rank(self) -> u8 {
        match self {
            Self::Preparing => 0,
            Self::Rendering => 1,
            Self::Encoding => 2,
            Self::Validating => 3,
            Self::Publishing => 4,
        }
    }
}

impl Default for ExportProgress {
    fn default() -> Self {
        Self::preparing(0.0)
    }
}

/// Structured failure category for an admitted export attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFailureReason {
    /// The prepared render/encode/validation execution failed.
    ExecutionFailed,
    /// The executor panicked and was isolated at the queue boundary.
    ExecutorPanicked,
}

/// Bounded structured failure retained in lightweight job history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFailure {
    /// Stable machine-readable failure category.
    pub reason: ExportFailureReason,
    /// Bounded operator-facing detail.
    pub detail: String,
}

impl ExportFailure {
    fn execution(detail: impl Into<String>) -> Self {
        Self {
            reason: ExportFailureReason::ExecutionFailed,
            detail: bounded_detail(detail.into()),
        }
    }

    fn panic() -> Self {
        Self {
            reason: ExportFailureReason::ExecutorPanicked,
            detail: "export executor panicked; the attempt was isolated".to_owned(),
        }
    }
}

impl std::fmt::Display for ExportFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

/// Observable lifecycle state for one admitted export job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum JobStatus {
    /// Admitted and waiting for the dedicated offline worker.
    Pending,
    /// Executing the reported production phase.
    Running { phase: ExportProgressPhase },
    /// Cancellation was requested and is awaiting a cooperative checkpoint.
    Cancelling { phase: ExportProgressPhase },
    /// Validated deliverable was published successfully.
    Completed,
    /// Attempt failed with structured detail.
    Failed(ExportFailure),
    /// Attempt ended after cancellation authority was asserted.
    Cancelled,
}

impl JobStatus {
    /// Whether this state is terminal.
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed(_) | Self::Cancelled)
    }

    /// Whether a new cancellation request is meaningful.
    pub const fn can_cancel(&self) -> bool {
        matches!(self, Self::Pending | Self::Running { .. })
    }
}

/// Immutable heavy submission consumed exactly once by the export worker.
#[derive(Debug)]
pub struct RenderJob {
    id: JobId,
    pub(crate) config: ExportConfig,
    created_at: DateTime<Utc>,
}

impl RenderJob {
    /// Create one immutable export submission.
    pub fn new(config: ExportConfig) -> Self {
        Self { id: JobId::new(), config, created_at: Utc::now() }
    }

    /// Stable identity assigned before admission.
    pub const fn id(&self) -> JobId {
        self.id
    }
}

/// Lightweight queue snapshot safe to clone every UI or Headless observation tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportJobSnapshot {
    /// Stable job identity.
    pub id: JobId,
    /// Module-local monotonic attempt generation.
    pub generation: u64,
    /// Final output path, without retaining the heavy timeline payload.
    pub output_path: PathBuf,
    /// Human-readable preset identity captured at admission.
    pub preset_name: String,
    /// Current lifecycle state.
    pub status: JobStatus,
    /// Latest monotonic progress.
    pub progress: ExportProgress,
    /// Bounded execution diagnostics.
    pub diagnostics: ExportJobDiagnostics,
    /// Admission timestamp.
    pub created_at: DateTime<Utc>,
    /// Worker-dispatch timestamp.
    pub started_at: Option<DateTime<Utc>>,
    /// Authoritative terminal timestamp.
    pub completed_at: Option<DateTime<Utc>>,
    /// Shared terminal evidence, present only after completion.
    pub terminal_evidence: Option<ExecutionTerminalEvidence>,
    /// Whether this attempt crossed the worker execution boundary.
    pub executed: bool,
}

/// Structured reason why a submission was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportAdmissionError {
    /// The bounded in-flight budget is exhausted.
    CapacityExceeded { capacity: usize },
    /// Another active job owns the same normalized output path.
    OutputPathBusy { path: PathBuf },
    /// The supplied final output path cannot name a deliverable.
    InvalidOutputPath { path: PathBuf },
    /// The dedicated worker could not be started.
    WorkerUnavailable { detail: String },
    /// The queue can no longer issue a unique monotonic attempt generation.
    GenerationExhausted,
}

impl std::fmt::Display for ExportAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityExceeded { capacity } => {
                write!(
                    formatter,
                    "export in-flight capacity {capacity} is exhausted"
                )
            }
            Self::OutputPathBusy { path } => {
                write!(formatter, "another active export owns {}", path.display())
            }
            Self::InvalidOutputPath { path } => {
                write!(formatter, "invalid export output path {}", path.display())
            }
            Self::WorkerUnavailable { detail } => formatter.write_str(detail),
            Self::GenerationExhausted => {
                formatter.write_str("export attempt generation space is exhausted")
            }
        }
    }
}

impl std::error::Error for ExportAdmissionError {}

/// Result of requesting cancellation for a queue identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportCancelOutcome {
    /// A live attempt accepted the cancellation request.
    Requested,
    /// Cancellation was already requested.
    AlreadyRequested,
    /// The identity exists but is already terminal.
    AlreadyTerminal,
    /// No retained job has this identity.
    NotFound,
}

/// Bounded Headless diagnostics for the offline export Module.
#[derive(Debug, Clone, Default)]
pub struct ExportQueueDiagnostics {
    /// Monotonic state revision.
    pub revision: u64,
    /// Jobs waiting for the worker.
    pub pending: usize,
    /// Jobs executing normally.
    pub running: usize,
    /// Jobs awaiting cooperative cancellation.
    pub cancelling: usize,
    /// Lightweight terminal history retained.
    pub terminal: usize,
    /// Successful admissions since queue creation.
    pub admissions: u64,
    /// Rejected admissions since queue creation.
    pub rejections: u64,
    /// Accepted cancellation requests.
    pub cancellation_requests: u64,
    /// Successful publications.
    pub completions: u64,
    /// Failed admitted attempts.
    pub failures: u64,
    /// Canceled admitted attempts.
    pub cancellations: u64,
    /// Current lightweight job snapshots in admission order.
    pub jobs: Vec<ExportJobSnapshot>,
}

#[derive(Debug, Default)]
struct ExportQueueCounters {
    admissions: u64,
    rejections: u64,
    cancellation_requests: u64,
    completions: u64,
    failures: u64,
    cancellations: u64,
}

struct ExportJobEntry {
    snapshot: ExportJobSnapshot,
    payload: Option<RenderJob>,
    cancellation: ExecutionCancellationToken,
    output_key: String,
}

#[derive(Default)]
struct ExportQueueState {
    jobs: VecDeque<ExportJobEntry>,
    next_generation: u64,
    worker_failure: Option<String>,
    counters: ExportQueueCounters,
}

struct RenderQueueInner {
    state: Mutex<ExportQueueState>,
    wake: Condvar,
    shutdown: AtomicBool,
    revision: AtomicU64,
}

/// Instance-owned bounded offline export queue.
pub struct RenderQueue {
    inner: Arc<RenderQueueInner>,
}

impl RenderQueue {
    /// Create a queue with the production executor and one dedicated offline worker.
    pub fn new() -> Arc<Self> {
        Self::new_with_executor(Arc::new(super::FfmpegExportExecutor))
    }

    pub(crate) fn new_with_executor(executor: Arc<dyn ExportExecutor>) -> Arc<Self> {
        let queue = Arc::new(Self {
            inner: Arc::new(RenderQueueInner {
                state: Mutex::new(ExportQueueState {
                    next_generation: 1,
                    ..ExportQueueState::default()
                }),
                wake: Condvar::new(),
                shutdown: AtomicBool::new(false),
                revision: AtomicU64::new(1),
            }),
        });
        queue.spawn_worker(executor);
        queue
    }

    fn spawn_worker(&self, executor: Arc<dyn ExportExecutor>) {
        let inner = Arc::clone(&self.inner);
        if let Err(error) = std::thread::Builder::new()
            .name("mondrian-export-worker".to_owned())
            .spawn(move || export_worker_loop(inner, executor))
        {
            let mut state = self.inner.state.lock();
            state.worker_failure = Some(format!("failed to start export worker: {error}"));
            drop(state);
            self.mark_changed();
        }
    }

    /// Admit a heavy immutable submission or return a structured rejection.
    pub fn enqueue(&self, job: RenderJob) -> Result<JobId, ExportAdmissionError> {
        let output_path = job.config.output_path.clone();
        let Some(output_key) =
            output_reservation_key(&output_path).filter(|_| !output_path.is_dir())
        else {
            return self.reject(ExportAdmissionError::InvalidOutputPath { path: output_path });
        };

        let mut state = self.inner.state.lock();
        if let Some(detail) = &state.worker_failure {
            let error = ExportAdmissionError::WorkerUnavailable { detail: detail.clone() };
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            drop(state);
            self.mark_changed();
            return Err(error);
        }
        let in_flight =
            state.jobs.iter().filter(|entry| !entry.snapshot.status.is_terminal()).count();
        if in_flight >= EXPORT_IN_FLIGHT_CAPACITY {
            let error =
                ExportAdmissionError::CapacityExceeded { capacity: EXPORT_IN_FLIGHT_CAPACITY };
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            drop(state);
            self.mark_changed();
            return Err(error);
        }
        if state
            .jobs
            .iter()
            .any(|entry| !entry.snapshot.status.is_terminal() && entry.output_key == output_key)
        {
            let error = ExportAdmissionError::OutputPathBusy { path: output_path };
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            drop(state);
            self.mark_changed();
            return Err(error);
        }

        let generation = state.next_generation.max(1);
        let Some(next_generation) = generation.checked_add(1) else {
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            drop(state);
            self.mark_changed();
            return Err(ExportAdmissionError::GenerationExhausted);
        };
        state.next_generation = next_generation;
        let id = job.id;
        let snapshot = ExportJobSnapshot {
            id,
            generation,
            output_path: job.config.output_path.clone(),
            preset_name: job.config.preset.name.clone(),
            status: JobStatus::Pending,
            progress: ExportProgress::default(),
            diagnostics: ExportJobDiagnostics::default(),
            created_at: job.created_at,
            started_at: None,
            completed_at: None,
            terminal_evidence: None,
            executed: false,
        };
        state.jobs.push_back(ExportJobEntry {
            snapshot,
            payload: Some(job),
            cancellation: ExecutionCancellationToken::new(),
            output_key,
        });
        state.counters.admissions = state.counters.admissions.saturating_add(1);
        drop(state);
        self.mark_changed();
        self.inner.wake.notify_one();
        Ok(id)
    }

    fn reject<T>(&self, error: ExportAdmissionError) -> Result<T, ExportAdmissionError> {
        let mut state = self.inner.state.lock();
        state.counters.rejections = state.counters.rejections.saturating_add(1);
        drop(state);
        self.mark_changed();
        Err(error)
    }

    /// Return lightweight snapshots without cloning any timeline or media payload.
    pub fn list_jobs(&self) -> Vec<ExportJobSnapshot> {
        self.inner
            .state
            .lock()
            .jobs
            .iter()
            .map(|entry| entry.snapshot.clone())
            .collect()
    }

    /// Request monotonic cooperative cancellation.
    pub fn cancel(&self, id: JobId) -> ExportCancelOutcome {
        let mut state = self.inner.state.lock();
        let Some(index) = state.jobs.iter().position(|entry| entry.snapshot.id == id) else {
            return ExportCancelOutcome::NotFound;
        };
        let outcome = match state.jobs[index].snapshot.status.clone() {
            JobStatus::Pending => {
                let entry = &mut state.jobs[index];
                entry.cancellation.cancel();
                entry.payload = None;
                entry.snapshot.status = JobStatus::Cancelled;
                entry.snapshot.completed_at = Some(Utc::now());
                entry.snapshot.terminal_evidence = Some(ExecutionTerminalEvidence {
                    generation: entry.snapshot.generation,
                    priority: ExecutionPriority::UserInitiated,
                    disposition: ExecutionTerminalDisposition::Canceled,
                    deadline: ExecutionDeadlineStatus::NotApplicable,
                });
                state.counters.cancellation_requests =
                    state.counters.cancellation_requests.saturating_add(1);
                state.counters.cancellations = state.counters.cancellations.saturating_add(1);
                trim_terminal_history(&mut state);
                ExportCancelOutcome::Requested
            }
            JobStatus::Running { phase } => {
                let entry = &mut state.jobs[index];
                entry.cancellation.cancel();
                entry.snapshot.status = JobStatus::Cancelling { phase };
                state.counters.cancellation_requests =
                    state.counters.cancellation_requests.saturating_add(1);
                ExportCancelOutcome::Requested
            }
            JobStatus::Cancelling { .. } => ExportCancelOutcome::AlreadyRequested,
            JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled => {
                ExportCancelOutcome::AlreadyTerminal
            }
        };
        drop(state);
        if outcome == ExportCancelOutcome::Requested {
            self.mark_changed();
            self.inner.wake.notify_all();
        }
        outcome
    }

    /// Remove all retained terminal snapshots. Active payloads are never affected.
    pub fn clear_completed(&self) {
        let mut state = self.inner.state.lock();
        let before = state.jobs.len();
        state.jobs.retain(|entry| !entry.snapshot.status.is_terminal());
        let changed = state.jobs.len() != before;
        drop(state);
        if changed {
            self.mark_changed();
        }
    }

    /// Current monotonic observation revision.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    /// Snapshot bounded queue health and lightweight job evidence.
    pub fn diagnostics(&self) -> ExportQueueDiagnostics {
        let state = self.inner.state.lock();
        let mut diagnostics = ExportQueueDiagnostics {
            revision: self.revision(),
            admissions: state.counters.admissions,
            rejections: state.counters.rejections,
            cancellation_requests: state.counters.cancellation_requests,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            jobs: state.jobs.iter().map(|entry| entry.snapshot.clone()).collect(),
            ..ExportQueueDiagnostics::default()
        };
        for entry in &state.jobs {
            match entry.snapshot.status {
                JobStatus::Pending => diagnostics.pending += 1,
                JobStatus::Running { .. } => diagnostics.running += 1,
                JobStatus::Cancelling { .. } => diagnostics.cancelling += 1,
                JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled => {
                    diagnostics.terminal += 1;
                }
            }
        }
        diagnostics
    }

    fn mark_changed(&self) {
        self.inner.revision.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for RenderQueue {
    fn drop(&mut self) {
        self.inner.shutdown.store(true, Ordering::Release);
        let state = self.inner.state.lock();
        for entry in &state.jobs {
            if !entry.snapshot.status.is_terminal() {
                entry.cancellation.cancel();
            }
        }
        drop(state);
        self.inner.wake.notify_all();
    }
}

struct ExportWork {
    job: RenderJob,
    generation: u64,
    cancellation: ExecutionCancellationToken,
}

fn export_worker_loop(inner: Arc<RenderQueueInner>, executor: Arc<dyn ExportExecutor>) {
    while let Some(work) = take_next_pending_job(&inner) {
        let job_id = work.job.id;
        let generation = work.generation;
        let report_inner = Arc::clone(&inner);
        let mut report = move |progress: ExportProgress| {
            update_job_progress(&report_inner, job_id, generation, progress);
        };
        let diagnostics_inner = Arc::clone(&inner);
        let mut report_diagnostics = move |diagnostics: ExportJobDiagnostics| {
            update_job_diagnostics(&diagnostics_inner, job_id, generation, diagnostics);
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor.execute(
                &work.job,
                &work.cancellation,
                &mut report,
                &mut report_diagnostics,
            )
        }))
        .map(ExportWorkerOutcome::Execution)
        .unwrap_or(ExportWorkerOutcome::Panicked);
        publish_terminal(&inner, job_id, generation, outcome);
    }
}

enum ExportWorkerOutcome {
    Execution(JobExecutionResult),
    Panicked,
}

fn take_next_pending_job(inner: &RenderQueueInner) -> Option<ExportWork> {
    let mut state = inner.state.lock();
    loop {
        if inner.shutdown.load(Ordering::Acquire) {
            return None;
        }
        if let Some(index) = state
            .jobs
            .iter()
            .position(|entry| matches!(entry.snapshot.status, JobStatus::Pending))
        {
            let Some(job) = state.jobs[index].payload.take() else {
                let generation = state.jobs[index].snapshot.generation;
                let entry = &mut state.jobs[index];
                entry.snapshot.status = JobStatus::Failed(ExportFailure::execution(
                    "admitted export payload was unavailable at dispatch",
                ));
                entry.snapshot.completed_at = Some(Utc::now());
                entry.snapshot.terminal_evidence = Some(ExecutionTerminalEvidence {
                    generation,
                    priority: ExecutionPriority::UserInitiated,
                    disposition: ExecutionTerminalDisposition::Failed,
                    deadline: ExecutionDeadlineStatus::NotApplicable,
                });
                state.counters.failures = state.counters.failures.saturating_add(1);
                trim_terminal_history(&mut state);
                inner.revision.fetch_add(1, Ordering::AcqRel);
                continue;
            };
            let entry = &mut state.jobs[index];
            entry.snapshot.status = JobStatus::Running { phase: ExportProgressPhase::Preparing };
            entry.snapshot.started_at = Some(Utc::now());
            entry.snapshot.executed = true;
            let work = ExportWork {
                job,
                generation: entry.snapshot.generation,
                cancellation: entry.cancellation.clone(),
            };
            drop(state);
            inner.revision.fetch_add(1, Ordering::AcqRel);
            return Some(work);
        }
        inner.wake.wait(&mut state);
    }
}

fn update_job_progress(
    inner: &RenderQueueInner,
    job_id: JobId,
    generation: u64,
    progress: ExportProgress,
) {
    let mut state = inner.state.lock();
    let Some(entry) = state
        .jobs
        .iter_mut()
        .find(|entry| entry.snapshot.id == job_id && entry.snapshot.generation == generation)
    else {
        return;
    };
    if !matches!(entry.snapshot.status, JobStatus::Running { .. }) {
        return;
    }
    if progress.phase.rank() < entry.snapshot.progress.phase.rank() {
        return;
    }
    let progress = progress.normalized(entry.snapshot.progress);
    entry.snapshot.progress = progress;
    entry.snapshot.status = JobStatus::Running { phase: progress.phase };
    drop(state);
    inner.revision.fetch_add(1, Ordering::AcqRel);
}

fn update_job_diagnostics(
    inner: &RenderQueueInner,
    job_id: JobId,
    generation: u64,
    diagnostics: ExportJobDiagnostics,
) {
    let mut state = inner.state.lock();
    let Some(entry) = state
        .jobs
        .iter_mut()
        .find(|entry| entry.snapshot.id == job_id && entry.snapshot.generation == generation)
    else {
        return;
    };
    if entry.snapshot.status.is_terminal() {
        return;
    }
    entry.snapshot.diagnostics = diagnostics;
    drop(state);
    inner.revision.fetch_add(1, Ordering::AcqRel);
}

fn publish_terminal(
    inner: &RenderQueueInner,
    job_id: JobId,
    generation: u64,
    outcome: ExportWorkerOutcome,
) {
    let mut state = inner.state.lock();
    let Some(index) = state
        .jobs
        .iter()
        .position(|entry| entry.snapshot.id == job_id && entry.snapshot.generation == generation)
    else {
        return;
    };
    if state.jobs[index].snapshot.status.is_terminal() {
        return;
    }
    let (status, disposition) = match outcome {
        ExportWorkerOutcome::Execution(JobExecutionResult::Completed) => {
            state.counters.completions = state.counters.completions.saturating_add(1);
            (
                JobStatus::Completed,
                ExecutionTerminalDisposition::Completed,
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::Failed(detail)) => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::execution(detail)),
                ExecutionTerminalDisposition::Failed,
            )
        }
        ExportWorkerOutcome::Panicked => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::panic()),
                ExecutionTerminalDisposition::Failed,
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::Cancelled) => {
            state.counters.cancellations = state.counters.cancellations.saturating_add(1);
            (JobStatus::Cancelled, ExecutionTerminalDisposition::Canceled)
        }
    };
    let entry = &mut state.jobs[index];
    if matches!(status, JobStatus::Completed) {
        entry.snapshot.progress = ExportProgress::publishing(1.0);
    }
    entry.snapshot.status = status;
    entry.snapshot.completed_at = Some(Utc::now());
    entry.snapshot.terminal_evidence = Some(ExecutionTerminalEvidence {
        generation,
        priority: ExecutionPriority::UserInitiated,
        disposition,
        deadline: ExecutionDeadlineStatus::NotApplicable,
    });
    trim_terminal_history(&mut state);
    drop(state);
    inner.revision.fetch_add(1, Ordering::AcqRel);
    inner.wake.notify_all();
}

fn trim_terminal_history(state: &mut ExportQueueState) {
    while state.jobs.iter().filter(|entry| entry.snapshot.status.is_terminal()).count()
        > EXPORT_TERMINAL_HISTORY_CAPACITY
    {
        let Some(index) = state.jobs.iter().position(|entry| entry.snapshot.status.is_terminal())
        else {
            break;
        };
        state.jobs.remove(index);
    }
}

fn output_reservation_key(path: &Path) -> Option<String> {
    if path.as_os_str().is_empty() || path.file_name().is_none() {
        return None;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let normalized = if normalized.exists() {
        std::fs::canonicalize(&normalized).unwrap_or(normalized)
    } else if let (Some(parent), Some(file_name)) = (normalized.parent(), normalized.file_name()) {
        std::fs::canonicalize(parent)
            .map(|canonical_parent| canonical_parent.join(file_name))
            .unwrap_or(normalized)
    } else {
        normalized
    };
    let key = normalized.to_string_lossy().replace('/', "\\");
    Some(if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    })
}

fn bounded_detail(detail: String) -> String {
    let mut chars = detail.chars();
    let bounded = chars.by_ref().take(EXPORT_FAILURE_DETAIL_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

#[cfg(test)]
mod tests;
