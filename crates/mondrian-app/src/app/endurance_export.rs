//! Phase-scoped repeated Export execution over one immutable Timeline snapshot.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mondrian_core::{
    ExecutionDeadlineStatus, ExecutionPriority, ExecutionTerminalDisposition,
    ExecutionTerminalEvidence, JobId, SequenceId,
};
use mondrian_export::preset::{
    ExportConfig, ExportOutputPolicy, ExportPreset, TimelineExportRange,
};
use mondrian_export::queue::{
    ExportArtifactPublicationEvidence, ExportCancelOutcome, ExportPublicationState,
    ExportQueueDiagnostics, JobStatus, RenderJob, RenderQueue,
};
use mondrian_export::{
    verify_export_artifact, IndependentExportArtifactPolicy, IndependentExportArtifactReceipt,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_campaign::EnduranceCampaignEvent;
use super::endurance_recovery::EnduranceRecoveryOperationReceipt;
use super::exporting::{export_preset_extension, TimelineExportRequest};
use super::AppState;

const MAXIMUM_ARTIFACT_PREFIX_BYTES: usize = 64;

/// Exact App inputs used to freeze one repeated-Export phase.
#[derive(Debug, Clone)]
pub struct FrozenRepeatedExportRequest {
    /// Single-file delivery preset frozen for every attempt.
    pub preset: ExportPreset,
    /// Exact Sequence; `None` selects the active Sequence at capture.
    pub sequence_id: Option<SequenceId>,
    /// Exact Timeline range frozen into the execution snapshot.
    pub range: TimelineExportRange,
    /// Existing real directory that receives create-only artifacts.
    pub output_directory: PathBuf,
    /// Link-free ASCII prefix used with the monotonically increasing ordinal.
    pub artifact_prefix: String,
    /// Optional broadcaster QC contract frozen with every attempt.
    pub broadcast_qc: Option<mondrian_broadcast::BroadcastQcProfile>,
    /// Independent full-decode verification resource bounds.
    pub verification_policy: IndependentExportArtifactPolicy,
}

#[derive(Debug, Clone)]
struct FrozenExportPlan {
    base_config: ExportConfig,
    output_directory: PathBuf,
    artifact_prefix: String,
    extension: &'static str,
}

impl FrozenExportPlan {
    fn capture(
        app: &AppState,
        request: FrozenRepeatedExportRequest,
    ) -> Result<(Self, ProductionFrozenExportBackend), FrozenRepeatedExportError> {
        validate_single_file_preset(&request.preset)?;
        validate_artifact_prefix(&request.artifact_prefix)?;
        let output_directory = canonical_existing_directory(&request.output_directory)?;
        let extension = export_preset_extension(&request.preset);
        let first_output = artifact_path(&output_directory, &request.artifact_prefix, extension, 1);
        let base_config = app
            .build_timeline_export_config(TimelineExportRequest {
                preset: request.preset,
                sequence_id: request.sequence_id,
                range: request.range,
                output_path: first_output,
                output_policy: ExportOutputPolicy::CreateNew,
                broadcast_qc: request.broadcast_qc,
            })
            .map_err(FrozenRepeatedExportError::InvalidPlan)?;
        Ok((
            Self {
                base_config,
                output_directory,
                artifact_prefix: request.artifact_prefix,
                extension,
            },
            ProductionFrozenExportBackend {
                queue: Arc::clone(&app.render_queue),
                verification_policy: request.verification_policy,
            },
        ))
    }

    fn config(&self, ordinal: u64) -> ExportConfig {
        let mut config = self.base_config.clone();
        config.output_path = artifact_path(
            &self.output_directory,
            &self.artifact_prefix,
            self.extension,
            ordinal,
        );
        config
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FrozenExportAttemptObservation {
    Pending {
        generation: u64,
        output_path: PathBuf,
        executed: bool,
        publication: ExportPublicationState,
    },
    Running {
        generation: u64,
        output_path: PathBuf,
        executed: bool,
        publication: ExportPublicationState,
    },
    Cancelling {
        generation: u64,
        output_path: PathBuf,
        executed: bool,
        publication: ExportPublicationState,
    },
    Completed {
        generation: u64,
        output_path: PathBuf,
    },
    Failed(String),
    Cancelled(FrozenCancelledExportTerminal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FrozenCancelledExportTerminal {
    job_id: JobId,
    generation: u64,
    output_path: PathBuf,
    executed: bool,
    publication: ExportPublicationState,
    terminal_evidence: Option<ExecutionTerminalEvidence>,
    artifact_publication: Option<ExportArtifactPublicationEvidence>,
}

struct VerifiedFrozenExportArtifact {
    event: EnduranceCampaignEvent,
    artifact_sha256: String,
    validation_report_sha256: String,
}

trait FrozenExportBackend {
    fn retained_jobs(&self) -> usize;
    fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String>;
    fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String>;
    fn verify(
        &mut self,
        id: JobId,
        output_path: &Path,
        completed_at_us: u64,
    ) -> Result<VerifiedFrozenExportArtifact, String>;
    fn diagnostics(&self) -> ExportQueueDiagnostics;
    fn cancel(&mut self, id: JobId) -> ExportCancelOutcome;
    fn clear_terminal_history(&mut self) -> usize;
}

struct ProductionFrozenExportBackend {
    queue: Arc<RenderQueue>,
    verification_policy: IndependentExportArtifactPolicy,
}

impl FrozenExportBackend for ProductionFrozenExportBackend {
    fn retained_jobs(&self) -> usize {
        self.queue.list_jobs().len()
    }

    fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String> {
        self.queue.enqueue(RenderJob::new(config)).map_err(|error| error.to_string())
    }

    fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String> {
        let snapshot = self
            .queue
            .list_jobs()
            .into_iter()
            .find(|snapshot| snapshot.id == id)
            .ok_or_else(|| format!("phase-owned Export job {id} disappeared"))?;
        let generation = snapshot.generation;
        let output_path = snapshot.output_path.clone();
        let executed = snapshot.executed;
        let publication = snapshot.publication;
        match snapshot.status {
            JobStatus::Pending => Ok(FrozenExportAttemptObservation::Pending {
                generation,
                output_path,
                executed,
                publication,
            }),
            JobStatus::Running { .. } => Ok(FrozenExportAttemptObservation::Running {
                generation,
                output_path,
                executed,
                publication,
            }),
            JobStatus::Cancelling { .. } => Ok(FrozenExportAttemptObservation::Cancelling {
                generation,
                output_path,
                executed,
                publication,
            }),
            JobStatus::Failed(error) => {
                Ok(FrozenExportAttemptObservation::Failed(error.to_string()))
            }
            JobStatus::Cancelled => Ok(FrozenExportAttemptObservation::Cancelled(
                FrozenCancelledExportTerminal {
                    job_id: snapshot.id,
                    generation,
                    output_path,
                    executed,
                    publication,
                    terminal_evidence: snapshot.terminal_evidence,
                    artifact_publication: snapshot.artifact_publication,
                },
            )),
            JobStatus::Completed => match (snapshot.publication, snapshot.artifact_publication) {
                (
                    ExportPublicationState::Published,
                    Some(ExportArtifactPublicationEvidence::Durable { output_path }),
                ) if output_path == snapshot.output_path => {
                    Ok(FrozenExportAttemptObservation::Completed { generation, output_path })
                }
                _ => Err(format!(
                    "phase-owned Export job {id} completed without exact durable publication"
                )),
            },
        }
    }

    fn verify(
        &mut self,
        id: JobId,
        output_path: &Path,
        completed_at_us: u64,
    ) -> Result<VerifiedFrozenExportArtifact, String> {
        let receipt = verify_export_artifact(
            output_path,
            format!("endurance-export-{id}"),
            self.verification_policy,
        )
        .map_err(|error| error.to_string())?;
        Ok(verified_artifact(completed_at_us, &receipt))
    }

    fn diagnostics(&self) -> ExportQueueDiagnostics {
        self.queue.diagnostics()
    }

    fn cancel(&mut self, id: JobId) -> ExportCancelOutcome {
        self.queue.cancel(id)
    }

    fn clear_terminal_history(&mut self) -> usize {
        self.queue.clear_terminal_history()
    }
}

fn verified_artifact(
    completed_at_us: u64,
    receipt: &IndependentExportArtifactReceipt,
) -> VerifiedFrozenExportArtifact {
    VerifiedFrozenExportArtifact {
        event: EnduranceCampaignEvent::export_artifact_verified(completed_at_us, receipt),
        artifact_sha256: receipt.report().artifact_sha256.clone(),
        validation_report_sha256: receipt.validation_report_sha256().to_owned(),
    }
}

#[derive(Debug, Clone)]
struct ActiveFrozenExportAttempt {
    id: JobId,
    output_path: PathBuf,
}

#[derive(Debug, Clone)]
enum FrozenExportRecovery {
    Running {
        cycle_index: u32,
    },
    Cancelled {
        cycle_index: u32,
        cancelled_job_id: JobId,
        cancelled_generation: u64,
        cancelled_output_path: PathBuf,
        cancellation_requests_before: u64,
        cancellations_before: u64,
        too_late_before: u64,
    },
    Retry {
        cycle_index: u32,
        cancelled_job_id: JobId,
        cancelled_generation: u64,
        cancelled_output_path: PathBuf,
        cancelled_terminal_sha256: String,
        cancellation_requests_before: u64,
        cancellations_before: u64,
        cancellations_after: u64,
        too_late_before: u64,
    },
}

/// Owner-derived facts accepted by the central recovery receipt sealer.
pub(super) struct ExportCancelRetryFacts {
    cycle_index: u32,
    operation_id: String,
    cancelled_job_id: String,
    retry_job_id: String,
    cancellation_count_before: u64,
    cancellation_count_after: u64,
    cancelled_terminal_sha256: String,
    retry_artifact_sha256: String,
    retry_validation_report_sha256: String,
}

impl ExportCancelRetryFacts {
    pub(super) const fn cycle_index(&self) -> u32 {
        self.cycle_index
    }

    pub(super) fn operation_id(&self) -> &str {
        &self.operation_id
    }

    pub(super) fn cancelled_job_id(&self) -> &str {
        &self.cancelled_job_id
    }

    pub(super) fn retry_job_id(&self) -> &str {
        &self.retry_job_id
    }

    pub(super) const fn cancellation_count_before(&self) -> u64 {
        self.cancellation_count_before
    }

    pub(super) const fn cancellation_count_after(&self) -> u64 {
        self.cancellation_count_after
    }

    pub(super) fn cancelled_terminal_sha256(&self) -> &str {
        &self.cancelled_terminal_sha256
    }

    pub(super) fn retry_artifact_sha256(&self) -> &str {
        &self.retry_artifact_sha256
    }

    pub(super) fn retry_validation_report_sha256(&self) -> &str {
        &self.retry_validation_report_sha256
    }
}

struct FrozenRepeatedExportState<B> {
    plan: FrozenExportPlan,
    backend: B,
    active: Option<ActiveFrozenExportAttempt>,
    next_ordinal: u64,
    verified_artifacts: u64,
    recovery: Option<FrozenExportRecovery>,
    closing: bool,
    fault: Option<String>,
}

/// Sequential phase owner that repeats one frozen Export and verifies every artifact.
pub struct FrozenRepeatedExportPhase {
    state: FrozenRepeatedExportState<ProductionFrozenExportBackend>,
}

impl FrozenRepeatedExportPhase {
    /// Capture one immutable snapshot and admit the first phase-owned attempt.
    pub fn start(
        app: &AppState,
        request: FrozenRepeatedExportRequest,
    ) -> Result<Self, FrozenRepeatedExportError> {
        let (plan, backend) = FrozenExportPlan::capture(app, request)?;
        Ok(Self {
            state: FrozenRepeatedExportState::start_with_backend(plan, backend)?,
        })
    }

    /// Observe one attempt, seal a verified event, or admit the next serial attempt.
    pub fn poll(
        &mut self,
        completed_at_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, FrozenRepeatedExportError> {
        self.state.poll(completed_at_us)
    }

    /// Request one controlled cancellation and distinct verified retry.
    ///
    /// Cancellation is not asserted until a later poll observes the exact
    /// phase-owned attempt executing inside the reversible publication region.
    pub fn begin_cancel_retry_recovery(
        &mut self,
        cycle_index: u32,
    ) -> Result<(), FrozenRepeatedExportError> {
        self.state.begin_cancel_retry_recovery(cycle_index)
    }

    /// Whether one controlled cancellation/retry operation is incomplete.
    pub const fn cancel_retry_recovery_in_progress(&self) -> bool {
        self.state.cancel_retry_recovery_in_progress()
    }

    /// Stop new admission while allowing the active attempt to publish and verify.
    pub fn begin_close(&mut self) {
        self.state.begin_close();
    }

    /// Whether no current attempt or terminal queue evidence remains.
    pub fn is_quiescent(&self) -> bool {
        self.state.is_quiescent()
    }

    /// Number of independently verified artifacts completed by this phase owner.
    pub const fn verified_artifacts(&self) -> u64 {
        self.state.verified_artifacts()
    }

    /// Close the legal between-attempt gap before a controlled recovery.
    ///
    /// Callers must first perform one ordinary poll so an already completed
    /// attempt is verified and removed before this admits its successor.
    pub fn ensure_attempt_admitted_for_recovery(
        &mut self,
    ) -> Result<(), FrozenRepeatedExportError> {
        self.state.ensure_attempt_admitted_for_recovery()
    }
}

impl<B: FrozenExportBackend> FrozenRepeatedExportState<B> {
    fn start_with_backend(
        plan: FrozenExportPlan,
        backend: B,
    ) -> Result<Self, FrozenRepeatedExportError> {
        if backend.retained_jobs() != 0 {
            return Err(FrozenRepeatedExportError::ContaminatedQueue);
        }
        let mut phase = Self {
            plan,
            backend,
            active: None,
            next_ordinal: 1,
            verified_artifacts: 0,
            recovery: None,
            closing: false,
            fault: None,
        };
        phase.enqueue_next()?;
        Ok(phase)
    }

    /// Observe one attempt, seal a verified event, or admit the next serial attempt.
    fn poll(
        &mut self,
        completed_at_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, FrozenRepeatedExportError> {
        if let Some(detail) = &self.fault {
            return Err(FrozenRepeatedExportError::Faulted(detail.clone()));
        }
        if self.recovery.is_some() {
            return self.poll_cancel_retry_recovery(completed_at_us);
        }
        let Some(active) = self.active.clone() else {
            if !self.closing {
                self.enqueue_next()?;
            }
            return Ok(Vec::new());
        };
        let retained_jobs = self.backend.retained_jobs();
        if retained_jobs != 1 {
            return Err(self.latch_fault(format!(
                "repeated Export phase expected exactly one owned job, found {retained_jobs}"
            )));
        }
        let observation =
            self.backend.observe(active.id).map_err(|detail| self.latch_fault(detail))?;
        match observation {
            FrozenExportAttemptObservation::Pending { .. }
            | FrozenExportAttemptObservation::Running { .. }
            | FrozenExportAttemptObservation::Cancelling { .. } => Ok(Vec::new()),
            FrozenExportAttemptObservation::Failed(detail) => {
                Err(self.latch_fault(format!("phase-owned Export failed: {detail}")))
            }
            FrozenExportAttemptObservation::Cancelled(_) => {
                Err(self.latch_fault("continuous Export attempt was cancelled".to_owned()))
            }
            FrozenExportAttemptObservation::Completed { generation: _, output_path } => {
                if output_path != active.output_path {
                    return Err(self.latch_fault(
                        "durable Export path differs from the admitted attempt".to_owned(),
                    ));
                }
                let verified = self
                    .backend
                    .verify(active.id, &output_path, completed_at_us)
                    .map_err(|detail| self.latch_fault(detail))?;
                let removed = self.backend.clear_terminal_history();
                if removed != 1 {
                    return Err(self.latch_fault(format!(
                        "phase-owned Export terminal cleanup removed {removed} jobs"
                    )));
                }
                self.active = None;
                self.verified_artifacts =
                    self.verified_artifacts.checked_add(1).ok_or_else(|| {
                        self.latch_fault("verified Export artifact counter overflow".to_owned())
                    })?;
                Ok(vec![verified.event])
            }
        }
    }

    fn poll_cancel_retry_recovery(
        &mut self,
        completed_at_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, FrozenRepeatedExportError> {
        let recovery = self
            .recovery
            .take()
            .ok_or_else(|| self.latch_fault("Export cancel/retry state disappeared".to_owned()))?;
        let active = self.active.clone().ok_or_else(|| {
            self.latch_fault("Export cancel/retry lost its active attempt".to_owned())
        })?;
        if self.backend.retained_jobs() != 1 {
            return Err(self.latch_fault(
                "Export cancel/retry requires exactly one retained owned job".to_owned(),
            ));
        }
        let observation =
            self.backend.observe(active.id).map_err(|detail| self.latch_fault(detail))?;

        match recovery {
            FrozenExportRecovery::Running { cycle_index } => match observation {
                FrozenExportAttemptObservation::Pending {
                    generation,
                    output_path,
                    executed,
                    publication,
                } => {
                    validate_active_attempt(
                        &active,
                        generation,
                        &output_path,
                        executed,
                        publication,
                        false,
                    )
                    .map_err(|detail| self.latch_fault(detail))?;
                    self.recovery = Some(FrozenExportRecovery::Running { cycle_index });
                    Ok(Vec::new())
                }
                FrozenExportAttemptObservation::Running {
                    generation,
                    output_path,
                    executed,
                    publication,
                } => {
                    validate_active_attempt(
                        &active,
                        generation,
                        &output_path,
                        executed,
                        publication,
                        true,
                    )
                    .map_err(|detail| self.latch_fault(detail))?;
                    let before = self.backend.diagnostics();
                    if before.committing != 0 || before.cancelling != 0 {
                        return Err(self.latch_fault(
                            "Export cancel/retry observed a contaminated or irreversible queue"
                                .to_owned(),
                        ));
                    }
                    match self.backend.cancel(active.id) {
                        ExportCancelOutcome::Requested => {}
                        outcome => {
                            return Err(self.latch_fault(format!(
                                "Export cancel/retry cancellation was not accepted: {outcome:?}"
                            )));
                        }
                    }
                    self.recovery = Some(FrozenExportRecovery::Cancelled {
                        cycle_index,
                        cancelled_job_id: active.id,
                        cancelled_generation: generation,
                        cancelled_output_path: active.output_path,
                        cancellation_requests_before: before.cancellation_requests,
                        cancellations_before: before.cancellations,
                        too_late_before: before.too_late_cancellation_requests,
                    });
                    Ok(Vec::new())
                }
                FrozenExportAttemptObservation::Cancelling { .. } => Err(self.latch_fault(
                    "phase-owned Export was cancelling before recovery asserted authority"
                        .to_owned(),
                )),
                FrozenExportAttemptObservation::Completed { .. } => Err(self.latch_fault(
                    "phase-owned Export completed before controlled cancellation".to_owned(),
                )),
                FrozenExportAttemptObservation::Failed(detail) => Err(self.latch_fault(format!(
                    "phase-owned Export failed before controlled cancellation: {detail}"
                ))),
                FrozenExportAttemptObservation::Cancelled(_) => Err(self.latch_fault(
                    "phase-owned Export was cancelled outside recovery authority".to_owned(),
                )),
            },
            FrozenExportRecovery::Cancelled {
                cycle_index,
                cancelled_job_id,
                cancelled_generation,
                cancelled_output_path,
                cancellation_requests_before,
                cancellations_before,
                too_late_before,
            } => match observation {
                FrozenExportAttemptObservation::Cancelling {
                    generation,
                    output_path,
                    executed,
                    publication,
                } => {
                    validate_cancel_pending_attempt(
                        cancelled_job_id,
                        cancelled_generation,
                        &cancelled_output_path,
                        &active,
                        generation,
                        &output_path,
                        executed,
                        publication,
                    )
                    .map_err(|detail| self.latch_fault(detail))?;
                    self.recovery = Some(FrozenExportRecovery::Cancelled {
                        cycle_index,
                        cancelled_job_id,
                        cancelled_generation,
                        cancelled_output_path,
                        cancellation_requests_before,
                        cancellations_before,
                        too_late_before,
                    });
                    Ok(Vec::new())
                }
                FrozenExportAttemptObservation::Cancelled(terminal) => {
                    validate_cancelled_terminal(
                        &terminal,
                        cancelled_job_id,
                        cancelled_generation,
                        &cancelled_output_path,
                    )
                    .map_err(|detail| self.latch_fault(detail))?;
                    let after = self.backend.diagnostics();
                    if after.cancellation_requests != cancellation_requests_before.saturating_add(1)
                        || after.cancellations != cancellations_before.saturating_add(1)
                        || after.too_late_cancellation_requests != too_late_before
                        || after.terminal != 1
                        || after.pending != 0
                        || after.running != 0
                        || after.cancelling != 0
                        || after.committing != 0
                    {
                        return Err(self.latch_fault(
                            "Export cancellation counters or terminal gauges did not close exactly"
                                .to_owned(),
                        ));
                    }
                    let cancelled_terminal_sha256 = canonical_cancelled_terminal_sha256(&terminal)
                        .map_err(|detail| self.latch_fault(detail))?;
                    let removed = self.backend.clear_terminal_history();
                    if removed != 1 || self.backend.retained_jobs() != 0 {
                        return Err(self.latch_fault(format!(
                            "cancelled Export cleanup removed {removed} jobs or retained ownership"
                        )));
                    }
                    self.active = None;
                    self.enqueue_next()?;
                    let retry = self.active.clone().ok_or_else(|| {
                        self.latch_fault("Export retry admission lost its identity".to_owned())
                    })?;
                    if retry.id == cancelled_job_id || retry.output_path == cancelled_output_path {
                        return Err(self.latch_fault(
                            "Export retry reused the cancelled identity or output path".to_owned(),
                        ));
                    }
                    self.recovery = Some(FrozenExportRecovery::Retry {
                        cycle_index,
                        cancelled_job_id,
                        cancelled_generation,
                        cancelled_output_path,
                        cancelled_terminal_sha256,
                        cancellation_requests_before,
                        cancellations_before,
                        cancellations_after: after.cancellations,
                        too_late_before,
                    });
                    Ok(Vec::new())
                }
                FrozenExportAttemptObservation::Pending { .. }
                | FrozenExportAttemptObservation::Running { .. } => Err(self.latch_fault(
                    "accepted Export cancellation regressed to a non-cancelling state".to_owned(),
                )),
                FrozenExportAttemptObservation::Completed { .. } => Err(self.latch_fault(
                    "accepted Export cancellation crossed into publication".to_owned(),
                )),
                FrozenExportAttemptObservation::Failed(detail) => Err(self.latch_fault(format!(
                    "cancelled Export failed instead of reaching Canceled: {detail}"
                ))),
            },
            FrozenExportRecovery::Retry {
                cycle_index,
                cancelled_job_id,
                cancelled_generation,
                cancelled_output_path,
                cancelled_terminal_sha256,
                cancellation_requests_before,
                cancellations_before,
                cancellations_after,
                too_late_before,
            } => match observation {
                FrozenExportAttemptObservation::Pending { generation, output_path, .. }
                | FrozenExportAttemptObservation::Running { generation, output_path, .. } => {
                    validate_retry_identity(
                        &active,
                        generation,
                        &output_path,
                        cancelled_job_id,
                        cancelled_generation,
                        &cancelled_output_path,
                    )
                    .map_err(|detail| self.latch_fault(detail))?;
                    self.recovery = Some(FrozenExportRecovery::Retry {
                        cycle_index,
                        cancelled_job_id,
                        cancelled_generation,
                        cancelled_output_path,
                        cancelled_terminal_sha256,
                        cancellation_requests_before,
                        cancellations_before,
                        cancellations_after,
                        too_late_before,
                    });
                    Ok(Vec::new())
                }
                FrozenExportAttemptObservation::Completed { generation, output_path } => {
                    validate_retry_identity(
                        &active,
                        generation,
                        &output_path,
                        cancelled_job_id,
                        cancelled_generation,
                        &cancelled_output_path,
                    )
                    .map_err(|detail| self.latch_fault(detail))?;
                    let verified = self
                        .backend
                        .verify(active.id, &output_path, completed_at_us)
                        .map_err(|detail| self.latch_fault(detail))?;
                    let removed = self.backend.clear_terminal_history();
                    if removed != 1 {
                        return Err(self.latch_fault(format!(
                            "retry Export terminal cleanup removed {removed} jobs"
                        )));
                    }
                    let final_diagnostics = self.backend.diagnostics();
                    if self.backend.retained_jobs() != 0
                        || final_diagnostics.cancellation_requests
                            != cancellation_requests_before.saturating_add(1)
                        || final_diagnostics.cancellations != cancellations_after
                        || final_diagnostics.cancellations != cancellations_before.saturating_add(1)
                        || final_diagnostics.too_late_cancellation_requests != too_late_before
                        || final_diagnostics.terminal != 0
                        || final_diagnostics.pending != 0
                        || final_diagnostics.running != 0
                        || final_diagnostics.cancelling != 0
                        || final_diagnostics.committing != 0
                    {
                        return Err(self.latch_fault(
                            "Export retry left residual jobs or changed cancellation evidence"
                                .to_owned(),
                        ));
                    }
                    let retry_job_id = active.id.to_string();
                    let facts = ExportCancelRetryFacts {
                        cycle_index,
                        operation_id: format!(
                            "export.c{cycle_index}.{}.{}",
                            cancelled_job_id, active.id
                        ),
                        cancelled_job_id: cancelled_job_id.to_string(),
                        retry_job_id,
                        cancellation_count_before: cancellations_before,
                        cancellation_count_after: cancellations_after,
                        cancelled_terminal_sha256,
                        retry_artifact_sha256: verified.artifact_sha256,
                        retry_validation_report_sha256: verified.validation_report_sha256,
                    };
                    let receipt =
                        EnduranceRecoveryOperationReceipt::from_export_cancel_retry_facts(facts)
                            .map_err(|error| {
                                self.latch_fault(format!(
                                    "seal Export cancel/retry recovery receipt: {error}"
                                ))
                            })?;
                    self.active = None;
                    self.verified_artifacts =
                        self.verified_artifacts.checked_add(1).ok_or_else(|| {
                            self.latch_fault("verified Export artifact counter overflow".to_owned())
                        })?;
                    Ok(vec![
                        verified.event,
                        EnduranceCampaignEvent::recovery_step_completed(completed_at_us, &receipt),
                    ])
                }
                FrozenExportAttemptObservation::Cancelling { .. }
                | FrozenExportAttemptObservation::Cancelled(_) => {
                    Err(self.latch_fault("distinct Export retry was cancelled".to_owned()))
                }
                FrozenExportAttemptObservation::Failed(detail) => {
                    Err(self.latch_fault(format!("distinct Export retry failed: {detail}")))
                }
            },
        }
    }

    /// Stop new admission while allowing the active attempt to publish and verify.
    fn begin_close(&mut self) {
        self.closing = true;
        if matches!(self.recovery, Some(FrozenExportRecovery::Running { .. })) {
            self.recovery = None;
        }
    }

    /// Whether no current attempt or terminal queue evidence remains.
    fn is_quiescent(&self) -> bool {
        self.closing
            && self.active.is_none()
            && self.fault.is_none()
            && self.backend.retained_jobs() == 0
    }

    /// Number of independently verified artifacts completed by this phase owner.
    const fn verified_artifacts(&self) -> u64 {
        self.verified_artifacts
    }

    const fn cancel_retry_recovery_in_progress(&self) -> bool {
        self.recovery.is_some()
    }

    fn ensure_attempt_admitted_for_recovery(&mut self) -> Result<(), FrozenRepeatedExportError> {
        if let Some(detail) = &self.fault {
            return Err(FrozenRepeatedExportError::Faulted(detail.clone()));
        }
        if self.closing || self.recovery.is_some() {
            return Err(self.latch_fault(
                "cannot admit an Export recovery attempt while closing or already recovering"
                    .to_owned(),
            ));
        }
        if self.active.is_none() {
            self.enqueue_next()?;
        }
        if self.backend.retained_jobs() != 1 {
            return Err(self.latch_fault(
                "Export recovery admission did not retain exactly one phase-owned attempt"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    fn begin_cancel_retry_recovery(
        &mut self,
        cycle_index: u32,
    ) -> Result<(), FrozenRepeatedExportError> {
        if let Some(detail) = &self.fault {
            return Err(FrozenRepeatedExportError::Faulted(detail.clone()));
        }
        if self.closing {
            return Err(self.latch_fault(
                "cannot begin Export cancel/retry while the phase is closing".to_owned(),
            ));
        }
        if self.recovery.is_some() {
            return Err(
                self.latch_fault("an Export cancel/retry recovery is already active".to_owned())
            );
        }
        if self.active.is_none() {
            return Err(self.latch_fault(
                "Export cancel/retry requires one admitted phase-owned attempt".to_owned(),
            ));
        }
        self.recovery = Some(FrozenExportRecovery::Running { cycle_index });
        Ok(())
    }

    fn enqueue_next(&mut self) -> Result<(), FrozenRepeatedExportError> {
        let retained_jobs = self.backend.retained_jobs();
        if retained_jobs != 0 {
            return Err(self.latch_fault(format!(
                "repeated Export phase cannot admit beside {retained_jobs} retained jobs"
            )));
        }
        let ordinal = self.next_ordinal;
        let config = self.plan.config(ordinal);
        let output_path = config.output_path.clone();
        let id = self.backend.enqueue(config).map_err(|detail| self.latch_fault(detail))?;
        self.next_ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| self.latch_fault("Export attempt ordinal overflow".to_owned()))?;
        self.active = Some(ActiveFrozenExportAttempt { id, output_path });
        Ok(())
    }

    fn latch_fault(&mut self, detail: String) -> FrozenRepeatedExportError {
        self.fault = Some(detail.clone());
        FrozenRepeatedExportError::Faulted(detail)
    }
}

fn validate_active_attempt(
    active: &ActiveFrozenExportAttempt,
    generation: u64,
    output_path: &Path,
    executed: bool,
    publication: ExportPublicationState,
    require_executed: bool,
) -> Result<(), String> {
    if generation == 0 || output_path != active.output_path {
        return Err("phase-owned Export active identity changed".to_owned());
    }
    if publication != ExportPublicationState::Reversible {
        return Err("phase-owned Export is outside reversible publication authority".to_owned());
    }
    if require_executed && !executed {
        return Err("phase-owned Export reported Running without execution evidence".to_owned());
    }
    if !require_executed && executed {
        return Err("phase-owned Export reported Pending after execution began".to_owned());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_cancel_pending_attempt(
    cancelled_job_id: JobId,
    cancelled_generation: u64,
    cancelled_output_path: &Path,
    active: &ActiveFrozenExportAttempt,
    generation: u64,
    output_path: &Path,
    executed: bool,
    publication: ExportPublicationState,
) -> Result<(), String> {
    if active.id != cancelled_job_id
        || active.output_path != cancelled_output_path
        || generation != cancelled_generation
        || output_path != cancelled_output_path
        || !executed
        || publication != ExportPublicationState::Reversible
    {
        return Err(
            "cancelling Export no longer matches its admitted reversible attempt".to_owned(),
        );
    }
    Ok(())
}

fn validate_cancelled_terminal(
    terminal: &FrozenCancelledExportTerminal,
    cancelled_job_id: JobId,
    cancelled_generation: u64,
    cancelled_output_path: &Path,
) -> Result<(), String> {
    let Some(evidence) = terminal.terminal_evidence else {
        return Err("cancelled Export lacks terminal execution evidence".to_owned());
    };
    if terminal.job_id != cancelled_job_id
        || terminal.generation != cancelled_generation
        || terminal.output_path != cancelled_output_path
        || !terminal.executed
        || terminal.publication != ExportPublicationState::NotPublished
        || terminal.artifact_publication.is_some()
        || evidence.generation != cancelled_generation
        || evidence.priority != ExecutionPriority::UserInitiated
        || evidence.disposition != ExecutionTerminalDisposition::Canceled
        || evidence.deadline != ExecutionDeadlineStatus::NotApplicable
    {
        return Err(
            "cancelled Export terminal is not exact Canceled/NotPublished evidence".to_owned(),
        );
    }
    Ok(())
}

fn validate_retry_identity(
    retry: &ActiveFrozenExportAttempt,
    retry_generation: u64,
    retry_output_path: &Path,
    cancelled_job_id: JobId,
    cancelled_generation: u64,
    cancelled_output_path: &Path,
) -> Result<(), String> {
    if retry.id == cancelled_job_id
        || retry.output_path == cancelled_output_path
        || retry.output_path != retry_output_path
        || retry_generation <= cancelled_generation
        || retry_generation == 0
    {
        return Err(
            "Export retry did not use a distinct newer identity and create-only path".to_owned(),
        );
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalCancelledExportTerminal<'a> {
    schema_version: u32,
    job_id: String,
    generation: u64,
    output_path: &'a Path,
    executed: bool,
    publication: ExportPublicationState,
    terminal_evidence: ExecutionTerminalEvidence,
    artifact_publication_absent: bool,
}

fn canonical_cancelled_terminal_sha256(
    terminal: &FrozenCancelledExportTerminal,
) -> Result<String, String> {
    let evidence = terminal
        .terminal_evidence
        .ok_or_else(|| "cancelled Export lacks terminal evidence for hashing".to_owned())?;
    let canonical = CanonicalCancelledExportTerminal {
        schema_version: 1,
        job_id: terminal.job_id.to_string(),
        generation: terminal.generation,
        output_path: &terminal.output_path,
        executed: terminal.executed,
        publication: terminal.publication,
        terminal_evidence: evidence,
        artifact_publication_absent: terminal.artifact_publication.is_none(),
    };
    let bytes = serde_json::to_vec(&canonical).map_err(|error| error.to_string())?;
    Ok(Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Stable repeated-Export phase failure.
#[derive(Debug, Error)]
pub enum FrozenRepeatedExportError {
    /// Frozen request, author snapshot, output route, or delivery contract was invalid.
    #[error("invalid frozen Export phase plan: {0}")]
    InvalidPlan(String),
    /// The App queue retained work before this phase attempted to start.
    #[error("repeated Export requires a fresh empty phase queue")]
    ContaminatedQueue,
    /// A started attempt, publication, verification, or exact cleanup failed.
    #[error("repeated Export phase failed: {0}")]
    Faulted(String),
}

fn validate_single_file_preset(preset: &ExportPreset) -> Result<(), FrozenRepeatedExportError> {
    if preset.media_file().is_none()
        || preset.professional_delivery().is_some()
        || preset.image_sequence_format().is_some()
        || preset.audio_stem_format().is_some()
    {
        return Err(FrozenRepeatedExportError::InvalidPlan(
            "independent repeated verification currently requires one media-file artifact"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_artifact_prefix(prefix: &str) -> Result<(), FrozenRepeatedExportError> {
    if prefix.is_empty()
        || prefix.len() > MAXIMUM_ARTIFACT_PREFIX_BYTES
        || matches!(prefix, "." | "..")
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(FrozenRepeatedExportError::InvalidPlan(
            "artifact prefix must be a non-empty link-free ASCII token".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_existing_directory(path: &Path) -> Result<PathBuf, FrozenRepeatedExportError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| FrozenRepeatedExportError::InvalidPlan(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(FrozenRepeatedExportError::InvalidPlan(
            "output directory must be an existing real directory".to_owned(),
        ));
    }
    path.canonicalize()
        .map_err(|error| FrozenRepeatedExportError::InvalidPlan(error.to_string()))
}

fn artifact_path(directory: &Path, prefix: &str, extension: &str, ordinal: u64) -> PathBuf {
    directory.join(format!("{prefix}-{ordinal:08}.{extension}"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use mondrian_export::preset::{BuiltinExportPreset, TimelineExportSnapshot};
    use mondrian_timeline::sequence::Sequence;

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Debug, Clone)]
    enum FakeTerminal {
        Completed,
        Failed,
        Cancelled,
        WrongPath,
    }

    struct FakeBackend {
        configs: Vec<ExportConfig>,
        active: Option<JobId>,
        terminal: FakeTerminal,
        verify_fails: bool,
        cleanup_count: usize,
        timeline_bytes: Vec<Vec<u8>>,
    }

    impl FakeBackend {
        fn clean() -> Self {
            Self {
                configs: Vec::new(),
                active: None,
                terminal: FakeTerminal::Completed,
                verify_fails: false,
                cleanup_count: 1,
                timeline_bytes: Vec::new(),
            }
        }
    }

    impl FrozenExportBackend for FakeBackend {
        fn retained_jobs(&self) -> usize {
            usize::from(self.active.is_some())
        }

        fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String> {
            let id = JobId::new();
            self.timeline_bytes.push(
                serde_json::to_vec(config.timeline.as_ref()).map_err(|error| error.to_string())?,
            );
            self.configs.push(config);
            self.active = Some(id);
            Ok(id)
        }

        fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String> {
            if self.active != Some(id) {
                return Err("unknown fake attempt".to_owned());
            }
            let path = self
                .configs
                .last()
                .map(|config| config.output_path.clone())
                .ok_or_else(|| "missing fake config".to_owned())?;
            let generation = u64::try_from(self.configs.len())
                .map_err(|_| "fake generation overflow".to_owned())?;
            Ok(match self.terminal {
                FakeTerminal::Completed => {
                    FrozenExportAttemptObservation::Completed { generation, output_path: path }
                }
                FakeTerminal::Failed => {
                    FrozenExportAttemptObservation::Failed("injected failure".to_owned())
                }
                FakeTerminal::Cancelled => {
                    FrozenExportAttemptObservation::Cancelled(FrozenCancelledExportTerminal {
                        job_id: id,
                        generation,
                        output_path: path,
                        executed: true,
                        publication: ExportPublicationState::NotPublished,
                        terminal_evidence: Some(ExecutionTerminalEvidence {
                            generation,
                            priority: ExecutionPriority::UserInitiated,
                            disposition: ExecutionTerminalDisposition::Canceled,
                            deadline: ExecutionDeadlineStatus::NotApplicable,
                        }),
                        artifact_publication: None,
                    })
                }
                FakeTerminal::WrongPath => FrozenExportAttemptObservation::Completed {
                    generation,
                    output_path: path.with_extension("wrong"),
                },
            })
        }

        fn verify(
            &mut self,
            id: JobId,
            _output_path: &Path,
            completed_at_us: u64,
        ) -> Result<VerifiedFrozenExportArtifact, String> {
            if self.verify_fails {
                return Err("injected verifier failure".to_owned());
            }
            Ok(VerifiedFrozenExportArtifact {
                event: EnduranceCampaignEvent::test_export_artifact_verified(
                    completed_at_us,
                    format!("fake-{id}"),
                    SHA,
                    "fake-verifier",
                    SHA,
                ),
                artifact_sha256: SHA.to_owned(),
                validation_report_sha256: SHA.to_owned(),
            })
        }

        fn diagnostics(&self) -> ExportQueueDiagnostics {
            ExportQueueDiagnostics {
                terminal: usize::from(
                    self.active.is_some() && !matches!(self.terminal, FakeTerminal::Failed),
                ),
                ..ExportQueueDiagnostics::default()
            }
        }

        fn cancel(&mut self, _id: JobId) -> ExportCancelOutcome {
            ExportCancelOutcome::AlreadyTerminal
        }

        fn clear_terminal_history(&mut self) -> usize {
            self.active = None;
            self.cleanup_count
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RecoveryFakeStage {
        Pending,
        Running,
        Cancelling,
        Cancelled,
        Completed,
        Failed,
    }

    struct RecoveryFakeBackend {
        configs: Vec<ExportConfig>,
        ids: Vec<JobId>,
        active: Option<JobId>,
        stage: RecoveryFakeStage,
        initial_stage: RecoveryFakeStage,
        retry_stage: RecoveryFakeStage,
        cancel_outcome: ExportCancelOutcome,
        cancellation_requests: u64,
        cancellations: u64,
        too_late_cancellations: u64,
        terminal_executed: bool,
        cleanup_count: usize,
        verify_fails: bool,
    }

    impl RecoveryFakeBackend {
        fn successful() -> Self {
            Self {
                configs: Vec::new(),
                ids: Vec::new(),
                active: None,
                stage: RecoveryFakeStage::Pending,
                initial_stage: RecoveryFakeStage::Running,
                retry_stage: RecoveryFakeStage::Completed,
                cancel_outcome: ExportCancelOutcome::Requested,
                cancellation_requests: 11,
                cancellations: 7,
                too_late_cancellations: 3,
                terminal_executed: true,
                cleanup_count: 1,
                verify_fails: false,
            }
        }

        fn generation(&self) -> Result<u64, String> {
            u64::try_from(self.configs.len()).map_err(|_| "fake generation overflow".to_owned())
        }

        fn output_path(&self) -> Result<PathBuf, String> {
            self.configs
                .last()
                .map(|config| config.output_path.clone())
                .ok_or_else(|| "missing recovery fake config".to_owned())
        }
    }

    impl FrozenExportBackend for RecoveryFakeBackend {
        fn retained_jobs(&self) -> usize {
            usize::from(self.active.is_some())
        }

        fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String> {
            let id = JobId::new();
            self.configs.push(config);
            self.ids.push(id);
            self.active = Some(id);
            self.stage = if self.configs.len() == 1 {
                self.initial_stage
            } else {
                self.retry_stage
            };
            Ok(id)
        }

        fn observe(&mut self, id: JobId) -> Result<FrozenExportAttemptObservation, String> {
            if self.active != Some(id) {
                return Err("unknown recovery fake attempt".to_owned());
            }
            let generation = self.generation()?;
            let output_path = self.output_path()?;
            Ok(match self.stage {
                RecoveryFakeStage::Pending => FrozenExportAttemptObservation::Pending {
                    generation,
                    output_path,
                    executed: false,
                    publication: ExportPublicationState::Reversible,
                },
                RecoveryFakeStage::Running => FrozenExportAttemptObservation::Running {
                    generation,
                    output_path,
                    executed: true,
                    publication: ExportPublicationState::Reversible,
                },
                RecoveryFakeStage::Cancelling => FrozenExportAttemptObservation::Cancelling {
                    generation,
                    output_path,
                    executed: true,
                    publication: ExportPublicationState::Reversible,
                },
                RecoveryFakeStage::Cancelled => {
                    FrozenExportAttemptObservation::Cancelled(FrozenCancelledExportTerminal {
                        job_id: id,
                        generation,
                        output_path,
                        executed: self.terminal_executed,
                        publication: ExportPublicationState::NotPublished,
                        terminal_evidence: Some(ExecutionTerminalEvidence {
                            generation,
                            priority: ExecutionPriority::UserInitiated,
                            disposition: ExecutionTerminalDisposition::Canceled,
                            deadline: ExecutionDeadlineStatus::NotApplicable,
                        }),
                        artifact_publication: None,
                    })
                }
                RecoveryFakeStage::Completed => {
                    FrozenExportAttemptObservation::Completed { generation, output_path }
                }
                RecoveryFakeStage::Failed => {
                    FrozenExportAttemptObservation::Failed("injected retry failure".to_owned())
                }
            })
        }

        fn verify(
            &mut self,
            id: JobId,
            _output_path: &Path,
            completed_at_us: u64,
        ) -> Result<VerifiedFrozenExportArtifact, String> {
            if self.verify_fails {
                return Err("injected recovery verifier failure".to_owned());
            }
            Ok(VerifiedFrozenExportArtifact {
                event: EnduranceCampaignEvent::test_export_artifact_verified(
                    completed_at_us,
                    format!("recovery-{id}"),
                    SHA,
                    "fake-verifier",
                    SHA,
                ),
                artifact_sha256: SHA.to_owned(),
                validation_report_sha256: SHA.to_owned(),
            })
        }

        fn diagnostics(&self) -> ExportQueueDiagnostics {
            ExportQueueDiagnostics {
                pending: usize::from(
                    self.active.is_some() && self.stage == RecoveryFakeStage::Pending,
                ),
                running: usize::from(
                    self.active.is_some() && self.stage == RecoveryFakeStage::Running,
                ),
                cancelling: usize::from(
                    self.active.is_some() && self.stage == RecoveryFakeStage::Cancelling,
                ),
                terminal: usize::from(
                    self.active.is_some()
                        && matches!(
                            self.stage,
                            RecoveryFakeStage::Cancelled
                                | RecoveryFakeStage::Completed
                                | RecoveryFakeStage::Failed
                        ),
                ),
                cancellation_requests: self.cancellation_requests,
                too_late_cancellation_requests: self.too_late_cancellations,
                cancellations: self.cancellations,
                ..ExportQueueDiagnostics::default()
            }
        }

        fn cancel(&mut self, id: JobId) -> ExportCancelOutcome {
            if self.active != Some(id) {
                return ExportCancelOutcome::NotFound;
            }
            match self.cancel_outcome {
                ExportCancelOutcome::Requested => {
                    self.cancellation_requests = self.cancellation_requests.saturating_add(1);
                    self.cancellations = self.cancellations.saturating_add(1);
                    self.stage = RecoveryFakeStage::Cancelled;
                }
                ExportCancelOutcome::TooLateCommitting => {
                    self.too_late_cancellations = self.too_late_cancellations.saturating_add(1);
                }
                _ => {}
            }
            self.cancel_outcome
        }

        fn clear_terminal_history(&mut self) -> usize {
            self.active = None;
            self.cleanup_count
        }
    }

    fn test_plan() -> (tempfile::TempDir, FrozenExportPlan) {
        let directory = tempfile::tempdir().expect("temporary output directory");
        let preset = BuiltinExportPreset::H264AacSdr1080p.preset();
        let sequence = Sequence::new("Frozen");
        let timeline = TimelineExportSnapshot::unprepared(
            Default::default(),
            sequence,
            Vec::new(),
            HashMap::new(),
            TimelineExportRange::SequenceInOut,
        );
        let plan = FrozenExportPlan {
            base_config: ExportConfig {
                preset,
                timeline: Box::new(timeline),
                output_path: directory.path().join("placeholder.mp4"),
                output_policy: ExportOutputPolicy::CreateNew,
                smart_render: mondrian_export::ExportSmartRenderPolicy::Automatic,
                broadcast_qc: None,
            },
            output_directory: directory.path().to_path_buf(),
            artifact_prefix: "endurance".to_owned(),
            extension: "mp4",
        };
        (directory, plan)
    }

    #[test]
    fn repeats_one_frozen_snapshot_with_unique_create_new_artifacts() {
        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");

        assert_eq!(phase.poll(10).expect("verify first").len(), 1);
        assert!(phase.poll(11).expect("enqueue second").is_empty());
        assert_eq!(phase.poll(20).expect("verify second").len(), 1);
        assert_eq!(phase.verified_artifacts(), 2);
        assert_eq!(phase.backend.configs.len(), 2);
        assert_ne!(
            phase.backend.configs[0].output_path,
            phase.backend.configs[1].output_path
        );
        assert_eq!(
            phase.backend.timeline_bytes[0],
            phase.backend.timeline_bytes[1]
        );
        assert!(phase
            .backend
            .configs
            .iter()
            .all(|config| config.output_policy == ExportOutputPolicy::CreateNew));
    }

    #[test]
    fn recovery_admission_closes_the_verified_between_attempt_gap() {
        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");

        assert_eq!(phase.poll(10).expect("verify first attempt").len(), 1);
        assert!(phase.active.is_none());
        assert_eq!(phase.backend.retained_jobs(), 0);

        phase
            .ensure_attempt_admitted_for_recovery()
            .expect("admit successor at recovery boundary");

        assert!(phase.active.is_some());
        assert_eq!(phase.backend.retained_jobs(), 1);
        assert_eq!(phase.backend.configs.len(), 2);
    }

    #[test]
    fn close_stops_new_admission_after_current_artifact_is_verified() {
        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");
        phase.begin_close();

        assert_eq!(phase.poll(10).expect("verify terminal attempt").len(), 1);
        assert!(phase.is_quiescent());
        assert!(phase.poll(11).expect("remain closed").is_empty());
        assert_eq!(phase.backend.configs.len(), 1);
    }

    #[test]
    fn failure_cancellation_wrong_path_and_verifier_failure_latch_terminal_fault() {
        for terminal in [
            FakeTerminal::Failed,
            FakeTerminal::Cancelled,
            FakeTerminal::WrongPath,
        ] {
            let (_directory, plan) = test_plan();
            let mut backend = FakeBackend::clean();
            backend.terminal = terminal;
            let mut phase =
                FrozenRepeatedExportState::start_with_backend(plan, backend).expect("start phase");
            assert!(phase.poll(10).is_err());
            assert!(phase.poll(11).is_err());
            assert_eq!(phase.backend.configs.len(), 1);
        }

        let (_directory, plan) = test_plan();
        let mut backend = FakeBackend::clean();
        backend.verify_fails = true;
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, backend).expect("start phase");
        assert!(phase.poll(10).is_err());
        assert!(phase.poll(11).is_err());
        assert_eq!(phase.backend.configs.len(), 1);
    }

    #[test]
    fn contaminated_queue_and_non_exact_cleanup_are_rejected() {
        let (_directory, plan) = test_plan();
        let mut contaminated = FakeBackend::clean();
        contaminated.active = Some(JobId::new());
        assert!(matches!(
            FrozenRepeatedExportState::start_with_backend(plan, contaminated),
            Err(FrozenRepeatedExportError::ContaminatedQueue)
        ));

        let (_directory, plan) = test_plan();
        let mut backend = FakeBackend::clean();
        backend.cleanup_count = 0;
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, backend).expect("start phase");
        assert!(phase.poll(10).is_err());
        assert_eq!(phase.verified_artifacts(), 0);

        let (_directory, plan) = test_plan();
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, FakeBackend::clean())
            .expect("start phase");
        assert_eq!(phase.poll(10).expect("verify first").len(), 1);
        phase.backend.active = Some(JobId::new());
        assert!(phase.poll(11).is_err());
        assert_eq!(phase.backend.configs.len(), 1);
    }

    #[test]
    fn policy_rejects_directory_artifacts_and_unsafe_prefixes() {
        let directory_preset = BuiltinExportPreset::PngSequence.preset();
        assert!(validate_single_file_preset(&directory_preset).is_err());
        for prefix in ["", ".", "..", "contains/slash", "包含非 ASCII"] {
            assert!(validate_artifact_prefix(prefix).is_err());
        }
        let policy = IndependentExportArtifactPolicy::new(1, Duration::from_secs(1))
            .expect("nonzero test policy");
        assert_eq!(policy.maximum_artifact_bytes(), 1);
    }

    #[test]
    fn controlled_cancel_waits_for_running_then_retries_with_distinct_verified_artifact() {
        let (_directory, plan) = test_plan();
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, RecoveryFakeBackend::successful())
                .expect("start recovery phase");
        phase.begin_cancel_retry_recovery(4).expect("request controlled recovery");

        assert!(phase.poll(10).expect("assert cancellation").is_empty());
        assert!(phase.cancel_retry_recovery_in_progress());
        assert_eq!(phase.backend.cancellations, 8);
        assert_eq!(phase.backend.configs.len(), 1);

        assert!(phase.poll(20).expect("close cancelled terminal").is_empty());
        assert_eq!(phase.backend.configs.len(), 2);
        assert_ne!(phase.backend.ids[0], phase.backend.ids[1]);
        assert_ne!(
            phase.backend.configs[0].output_path,
            phase.backend.configs[1].output_path
        );
        assert!(phase
            .backend
            .configs
            .iter()
            .all(|config| config.output_policy == ExportOutputPolicy::CreateNew));

        let events = phase.poll(30).expect("verify distinct retry");
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[0],
            EnduranceCampaignEvent::ExportArtifactVerified(_)
        ));
        let (cycle_index, step, receipt_json, receipt_sha256) =
            events[1].test_recovery_receipt().expect("second event must seal recovery");
        assert_eq!(cycle_index, 4);
        assert_eq!(
            step,
            super::super::endurance_qualification::EnduranceRecoveryStep::ExportCancelRetry
        );
        let receipt =
            EnduranceRecoveryOperationReceipt::parse_and_validate(receipt_json, receipt_sha256)
                .expect("externally replayable recovery receipt");
        assert_eq!(receipt.cycle_index(), 4);
        assert!(!phase.cancel_retry_recovery_in_progress());
        assert_eq!(phase.verified_artifacts(), 1);
        assert_eq!(phase.backend.retained_jobs(), 0);
    }

    #[test]
    fn close_before_cancel_authority_never_cancels_the_active_export() {
        let (_directory, plan) = test_plan();
        let mut backend = RecoveryFakeBackend::successful();
        backend.initial_stage = RecoveryFakeStage::Pending;
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, backend)
            .expect("start pending phase");
        phase.begin_cancel_retry_recovery(0).expect("request recovery");
        phase.begin_close();
        assert!(!phase.cancel_retry_recovery_in_progress());
        assert_eq!(phase.backend.cancellations, 7);

        phase.backend.stage = RecoveryFakeStage::Completed;
        assert_eq!(phase.poll(10).expect("verify closing attempt").len(), 1);
        assert!(phase.is_quiescent());
        assert_eq!(phase.backend.cancellations, 7);
    }

    #[test]
    fn cancel_retry_faults_on_too_late_invalid_terminal_cleanup_retry_or_verifier() {
        let (_directory, plan) = test_plan();
        let mut backend = RecoveryFakeBackend::successful();
        backend.cancel_outcome = ExportCancelOutcome::TooLateCommitting;
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, backend)
            .expect("start too-late phase");
        phase.begin_cancel_retry_recovery(0).expect("request too-late recovery");
        assert!(phase.poll(10).is_err());
        assert!(phase.poll(11).is_err());

        let (_directory, plan) = test_plan();
        let mut backend = RecoveryFakeBackend::successful();
        backend.terminal_executed = false;
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, backend)
            .expect("start invalid-terminal phase");
        phase.begin_cancel_retry_recovery(0).expect("request invalid-terminal recovery");
        assert!(phase.poll(10).is_ok());
        assert!(phase.poll(20).is_err());

        let (_directory, plan) = test_plan();
        let mut backend = RecoveryFakeBackend::successful();
        backend.cleanup_count = 0;
        let mut phase = FrozenRepeatedExportState::start_with_backend(plan, backend)
            .expect("start cleanup-failure phase");
        phase.begin_cancel_retry_recovery(0).expect("request cleanup-failure recovery");
        assert!(phase.poll(10).is_ok());
        assert!(phase.poll(20).is_err());

        for verifier_fails in [false, true] {
            let (_directory, plan) = test_plan();
            let mut backend = RecoveryFakeBackend::successful();
            backend.retry_stage = if verifier_fails {
                RecoveryFakeStage::Completed
            } else {
                RecoveryFakeStage::Failed
            };
            backend.verify_fails = verifier_fails;
            let mut phase = FrozenRepeatedExportState::start_with_backend(plan, backend)
                .expect("start retry-failure phase");
            phase.begin_cancel_retry_recovery(0).expect("request retry-failure recovery");
            assert!(phase.poll(10).is_ok());
            assert!(phase.poll(20).is_ok());
            assert!(phase.poll(30).is_err());
            assert!(phase.poll(31).is_err());
        }
    }
}
