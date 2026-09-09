//! Phase-scoped repeated Export execution over one immutable Timeline snapshot.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mondrian_core::{
    ExecutionCancellationToken, ExecutionDeadlineStatus, ExecutionPriority,
    ExecutionTerminalDisposition, ExecutionTerminalEvidence, JobId, SequenceId,
};
use mondrian_export::preset::{
    ExportConfig, ExportOutputPolicy, ExportPreset, TimelineExportRange,
};
use mondrian_export::queue::{
    ExportArtifactPublicationEvidence, ExportCancelOutcome, ExportJobSnapshot,
    ExportPublicationState, ExportQueueDiagnostics, JobStatus, RenderJob, RenderQueue,
};
use mondrian_export::{
    verify_export_artifact_until, IndependentExportArtifactPolicy, IndependentExportArtifactReceipt,
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
    /// Exact profile phase retaining all joined verifier evidence.
    pub phase_id: String,
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
    /// Explicit externally approved PSE provider, frozen for every attempt.
    pub regulatory_pse: Option<mondrian_export::RegulatoryPseProviderConfig>,
    /// One immutable machine-plan source retained across every attempt and retry.
    pub frozen_ancillary:
        Option<Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>,
    /// Exact BMX runtime retained by the consuming phase owner.
    pub approved_bmx: Option<mondrian_media::BmxRuntimeHandle>,
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
        let mut base_config = app
            .build_timeline_export_config(TimelineExportRequest {
                preset: request.preset,
                sequence_id: request.sequence_id,
                range: request.range,
                output_path: first_output,
                output_policy: ExportOutputPolicy::CreateNew,
                broadcast_qc: request.broadcast_qc,
                regulatory_pse: request.regulatory_pse,
                frozen_ancillary: request
                    .frozen_ancillary
                    .as_ref()
                    .map(|owner| owner.program().clone()),
            })
            .map_err(FrozenRepeatedExportError::InvalidPlan)?;
        base_config.approved_bmx = request.approved_bmx;
        let mut verifier = FrozenArtifactVerifierOwner::default();
        verifier.ancillary = request.frozen_ancillary;
        verifier.phase_id = Some(request.phase_id);
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
                verification_deadline: None,
                last_persisted_terminal_job: None,
                terminal_job_deadline: None,
                verifier,
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

#[derive(Debug)]
struct VerifiedFrozenExportArtifact {
    event: EnduranceCampaignEvent,
    artifact_sha256: String,
    validation_report_sha256: String,
}

trait FrozenExportBackend {
    fn retained_jobs(&self) -> usize;
    fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String>;
    fn observe(&mut self, id: JobId) -> Result<Option<FrozenExportAttemptObservation>, String>;
    fn verify(
        &mut self,
        id: JobId,
        output_path: &Path,
        completed_at_us: u64,
    ) -> Result<Option<VerifiedFrozenExportArtifact>, String>;
    fn verification_pending(&self) -> bool {
        false
    }
    fn diagnostics(&self) -> ExportQueueDiagnostics;
    fn cancel(&mut self, id: JobId) -> ExportCancelOutcome;
    fn clear_terminal_history(&mut self) -> usize;
}

struct ProductionFrozenExportBackend {
    queue: Arc<RenderQueue>,
    verification_policy: IndependentExportArtifactPolicy,
    verification_deadline: Option<Instant>,
    last_persisted_terminal_job: Option<JobId>,
    terminal_job_deadline: Option<(JobId, Instant)>,
    verifier: FrozenArtifactVerifierOwner,
}

/// Actual terminal facts of one phase-owned independent verification thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrozenExportVerifierWorkerReceipt {
    /// Exact Export job whose finished artifact was inspected.
    pub job_id: JobId,
    /// Exact published artifact path.
    pub output_path: PathBuf,
    /// The owning phase consumed the finished native thread handle.
    pub thread_joined: bool,
    /// Full independent result evidence was durably written by this worker.
    pub evidence_persisted: bool,
    /// Actual decoder/probe cleanup, when observed.
    pub native_cleanup: Option<mondrian_media::SupervisedProcessCleanupReceipt>,
    /// Original independent failure, including native and snapshot cleanup facts.
    pub verification_failure: Option<mondrian_export::IndependentExportArtifactFailureEvidence>,
    /// Original panic diagnostic; no successful owner closure is inferred from unwind.
    pub panic: Option<String>,
    /// Worker or evidence-publication failure.
    pub failure: Option<String>,
}

/// Consuming closure of the phase's capacity-one independent verifier owner.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FrozenExportVerifierShutdownReceipt {
    /// Successfully started native worker threads.
    pub workers_started: u64,
    /// Actual finished thread handles consumed by the phase.
    pub workers_joined: u64,
    /// Workers not observed joined at the original cleanup deadline.
    pub workers_remaining: u64,
    /// Handles deliberately retained when bounded cleanup could not finish.
    pub workers_abandoned: u64,
    /// Explicit phase cancellation was sent to the worker and supervised children.
    pub cancellation_requested: bool,
    /// The caller's cleanup deadline elapsed before closure completed.
    pub deadline_exceeded: bool,
    /// Last joined or abandoned worker's actual terminal facts.
    pub last_worker: Option<FrozenExportVerifierWorkerReceipt>,
    /// First owner failure; a later successful close cannot erase it.
    pub failure: Option<String>,
    /// Metadata-only worker ownership, independent of the native verifier receipt.
    pub terminal_publications: FrozenExportTerminalPublicationReceipt,
}

/// Raw terminal job metadata publication performed outside realtime polling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrozenExportTerminalWorkerReceipt {
    /// Exact Export job bound to the sidecar.
    pub job_id: JobId,
    /// Exact published or canceled target path.
    pub output_path: PathBuf,
    /// The owning phase consumed this finished thread handle.
    pub thread_joined: bool,
    /// Create-only sidecar write and durability sync both completed.
    pub evidence_persisted: bool,
    /// Original full serialized terminal snapshot, including failed-publication cases.
    pub terminal_snapshot_json: Option<String>,
    /// Original panic diagnostic, when observed.
    pub panic: Option<String>,
    /// Original worker or publication failure.
    pub failure: Option<String>,
}

/// Bounded cumulative ownership of terminal-only metadata workers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FrozenExportTerminalPublicationReceipt {
    /// Native metadata threads admitted by the phase.
    pub workers_started: u64,
    /// Actual thread handles consumed after completion.
    pub workers_joined: u64,
    /// Threads not observed joined at consuming close.
    pub workers_remaining: u64,
    /// Unjoined handles retained after the original deadline.
    pub workers_abandoned: u64,
    /// Latest metadata operation's complete raw facts.
    pub last_worker: Option<FrozenExportTerminalWorkerReceipt>,
    /// First metadata owner failure.
    pub failure: Option<String>,
}

impl FrozenExportTerminalPublicationReceipt {
    /// Every admitted metadata worker joined and durably preserved its terminal snapshot.
    pub fn all_resources_released(&self) -> bool {
        self.workers_started == self.workers_joined
            && (self.workers_started == 0) == self.last_worker.is_none()
            && self.workers_remaining == 0
            && self.workers_abandoned == 0
            && self.failure.is_none()
            && self.last_worker.as_ref().is_none_or(|worker| {
                worker.thread_joined
                    && worker.evidence_persisted
                    && worker.terminal_snapshot_json.is_some()
                    && worker.panic.is_none()
                    && worker.failure.is_none()
            })
    }
}

impl FrozenExportVerifierShutdownReceipt {
    /// All admitted workers joined, without panic, abandonment or lost evidence.
    pub fn all_resources_released(&self) -> bool {
        self.workers_started == self.workers_joined
            && self.terminal_publications.all_resources_released()
            && (self.workers_started == 0) == self.last_worker.is_none()
            && self.workers_remaining == 0
            && self.workers_abandoned == 0
            && !self.deadline_exceeded
            && self.failure.is_none()
            && self.last_worker.as_ref().is_none_or(|worker| {
                worker.thread_joined
                    && worker.evidence_persisted
                    && worker.panic.is_none()
                    && worker.failure.is_none()
                    && worker.verification_failure.is_none()
                    && worker
                        .native_cleanup
                        .as_ref()
                        .is_some_and(|cleanup| cleanup.all_resources_released())
            })
    }
}

struct FrozenArtifactVerificationWorker {
    id: JobId,
    path: PathBuf,
    deadline: Instant,
    operation: FrozenWorkerOperation,
    handle: JoinHandle<FrozenArtifactVerificationCompletion>,
}

struct FrozenArtifactVerificationCompletion {
    ancillary_artifact: Option<super::endurance_campaign::EnduranceAncillaryExportArtifact>,
    receipt: FrozenExportVerifierWorkerReceipt,
    result: Result<Option<IndependentExportArtifactReceipt>, String>,
    terminal_snapshot_json: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrozenWorkerOperation {
    Verification,
    TerminalPublication,
}

#[derive(Default)]
struct FrozenArtifactVerifierOwner {
    phase_id: Option<String>,
    ancillary: Option<Arc<super::endurance_ancillary::PreparedEnduranceAncillaryProgram>>,
    worker: Option<FrozenArtifactVerificationWorker>,
    cancellation: ExecutionCancellationToken,
    closure: FrozenExportVerifierShutdownReceipt,
}

impl FrozenArtifactVerifierOwner {
    fn poll(
        &mut self,
        id: JobId,
        path: &Path,
        policy: IndependentExportArtifactPolicy,
        outer_deadline: Option<Instant>,
        completed_at_us: u64,
    ) -> Result<Option<VerifiedFrozenExportArtifact>, String> {
        if let Some(error) = &self.closure.failure {
            return Err(error.clone());
        }
        if self.worker.is_none() {
            if self.cancellation.is_canceled() {
                return Err("phase verifier admission is canceled".to_owned());
            }
            let policy_deadline = Instant::now()
                .checked_add(policy.decode_timeout())
                .ok_or_else(|| "independent verification deadline overflow".to_owned())?;
            let deadline =
                outer_deadline.map_or(policy_deadline, |outer| outer.min(policy_deadline));
            let next_count = self
                .closure
                .workers_started
                .checked_add(1)
                .ok_or_else(|| "independent worker count overflow".to_owned())?;
            let owned_path = path.to_path_buf();
            let worker_path = owned_path.clone();
            let cancellation = self.cancellation.clone();
            let ancillary = self.ancillary.clone();
            let handle = std::thread::Builder::new()
                .name("endurance-artifact-verifier".to_owned())
                .spawn(move || {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        verify_frozen_artifact(
                            &worker_path,
                            id,
                            policy,
                            deadline,
                            completed_at_us,
                            &cancellation,
                            ancillary.as_deref(),
                        )
                    })) {
                        Ok(completion) => completion,
                        Err(payload) => verifier_panic_completion(
                            id,
                            worker_path,
                            payload,
                            FrozenWorkerOperation::Verification,
                        ),
                    }
                })
                .map_err(|error| format!("spawn independent artifact verifier: {error}"))?;
            self.worker = Some(FrozenArtifactVerificationWorker {
                id,
                path: owned_path,
                deadline,
                operation: FrozenWorkerOperation::Verification,
                handle,
            });
            self.closure.workers_started = next_count;
            self.closure.workers_remaining = 1;
            return Ok(None);
        }
        let worker = self.worker.as_ref().ok_or("independent verifier owner disappeared")?;
        if worker.id != id
            || worker.path != path
            || worker.operation != FrozenWorkerOperation::Verification
        {
            return Err("capacity-one verifier was polled for another artifact".to_owned());
        }
        match self.join_finished() {
            None => Ok(None),
            Some(Ok(Some(receipt))) => {
                let mut verified = verified_artifact(completed_at_us, &receipt);
                if let Some(owner) = &self.ancillary {
                    let phase =
                        self.phase_id.as_deref().ok_or("ANC verifier phase identity missing")?;
                    let artifact_id = format!("endurance-export-{id}");
                    let binding = owner
                        .verified_exports(phase)?
                        .into_iter()
                        .find(|item| item.artifact_id == artifact_id)
                        .ok_or("joined ANC verifier has no durable binding")?;
                    verified.event = verified.event.with_ancillary_export_artifact(binding);
                }
                Ok(Some(verified))
            }
            Some(Ok(None)) => Err("verification worker returned a metadata-only result".to_owned()),
            Some(Err(error)) => Err(error),
        }
    }

    fn poll_terminal_publication(
        &mut self,
        snapshot: &ExportJobSnapshot,
        policy: IndependentExportArtifactPolicy,
        outer_deadline: Option<Instant>,
    ) -> Result<bool, String> {
        if let Some(failure) = &self.closure.failure {
            return Err(failure.clone());
        }
        if self.worker.is_none() {
            let deadline = Instant::now()
                .checked_add(policy.decode_timeout())
                .ok_or("metadata worker deadline overflow")?;
            let deadline = outer_deadline.map_or(deadline, |outer| outer.min(deadline));
            let next = self
                .closure
                .terminal_publications
                .workers_started
                .checked_add(1)
                .ok_or("terminal publication worker count overflow")?;
            let id = snapshot.id;
            let path = snapshot.output_path.clone();
            let owned_snapshot = snapshot.clone();
            let worker_path = path.clone();
            let cancellation = self.cancellation.clone();
            let handle = std::thread::Builder::new()
                .name("endurance-export-terminal".to_owned())
                .spawn(move || {
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        publish_terminal_snapshot(owned_snapshot, deadline, &cancellation)
                    })) {
                        Ok(result) => result,
                        Err(payload) => verifier_panic_completion(
                            id,
                            worker_path,
                            payload,
                            FrozenWorkerOperation::TerminalPublication,
                        ),
                    }
                })
                .map_err(|error| format!("spawn terminal publication worker: {error}"))?;
            self.worker = Some(FrozenArtifactVerificationWorker {
                id,
                path,
                deadline,
                operation: FrozenWorkerOperation::TerminalPublication,
                handle,
            });
            self.closure.terminal_publications.workers_started = next;
            self.closure.terminal_publications.workers_remaining = 1;
            return Ok(false);
        }
        let worker = self.worker.as_ref().ok_or("terminal publication worker disappeared")?;
        if worker.operation != FrozenWorkerOperation::TerminalPublication
            || worker.id != snapshot.id
            || worker.path != snapshot.output_path
        {
            return Err(
                "terminal publication cannot share the occupied verification slot".to_owned(),
            );
        }
        match self.join_finished() {
            None => Ok(false),
            Some(Ok(None)) => Ok(true),
            Some(Ok(Some(_))) => {
                Err("metadata worker returned a native verification receipt".to_owned())
            }
            Some(Err(error)) => Err(error),
        }
    }

    fn join_finished(
        &mut self,
    ) -> Option<Result<Option<IndependentExportArtifactReceipt>, String>> {
        if !self.worker.as_ref().is_some_and(|worker| worker.handle.is_finished()) {
            return None;
        }
        let worker = self.worker.take()?;
        let original_deadline = worker.deadline;
        let mut completion = match worker.handle.join() {
            Ok(completion) => completion,
            Err(payload) => {
                let error = super::execution_panic_diagnostic::execution_panic_diagnostic(
                    payload,
                    "independent verifier thread terminal",
                )
                .to_string();
                FrozenArtifactVerificationCompletion {
                    ancillary_artifact: None,
                    receipt: FrozenExportVerifierWorkerReceipt {
                        job_id: worker.id,
                        output_path: worker.path,
                        thread_joined: false,
                        evidence_persisted: false,
                        native_cleanup: None,
                        verification_failure: None,
                        panic: Some(error.clone()),
                        failure: Some(error.clone()),
                    },
                    result: Err(error),
                    terminal_snapshot_json: None,
                }
            }
        };
        completion.receipt.thread_joined = true;
        if let Some(artifact) = completion.ancillary_artifact.take() {
            let registration = match (&self.ancillary, &self.phase_id) {
                (Some(owner), Some(phase)) => owner.register_verified_export(phase, artifact),
                _ => Err("joined ANC verifier lost its immutable phase owner".to_owned()),
            };
            if let Err(error) = registration {
                completion.receipt.failure = Some(error.clone());
                completion.result = Err(error);
            }
        }
        if Instant::now() >= original_deadline {
            self.closure.deadline_exceeded = true;
            let deadline_error =
                "independent verifier result was consumed after its original deadline";
            let error = completion.receipt.failure.as_ref().map_or_else(
                || deadline_error.to_owned(),
                |original| format!("{original}; {deadline_error}"),
            );
            completion.receipt.failure = Some(error.clone());
            completion.result = Err(error);
        }
        self.record_terminal(
            worker.operation,
            completion.receipt,
            completion.terminal_snapshot_json,
        );
        Some(completion.result)
    }

    fn record_terminal(
        &mut self,
        operation: FrozenWorkerOperation,
        receipt: FrozenExportVerifierWorkerReceipt,
        terminal_snapshot_json: Option<String>,
    ) {
        if self.closure.failure.is_none() {
            self.closure.failure = receipt.failure.clone();
        }
        let joined = u64::from(receipt.thread_joined);
        let abandoned = u64::from(!receipt.thread_joined);
        match operation {
            FrozenWorkerOperation::Verification => {
                self.closure.workers_joined += joined;
                self.closure.workers_abandoned += abandoned;
                self.closure.workers_remaining = abandoned;
                self.closure.last_worker = Some(receipt);
            }
            FrozenWorkerOperation::TerminalPublication => {
                let owner = &mut self.closure.terminal_publications;
                owner.workers_joined += joined;
                owner.workers_abandoned += abandoned;
                owner.workers_remaining = abandoned;
                if owner.failure.is_none() {
                    owner.failure = receipt.failure.clone();
                }
                owner.last_worker = Some(FrozenExportTerminalWorkerReceipt {
                    job_id: receipt.job_id,
                    output_path: receipt.output_path,
                    thread_joined: receipt.thread_joined,
                    evidence_persisted: receipt.evidence_persisted,
                    terminal_snapshot_json,
                    panic: receipt.panic,
                    failure: receipt.failure,
                });
            }
        }
    }

    fn shutdown_until(&mut self, deadline: Instant) -> FrozenExportVerifierShutdownReceipt {
        let deadline =
            self.worker.as_ref().map_or(deadline, |worker| deadline.min(worker.deadline));
        if self.worker.is_some() {
            self.cancellation.cancel();
            self.closure.cancellation_requested = true;
            self.closure.deadline_exceeded |= Instant::now() >= deadline;
        }
        while self.worker.is_some() {
            if self.join_finished().is_some() {
                break;
            }
            if Instant::now() >= deadline {
                self.closure.deadline_exceeded = true;
                break;
            }
            std::thread::sleep(
                Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
        if let Some(worker) = self.worker.take() {
            let failure =
                "independent verifier thread did not join by the original shutdown deadline"
                    .to_owned();
            self.closure.failure.get_or_insert_with(|| failure.clone());
            self.record_terminal(
                worker.operation,
                FrozenExportVerifierWorkerReceipt {
                    job_id: worker.id,
                    output_path: worker.path,
                    thread_joined: false,
                    evidence_persisted: false,
                    native_cleanup: None,
                    verification_failure: None,
                    panic: None,
                    failure: Some(failure),
                },
                None,
            );
            // The running thread still owns cancellation/native leases and may finish;
            // an unjoined handle is never represented as terminated or safe to reuse.
            std::mem::forget(worker.handle);
        }
        self.closure.clone()
    }
}

impl Drop for FrozenArtifactVerifierOwner {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            let deadline = worker.deadline.min(Instant::now() + Duration::from_millis(100));
            let receipt = self.shutdown_until(deadline);
            tracing::error!(?receipt, "independent verifier required fallback closure instead of consuming phase shutdown");
        }
    }
}

fn verifier_panic_completion(
    id: JobId,
    path: PathBuf,
    payload: Box<dyn std::any::Any + Send>,
    operation: FrozenWorkerOperation,
) -> FrozenArtifactVerificationCompletion {
    let panic = super::execution_panic_diagnostic::execution_panic_diagnostic(
        payload,
        "independent artifact verifier",
    )
    .to_string();
    let suffix = match operation {
        FrozenWorkerOperation::Verification => ".independent-verification.json".to_owned(),
        FrozenWorkerOperation::TerminalPublication => format!(".job-{id}.terminal.json"),
    };
    let persistence = write_frozen_export_evidence(
        &path,
        &suffix,
        &serde_json::json!({
            "schema_version":1,"status":"panicked","job_id":id,"path":path,"panic":panic,
        }),
    );
    let evidence_persisted = persistence.is_ok();
    let error = match persistence {
        Ok(()) => panic.clone(),
        Err(error) => format!("{panic}; persist panic evidence: {error}"),
    };
    FrozenArtifactVerificationCompletion {
        ancillary_artifact: None,
        receipt: FrozenExportVerifierWorkerReceipt {
            job_id: id,
            output_path: path,
            thread_joined: false,
            evidence_persisted,
            native_cleanup: None,
            verification_failure: None,
            panic: Some(panic),
            failure: Some(error.clone()),
        },
        result: Err(error),
        terminal_snapshot_json: None,
    }
}

impl FrozenExportBackend for ProductionFrozenExportBackend {
    fn retained_jobs(&self) -> usize {
        self.queue.list_jobs().len()
    }

    fn enqueue(&mut self, config: ExportConfig) -> Result<JobId, String> {
        self.queue.enqueue(RenderJob::new(config)).map_err(|error| error.to_string())
    }

    fn observe(&mut self, id: JobId) -> Result<Option<FrozenExportAttemptObservation>, String> {
        let snapshot = self
            .queue
            .list_jobs()
            .into_iter()
            .find(|snapshot| snapshot.id == id)
            .ok_or_else(|| format!("phase-owned Export job {id} disappeared"))?;
        if matches!(
            &snapshot.status,
            JobStatus::Completed | JobStatus::Cancelled | JobStatus::Failed(_)
        ) && self.last_persisted_terminal_job != Some(id)
        {
            if self.terminal_job_deadline.is_none_or(|(job, _)| job != id) {
                let deadline = Instant::now()
                    .checked_add(self.verification_policy.decode_timeout())
                    .ok_or("terminal metadata and verification deadline overflow")?;
                let deadline =
                    self.verification_deadline.map_or(deadline, |outer| outer.min(deadline));
                self.terminal_job_deadline = Some((id, deadline));
            }
            let deadline = self.terminal_job_deadline.map(|(_, deadline)| deadline);
            if !self.verifier.poll_terminal_publication(
                &snapshot,
                self.verification_policy,
                deadline,
            )? {
                return Ok(None);
            }
            self.last_persisted_terminal_job = Some(id);
        }
        let generation = snapshot.generation;
        let output_path = snapshot.output_path.clone();
        let executed = snapshot.executed;
        let publication = snapshot.publication;
        let observation = match snapshot.status {
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
        };
        observation.map(Some)
    }

    fn verify(
        &mut self,
        id: JobId,
        output_path: &Path,
        completed_at_us: u64,
    ) -> Result<Option<VerifiedFrozenExportArtifact>, String> {
        let deadline = self
            .terminal_job_deadline
            .filter(|(job, _)| *job == id)
            .map(|(_, deadline)| deadline)
            .ok_or("independent verifier has no original terminal-publication deadline")?;
        self.verifier.poll(
            id,
            output_path,
            self.verification_policy,
            Some(deadline),
            completed_at_us,
        )
    }
    fn verification_pending(&self) -> bool {
        self.verifier.worker.is_some()
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

struct AncillaryVerificationReader<'a> {
    file: fs::File,
    deadline: Instant,
    cancellation: &'a ExecutionCancellationToken,
}
impl std::io::Read for AncillaryVerificationReader<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if Instant::now() >= self.deadline || self.cancellation.is_canceled() {
            // Interrupted would make read_exact silently retry cancellation.
            return Err(std::io::Error::other(
                "ANC verification canceled or original deadline expired",
            ));
        }
        std::io::Read::read(&mut self.file, bytes)
    }
}

fn rescan_ancillary_artifact(
    path: &Path,
    program: &mondrian_broadcast::FrozenAncillaryProgram,
    maximum_bytes: u64,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
    expected_artifact_sha256: &str,
) -> Result<u64, String> {
    use std::io::{Read, Seek, SeekFrom};
    #[cfg(windows)]
    let file = super::project_runtime::open_direct_read_file(path, "ANC final MXF")
        .map_err(|error| error.to_string())?;
    #[cfg(not(windows))]
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err("ANC final MXF exceeds independent artifact bound".to_owned());
    }
    let mut reader = AncillaryVerificationReader { file, deadline, cancellation };
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|value| *value <= maximum_bytes)
            .ok_or("ANC artifact changed or exceeded its bound")?;
        digest.update(&buffer[..count]);
    }
    if total != metadata.len() || format!("{:x}", digest.finalize()) != expected_artifact_sha256 {
        return Err("ANC rescan source differs from the independently decoded artifact".to_owned());
    }
    reader.file.seek(SeekFrom::Start(0)).map_err(|error| error.to_string())?;
    let frames = program
        .verify_mxf(&mut reader, maximum_bytes)
        .map_err(|error| error.to_string())?;
    if Instant::now() >= deadline || cancellation.is_canceled() {
        return Err("ANC rescan completed outside the original deadline".to_owned());
    }
    Ok(frames)
}

fn verify_frozen_artifact(
    output_path: &Path,
    id: JobId,
    policy: IndependentExportArtifactPolicy,
    deadline: Instant,
    completed_at_us: u64,
    cancellation: &ExecutionCancellationToken,
    ancillary: Option<&super::endurance_ancillary::PreparedEnduranceAncillaryProgram>,
) -> FrozenArtifactVerificationCompletion {
    let artifact_id = format!("endurance-export-{id}");
    let mut request = serde_json::json!({
        "artifact_id": artifact_id, "path": output_path,
        "maximum_artifact_bytes": policy.maximum_artifact_bytes(),
        "remaining_deadline_micros": deadline.saturating_duration_since(Instant::now()).as_micros(),
        "completed_at_us": completed_at_us,
    });
    if let Some(owner) = ancillary {
        request["ancillary_program_sha256"] = serde_json::json!(owner.sha256());
    }
    let result = verify_export_artifact_until(
        output_path,
        artifact_id,
        policy.maximum_artifact_bytes(),
        deadline,
        cancellation,
    );
    let verification_failure = result.as_ref().err().map(|error| error.evidence());
    let native_cleanup = match &result {
        Ok(receipt) => Some(receipt.native_execution().cleanup.clone()),
        Err(_) => verification_failure
            .as_ref()
            .and_then(|evidence| evidence.child_cleanup.clone()),
    };
    let mut evidence = match &result {
        Ok(receipt) => {
            serde_json::json!({"schema_version":1,"status":"verified","request":request,"evidence":receipt.evidence()})
        }
        Err(_) => {
            serde_json::json!({"schema_version":1,"status":"failed","request":request,"evidence":verification_failure})
        }
    };
    let mut result = result.map_err(|error| error.to_string());
    if let (Some(owner), Ok(receipt)) = (ancillary, &result) {
        let scan = rescan_ancillary_artifact(
            output_path,
            owner.program(),
            policy.maximum_artifact_bytes(),
            deadline,
            cancellation,
            &receipt.report().artifact_sha256,
        );
        match scan {
            Ok(frames) => {
                evidence["ancillary_mxf_rescan"] = serde_json::json!({"frames_verified":frames,"ancillary_program_sha256":owner.sha256()})
            }
            Err(error) => {
                evidence["status"] = serde_json::json!("failed");
                evidence["ancillary_mxf_rescan"] =
                    serde_json::json!({"failure":error,"ancillary_program_sha256":owner.sha256()});
                result = Err(error);
            }
        }
    }
    let persistence = write_verification_evidence(output_path, &evidence);
    let evidence_persisted = persistence.is_ok();
    let ancillary_artifact = if ancillary.is_some() && result.is_ok() {
        persistence.as_ref().ok().map(|(path, sha)| {
            super::endurance_campaign::EnduranceAncillaryExportArtifact {
                artifact_id: format!("endurance-export-{id}"),
                verification_path: path.clone(),
                verification_sha256: sha.clone(),
            }
        })
    } else {
        None
    };
    if let Err(error) = persistence {
        let primary = result.err().map(|error| format!("{error}; ")).unwrap_or_default();
        result = Err(format!(
            "{primary}persist original independent verifier evidence: {error}"
        ));
    }
    if result.is_ok() && (Instant::now() >= deadline || cancellation.is_canceled()) {
        result = Err("independent verification evidence publication exceeded its original deadline or was canceled".to_owned());
    }
    let failure = result.as_ref().err().cloned();
    FrozenArtifactVerificationCompletion {
        ancillary_artifact,
        receipt: FrozenExportVerifierWorkerReceipt {
            job_id: id,
            output_path: output_path.to_path_buf(),
            thread_joined: false,
            evidence_persisted,
            native_cleanup,
            verification_failure,
            panic: None,
            failure,
        },
        result: result.map(Some),
        terminal_snapshot_json: None,
    }
}

fn publish_terminal_snapshot(
    snapshot: ExportJobSnapshot,
    deadline: Instant,
    cancellation: &ExecutionCancellationToken,
) -> FrozenArtifactVerificationCompletion {
    let id = snapshot.id;
    let output_path = snapshot.output_path.clone();
    let raw = serde_json::json!({"schema_version":1,"job":snapshot});
    let serialized = serde_json::to_string(&raw).map_err(|error| error.to_string());
    let mut evidence_persisted = false;
    let result = (|| -> Result<(), String> {
        let json = serialized.as_ref().map_err(Clone::clone)?;
        if json.len() > 2 * 1024 * 1024 {
            return Err("terminal snapshot exceeds the 2 MiB record bound".to_owned());
        }
        if Instant::now() >= deadline || cancellation.is_canceled() {
            return Err(
                "terminal publication was canceled or exceeded its original deadline".to_owned(),
            );
        }
        write_frozen_export_evidence(&output_path, &format!(".job-{id}.terminal.json"), &raw)?;
        evidence_persisted = true;
        if Instant::now() >= deadline || cancellation.is_canceled() {
            return Err(
                "terminal publication completed after cancellation or its original deadline"
                    .to_owned(),
            );
        }
        Ok(())
    })();
    FrozenArtifactVerificationCompletion {
        ancillary_artifact: None,
        receipt: FrozenExportVerifierWorkerReceipt {
            job_id: id,
            output_path,
            thread_joined: false,
            evidence_persisted,
            native_cleanup: None,
            verification_failure: None,
            panic: None,
            failure: result.as_ref().err().cloned(),
        },
        result: result.map(|()| None),
        terminal_snapshot_json: serialized.ok().filter(|json| json.len() <= 2 * 1024 * 1024),
    }
}

fn write_verification_evidence(
    output_path: &Path,
    evidence: &serde_json::Value,
) -> Result<(PathBuf, String), String> {
    write_frozen_export_evidence_bound(output_path, ".independent-verification.json", evidence)
}

fn write_frozen_export_evidence(
    output_path: &Path,
    suffix: &str,
    evidence: &serde_json::Value,
) -> Result<(), String> {
    write_frozen_export_evidence_bound(output_path, suffix, evidence).map(|_| ())
}
fn write_frozen_export_evidence_bound(
    output_path: &Path,
    suffix: &str,
    evidence: &serde_json::Value,
) -> Result<(PathBuf, String), String> {
    let mut leaf = output_path
        .file_name()
        .ok_or_else(|| "verified artifact has no filename".to_owned())?
        .to_os_string();
    leaf.push(suffix);
    let path = output_path.with_file_name(leaf);
    let bytes = serde_json::to_vec_pretty(evidence).map_err(|error| error.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(
            "independent verifier evidence exceeds the 2 MiB durable-record bound".to_owned(),
        );
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("flush {}: {error}", path.display()))?;
    Ok((path, format!("{:x}", Sha256::digest(&bytes))))
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
    recovery_after_verification: Option<u32>,
    closing: bool,
    fault: Option<String>,
}

/// Sequential phase owner that repeats one frozen Export and verifies every artifact.
pub struct FrozenRepeatedExportPhase {
    state: FrozenRepeatedExportState<ProductionFrozenExportBackend>,
}

/// Frozen product Export inputs before any job or verifier has been admitted.
pub(crate) struct PreparedFrozenRepeatedExportPhase {
    plan: FrozenExportPlan,
    backend: ProductionFrozenExportBackend,
}

impl PreparedFrozenRepeatedExportPhase {
    /// Capture the ordinary Export configuration without starting phase work.
    pub(crate) fn prepare(
        app: &AppState,
        request: FrozenRepeatedExportRequest,
    ) -> Result<Self, FrozenRepeatedExportError> {
        let (plan, backend) = FrozenExportPlan::capture(app, request)?;
        if backend.retained_jobs() != 0 {
            return Err(FrozenRepeatedExportError::ContaminatedQueue);
        }
        Ok(Self { plan, backend })
    }

    /// Consume preparation once and bind the first job to the measurement horizon.
    pub(crate) fn activate_until(
        mut self,
        deadline: Instant,
    ) -> Result<FrozenRepeatedExportPhase, FrozenRepeatedExportError> {
        if Instant::now() >= deadline {
            return Err(FrozenRepeatedExportError::InvalidPlan(
                "measurement horizon elapsed before Export activation".to_owned(),
            ));
        }
        self.backend.verification_deadline = Some(deadline);
        Ok(FrozenRepeatedExportPhase {
            state: FrozenRepeatedExportState::start_with_backend(self.plan, self.backend)?,
        })
    }
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

    /// Bind every independent verifier attempt to one original outer deadline.
    pub fn start_until(
        app: &AppState,
        request: FrozenRepeatedExportRequest,
        deadline: Instant,
    ) -> Result<Self, FrozenRepeatedExportError> {
        if Instant::now() >= deadline {
            return Err(FrozenRepeatedExportError::InvalidPlan(
                "repeated Export deadline already elapsed".to_owned(),
            ));
        }
        let (plan, mut backend) = FrozenExportPlan::capture(app, request)?;
        backend.verification_deadline = Some(deadline);
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

    /// Cancel and consume the phase's verifier thread under the original close deadline.
    pub fn shutdown_until(mut self, deadline: Instant) -> FrozenExportVerifierShutdownReceipt {
        self.state.begin_close();
        self.state.backend.verifier.shutdown_until(deadline)
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
            recovery_after_verification: None,
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
        if let Some(cycle_index) = self.recovery_after_verification
            && self.active.is_none()
        {
            self.enqueue_next()?;
            self.recovery_after_verification = None;
            self.recovery = Some(FrozenExportRecovery::Running { cycle_index });
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
        let Some(observation) =
            self.backend.observe(active.id).map_err(|detail| self.latch_fault(detail))?
        else {
            return Ok(Vec::new());
        };
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
                let Some(verified) = verified else {
                    return Ok(Vec::new());
                };
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
        let Some(observation) =
            self.backend.observe(active.id).map_err(|detail| self.latch_fault(detail))?
        else {
            self.recovery = Some(recovery);
            return Ok(Vec::new());
        };

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
                    let Some(verified) = verified else {
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
                        return Ok(Vec::new());
                    };
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
        self.recovery_after_verification = None;
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
        self.recovery.is_some() || self.recovery_after_verification.is_some()
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
        if self.recovery.is_some() || self.recovery_after_verification.is_some() {
            return Err(
                self.latch_fault("an Export cancel/retry recovery is already active".to_owned())
            );
        }
        if self.active.is_none() {
            return Err(self.latch_fault(
                "Export cancel/retry requires one admitted phase-owned attempt".to_owned(),
            ));
        }
        if self.backend.verification_pending() {
            self.recovery_after_verification = Some(cycle_index);
        } else {
            self.recovery = Some(FrozenExportRecovery::Running { cycle_index });
        }
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
    let as11 = preset.professional_delivery().is_some_and(|delivery| {
        delivery.profile == mondrian_export::ProfessionalDeliveryProfile::As11X9NabaHd720p5994
    });
    if (preset.media_file().is_none() && !as11)
        || (preset.professional_delivery().is_some() && !as11)
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
        verify_polls_remaining: u32,
        verification_started: bool,
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
                verify_polls_remaining: 0,
                verification_started: false,
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

        fn observe(&mut self, id: JobId) -> Result<Option<FrozenExportAttemptObservation>, String> {
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
            Ok(Some(match self.terminal {
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
            }))
        }

        fn verify(
            &mut self,
            id: JobId,
            _output_path: &Path,
            completed_at_us: u64,
        ) -> Result<Option<VerifiedFrozenExportArtifact>, String> {
            if self.verify_fails {
                return Err("injected verifier failure".to_owned());
            }
            if self.verify_polls_remaining > 0 {
                self.verify_polls_remaining -= 1;
                self.verification_started = true;
                return Ok(None);
            }
            self.verification_started = false;
            Ok(Some(VerifiedFrozenExportArtifact {
                event: EnduranceCampaignEvent::test_export_artifact_verified(
                    completed_at_us,
                    format!("fake-{id}"),
                    SHA,
                    "fake-verifier",
                    SHA,
                ),
                artifact_sha256: SHA.to_owned(),
                validation_report_sha256: SHA.to_owned(),
            }))
        }

        fn diagnostics(&self) -> ExportQueueDiagnostics {
            ExportQueueDiagnostics {
                terminal: usize::from(
                    self.active.is_some() && !matches!(self.terminal, FakeTerminal::Failed),
                ),
                ..ExportQueueDiagnostics::default()
            }
        }

        fn verification_pending(&self) -> bool {
            self.verification_started
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

        fn observe(&mut self, id: JobId) -> Result<Option<FrozenExportAttemptObservation>, String> {
            if self.active != Some(id) {
                return Err("unknown recovery fake attempt".to_owned());
            }
            let generation = self.generation()?;
            let output_path = self.output_path()?;
            Ok(Some(match self.stage {
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
            }))
        }

        fn verify(
            &mut self,
            id: JobId,
            _output_path: &Path,
            completed_at_us: u64,
        ) -> Result<Option<VerifiedFrozenExportArtifact>, String> {
            if self.verify_fails {
                return Err("injected recovery verifier failure".to_owned());
            }
            Ok(Some(VerifiedFrozenExportArtifact {
                event: EnduranceCampaignEvent::test_export_artifact_verified(
                    completed_at_us,
                    format!("recovery-{id}"),
                    SHA,
                    "fake-verifier",
                    SHA,
                ),
                artifact_sha256: SHA.to_owned(),
                validation_report_sha256: SHA.to_owned(),
            }))
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
                regulatory_pse: None,
                frozen_ancillary: None,
                approved_bmx: None,
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
    fn recovery_request_during_async_verification_waits_without_canceling_published_attempt() {
        let (_directory, plan) = test_plan();
        let mut backend = FakeBackend::clean();
        backend.verify_polls_remaining = 2;
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, backend).expect("phase");
        let original_id = phase.active.as_ref().expect("owned attempt").id;
        assert!(phase.poll(10).expect("verification pending").is_empty());
        phase.begin_cancel_retry_recovery(0).expect("queue recovery without waiting");
        assert!(phase.cancel_retry_recovery_in_progress());
        assert_eq!(phase.recovery_after_verification, Some(0));
        assert!(phase.recovery.is_none());
        assert!(phase.poll(11).expect("still pending").is_empty());
        assert_eq!(
            phase.active.as_ref().expect("same admitted object").id,
            original_id
        );
        assert_eq!(phase.backend.configs.len(), 1);
        assert_eq!(
            phase.poll(12).expect("observed completed verification").len(),
            1
        );
        assert!(phase.active.is_none());
        assert_eq!(phase.verified_artifacts(), 1);
        assert_eq!(phase.recovery_after_verification, Some(0));
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
    fn cancellation_retry_preserves_the_same_canonical_ancillary_attachment() {
        let (_directory, mut plan) = test_plan();
        let packet = mondrian_broadcast::AncillaryPacket {
            placement: mondrian_broadcast::AncillaryPlacement::new(
                mondrian_broadcast::AncillarySpace::Vanc,
                mondrian_broadcast::AncillaryField::Progressive,
                20,
                0,
            )
            .expect("placement"),
            packet: mondrian_broadcast::St291Type2Packet::from_8bit_payload(0x45, 1, &[9, 7])
                .expect("packet"),
            origin: mondrian_broadcast::AncillaryOrigin::Derived,
            validation: mondrian_broadcast::AncillaryValidationLevel::Transport,
        };
        let program = mondrian_broadcast::FrozenAncillaryProgram::new(
            mondrian_core::TimelineTime::new(1001, 30000).expect("origin"),
            mondrian_core::Rational::new(60000, 1001),
            3,
            vec![mondrian_broadcast::AncillaryFrame::new(1, vec![packet]).expect("frame")],
        )
        .expect("program");
        plan.base_config.frozen_ancillary = Some(program.clone());
        let mut phase =
            FrozenRepeatedExportState::start_with_backend(plan, RecoveryFakeBackend::successful())
                .expect("start");
        phase.begin_cancel_retry_recovery(1).expect("request");
        phase.poll(10).expect("cancel");
        phase.poll(20).expect("retry");
        phase.poll(30).expect("complete retry");
        assert_eq!(phase.backend.configs.len(), 2);
        assert_ne!(
            phase.backend.configs[0].output_path,
            phase.backend.configs[1].output_path
        );
        assert!(phase
            .backend
            .configs
            .iter()
            .all(|config| config.frozen_ancillary.as_ref() == Some(&program)));
    }

    #[test]
    fn canonical_ancillary_rescan_binds_artifact_bytes_and_original_deadline() {
        let root = tempfile::tempdir().expect("root");
        let path = mondrian_assets::canonical_native_path(root.path())
            .expect("canonical")
            .join("transport-klv.mxf");
        let program = mondrian_broadcast::FrozenAncillaryProgram::new(
            mondrian_core::TimelineTime::ZERO,
            mondrian_core::Rational::new(60000, 1001),
            2,
            vec![],
        )
        .expect("program");
        // Pure KLV parser boundary test, not a qualified muxed MXF artifact.
        let mut bytes = Vec::new();
        for index in 0..2 {
            mondrian_broadcast::write_st436_klv_frame(
                &mut bytes,
                &program.frame(index).expect("frame"),
            )
            .expect("write");
        }
        fs::write(&path, &bytes).expect("write KLV");
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let cancel = ExecutionCancellationToken::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        assert_eq!(
            rescan_ancillary_artifact(
                &path,
                &program,
                bytes.len() as u64,
                deadline,
                &cancel,
                &hash
            )
            .expect("matching program"),
            2
        );
        assert!(rescan_ancillary_artifact(
            &path,
            &program,
            bytes.len() as u64 - 1,
            deadline,
            &cancel,
            &hash
        )
        .is_err());
        assert!(rescan_ancillary_artifact(
            &path,
            &program,
            bytes.len() as u64,
            deadline,
            &cancel,
            &"0".repeat(64)
        )
        .is_err());
        assert!(rescan_ancillary_artifact(
            &path,
            &program,
            bytes.len() as u64,
            Instant::now(),
            &cancel,
            &hash
        )
        .is_err());
        let mismatch = mondrian_broadcast::FrozenAncillaryProgram::new(
            mondrian_core::TimelineTime::ZERO,
            mondrian_core::Rational::new(60000, 1001),
            2,
            vec![mondrian_broadcast::AncillaryFrame::new(
                0,
                vec![mondrian_broadcast::AncillaryPacket {
                    placement: mondrian_broadcast::AncillaryPlacement::new(
                        mondrian_broadcast::AncillarySpace::Vanc,
                        mondrian_broadcast::AncillaryField::Progressive,
                        20,
                        0,
                    )
                    .expect("placement"),
                    packet: mondrian_broadcast::St291Type2Packet::from_8bit_payload(0x45, 1, &[8])
                        .expect("packet"),
                    origin: mondrian_broadcast::AncillaryOrigin::Derived,
                    validation: mondrian_broadcast::AncillaryValidationLevel::Transport,
                }],
            )
            .expect("different frame")],
        )
        .expect("different canonical program");
        assert!(rescan_ancillary_artifact(
            &path,
            &mismatch,
            bytes.len() as u64,
            deadline,
            &cancel,
            &hash
        )
        .is_err());
        cancel.cancel();
        assert!(rescan_ancillary_artifact(
            &path,
            &program,
            bytes.len() as u64,
            deadline,
            &cancel,
            &hash
        )
        .is_err());
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

#[cfg(test)]
mod durable_verifier_evidence_tests {
    use super::*;

    fn terminal_snapshot(path: PathBuf, status: JobStatus) -> ExportJobSnapshot {
        use mondrian_export::queue::{
            ExportJobDiagnostics, ExportProgress, ExportProgressDetail, ExportProgressPhase,
        };
        let completed = matches!(status, JobStatus::Completed);
        ExportJobSnapshot {
            id: JobId::new(),
            generation: 7,
            output_path: path.clone(),
            output_policy: ExportOutputPolicy::CreateNew,
            preset_name: "terminal owner fixture".to_owned(),
            status,
            progress: ExportProgress {
                phase: ExportProgressPhase::Validating,
                fraction: 1.0,
                detail: ExportProgressDetail::None,
            },
            publication: if completed {
                ExportPublicationState::Published
            } else {
                ExportPublicationState::NotPublished
            },
            diagnostics: ExportJobDiagnostics::default(),
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: Some(chrono::Utc::now()),
            terminal_evidence: None,
            artifact_publication: completed
                .then_some(ExportArtifactPublicationEvidence::Durable { output_path: path }),
            executed: true,
        }
    }

    #[test]
    fn all_terminal_kinds_publish_asynchronously_without_inventing_native_verification() {
        use mondrian_export::queue::{ExportFailure, ExportFailureReason};
        for status in [
            JobStatus::Completed,
            JobStatus::Cancelled,
            JobStatus::Failed(ExportFailure {
                reason: ExportFailureReason::ExecutionFailed,
                detail: "original failed export fixture".to_owned(),
            }),
        ] {
            let root = tempfile::tempdir().expect("sidecar root");
            let snapshot = terminal_snapshot(root.path().join("artifact.mp4"), status);
            let deadline = Instant::now() + Duration::from_secs(5);
            let policy =
                IndependentExportArtifactPolicy::new(1024, Duration::from_secs(5)).expect("policy");
            let mut owner = FrozenArtifactVerifierOwner::default();
            assert!(!owner
                .poll_terminal_publication(&snapshot, policy, Some(deadline))
                .expect("admit metadata"));
            loop {
                assert!(Instant::now() < deadline, "metadata worker did not settle");
                if owner
                    .poll_terminal_publication(&snapshot, policy, Some(deadline))
                    .expect("metadata poll")
                {
                    break;
                }
                std::thread::yield_now();
            }
            let raw = owner.shutdown_until(deadline);
            assert!(raw.all_resources_released(), "{raw:?}");
            assert_eq!(raw.workers_started, 0);
            assert!(raw.last_worker.is_none());
            assert_eq!(raw.terminal_publications.workers_started, 1);
            assert_eq!(raw.terminal_publications.workers_joined, 1);
            let worker = raw.terminal_publications.last_worker.expect("actual metadata receipt");
            let original: serde_json::Value = serde_json::from_str(
                &worker.terminal_snapshot_json.expect("original full snapshot"),
            )
            .expect("raw snapshot JSON");
            assert_eq!(
                original["job"],
                serde_json::to_value(&snapshot).expect("expected snapshot")
            );
            let sidecar =
                root.path().join(format!("artifact.mp4.job-{}.terminal.json", snapshot.id));
            let written: serde_json::Value =
                serde_json::from_slice(&fs::read(sidecar).expect("durable terminal sidecar"))
                    .expect("sidecar JSON");
            assert_eq!(written, original);
        }
    }

    #[test]
    fn failed_terminal_publication_preserves_original_snapshot_and_never_replaces_existing_evidence(
    ) {
        let root = tempfile::tempdir().expect("root");
        let snapshot = terminal_snapshot(root.path().join("artifact.mp4"), JobStatus::Cancelled);
        let sidecar = root.path().join(format!("artifact.mp4.job-{}.terminal.json", snapshot.id));
        fs::write(&sidecar, b"existing evidence must survive").expect("occupied sidecar");
        let completion = publish_terminal_snapshot(
            snapshot.clone(),
            Instant::now() + Duration::from_secs(5),
            &ExecutionCancellationToken::new(),
        );
        assert!(completion.result.is_err());
        assert!(!completion.receipt.evidence_persisted);
        let original: serde_json::Value = serde_json::from_str(
            &completion.terminal_snapshot_json.expect("original failed-write snapshot"),
        )
        .expect("raw JSON");
        assert_eq!(
            original["job"],
            serde_json::to_value(snapshot).expect("original snapshot")
        );
        assert_eq!(
            fs::read(sidecar).expect("original retained"),
            b"existing evidence must survive"
        );
    }

    #[test]
    fn oversized_terminal_snapshot_fails_without_an_unbounded_retained_receipt_or_file() {
        let root = tempfile::tempdir().expect("root");
        let mut snapshot =
            terminal_snapshot(root.path().join("artifact.mp4"), JobStatus::Cancelled);
        snapshot.preset_name = "x".repeat(2 * 1024 * 1024);
        let completion = publish_terminal_snapshot(
            snapshot,
            Instant::now() + Duration::from_secs(5),
            &ExecutionCancellationToken::new(),
        );
        assert!(completion.result.is_err());
        assert!(completion.terminal_snapshot_json.is_none());
        assert!(!completion.receipt.evidence_persisted);
        assert_eq!(fs::read_dir(root.path()).expect("root").count(), 0);
    }

    fn failed_completion(
        id: JobId,
        path: PathBuf,
        error: &str,
    ) -> FrozenArtifactVerificationCompletion {
        FrozenArtifactVerificationCompletion {
            ancillary_artifact: None,
            receipt: FrozenExportVerifierWorkerReceipt {
                job_id: id,
                output_path: path,
                thread_joined: false,
                evidence_persisted: false,
                native_cleanup: None,
                verification_failure: None,
                panic: None,
                failure: Some(error.to_owned()),
            },
            result: Err(error.to_owned()),
            terminal_snapshot_json: None,
        }
    }

    #[test]
    fn pending_verifier_poll_is_nonblocking_and_cannot_admit_a_second_artifact() {
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let id = JobId::new();
        let path = PathBuf::from("owned-artifact.mp4");
        let worker_path = path.clone();
        let handle = std::thread::spawn(move || {
            wait.recv().expect("test release");
            failed_completion(id, worker_path, "controlled test result")
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut owner = FrozenArtifactVerifierOwner::default();
        owner.worker = Some(FrozenArtifactVerificationWorker {
            id,
            path: path.clone(),
            deadline,
            operation: FrozenWorkerOperation::Verification,
            handle,
        });
        owner.closure.workers_started = 1;
        owner.closure.workers_remaining = 1;
        let policy =
            IndependentExportArtifactPolicy::new(1024, Duration::from_secs(5)).expect("policy");
        let began = Instant::now();
        assert!(owner
            .poll(id, &path, policy, Some(deadline), 1)
            .expect("pending poll")
            .is_none());
        assert!(began.elapsed() < Duration::from_millis(100));
        assert!(owner.poll(JobId::new(), &path, policy, Some(deadline), 2).is_err());
        assert_eq!(owner.closure.workers_started, 1);
        release.send(()).expect("release actual thread");
        let closure = owner.shutdown_until(deadline);
        assert_eq!(closure.workers_joined, 1);
        assert_eq!(closure.workers_remaining, 0);
        assert!(closure.last_worker.expect("raw joined worker").thread_joined);
    }

    #[test]
    fn original_shutdown_deadline_retains_unjoined_thread_failure_without_claiming_exit() {
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let (finished, acknowledged) = std::sync::mpsc::sync_channel(1);
        let id = JobId::new();
        let path = PathBuf::from("blocked-artifact.mp4");
        let worker_path = path.clone();
        let handle = std::thread::spawn(move || {
            wait.recv().expect("release abandoned test thread");
            let completion = failed_completion(id, worker_path, "released after original deadline");
            finished.send(()).expect("notify no test body remains blocked");
            completion
        });
        let mut owner = FrozenArtifactVerifierOwner::default();
        owner.worker = Some(FrozenArtifactVerificationWorker {
            id,
            path,
            deadline: Instant::now() + Duration::from_secs(5),
            operation: FrozenWorkerOperation::Verification,
            handle,
        });
        owner.closure.workers_started = 1;
        owner.closure.workers_remaining = 1;
        let closure = owner.shutdown_until(Instant::now());
        assert!(closure.deadline_exceeded);
        assert!(closure.cancellation_requested);
        assert_eq!(closure.workers_joined, 0);
        assert_eq!(closure.workers_remaining, 1);
        assert_eq!(closure.workers_abandoned, 1);
        assert!(!closure.all_resources_released());
        assert!(!closure.last_worker.expect("raw abandonment").thread_joined);
        release.send(()).expect("release test fixture");
        acknowledged.recv_timeout(Duration::from_secs(5)).expect("fixture body ended");
    }

    #[test]
    fn worker_panic_is_joined_and_retained_without_inventing_native_or_snapshot_closure() {
        let root = tempfile::tempdir().expect("panic root");
        let path = root.path().join("panic.mp4");
        let id = JobId::new();
        let handle = std::thread::spawn(|| -> FrozenArtifactVerificationCompletion {
            panic!("actual worker panic fixture")
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut owner = FrozenArtifactVerifierOwner::default();
        owner.worker = Some(FrozenArtifactVerificationWorker {
            id,
            path,
            deadline,
            operation: FrozenWorkerOperation::Verification,
            handle,
        });
        owner.closure.workers_started = 1;
        owner.closure.workers_remaining = 1;
        let closure = owner.shutdown_until(deadline);
        assert_eq!(closure.workers_joined, 1);
        assert_eq!(closure.workers_remaining, 0);
        assert!(!closure.all_resources_released());
        let worker = closure.last_worker.expect("original joined panic");
        assert!(worker.panic.expect("panic retained").contains("actual worker panic fixture"));
        assert!(worker.native_cleanup.is_none());
        assert!(worker.verification_failure.is_none());
    }

    #[test]
    fn asynchronous_real_probe_failure_keeps_durable_original_child_and_snapshot_receipt() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("invalid.mp4");
        fs::write(&path, b"invalid native media fixture").expect("fixture");
        let id = JobId::new();
        let deadline = Instant::now() + Duration::from_secs(15);
        let policy =
            IndependentExportArtifactPolicy::new(1024, Duration::from_secs(15)).expect("policy");
        let mut owner = FrozenArtifactVerifierOwner::default();
        assert!(owner
            .poll(id, &path, policy, Some(deadline), 0)
            .expect("admit worker")
            .is_none());
        loop {
            assert!(Instant::now() < deadline, "real probe did not terminate");
            match owner.poll(id, &path, policy, Some(deadline), 1) {
                Ok(None) => std::thread::yield_now(),
                Ok(Some(_)) => panic!("invalid fixture was accepted"),
                Err(_) => break,
            }
        }
        let closure = owner.shutdown_until(deadline);
        assert_eq!(closure.workers_started, 1);
        assert_eq!(closure.workers_joined, 1);
        assert!(!closure.all_resources_released());
        let worker = closure.last_worker.expect("completed original verifier failure");
        assert!(worker.evidence_persisted);
        assert!(worker.thread_joined);
        let raw = worker.verification_failure.expect("raw failure");
        assert!(raw.snapshot_admitted && raw.snapshot_removed);
        assert!(raw.snapshot_cleanup_error.is_none());
        assert!(worker.native_cleanup.expect("actual ffprobe cleanup").all_resources_released());
        assert!(root.path().join("invalid.mp4.independent-verification.json").exists());
    }

    #[test]
    fn expired_verifier_deadline_persists_exact_request_and_no_invented_native_execution() {
        let root = tempfile::tempdir().expect("root");
        let artifact = root.path().join("native-output.mp4");
        fs::write(&artifact, b"uninspected artifact").expect("artifact");
        let id = JobId::new();
        let deadline = Instant::now();
        let result = verify_frozen_artifact(
            &artifact,
            id,
            IndependentExportArtifactPolicy::new(1024, std::time::Duration::from_secs(1))
                .expect("policy"),
            deadline,
            7,
            &ExecutionCancellationToken::new(),
            None,
        );
        assert!(result.result.is_err());
        assert!(result.receipt.evidence_persisted);
        assert!(!result.receipt.thread_joined);
        let evidence_path = root.path().join("native-output.mp4.independent-verification.json");
        let bytes = fs::read(&evidence_path).expect("failure already durable");
        let evidence: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(evidence["status"], "failed");
        assert_eq!(
            evidence["request"]["artifact_id"],
            format!("endurance-export-{id}")
        );
        assert_eq!(evidence["request"]["remaining_deadline_micros"], 0);
        assert_eq!(evidence["evidence"]["snapshot_admitted"], false);
        assert!(evidence["evidence"]["decode_execution"].is_null());
        assert!(evidence["evidence"]["child_cleanup"].is_null());
        assert!(
            write_verification_evidence(&artifact, &serde_json::json!({"replacement":true}))
                .is_err()
        );
        assert_eq!(fs::read(evidence_path).expect("original"), bytes);
    }
}
