//! Bounded offline export admission, lifecycle, cancellation, and evidence.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use mondrian_audio::AudioRuntimeResourceGrant;
use mondrian_core::{
    ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence, JobId,
};
use mondrian_media::AudioSourceCacheConfig;
use mondrian_renderer::{
    GpuVisualFrameExecutionResourceGrant, RenderGpuOutputExecutionResourceGrant,
    TimelineCpuWorkingSetGrant,
};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};

use crate::preset::{ExportConfig, ExportOutputPolicy};
use crate::{
    prepare_timeline_export_dependencies_with_audio_selection,
    validate_timeline_export_execution_snapshot_with_audio_selection,
};

use super::{
    ExportExecutionOwnerEvent, ExportExecutor, ExportJobDiagnostics, ExportPublicationFailure,
    JobExecutionResult,
};

/// Maximum number of admitted jobs that may be pending or executing.
pub const EXPORT_IN_FLIGHT_CAPACITY: usize = 64;
/// Maximum number of lightweight terminal snapshots retained without user cleanup.
pub const EXPORT_TERMINAL_HISTORY_CAPACITY: usize = 256;
/// Conservative logical charge of one immutable heterogeneous route contract.
pub const EXPORT_HETEROGENEOUS_ROUTE_CONTRACT_LOGICAL_BYTES: usize = 128;
const EXPORT_FAILURE_DETAIL_CHARS: usize = 4_096;

/// Immutable resource grant frozen when one Export attempt starts.
///
/// The queue owns policy publication; the executor snapshots it exactly once
/// after crossing the Preparing gate. Preview resources are never borrowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportExecutionResourcePolicy {
    /// Maximum prepared Sequence visual programs in the frozen reachable closure.
    pub visual_program_entries: usize,
    /// Maximum aggregate conservative logical bytes for that visual closure.
    pub visual_program_bytes: usize,
    /// Maximum prepared LUT resources retained by this visual attempt.
    pub lut_cache_entries: usize,
    /// Maximum conservative logical bytes retained by prepared LUT resources.
    pub lut_cache_bytes: usize,
    /// Maximum retained Effect pixel/node entries.
    pub effect_cache_entries: usize,
    /// Aggregate retained Effect pixel/node bytes.
    pub effect_cache_bytes: usize,
    /// Maximum transient Temporal/ROI working bytes for one frame.
    pub effect_working_bytes: usize,
    /// Hard compositor-owned transient and retained working-set grant.
    ///
    /// This is frozen per attempt and does not shrink with cache pressure.
    pub cpu_composite_working_set: TimelineCpuWorkingSetGrant,
    /// Maximum retained GPU Effect plans or deterministic blockers.
    pub effect_gpu_plan_entries: usize,
    /// Conservative retained bytes for GPU Effect planning.
    pub effect_gpu_plan_bytes: usize,
    /// Maximum immutable heterogeneous route contracts frozen by preflight.
    ///
    /// This is a correctness/admission grant and must not shrink with cache
    /// pressure after an Export snapshot has been accepted.
    pub heterogeneous_route_contract_entries: usize,
    /// Maximum logical bytes for the immutable heterogeneous route ledger.
    ///
    /// This is independent of retained GPU-plan cache bytes.
    pub heterogeneous_route_contract_bytes: usize,
    /// Maximum OCIO CPU processors retained by this job.
    pub cpu_color_processor_capacity: usize,
    /// Maximum idle GPU output textures retained per exact contract.
    pub gpu_output_idle_per_contract: usize,
    /// Aggregate approximate idle GPU output texture bytes.
    pub gpu_output_idle_bytes: u64,
    /// Hard active texture grant for the complete GPU visual closure.
    ///
    /// Existing nested outputs and every new upload/Effect/Transition/composite
    /// texture are admitted together before each node records.
    pub gpu_visual_active: GpuVisualFrameExecutionResourceGrant,
    /// Hard active texture/readback grant for one final GPU output boundary.
    ///
    /// Unlike idle retention, this grant is frozen for the accepted Export
    /// attempt and must not shrink in response to online memory pressure.
    pub gpu_output_active: RenderGpuOutputExecutionResourceGrant,
    /// Maximum FFmpeg-owned NV12/P010 encoder surfaces in one resident Session.
    pub resident_encoder_surfaces: u32,
    /// Hard logical byte grant for the complete resident encoder surface pool.
    pub resident_encoder_surface_bytes: u64,
    /// Maximum retained Basic Title raster identities.
    pub title_cache_entries: usize,
    /// Aggregate Basic Title frame and glyph cache bytes.
    pub title_cache_bytes: usize,
    /// Aggregate byte-frozen font-source bytes admitted for Basic Titles.
    pub title_font_bytes: usize,
    /// Job-local decoded-audio source cache residency.
    pub audio_source_cache: AudioSourceCacheConfig,
    /// Closure-wide hard grant for the immutable attempt's Audio Runtime.
    pub audio_runtime_grant: AudioRuntimeResourceGrant,
}

impl Default for ExportExecutionResourcePolicy {
    fn default() -> Self {
        Self {
            visual_program_entries: 32,
            visual_program_bytes: 64 * 1024 * 1024,
            lut_cache_entries: 8,
            lut_cache_bytes: 32 * 1024 * 1024,
            effect_cache_entries: 32,
            effect_cache_bytes: 96 * 1024 * 1024,
            effect_working_bytes: 384 * 1024 * 1024,
            cpu_composite_working_set: TimelineCpuWorkingSetGrant {
                max_active_bytes: 1024 * 1024 * 1024,
                max_retained_scratch_bytes: 512 * 1024 * 1024,
            },
            effect_gpu_plan_entries: 32,
            effect_gpu_plan_bytes: 2 * 1024 * 1024,
            heterogeneous_route_contract_entries: 32,
            heterogeneous_route_contract_bytes: 4 * 1024,
            cpu_color_processor_capacity: 32,
            gpu_output_idle_per_contract: 1,
            gpu_output_idle_bytes: 96 * 1024 * 1024,
            gpu_visual_active: GpuVisualFrameExecutionResourceGrant::new(
                2 * 1024 * 1024 * 1024,
                128,
            ),
            gpu_output_active: RenderGpuOutputExecutionResourceGrant::new(1024 * 1024 * 1024, 4),
            resident_encoder_surfaces: 8,
            resident_encoder_surface_bytes: 512 * 1024 * 1024,
            title_cache_entries: 16,
            title_cache_bytes: 64 * 1024 * 1024,
            title_font_bytes: 128 * 1024 * 1024,
            audio_source_cache: AudioSourceCacheConfig::new(16, 64 * 1024 * 1024, 2),
            audio_runtime_grant: AudioRuntimeResourceGrant::new(
                64,
                768 * 1024 * 1024,
                128 * 1024 * 1024,
            ),
        }
    }
}

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
    /// Wrap encoded essence and construct a delivery package.
    Packaging,
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

    pub(crate) const fn packaging(fraction: f32) -> Self {
        Self {
            phase: ExportProgressPhase::Packaging,
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
            Self::Packaging => 3,
            Self::Validating => 4,
            Self::Publishing => 5,
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
    /// Publication failed before any irreversible namespace operation was
    /// observed.
    PublicationBeforeNamespace,
    /// The final path names the new deliverable, but containing-directory
    /// crash durability could not be confirmed.
    PublicationDurabilityUnconfirmed,
    /// Publication crossed the commit gate but the final namespace
    /// postcondition could not be proven.
    PublicationNamespaceIndeterminate,
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

    fn publication(reason: ExportFailureReason, detail: impl Into<String>) -> Self {
        debug_assert!(matches!(
            reason,
            ExportFailureReason::PublicationBeforeNamespace
                | ExportFailureReason::PublicationDurabilityUnconfirmed
                | ExportFailureReason::PublicationNamespaceIndeterminate
        ));
        Self { reason, detail: bounded_detail(detail.into()) }
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
        matches!(self, Self::Pending)
            || matches!(
                self,
                Self::Running {
                    phase: ExportProgressPhase::Preparing
                        | ExportProgressPhase::Rendering
                        | ExportProgressPhase::Encoding
                        | ExportProgressPhase::Packaging
                        | ExportProgressPhase::Validating
                }
            )
    }
}

/// Publication authority for one exact export attempt.
///
/// `Committing` is the queue-locked irreversible boundary. Cancellation and
/// resource-yield requests arriving in that state are explicitly too late;
/// they must not mutate the shared cancellation token or claim success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPublicationState {
    /// The attempt has not crossed atomic deliverable publication.
    #[default]
    Reversible,
    /// The attempt owns the irreversible publication section.
    Committing,
    /// The validated deliverable was published.
    Published,
    /// The target names the validated deliverable, but directory durability
    /// was not confirmed. This is not a successful publication and requires
    /// operator-visible recovery before retry.
    DurabilityUnconfirmed,
    /// The attempt ended before publication.
    NotPublished,
    /// Execution failed after committing publication authority, so a final
    /// artifact observation is required before retry or cleanup.
    OutcomeUnknown,
}

/// Typed terminal evidence for the deliverable namespace of one exact export
/// attempt.
///
/// Paths are absolute routes frozen at queue admission. `Durable` is the only
/// variant that authorizes `JobStatus::Completed` and
/// `ExportPublicationState::Published`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ExportArtifactPublicationEvidence {
    /// The validated deliverable is durably published at the admitted route.
    Durable {
        /// Absolute final output route.
        output_path: PathBuf,
    },
    /// No irreversible namespace operation is known to have completed.
    BeforeNamespace {
        /// Absolute intended final output route.
        output_path: PathBuf,
        /// Exact validated partial object retained for diagnosis or retry.
        retained_partial_path: Option<PathBuf>,
    },
    /// The final route names the new object, but directory crash durability is
    /// unconfirmed.
    DurabilityUnconfirmed {
        /// Absolute final output route.
        output_path: PathBuf,
    },
    /// The final namespace postcondition cannot be proven.
    NamespaceIndeterminate {
        /// Absolute intended final output route.
        output_path: PathBuf,
        /// Verified surviving route to the new bytes, when one was observed.
        retained_partial_path: Option<PathBuf>,
    },
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
    /// Final namespace policy frozen at admission.
    pub output_policy: ExportOutputPolicy,
    /// Human-readable preset identity captured at admission.
    pub preset_name: String,
    /// Current lifecycle state.
    pub status: JobStatus,
    /// Latest monotonic progress.
    pub progress: ExportProgress,
    /// Typed publication authority and terminal outcome.
    pub publication: ExportPublicationState,
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
    /// Deliverable publication evidence, present after an attempted
    /// publication reaches a typed terminal result.
    pub artifact_publication: Option<ExportArtifactPublicationEvidence>,
    /// Whether this attempt crossed the worker execution boundary.
    pub executed: bool,
}

/// Structured reason why a submission was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportAdmissionError {
    /// Preset and immutable Sequence delivery intent cannot form a legal output.
    InvalidDelivery { detail: String },
    /// The bounded in-flight budget is exhausted.
    CapacityExceeded { capacity: usize },
    /// Another active job owns the same normalized output path.
    OutputPathBusy { path: PathBuf },
    /// The supplied final output path cannot name a deliverable.
    InvalidOutputPath { path: PathBuf },
    /// Create-only publication was requested for an already occupied route.
    OutputAlreadyExists { path: PathBuf },
    /// The dedicated worker could not be started.
    WorkerUnavailable { detail: String },
    /// The queue can no longer issue a unique monotonic attempt generation.
    GenerationExhausted,
    /// The queue has entered its permanent shutdown state.
    QueueShutdown,
}

impl std::fmt::Display for ExportAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDelivery { detail } => formatter.write_str(detail),
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
            Self::OutputAlreadyExists { path } => {
                write!(
                    formatter,
                    "export output already exists: {}",
                    path.display()
                )
            }
            Self::WorkerUnavailable { detail } => formatter.write_str(detail),
            Self::GenerationExhausted => {
                formatter.write_str("export attempt generation space is exhausted")
            }
            Self::QueueShutdown => formatter.write_str("export queue has shut down"),
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
    /// Publication already crossed the irreversible queue-locked boundary.
    TooLateCommitting,
    /// The identity exists but is already terminal.
    AlreadyTerminal,
    /// No retained job has this identity.
    NotFound,
}

/// Bounded Headless diagnostics for the offline export Module.
#[derive(Debug, Clone, Default)]
pub struct ExportQueueDiagnostics {
    /// Wrapping observation token for all queue state exposed here.
    ///
    /// Consumers compare this token only for equality; it is not a job
    /// generation, event count, or linearizable snapshot version.
    pub revision: u64,
    /// Bounded worker-start failure, if the queue could not create its executor.
    pub worker_failure: Option<String>,
    /// Whether pending dispatch and running safe-boundary execution are admitted.
    pub dispatch_enabled: bool,
    /// Resource grant that the next dispatched attempt will freeze.
    pub resource_policy: ExportExecutionResourcePolicy,
    /// Whether running attempts have been asked to yield at their next safe boundary.
    pub running_yield_requested: bool,
    /// Running or cancelling attempts currently blocked at a safe execution boundary.
    pub running_yielded: usize,
    /// Jobs waiting for the worker.
    pub pending: usize,
    /// Jobs executing normally.
    pub running: usize,
    /// Jobs awaiting cooperative cancellation.
    pub cancelling: usize,
    /// Jobs inside the irreversible publication section.
    pub committing: usize,
    /// Lightweight terminal history retained.
    pub terminal: usize,
    /// Successful admissions since queue creation.
    pub admissions: u64,
    /// Rejected admissions since queue creation.
    pub rejections: u64,
    /// Accepted cancellation requests.
    pub cancellation_requests: u64,
    /// Cancellation requests rejected at the irreversible publication boundary.
    pub too_late_cancellation_requests: u64,
    /// Successful publications.
    pub completions: u64,
    /// Failed admitted attempts.
    pub failures: u64,
    /// Canceled admitted attempts.
    pub cancellations: u64,
    /// Timeline frames accepted through monotonic job progress.
    pub rendered_frames: u64,
    /// Completed jobs carrying durable artifact publication evidence.
    pub durable_artifacts: u64,
    /// Current lightweight job snapshots in admission order.
    pub jobs: Vec<ExportJobSnapshot>,
}

/// Fixed-size long-duration observation of the Export queue.
///
/// Heavy job payloads and bounded terminal history never enter this snapshot.
/// A qualification Adapter samples it on its own cadence and maps the fields
/// into the platform-neutral endurance contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportEnduranceSnapshot {
    /// Snapshot schema version.
    pub schema_version: u32,
    /// Monotonic observation offset owned by the capture Adapter.
    pub observed_at_us: u64,
    /// Whether shutdown was requested.
    pub shutdown_requested: bool,
    /// Whether the dedicated worker is currently alive.
    pub worker_running: bool,
    /// Whether the dedicated worker returned normally and its handle was joined.
    pub worker_terminated: bool,
    /// Monotonic accepted queue/job activity count.
    pub activity_events: u64,
    /// Successful admissions.
    pub admissions: u64,
    /// Rejected admissions.
    pub rejections: u64,
    /// Successful durable publications.
    pub completions: u64,
    /// Failed admitted attempts.
    pub failures: u64,
    /// Canceled admitted attempts.
    pub cancellations: u64,
    /// Frames truthfully reported by retained job progress.
    pub rendered_frames: u64,
    /// Completed jobs carrying durable artifact evidence.
    pub durable_artifacts: u64,
    /// Pending jobs.
    pub pending_jobs: u64,
    /// Running, cancelling, or committing jobs.
    pub active_jobs: u64,
    /// Whether worker startup failed.
    pub worker_failed: bool,
    /// Job-scoped decoded-audio source owners created since Queue start.
    pub audio_source_owners_started: u64,
    /// Job-scoped decoded-audio source owners closed since Queue start.
    pub audio_source_owners_closed: u64,
    /// Audio-source closures that retained or abandoned resources.
    pub audio_source_owner_failures: u64,
    /// Audio-source owners currently live inside an executing job.
    pub active_audio_source_owners: u64,
}

/// Terminal evidence returned by explicit queue retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportQueueShutdownEvidence {
    /// Shutdown evidence schema version.
    pub schema_version: u32,
    /// Whether the dedicated worker entered its owned execution closure.
    pub worker_started: bool,
    /// Whether creating the dedicated worker failed before it could start.
    pub worker_start_failed: bool,
    /// Whether the worker handle was joined after a normal return.
    pub worker_terminated: bool,
    /// Whether the worker's terminal guard or joined handle observed a panic
    /// outside the per-Job executor panic boundary.
    pub worker_panicked: bool,
    /// Whether the worker missed the caller's absolute shutdown deadline.
    ///
    /// A worker that had already finished after the deadline can still be
    /// joined and therefore need not be detached.
    pub worker_timed_out: bool,
    /// Whether the Queue relinquished a still-running worker handle.
    pub worker_detached: bool,
    /// Whether a worker-owned executor, hook, factory error, or opaque panic
    /// payload had to be abandoned rather than destroyed on a latency-sensitive
    /// thread.
    pub worker_owner_abandoned: bool,
    /// Pending jobs remaining after the wait.
    pub pending_jobs: u64,
    /// Active jobs remaining after the wait.
    pub active_jobs: u64,
    /// Final activity-event count.
    pub activity_events: u64,
    /// Job-scoped decoded-audio source owners created over Queue lifetime.
    pub audio_source_owners_started: u64,
    /// Job-scoped decoded-audio source owners closed over Queue lifetime.
    pub audio_source_owners_closed: u64,
    /// Audio-source closures that retained or abandoned resources.
    pub audio_source_owner_failures: u64,
    /// Audio-source owners still live after Queue retirement.
    pub active_audio_source_owners: u64,
}

impl ExportQueueShutdownEvidence {
    /// Whether the queue proved that its worker and all admitted work were retired.
    ///
    /// A failed or never-observed worker start is deliberately not equivalent to
    /// normal worker termination, even when no jobs were admitted.
    #[must_use]
    pub const fn all_resources_released(&self) -> bool {
        self.schema_version == 4
            && self.worker_started
            && !self.worker_start_failed
            && self.worker_terminated
            && !self.worker_panicked
            && !self.worker_timed_out
            && !self.worker_detached
            && !self.worker_owner_abandoned
            && self.pending_jobs == 0
            && self.active_jobs == 0
            && self.audio_source_owners_started == self.audio_source_owners_closed
            && self.audio_source_owner_failures == 0
            && self.active_audio_source_owners == 0
    }
}

#[derive(Debug, Default)]
struct ExportQueueCounters {
    admissions: u64,
    rejections: u64,
    cancellation_requests: u64,
    too_late_cancellation_requests: u64,
    completions: u64,
    failures: u64,
    cancellations: u64,
    rendered_frames: u64,
    durable_artifacts: u64,
    audio_source_owners_started: u64,
    audio_source_owners_closed: u64,
    audio_source_owner_failures: u64,
}

struct ExportJobEntry {
    snapshot: ExportJobSnapshot,
    payload: Option<RenderJob>,
    cancellation: ExecutionCancellationToken,
    output_key: String,
    execution_yielded: bool,
}

#[derive(Default)]
struct ExportQueueState {
    jobs: VecDeque<ExportJobEntry>,
    next_generation: u64,
    dispatch_enabled: bool,
    resource_policy: ExportExecutionResourcePolicy,
    worker_failure: Option<String>,
    counters: ExportQueueCounters,
    activity_events: u64,
    shutdown_requested: bool,
    worker_started: bool,
    worker_start_failed: bool,
    worker_running: bool,
    worker_terminated: bool,
    worker_completed_at: Option<Instant>,
    worker_panicked: bool,
    worker_timed_out: bool,
    worker_detached: bool,
    worker_owner_abandoned: bool,
    active_audio_source_owners: u64,
}

struct RenderQueueInner {
    state: Mutex<ExportQueueState>,
    wake: Condvar,
    shutdown: AtomicBool,
    revision: AtomicU64,
    jobs_revision: AtomicU64,
}

impl RenderQueueInner {
    fn mark_diagnostics_changed_locked(&self, _state: &ExportQueueState) {
        self.revision.fetch_add(1, Ordering::AcqRel);
    }

    fn mark_jobs_changed_locked(&self, state: &mut ExportQueueState) {
        state.activity_events = state.activity_events.saturating_add(1);
        self.revision.fetch_add(1, Ordering::AcqRel);
        self.jobs_revision.fetch_add(1, Ordering::AcqRel);
    }
}

fn mark_export_worker_started(inner: &RenderQueueInner) {
    let mut state = inner.state.lock();
    state.worker_started = true;
    state.worker_running = true;
    inner.mark_diagnostics_changed_locked(&state);
    drop(state);
    inner.wake.notify_all();
}

fn mark_export_worker_completed(
    inner: &RenderQueueInner,
    completed_at: Instant,
    unwind_observed: bool,
) {
    let mut state = inner.state.lock();
    state.worker_running = false;
    state.worker_completed_at.get_or_insert(completed_at);
    if unwind_observed {
        state.worker_panicked = true;
        if state.worker_failure.is_none() {
            state.worker_failure =
                Some("export worker panicked outside the per-Job executor boundary".to_owned());
        }
    }
    inner.mark_diagnostics_changed_locked(&state);
    drop(state);
    inner.wake.notify_all();
}

fn mark_export_worker_start_failed(inner: &RenderQueueInner, detail: String) {
    let mut state = inner.state.lock();
    state.worker_running = false;
    state.worker_start_failed = true;
    state.worker_owner_abandoned = true;
    state.worker_failure = Some(detail);
    inner.mark_diagnostics_changed_locked(&state);
    drop(state);
    inner.wake.notify_all();
}

fn mark_export_worker_owner_abandoned(inner: &RenderQueueInner, detail: &str) {
    let mut state = inner.state.lock();
    state.worker_owner_abandoned = true;
    if state.worker_failure.is_none() {
        state.worker_failure = Some(detail.to_owned());
    }
    inner.mark_diagnostics_changed_locked(&state);
    drop(state);
    inner.wake.notify_all();
}

fn update_execution_owner_event(inner: &RenderQueueInner, event: ExportExecutionOwnerEvent) {
    let mut state = inner.state.lock();
    match event {
        ExportExecutionOwnerEvent::AudioSourceStarted => {
            state.counters.audio_source_owners_started =
                state.counters.audio_source_owners_started.saturating_add(1);
            state.active_audio_source_owners = state.active_audio_source_owners.saturating_add(1);
        }
        ExportExecutionOwnerEvent::AudioSourceClosed { all_resources_released } => {
            state.counters.audio_source_owners_closed =
                state.counters.audio_source_owners_closed.saturating_add(1);
            if state.active_audio_source_owners == 0 {
                state.counters.audio_source_owner_failures =
                    state.counters.audio_source_owner_failures.saturating_add(1);
            } else {
                state.active_audio_source_owners -= 1;
            }
            if !all_resources_released {
                state.counters.audio_source_owner_failures =
                    state.counters.audio_source_owner_failures.saturating_add(1);
            }
        }
    }
    inner.mark_diagnostics_changed_locked(&state);
    drop(state);
    inner.wake.notify_all();
}

/// Queue-owned cooperative execution authority for one exact export attempt.
///
/// The handle is intentionally crate-private: every executor is owned by the
/// export Module and must rendezvous here at frame, audio-block, or phase
/// boundaries. Entering `Publishing` atomically commits the attempt while
/// dispatch is enabled; after that irreversible point neither a later yield
/// request nor queue shutdown may interrupt publication.
pub(crate) struct ExportExecutionGate {
    authority: ExportExecutionGateAuthority,
}

enum ExportExecutionGateAuthority {
    Queue {
        inner: Arc<RenderQueueInner>,
        job_id: JobId,
        generation: u64,
        resource_policy: Box<ExportExecutionResourcePolicy>,
    },
    #[cfg(test)]
    AlwaysOpen,
}

impl ExportExecutionGate {
    fn for_attempt(
        inner: Arc<RenderQueueInner>,
        job_id: JobId,
        generation: u64,
        resource_policy: ExportExecutionResourcePolicy,
    ) -> Self {
        Self {
            authority: ExportExecutionGateAuthority::Queue {
                inner,
                job_id,
                generation,
                resource_policy: Box::new(resource_policy),
            },
        }
    }

    #[cfg(test)]
    pub(crate) const fn always_open_for_test() -> Self {
        Self {
            authority: ExportExecutionGateAuthority::AlwaysOpen,
        }
    }

    /// Monotonic identity of the exact admitted export attempt.
    ///
    /// Execution-owned caches use this as a generation barrier. Test-only
    /// always-open gates use generation zero inside their isolated Sessions.
    pub(crate) fn attempt_generation(&self) -> u64 {
        match &self.authority {
            ExportExecutionGateAuthority::Queue { generation, .. } => *generation,
            #[cfg(test)]
            ExportExecutionGateAuthority::AlwaysOpen => 0,
        }
    }

    /// Freeze the queue's current resource grant for this attempt.
    ///
    /// Callers invoke this once after the Preparing gate opens and retain the
    /// returned value for the complete attempt.
    pub(crate) fn resource_policy(&self) -> ExportExecutionResourcePolicy {
        match &self.authority {
            ExportExecutionGateAuthority::Queue { resource_policy, .. } => **resource_policy,
            #[cfg(test)]
            ExportExecutionGateAuthority::AlwaysOpen => ExportExecutionResourcePolicy::default(),
        }
    }

    /// Wait until execution is admitted at this safe boundary.
    ///
    /// `false` means cancellation or queue shutdown was observed before the
    /// irreversible publication point. A `Publishing` checkpoint waits for an
    /// already-requested yield, then atomically commits publication. Every
    /// later checkpoint for that attempt remains open.
    #[must_use = "a closed export execution gate requires the executor to stop cooperatively"]
    pub(crate) fn wait_at_boundary(
        &self,
        phase: ExportProgressPhase,
        cancellation: &ExecutionCancellationToken,
    ) -> bool {
        match &self.authority {
            ExportExecutionGateAuthority::Queue { inner, job_id, generation, .. } => {
                wait_at_queue_execution_boundary(inner, *job_id, *generation, phase, cancellation)
            }
            #[cfg(test)]
            ExportExecutionGateAuthority::AlwaysOpen => {
                phase == ExportProgressPhase::Publishing || !cancellation.is_canceled()
            }
        }
    }
}

fn wait_at_queue_execution_boundary(
    inner: &RenderQueueInner,
    job_id: JobId,
    generation: u64,
    phase: ExportProgressPhase,
    cancellation: &ExecutionCancellationToken,
) -> bool {
    let mut state = inner.state.lock();
    loop {
        let Some(index) = state.jobs.iter().position(|entry| {
            entry.snapshot.id == job_id && entry.snapshot.generation == generation
        }) else {
            return false;
        };
        if state.jobs[index].snapshot.publication == ExportPublicationState::Committing {
            return true;
        }
        if state.jobs[index].snapshot.status.is_terminal()
            || cancellation.is_canceled()
            || inner.shutdown.load(Ordering::Acquire)
        {
            if state.jobs[index].execution_yielded {
                state.jobs[index].execution_yielded = false;
                inner.mark_diagnostics_changed_locked(&state);
            }
            return false;
        }
        if state.dispatch_enabled {
            let entry = &mut state.jobs[index];
            let changed = entry.execution_yielded;
            entry.execution_yielded = false;
            if phase == ExportProgressPhase::Publishing {
                entry.snapshot.publication = ExportPublicationState::Committing;
                entry.snapshot.progress =
                    ExportProgress::publishing(entry.snapshot.progress.fraction);
                entry.snapshot.status =
                    JobStatus::Running { phase: ExportProgressPhase::Publishing };
            }
            if phase == ExportProgressPhase::Publishing {
                inner.mark_jobs_changed_locked(&mut state);
            } else if changed {
                inner.mark_diagnostics_changed_locked(&state);
            }
            return true;
        }
        if !state.jobs[index].execution_yielded {
            state.jobs[index].execution_yielded = true;
            inner.mark_diagnostics_changed_locked(&state);
        }
        inner.wake.wait(&mut state);
    }
}

type ExportWorkerTask = Box<dyn FnOnce() + Send + 'static>;
type ExportWorkerEntryHook = Box<dyn FnOnce() + Send + 'static>;

struct ExportWorkerPayload {
    inner: Arc<RenderQueueInner>,
    executor: Arc<dyn ExportExecutor>,
    entry_hook: Option<ExportWorkerEntryHook>,
}

impl ExportWorkerPayload {
    fn run(self) {
        mark_export_worker_started(&self.inner);
        let Self { inner, executor, entry_hook } = self;
        if let Some(entry_hook) = entry_hook {
            entry_hook();
        }
        export_worker_loop(Arc::clone(&inner), executor);
    }
}

fn abandon_export_worker_payload(retained_payload: &Arc<Mutex<Option<ExportWorkerPayload>>>) {
    let Some(payload) = retained_payload.lock().take() else {
        return;
    };
    let ExportWorkerPayload { inner, executor, entry_hook } = payload;
    drop(inner);
    // A worker factory failure normally drops the task closure on this caller.
    // The retained slot keeps the potentially foreign executor out of that
    // destructor path. Qualification records this deliberate abandonment and
    // can therefore never mistake it for a clean start or a worker detach.
    std::mem::forget(executor);
    if let Some(entry_hook) = entry_hook {
        std::mem::forget(entry_hook);
    }
}

fn dispose_canonical_or_abandon_opaque_panic_payload(
    payload: Box<dyn std::any::Any + Send>,
) -> bool {
    if payload.is::<&'static str>() || payload.is::<String>() {
        drop(payload);
        false
    } else {
        // An arbitrary `panic_any` payload can own a blocking or panicking
        // destructor. Never run it on the queue worker or shutdown caller.
        std::mem::forget(payload);
        true
    }
}

/// Instance-owned bounded offline export queue.
pub struct RenderQueue {
    inner: Arc<RenderQueueInner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl RenderQueue {
    /// Create a queue with the production executor and one dedicated offline worker.
    pub fn new() -> Arc<Self> {
        Self::new_with_executor(Arc::new(super::FfmpegExportExecutor))
    }

    pub(crate) fn new_with_executor(executor: Arc<dyn ExportExecutor>) -> Arc<Self> {
        let queue = Self::new_unstarted();
        queue.spawn_worker(executor);
        queue
    }

    fn new_unstarted() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(RenderQueueInner {
                state: Mutex::new(ExportQueueState {
                    next_generation: 1,
                    dispatch_enabled: true,
                    resource_policy: ExportExecutionResourcePolicy::default(),
                    ..ExportQueueState::default()
                }),
                wake: Condvar::new(),
                shutdown: AtomicBool::new(false),
                revision: AtomicU64::new(0),
                jobs_revision: AtomicU64::new(0),
            }),
            worker: Mutex::new(None),
        })
    }

    #[cfg(test)]
    fn new_with_executor_and_spawner(
        executor: Arc<dyn ExportExecutor>,
        entry_hook: Option<ExportWorkerEntryHook>,
        spawner: impl FnOnce(ExportWorkerTask) -> std::io::Result<JoinHandle<()>>,
    ) -> Arc<Self> {
        let queue = Self::new_unstarted();
        queue.spawn_worker_with(executor, entry_hook, spawner);
        queue
    }

    fn spawn_worker(&self, executor: Arc<dyn ExportExecutor>) {
        self.spawn_worker_with(executor, None, |task| {
            thread::Builder::new().name("mondrian-export-worker".to_owned()).spawn(task)
        });
    }

    fn spawn_worker_with(
        &self,
        executor: Arc<dyn ExportExecutor>,
        entry_hook: Option<ExportWorkerEntryHook>,
        spawner: impl FnOnce(ExportWorkerTask) -> std::io::Result<JoinHandle<()>>,
    ) {
        let retained_payload = Arc::new(Mutex::new(Some(ExportWorkerPayload {
            inner: Arc::clone(&self.inner),
            executor,
            entry_hook,
        })));
        let worker_payload = Arc::clone(&retained_payload);
        let task_inner = Arc::clone(&self.inner);
        let task: ExportWorkerTask = Box::new(move || {
            let run_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let Some(payload) = worker_payload.lock().take() else {
                    panic!("export worker started without its retained owner payload");
                };
                payload.run();
            }));
            let panicked = match run_result {
                Ok(()) => false,
                Err(payload) => {
                    if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                        mark_export_worker_owner_abandoned(
                            &task_inner,
                            "export worker panicked with an opaque payload whose owner was abandoned",
                        );
                    }
                    true
                }
            };
            mark_export_worker_completed(&task_inner, Instant::now(), panicked);
        });
        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawner(task)));
        match spawn_result {
            Ok(Ok(worker)) => {
                *self.worker.lock() = Some(worker);
            }
            Ok(Err(error)) => {
                let error_kind = error.kind();
                // `io::Error::other` may carry an arbitrary foreign Error with
                // a blocking or panicking destructor. Startup already failed
                // closed, so retain only the stable kind and abandon that
                // opaque owner with the executor payload.
                std::mem::forget(error);
                abandon_export_worker_payload(&retained_payload);
                mark_export_worker_start_failed(
                    &self.inner,
                    bounded_detail(format!("failed to start export worker: {error_kind:?}")),
                );
            }
            Err(payload) => {
                let _ = dispose_canonical_or_abandon_opaque_panic_payload(payload);
                abandon_export_worker_payload(&retained_payload);
                mark_export_worker_start_failed(
                    &self.inner,
                    "export worker spawner panicked before returning ownership".to_owned(),
                );
            }
        }
    }

    /// Admit a heavy immutable submission or return a structured rejection.
    pub fn enqueue(&self, mut job: RenderJob) -> Result<JobId, ExportAdmissionError> {
        if self.inner.shutdown.load(Ordering::Acquire) {
            return self.reject(ExportAdmissionError::QueueShutdown);
        }
        let audio_selection = job.config.preset.audio_program_selection();
        let resource_policy = self.inner.state.lock().resource_policy;
        if job.config.timeline.prepared_execution().is_none() {
            let prepared = match prepare_timeline_export_dependencies_with_audio_selection(
                &job.config.timeline.sequence,
                &job.config.timeline.sequences,
                job.config.timeline.range,
                audio_selection,
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    return self.reject(ExportAdmissionError::InvalidDelivery {
                        detail: format!(
                            "failed to prepare immutable export execution snapshot: {error}"
                        ),
                    });
                }
            };
            let root_sequence_id = job.config.timeline.sequence.id;
            job.config.timeline.sequences.retain(|sequence| {
                sequence.id != root_sequence_id && prepared.sequence_ids().contains(&sequence.id)
            });
            job.config.timeline.media.retain(|asset_id, dependency| {
                let Some(components) = prepared.media_components().get(asset_id) else {
                    return false;
                };
                dependency
                    .audio_components
                    .retain(|component_id, _| components.contains(component_id));
                true
            });
            job.config
                .timeline
                .install_prepared_execution(prepared.execution_snapshot().clone());
        }
        for sequence in std::iter::once(&job.config.timeline.sequence)
            .chain(job.config.timeline.sequences.iter())
        {
            if let Err(error) =
                sequence.validate_author_contract(&job.config.timeline.color_environment)
            {
                return self.reject(ExportAdmissionError::InvalidDelivery {
                    detail: format!(
                        "selected Sequence {} has invalid author state: {error}",
                        sequence.id
                    ),
                });
            }
        }
        if job.config.timeline.prepared_execution().is_none() {
            return self.reject(ExportAdmissionError::InvalidDelivery {
                detail: "immutable export visual execution snapshot is unavailable".to_owned(),
            });
        }
        if let Err(error) = job
            .config
            .timeline
            .freeze_prepared_title_fonts(resource_policy.title_font_bytes)
        {
            return self
                .reject(ExportAdmissionError::InvalidDelivery { detail: error.to_string() });
        }
        let Some(prepared_execution) = job.config.timeline.prepared_execution() else {
            return self.reject(ExportAdmissionError::InvalidDelivery {
                detail: "immutable export visual execution snapshot is unavailable".to_owned(),
            });
        };
        if let Err(error) = validate_timeline_export_execution_snapshot_with_audio_selection(
            &job.config.timeline.sequence,
            &job.config.timeline.sequences,
            &job.config.timeline.color_environment,
            job.config.timeline.range,
            audio_selection,
            prepared_execution,
            &job.config.timeline.media,
        ) {
            return self.reject(ExportAdmissionError::InvalidDelivery {
                detail: format!("invalid immutable export execution snapshot: {error}"),
            });
        }
        if let Err(detail) =
            super::resolve_timeline_export_delivery(&job.config, job.config.timeline.as_ref())
        {
            return self.reject(ExportAdmissionError::InvalidDelivery { detail });
        }
        let requested_output_path = job.config.output_path.clone();
        let Some((output_path, output_key)) = resolve_output_route(&requested_output_path) else {
            return self
                .reject(ExportAdmissionError::InvalidOutputPath { path: requested_output_path });
        };
        match std::fs::symlink_metadata(&output_path) {
            Ok(metadata) if !metadata.file_type().is_file() => {
                return self.reject(ExportAdmissionError::InvalidOutputPath { path: output_path });
            }
            Ok(_) if job.config.output_policy == ExportOutputPolicy::CreateNew => {
                return self
                    .reject(ExportAdmissionError::OutputAlreadyExists { path: output_path });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return self.reject(ExportAdmissionError::InvalidOutputPath { path: output_path });
            }
        }
        job.config.output_path = output_path.clone();

        let mut state = self.inner.state.lock();
        if self.inner.shutdown.load(Ordering::Acquire) {
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            self.inner.mark_diagnostics_changed_locked(&state);
            drop(state);
            return Err(ExportAdmissionError::QueueShutdown);
        }
        if let Some(detail) = &state.worker_failure {
            let error = ExportAdmissionError::WorkerUnavailable { detail: detail.clone() };
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            self.inner.mark_diagnostics_changed_locked(&state);
            drop(state);
            return Err(error);
        }
        let in_flight =
            state.jobs.iter().filter(|entry| !entry.snapshot.status.is_terminal()).count();
        if in_flight >= EXPORT_IN_FLIGHT_CAPACITY {
            let error =
                ExportAdmissionError::CapacityExceeded { capacity: EXPORT_IN_FLIGHT_CAPACITY };
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            self.inner.mark_diagnostics_changed_locked(&state);
            drop(state);
            return Err(error);
        }
        if state
            .jobs
            .iter()
            .any(|entry| !entry.snapshot.status.is_terminal() && entry.output_key == output_key)
        {
            let error = ExportAdmissionError::OutputPathBusy { path: output_path };
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            self.inner.mark_diagnostics_changed_locked(&state);
            drop(state);
            return Err(error);
        }

        let generation = state.next_generation.max(1);
        let Some(next_generation) = generation.checked_add(1) else {
            state.counters.rejections = state.counters.rejections.saturating_add(1);
            self.inner.mark_diagnostics_changed_locked(&state);
            drop(state);
            return Err(ExportAdmissionError::GenerationExhausted);
        };
        state.next_generation = next_generation;
        let id = job.id;
        let snapshot = ExportJobSnapshot {
            id,
            generation,
            output_path: job.config.output_path.clone(),
            output_policy: job.config.output_policy,
            preset_name: job.config.preset.name.clone(),
            status: JobStatus::Pending,
            progress: ExportProgress::default(),
            publication: ExportPublicationState::Reversible,
            diagnostics: ExportJobDiagnostics::default(),
            created_at: job.created_at,
            started_at: None,
            completed_at: None,
            terminal_evidence: None,
            artifact_publication: None,
            executed: false,
        };
        state.jobs.push_back(ExportJobEntry {
            snapshot,
            payload: Some(job),
            cancellation: ExecutionCancellationToken::new(),
            output_key,
            execution_yielded: false,
        });
        state.counters.admissions = state.counters.admissions.saturating_add(1);
        self.inner.mark_jobs_changed_locked(&mut state);
        drop(state);
        self.inner.wake.notify_one();
        Ok(id)
    }

    fn reject<T>(&self, error: ExportAdmissionError) -> Result<T, ExportAdmissionError> {
        let mut state = self.inner.state.lock();
        state.counters.rejections = state.counters.rejections.saturating_add(1);
        self.inner.mark_diagnostics_changed_locked(&state);
        drop(state);
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

    /// Whether the retained job can still accept a new cancellation request.
    ///
    /// This is a non-authoritative UI projection. Callers must still inspect
    /// [`Self::cancel`]'s exact outcome because execution can cross the
    /// irreversible publication boundary immediately after this query.
    pub fn can_cancel(&self, id: JobId) -> bool {
        self.inner
            .state
            .lock()
            .jobs
            .iter()
            .find(|entry| entry.snapshot.id == id)
            .is_some_and(|entry| {
                entry.snapshot.publication != ExportPublicationState::Committing
                    && entry.snapshot.status.can_cancel()
            })
    }

    /// Whether any bounded terminal job evidence is currently retained.
    pub fn has_terminal_history(&self) -> bool {
        self.inner
            .state
            .lock()
            .jobs
            .iter()
            .any(|entry| entry.snapshot.status.is_terminal())
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
                entry.snapshot.publication = ExportPublicationState::NotPublished;
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
                if state.jobs[index].snapshot.publication == ExportPublicationState::Committing {
                    state.counters.too_late_cancellation_requests =
                        state.counters.too_late_cancellation_requests.saturating_add(1);
                    ExportCancelOutcome::TooLateCommitting
                } else {
                    let entry = &mut state.jobs[index];
                    entry.cancellation.cancel();
                    entry.snapshot.status = JobStatus::Cancelling { phase };
                    state.counters.cancellation_requests =
                        state.counters.cancellation_requests.saturating_add(1);
                    ExportCancelOutcome::Requested
                }
            }
            JobStatus::Cancelling { .. } => ExportCancelOutcome::AlreadyRequested,
            JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled => {
                ExportCancelOutcome::AlreadyTerminal
            }
        };
        if outcome == ExportCancelOutcome::Requested {
            self.inner.mark_jobs_changed_locked(&mut state);
            drop(state);
            self.inner.wake.notify_all();
        } else if outcome == ExportCancelOutcome::TooLateCommitting {
            self.inner.mark_diagnostics_changed_locked(&state);
            drop(state);
        } else {
            drop(state);
        }
        outcome
    }

    /// Remove all retained terminal snapshots. Active payloads are never affected.
    ///
    /// Returns the exact number removed while holding queue authority.
    pub fn clear_terminal_history(&self) -> usize {
        let mut state = self.inner.state.lock();
        let before = state.jobs.len();
        state.jobs.retain(|entry| !entry.snapshot.status.is_terminal());
        let removed = before - state.jobs.len();
        if removed > 0 {
            self.inner.mark_jobs_changed_locked(&mut state);
        }
        drop(state);
        removed
    }

    /// Current wrapping observation token for all queue state and diagnostics.
    ///
    /// Only equality comparison is meaningful. The token is a non-consuming
    /// dirty hint, not a job generation, event count, or snapshot version.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    /// Current wrapping observation token for the retained job snapshots.
    ///
    /// Resource policy, dispatch admission, and execution-yield diagnostics do
    /// not advance this revision. Presentation observers can therefore update
    /// export jobs without invalidating unrelated editor or Preview state.
    /// Only equality comparison is meaningful.
    pub fn jobs_revision(&self) -> u64 {
        self.inner.jobs_revision.load(Ordering::Acquire)
    }

    /// Pause or resume pending dispatch and running cooperative execution.
    ///
    /// Admission remains bounded and available while paused, so an explicit
    /// user export is retained and visible rather than silently discarded.
    /// Running attempts yield at their next declared safe boundary. An attempt
    /// that atomically entered `Publishing` is already irreversible and is
    /// never interrupted by this policy.
    pub fn set_dispatch_enabled(&self, enabled: bool) {
        let mut state = self.inner.state.lock();
        if state.dispatch_enabled == enabled {
            return;
        }
        state.dispatch_enabled = enabled;
        self.inner.mark_diagnostics_changed_locked(&state);
        drop(state);
        self.inner.wake.notify_all();
    }

    /// Publish the resource grant that the next dispatched attempt will freeze.
    ///
    /// An already-running attempt retains its prior immutable grant. This
    /// avoids silently changing temporal admission or cache residency halfway
    /// through one deterministic offline render.
    pub fn set_resource_policy(&self, policy: ExportExecutionResourcePolicy) {
        let mut state = self.inner.state.lock();
        if state.resource_policy == policy {
            return;
        }
        state.resource_policy = policy;
        self.inner.mark_diagnostics_changed_locked(&state);
        drop(state);
    }

    /// Snapshot bounded queue health and lightweight job evidence.
    pub fn diagnostics(&self) -> ExportQueueDiagnostics {
        let state = self.inner.state.lock();
        let mut diagnostics = ExportQueueDiagnostics {
            revision: self.revision(),
            worker_failure: state.worker_failure.clone(),
            dispatch_enabled: state.dispatch_enabled,
            resource_policy: state.resource_policy,
            admissions: state.counters.admissions,
            rejections: state.counters.rejections,
            cancellation_requests: state.counters.cancellation_requests,
            too_late_cancellation_requests: state.counters.too_late_cancellation_requests,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            rendered_frames: state.counters.rendered_frames,
            durable_artifacts: state.counters.durable_artifacts,
            jobs: state.jobs.iter().map(|entry| entry.snapshot.clone()).collect(),
            ..ExportQueueDiagnostics::default()
        };
        diagnostics.running_yield_requested = !state.dispatch_enabled
            && state.jobs.iter().any(|entry| {
                matches!(entry.snapshot.status, JobStatus::Running { .. })
                    && entry.snapshot.publication == ExportPublicationState::Reversible
            });
        for entry in &state.jobs {
            diagnostics.running_yielded += usize::from(entry.execution_yielded);
            diagnostics.committing +=
                usize::from(entry.snapshot.publication == ExportPublicationState::Committing);
            match entry.snapshot.status {
                JobStatus::Pending => diagnostics.pending += 1,
                JobStatus::Running { .. }
                    if entry.snapshot.publication == ExportPublicationState::Committing => {}
                JobStatus::Running { .. } => diagnostics.running += 1,
                JobStatus::Cancelling { .. } => diagnostics.cancelling += 1,
                JobStatus::Completed | JobStatus::Failed(_) | JobStatus::Cancelled => {
                    diagnostics.terminal += 1;
                }
            }
        }
        diagnostics
    }

    /// Capture one fixed-size queue observation for a long-duration producer.
    pub fn endurance_snapshot(&self, observed_at_us: u64) -> ExportEnduranceSnapshot {
        let state = self.inner.state.lock();
        let pending_jobs = state
            .jobs
            .iter()
            .filter(|entry| matches!(entry.snapshot.status, JobStatus::Pending))
            .count() as u64;
        let active_jobs = state
            .jobs
            .iter()
            .filter(|entry| {
                matches!(
                    entry.snapshot.status,
                    JobStatus::Running { .. } | JobStatus::Cancelling { .. }
                )
            })
            .count() as u64;
        ExportEnduranceSnapshot {
            schema_version: 2,
            observed_at_us,
            shutdown_requested: state.shutdown_requested,
            worker_running: state.worker_running,
            worker_terminated: state.worker_terminated,
            activity_events: state.activity_events,
            admissions: state.counters.admissions,
            rejections: state.counters.rejections,
            completions: state.counters.completions,
            failures: state.counters.failures,
            cancellations: state.counters.cancellations,
            rendered_frames: state.counters.rendered_frames,
            durable_artifacts: state.counters.durable_artifacts,
            pending_jobs,
            active_jobs,
            worker_failed: state.worker_failure.is_some(),
            audio_source_owners_started: state.counters.audio_source_owners_started,
            audio_source_owners_closed: state.counters.audio_source_owners_closed,
            audio_source_owner_failures: state.counters.audio_source_owner_failures,
            active_audio_source_owners: state.active_audio_source_owners,
        }
    }

    /// Request queue shutdown and wait a bounded interval for worker return.
    ///
    /// Publication that already crossed the irreversible namespace boundary is
    /// allowed to finish; every other live attempt receives cancellation.
    pub fn shutdown_and_wait(&self, timeout: Duration) -> ExportQueueShutdownEvidence {
        let started_at = Instant::now();
        let deadline = started_at.checked_add(timeout).unwrap_or(started_at);
        self.shutdown_until(deadline)
    }

    /// Request queue shutdown and consume worker ownership through one absolute deadline.
    ///
    /// Normal termination is accepted only after the owned handle is joined and
    /// its worker-published monotonic completion stamp is no later than
    /// `deadline`. A handle still running at the deadline is relinquished once,
    /// with timeout and detach facts latched permanently. If the handle is
    /// already finished but its completion stamp is late, it is still joined to
    /// reclaim ownership; timeout remains true while detach remains false.
    ///
    /// Exactly one product coordinator owns this consuming call. Sequential
    /// repeats return the latched terminal receipt; concurrent consumers are
    /// unsupported and any receipt observed while another caller owns the
    /// handle is fail-closed rather than qualification evidence.
    pub fn shutdown_until(&self, deadline: Instant) -> ExportQueueShutdownEvidence {
        self.begin_shutdown();
        let worker = self.worker.lock().take();
        if let Some(worker) = worker {
            self.finish_worker_until(worker, deadline);
        }
        self.shutdown_evidence()
    }

    fn finish_worker_until(&self, worker: JoinHandle<()>, deadline: Instant) {
        if worker.thread().id() == thread::current().id() {
            self.mark_worker_detached(
                Instant::now() >= deadline,
                "export worker cannot join its own thread",
            );
            drop(worker);
            return;
        }

        loop {
            if worker.is_finished() {
                self.join_finished_worker(worker, Some(deadline));
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                // Completion wins the observation race at the exact boundary,
                // but its own monotonic stamp still decides whether it was late.
                if worker.is_finished() {
                    self.join_finished_worker(worker, Some(deadline));
                } else {
                    self.mark_worker_detached(
                        true,
                        "export worker exceeded the absolute shutdown deadline",
                    );
                    drop(worker);
                }
                return;
            }

            let remaining = deadline.saturating_duration_since(now);
            let mut state = self.inner.state.lock();
            if !worker.is_finished() {
                // The outer supervisor publishes completion before the final
                // trivial closure captures are destroyed, so retain a short
                // bounded poll to observe JoinHandle completion without ever
                // blocking join.
                self.inner.wake.wait_for(&mut state, remaining.min(Duration::from_millis(1)));
            }
        }
    }

    fn join_finished_worker(&self, worker: JoinHandle<()>, deadline: Option<Instant>) {
        debug_assert!(worker.is_finished());
        let (worker_panicked, panic_payload_abandoned) = match worker.join() {
            Ok(()) => (false, false),
            Err(payload) => (
                true,
                dispose_canonical_or_abandon_opaque_panic_payload(payload),
            ),
        };
        let mut state = self.inner.state.lock();
        state.worker_running = false;
        let completed_at = state.worker_completed_at;
        let missed_deadline = deadline.is_some_and(|deadline| {
            completed_at.is_none_or(|completed_at| completed_at > deadline)
        });
        state.worker_timed_out |= missed_deadline;
        state.worker_owner_abandoned |= panic_payload_abandoned;
        let logical_worker_panicked = worker_panicked || state.worker_panicked;
        match (logical_worker_panicked, completed_at) {
            (false, Some(_)) => {
                state.worker_terminated = true;
            }
            (false, None) => {
                state.worker_failure =
                    Some("export worker returned without a monotonic completion stamp".to_owned());
            }
            (true, _) => {
                state.worker_panicked = true;
                state.worker_failure =
                    Some("export worker panicked outside the per-Job executor boundary".to_owned());
            }
        }
        self.inner.mark_diagnostics_changed_locked(&state);
        drop(state);
        self.inner.wake.notify_all();
    }

    fn mark_worker_detached(&self, timed_out: bool, detail: &str) {
        let mut state = self.inner.state.lock();
        state.worker_timed_out |= timed_out;
        state.worker_detached = true;
        if state.worker_failure.is_none() {
            state.worker_failure = Some(detail.to_owned());
        }
        self.inner.mark_diagnostics_changed_locked(&state);
        drop(state);
        self.inner.wake.notify_all();
    }

    fn shutdown_evidence(&self) -> ExportQueueShutdownEvidence {
        let state = self.inner.state.lock();
        let pending_jobs = state
            .jobs
            .iter()
            .filter(|entry| matches!(entry.snapshot.status, JobStatus::Pending))
            .count() as u64;
        let active_jobs = state
            .jobs
            .iter()
            .filter(|entry| {
                matches!(
                    entry.snapshot.status,
                    JobStatus::Running { .. } | JobStatus::Cancelling { .. }
                )
            })
            .count() as u64;
        ExportQueueShutdownEvidence {
            schema_version: 4,
            worker_started: state.worker_started,
            worker_start_failed: state.worker_start_failed,
            worker_terminated: state.worker_terminated,
            worker_panicked: state.worker_panicked,
            worker_timed_out: state.worker_timed_out,
            worker_detached: state.worker_detached,
            worker_owner_abandoned: state.worker_owner_abandoned,
            pending_jobs,
            active_jobs,
            activity_events: state.activity_events,
            audio_source_owners_started: state.counters.audio_source_owners_started,
            audio_source_owners_closed: state.counters.audio_source_owners_closed,
            audio_source_owner_failures: state.counters.audio_source_owner_failures,
            active_audio_source_owners: state.active_audio_source_owners,
        }
    }

    /// Close queue admission and cooperatively cancel every reversible attempt.
    ///
    /// This is the non-waiting half of [`Self::shutdown_and_wait`]. A caller
    /// coordinating several owners can signal all of them before spending one
    /// shared absolute shutdown deadline on terminal receipts.
    pub fn begin_shutdown(&self) {
        let mut state = self.inner.state.lock();
        let first_request = !self.inner.shutdown.swap(true, Ordering::AcqRel);
        state.shutdown_requested = true;
        let mut pending_cancellations = 0_u64;
        if first_request {
            for entry in &mut state.jobs {
                match entry.snapshot.status {
                    JobStatus::Pending => {
                        entry.cancellation.cancel();
                        entry.payload = None;
                        entry.snapshot.status = JobStatus::Cancelled;
                        entry.snapshot.publication = ExportPublicationState::NotPublished;
                        entry.snapshot.completed_at = Some(Utc::now());
                        entry.snapshot.terminal_evidence = Some(ExecutionTerminalEvidence {
                            generation: entry.snapshot.generation,
                            priority: ExecutionPriority::UserInitiated,
                            disposition: ExecutionTerminalDisposition::Canceled,
                            deadline: ExecutionDeadlineStatus::NotApplicable,
                        });
                        pending_cancellations = pending_cancellations.saturating_add(1);
                    }
                    JobStatus::Running { .. } | JobStatus::Cancelling { .. }
                        if entry.snapshot.publication != ExportPublicationState::Committing =>
                    {
                        entry.cancellation.cancel();
                    }
                    JobStatus::Running { .. }
                    | JobStatus::Cancelling { .. }
                    | JobStatus::Completed
                    | JobStatus::Failed(_)
                    | JobStatus::Cancelled => {}
                }
            }
            state.counters.cancellations =
                state.counters.cancellations.saturating_add(pending_cancellations);
            if pending_cancellations > 0 {
                trim_terminal_history(&mut state);
            }
        }
        if first_request {
            self.inner.mark_jobs_changed_locked(&mut state);
        }
        drop(state);
        self.inner.wake.notify_all();
    }
}

impl Drop for RenderQueue {
    fn drop(&mut self) {
        self.begin_shutdown();
        let Some(worker) = self.worker.lock().take() else {
            return;
        };
        if worker.thread().id() == thread::current().id() {
            self.mark_worker_detached(false, "export worker dropped its own Queue owner");
            drop(worker);
        } else if worker.is_finished() {
            self.join_finished_worker(worker, None);
        } else {
            self.mark_worker_detached(false, "export Queue dropped while its worker was active");
            drop(worker);
        }
    }
}

struct ExportWork {
    job: RenderJob,
    generation: u64,
    cancellation: ExecutionCancellationToken,
    resource_policy: ExportExecutionResourcePolicy,
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
        let owner_inner = Arc::clone(&inner);
        let mut report_owner = move |event: ExportExecutionOwnerEvent| {
            update_execution_owner_event(&owner_inner, event);
        };
        let execution_gate = ExportExecutionGate::for_attempt(
            Arc::clone(&inner),
            job_id,
            generation,
            work.resource_policy,
        );
        let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor.execute(
                &work.job,
                &work.cancellation,
                &execution_gate,
                &mut report,
                &mut report_diagnostics,
                &mut report_owner,
            )
        })) {
            Ok(outcome) => ExportWorkerOutcome::Execution(outcome),
            Err(payload) => {
                if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                    mark_export_worker_owner_abandoned(
                        &inner,
                        "export executor panicked with an opaque payload whose owner was abandoned",
                    );
                }
                ExportWorkerOutcome::Panicked
            }
        };
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
        let pending_index = state.dispatch_enabled.then(|| {
            state
                .jobs
                .iter()
                .position(|entry| matches!(entry.snapshot.status, JobStatus::Pending))
        });
        if let Some(index) = pending_index.flatten() {
            let Some(job) = state.jobs[index].payload.take() else {
                let generation = state.jobs[index].snapshot.generation;
                let entry = &mut state.jobs[index];
                entry.snapshot.status = JobStatus::Failed(ExportFailure::execution(
                    "admitted export payload was unavailable at dispatch",
                ));
                entry.snapshot.publication = ExportPublicationState::NotPublished;
                entry.snapshot.completed_at = Some(Utc::now());
                entry.snapshot.terminal_evidence = Some(ExecutionTerminalEvidence {
                    generation,
                    priority: ExecutionPriority::UserInitiated,
                    disposition: ExecutionTerminalDisposition::Failed,
                    deadline: ExecutionDeadlineStatus::NotApplicable,
                });
                state.counters.failures = state.counters.failures.saturating_add(1);
                trim_terminal_history(&mut state);
                inner.mark_jobs_changed_locked(&mut state);
                continue;
            };
            let resource_policy = state.resource_policy;
            let entry = &mut state.jobs[index];
            entry.snapshot.status = JobStatus::Running { phase: ExportProgressPhase::Preparing };
            entry.snapshot.started_at = Some(Utc::now());
            entry.snapshot.executed = true;
            let work = ExportWork {
                job,
                generation: entry.snapshot.generation,
                cancellation: entry.cancellation.clone(),
                resource_policy,
            };
            inner.mark_jobs_changed_locked(&mut state);
            drop(state);
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
    if !matches!(entry.snapshot.status, JobStatus::Running { .. })
        || entry.snapshot.publication == ExportPublicationState::Committing
        || progress.phase == ExportProgressPhase::Publishing
    {
        return;
    }
    if progress.phase.rank() < entry.snapshot.progress.phase.rank() {
        return;
    }
    let previous_rendered_frames = progress_completed_frames(entry.snapshot.progress.detail);
    let progress = progress.normalized(entry.snapshot.progress);
    let rendered_frame_delta =
        progress_completed_frames(progress.detail).saturating_sub(previous_rendered_frames);
    let status = JobStatus::Running { phase: progress.phase };
    if entry.snapshot.progress == progress && entry.snapshot.status == status {
        return;
    }
    entry.snapshot.progress = progress;
    entry.snapshot.status = status;
    state.counters.rendered_frames =
        state.counters.rendered_frames.saturating_add(rendered_frame_delta);
    inner.mark_jobs_changed_locked(&mut state);
    drop(state);
}

fn progress_completed_frames(detail: ExportProgressDetail) -> u64 {
    match detail {
        ExportProgressDetail::Frames { completed, .. } => completed,
        ExportProgressDetail::None | ExportProgressDetail::MediaTimeMicros { .. } => 0,
    }
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
    if entry.snapshot.diagnostics == diagnostics {
        return;
    }
    entry.snapshot.diagnostics = diagnostics;
    inner.mark_jobs_changed_locked(&mut state);
    drop(state);
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
    let publication_was_committing =
        state.jobs[index].snapshot.publication == ExportPublicationState::Committing;
    let (status, disposition, publication, artifact_publication) = match outcome {
        ExportWorkerOutcome::Execution(JobExecutionResult::Published(evidence))
            if !publication_was_committing =>
        {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::execution(
                    "export executor reported completion without entering the irreversible publication gate",
                )),
                ExecutionTerminalDisposition::Failed,
                ExportPublicationState::Published,
                Some(evidence.into_terminal_evidence()),
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::Published(evidence)) => {
            state.counters.completions = state.counters.completions.saturating_add(1);
            state.counters.durable_artifacts = state.counters.durable_artifacts.saturating_add(1);
            (
                JobStatus::Completed,
                ExecutionTerminalDisposition::Completed,
                ExportPublicationState::Published,
                Some(evidence.into_terminal_evidence()),
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::PublicationFailed(failure)) => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            let (failure, publication, evidence) = match failure {
                ExportPublicationFailure::BeforeNamespace {
                    output_path,
                    retained_partial_path,
                    detail,
                } => (
                    ExportFailure::publication(
                        ExportFailureReason::PublicationBeforeNamespace,
                        detail,
                    ),
                    ExportPublicationState::NotPublished,
                    ExportArtifactPublicationEvidence::BeforeNamespace {
                        output_path,
                        retained_partial_path,
                    },
                ),
                ExportPublicationFailure::DurabilityUnconfirmed { output_path, detail } => (
                    ExportFailure::publication(
                        ExportFailureReason::PublicationDurabilityUnconfirmed,
                        detail,
                    ),
                    ExportPublicationState::DurabilityUnconfirmed,
                    ExportArtifactPublicationEvidence::DurabilityUnconfirmed { output_path },
                ),
                ExportPublicationFailure::NamespaceIndeterminate {
                    output_path,
                    retained_partial_path,
                    detail,
                } => (
                    ExportFailure::publication(
                        ExportFailureReason::PublicationNamespaceIndeterminate,
                        detail,
                    ),
                    ExportPublicationState::OutcomeUnknown,
                    ExportArtifactPublicationEvidence::NamespaceIndeterminate {
                        output_path,
                        retained_partial_path,
                    },
                ),
            };
            (
                JobStatus::Failed(failure),
                ExecutionTerminalDisposition::Failed,
                publication,
                Some(evidence),
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::ReversibleWorkCompleted) => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::execution(
                    "export executor returned reversible work completion as a terminal queue outcome",
                )),
                ExecutionTerminalDisposition::Failed,
                if publication_was_committing {
                    ExportPublicationState::OutcomeUnknown
                } else {
                    ExportPublicationState::NotPublished
                },
                None,
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::Failed(detail)) => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::execution(detail)),
                ExecutionTerminalDisposition::Failed,
                if publication_was_committing {
                    ExportPublicationState::OutcomeUnknown
                } else {
                    ExportPublicationState::NotPublished
                },
                None,
            )
        }
        ExportWorkerOutcome::Panicked => {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::panic()),
                ExecutionTerminalDisposition::Failed,
                if publication_was_committing {
                    ExportPublicationState::OutcomeUnknown
                } else {
                    ExportPublicationState::NotPublished
                },
                None,
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::Cancelled)
            if publication_was_committing =>
        {
            state.counters.failures = state.counters.failures.saturating_add(1);
            (
                JobStatus::Failed(ExportFailure::execution(
                    "export executor reported cancellation after irreversible publication authority was committed",
                )),
                ExecutionTerminalDisposition::Failed,
                ExportPublicationState::OutcomeUnknown,
                None,
            )
        }
        ExportWorkerOutcome::Execution(JobExecutionResult::Cancelled) => {
            state.counters.cancellations = state.counters.cancellations.saturating_add(1);
            (
                JobStatus::Cancelled,
                ExecutionTerminalDisposition::Canceled,
                ExportPublicationState::NotPublished,
                None,
            )
        }
    };
    let entry = &mut state.jobs[index];
    if matches!(status, JobStatus::Completed) {
        entry.snapshot.progress = ExportProgress::publishing(1.0);
    }
    entry.execution_yielded = false;
    entry.snapshot.publication = publication;
    entry.snapshot.artifact_publication = artifact_publication;
    entry.snapshot.status = status;
    entry.snapshot.completed_at = Some(Utc::now());
    entry.snapshot.terminal_evidence = Some(ExecutionTerminalEvidence {
        generation,
        priority: ExecutionPriority::UserInitiated,
        disposition,
        deadline: ExecutionDeadlineStatus::NotApplicable,
    });
    trim_terminal_history(&mut state);
    inner.mark_jobs_changed_locked(&mut state);
    drop(state);
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

fn resolve_output_route(path: &Path) -> Option<(PathBuf, String)> {
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
    let (parent, file_name) = (normalized.parent()?, normalized.file_name()?);
    let canonical_parent = std::fs::canonicalize(parent).ok()?;
    if !std::fs::metadata(&canonical_parent).ok()?.is_dir() {
        return None;
    }
    let normalized = canonical_parent.join(file_name);
    let key = normalized.to_string_lossy().replace('/', "\\");
    let key = if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    };
    Some((normalized, key))
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
mod shutdown_evidence_contract_tests {
    use super::*;

    #[test]
    fn shutdown_evidence_requires_started_terminated_worker_and_empty_workset() {
        let clean = ExportQueueShutdownEvidence {
            schema_version: 4,
            worker_started: true,
            worker_start_failed: false,
            worker_terminated: true,
            worker_panicked: false,
            worker_timed_out: false,
            worker_detached: false,
            worker_owner_abandoned: false,
            pending_jobs: 0,
            active_jobs: 0,
            activity_events: 0,
            audio_source_owners_started: 1,
            audio_source_owners_closed: 1,
            audio_source_owner_failures: 0,
            active_audio_source_owners: 0,
        };

        assert!(clean.all_resources_released());
        assert!(
            !ExportQueueShutdownEvidence { schema_version: 3, ..clean }.all_resources_released()
        );
        assert!(!ExportQueueShutdownEvidence { pending_jobs: 1, ..clean }.all_resources_released());
        assert!(!ExportQueueShutdownEvidence { active_jobs: 1, ..clean }.all_resources_released());
        assert!(
            !ExportQueueShutdownEvidence { worker_started: false, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { worker_start_failed: true, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { worker_terminated: false, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { worker_panicked: true, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { worker_timed_out: true, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { worker_detached: true, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { worker_owner_abandoned: true, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { audio_source_owners_closed: 0, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { audio_source_owner_failures: 1, ..clean }
                .all_resources_released()
        );
        assert!(
            !ExportQueueShutdownEvidence { active_audio_source_owners: 1, ..clean }
                .all_resources_released()
        );
    }

    #[test]
    fn shutdown_evidence_schema_four_round_trips_exact_owner_facts() {
        let evidence = ExportQueueShutdownEvidence {
            schema_version: 4,
            worker_started: false,
            worker_start_failed: true,
            worker_terminated: false,
            worker_panicked: false,
            worker_timed_out: false,
            worker_detached: false,
            worker_owner_abandoned: true,
            pending_jobs: 0,
            active_jobs: 0,
            activity_events: 7,
            audio_source_owners_started: 1,
            audio_source_owners_closed: 1,
            audio_source_owner_failures: 1,
            active_audio_source_owners: 0,
        };

        let encoded = serde_json::to_string(&evidence).expect("serialize shutdown evidence");
        let decoded = serde_json::from_str::<ExportQueueShutdownEvidence>(&encoded)
            .expect("deserialize shutdown evidence");

        assert_eq!(decoded, evidence);
        assert!(encoded.contains("\"worker_started\":false"));
        assert!(encoded.contains("\"worker_start_failed\":true"));
        assert!(encoded.contains("\"worker_owner_abandoned\":true"));
        assert!(encoded.contains("\"audio_source_owner_failures\":1"));
    }

    #[test]
    fn legacy_schema_two_json_without_exact_worker_facts_is_rejected() {
        let encoded = r#"{
            "schema_version": 2,
            "worker_started": true,
            "worker_start_failed": false,
            "worker_terminated": true,
            "pending_jobs": 0,
            "active_jobs": 0,
            "activity_events": 1
        }"#;

        assert!(serde_json::from_str::<ExportQueueShutdownEvidence>(encoded).is_err());
    }
}

#[cfg(test)]
mod tests;
