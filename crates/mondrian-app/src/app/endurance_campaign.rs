//! Validation-only serial coordinator for commercial endurance workloads.
//!
//! The coordinator owns cadence, native process-tree sampling, phase order,
//! bounded evidence publication, and fail-closed terminal capture. Concrete
//! product runtimes own Playback, Reference Output, Export, recovery, and
//! synchronous worker shutdown; this module never reinterprets their facts.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mondrian_export::{ExportEnduranceSnapshot, ExportQueueShutdownEvidence};
use mondrian_platform::{
    EndurancePhaseKind, EndurancePhaseRequirement, EndurancePhaseTerminalStatus,
    EnduranceQualificationProfile, EnduranceRunManifest, ProcessMemoryProbe, ProcessMemoryScope,
};
use mondrian_playback::PlaybackEvidenceReport;
use mondrian_reference_output::ReferenceOutputDiagnostics;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_qualification::{
    EnduranceCaptureError, EnduranceCaptureFacts, EndurancePhaseCapture, EnduranceRecoveryStep,
    EnduranceRunCapture, EnduranceRunIdentity, EnduranceSampleTiming,
};
use super::endurance_recovery::EnduranceRecoveryOperationReceipt;
pub use super::endurance_shutdown::{
    AppAudioSourceCacheShutdownEvidence, AppEnduranceShutdownEvidence, AppProjectShutdownEvidence,
    EnduranceWorkerShutdownEvidence,
};
pub use super::endurance_workload::{EnduranceNotRunAdmission, EndurancePhaseAdmission};
use super::endurance_workload::{EnduranceWorkloadError, PreparedEnduranceWorkload};
use super::execution_resource_coordination::{
    ExecutionResourcePressure, ExecutionResourcePressureSource, ResourceTrimRequest,
};
use super::headless_realtime_playback::{
    capture_headless_endurance_owner_snapshot, HeadlessEnduranceOwnerSnapshot,
    HeadlessEnduranceShutdownProjection, HeadlessGpuExecutionDisposition,
    HeadlessGpuExecutionObserver, HeadlessPreviewSample, HeadlessRealtimePlaybackSession,
};
use super::preview_runtime::PreviewRuntimeShutdownEvidence;
use super::viewer_gpu_device_progress::ViewerGpuDeviceGenerationTerminalKind;
use super::AppState;

/// Public projection of bounded Headless GPU retirement evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceGpuShutdownEvidence {
    /// Whether the progress worker started.
    pub worker_started: bool,
    /// Whether it returned within the shutdown bound.
    pub worker_terminated: bool,
    /// Whether the worker panicked.
    pub worker_panicked: bool,
    /// Whether bounded shutdown expired.
    pub timed_out: bool,
    /// Whether the complete device-generation envelope reached the worker.
    pub retirement_handoff_accepted: bool,
    /// Whether every accepted GPU/native resource became safe to release.
    pub retirement_completed: bool,
    /// Unexpected device losses observed through final retirement.
    pub device_loss_count: u64,
    /// Progress-domain failures observed through final retirement.
    pub fatal_error_count: u64,
}

impl EnduranceGpuShutdownEvidence {
    /// Whether GPU progress and generation retirement closed exactly.
    pub const fn all_resources_retired(self) -> bool {
        self.worker_started
            && self.worker_terminated
            && !self.worker_panicked
            && !self.timed_out
            && self.retirement_handoff_accepted
            && self.retirement_completed
    }
}

/// Synchronous closure across Headless and every AppState execution owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnduranceExecutionOwnerClosure {
    /// Complete Preview worker inventory.
    pub preview: PreviewRuntimeShutdownEvidence,
    /// Complete consuming inventory for every owner embedded in AppState.
    pub app: AppEnduranceShutdownEvidence,
    /// Bounded GPU progress and generation-retirement evidence.
    pub gpu: EnduranceGpuShutdownEvidence,
    /// Owner snapshot failure retained after consuming cleanup completed.
    pub owner_snapshot_failure: Option<String>,
    /// Terminal projection failure retained without discarding consuming receipts.
    pub terminal_projection_failure: Option<String>,
    terminal_owner_snapshot: HeadlessEnduranceOwnerSnapshot,
}

impl EnduranceExecutionOwnerClosure {
    /// Whether every software execution owner returned without panic or detach.
    pub fn all_workers_terminated(&self) -> bool {
        self.preview.all_workers_terminated()
            && self.app.all_resources_released()
            && self.gpu.all_resources_retired()
    }

    /// Seal terminal gauges while retaining cumulative pre-shutdown failures.
    pub fn capture_facts(&self) -> EnduranceCaptureFacts {
        EnduranceCaptureFacts::from_headless_owner_snapshot(self.terminal_owner_snapshot)
    }
}

fn retain_terminal_owner_projection(
    owner_snapshot: HeadlessEnduranceOwnerSnapshot,
    projection: HeadlessEnduranceShutdownProjection,
    prior_failure: Option<String>,
) -> (HeadlessEnduranceOwnerSnapshot, Option<String>) {
    if let Some(failure) = prior_failure {
        return (
            owner_snapshot.fail_closed_after_shutdown(projection),
            Some(failure),
        );
    }
    match owner_snapshot.after_shutdown(projection) {
        Ok(snapshot) => (snapshot, None),
        Err(error) => (
            owner_snapshot.fail_closed_after_shutdown(projection),
            Some(format!("project Headless terminal owner snapshot: {error}")),
        ),
    }
}

/// Validation owner group using the production Headless Preview/GPU and Audio paths.
pub struct EnduranceExecutionOwners {
    realtime: Option<HeadlessRealtimePlaybackSession>,
}

#[derive(Default)]
struct EnduranceRealtimeObserver;

impl HeadlessGpuExecutionObserver for EnduranceRealtimeObserver {
    fn execution_completed(
        &mut self,
        _execution: super::headless_viewer_gpu::HeadlessViewerGpuExecution,
        _disposition: HeadlessGpuExecutionDisposition,
        _completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
    }

    fn current_output_presented(
        &mut self,
        _completed_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) {
    }

    fn successor_preparation(&mut self, _ready: bool) {}
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EnduranceRealtimeIntervalObservation {
    pub(super) before_epoch: u64,
    pub(super) before_frame: i64,
    pub(super) after_epoch: u64,
    pub(super) after_frame: i64,
    pub(super) sample: HeadlessPreviewSample,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EnduranceCachePressureObservation {
    pub(super) decision_revision: u64,
    pub(super) pressure: ExecutionResourcePressure,
    pub(super) trim: ResourceTrimRequest,
    pub(super) decision_sha256: String,
    pub(super) resource_decision_applications: u64,
    pub(super) media_cache_bytes: u64,
    pub(super) media_cache_entries: u64,
    pub(super) media_cache_resource_units: u64,
    pub(super) gpu_device_losses: u64,
    pub(super) fatal_errors: u64,
    pub(super) export_failures: u64,
}

impl EnduranceExecutionOwners {
    /// Start real software execution owners without admitting a campaign phase.
    pub fn start() -> Result<Self, EnduranceCampaignError> {
        let realtime = HeadlessRealtimePlaybackSession::new()
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        Ok(Self { realtime: Some(realtime) })
    }

    pub(super) fn begin_realtime_window(
        &mut self,
        app: &AppState,
        absolute_deadline: Option<Instant>,
    ) -> Result<(), EnduranceCampaignError> {
        self.realtime
            .as_mut()
            .ok_or_else(|| {
                EnduranceCampaignError::Runtime(
                    "Headless realtime execution session is missing".to_owned(),
                )
            })?
            .begin_realtime(app, absolute_deadline)
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))
    }

    pub(super) fn run_realtime_interval(
        &mut self,
        app: &mut AppState,
        gpu_completion_timeout: Duration,
    ) -> Result<EnduranceRealtimeIntervalObservation, EnduranceCampaignError> {
        let before_epoch = app.playback_epoch().get();
        let before_frame = app.current_frame();
        let mut observer = EnduranceRealtimeObserver;
        let sample = self
            .realtime
            .as_mut()
            .ok_or_else(|| {
                EnduranceCampaignError::Runtime(
                    "Headless realtime execution session is missing".to_owned(),
                )
            })?
            .run_production_av_interval(app, &mut observer, gpu_completion_timeout)
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        Ok(EnduranceRealtimeIntervalObservation {
            before_epoch,
            before_frame,
            after_epoch: app.playback_epoch().get(),
            after_frame: app.current_frame(),
            sample,
        })
    }

    pub(super) fn complete_current_picture(
        &mut self,
        app: &mut AppState,
        gpu_completion_timeout: Duration,
    ) -> Result<HeadlessPreviewSample, EnduranceCampaignError> {
        let mut observer = EnduranceRealtimeObserver;
        self.realtime
            .as_mut()
            .ok_or_else(|| {
                EnduranceCampaignError::Runtime(
                    "Headless realtime execution session is missing".to_owned(),
                )
            })?
            .complete_current_video_opportunity(app, &mut observer, gpu_completion_timeout)
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))
    }

    pub(super) fn capture_cache_pressure_observation(
        &self,
        app: &AppState,
    ) -> Result<EnduranceCachePressureObservation, EnduranceCampaignError> {
        let realtime = self.realtime.as_ref().ok_or_else(|| {
            EnduranceCampaignError::Runtime(
                "Headless realtime execution session is missing".to_owned(),
            )
        })?;
        let preview = realtime
            .preview()
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?
            .diagnostics();
        let owner = realtime
            .endurance_owner_snapshot(app)
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        let decision = app.execution_resource_decision();
        let export = app.export_endurance_snapshot(0);
        Ok(EnduranceCachePressureObservation {
            decision_revision: decision.revision,
            pressure: decision.pressure,
            trim: decision.preview.trim,
            decision_sha256: cache_pressure_decision_sha256(&decision)?,
            resource_decision_applications: preview.resource_decision_applications,
            media_cache_bytes: u64::try_from(preview.frame_store.media_reserved_bytes).map_err(
                |_| {
                    EnduranceCampaignError::Runtime(
                        "Preview media-cache bytes exceeded u64".to_owned(),
                    )
                },
            )?,
            media_cache_entries: u64::try_from(preview.frame_store.media_entries).map_err(
                |_| {
                    EnduranceCampaignError::Runtime(
                        "Preview media-cache entries exceeded u64".to_owned(),
                    )
                },
            )?,
            media_cache_resource_units: u64::try_from(preview.frame_store.media_resource_units)
                .map_err(|_| {
                    EnduranceCampaignError::Runtime(
                        "Preview media-cache resource units exceeded u64".to_owned(),
                    )
                })?,
            gpu_device_losses: owner.gpu_device_losses(),
            fatal_errors: owner.fatal_errors(),
            export_failures: export.failures,
        })
    }

    pub(super) fn apply_cache_pressure(
        &mut self,
        app: &AppState,
        pressure: ExecutionResourcePressure,
    ) -> Result<EnduranceCachePressureObservation, EnduranceCampaignError> {
        app.observe_execution_resource_pressure(pressure);
        let decision = app.execution_resource_decision();
        if decision.pressure != pressure
            || decision.pressure_source != ExecutionResourcePressureSource::Manual
        {
            return Err(EnduranceCampaignError::Runtime(
                "manual cache-pressure decision was not retained by the product coordinator"
                    .to_owned(),
            ));
        }
        let realtime = self.realtime.as_mut().ok_or_else(|| {
            EnduranceCampaignError::Runtime(
                "Headless realtime execution session is missing".to_owned(),
            )
        })?;
        let (preview, gpu) = realtime
            .bound_resources()
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        preview.apply_resource_decision(&decision.preview);
        gpu.apply_resource_decision(&decision.preview.viewer_gpu)
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        self.capture_cache_pressure_observation(app)
    }

    pub(super) fn finish_realtime_window(&mut self) -> Result<(), EnduranceCampaignError> {
        let _timing = self
            .realtime
            .as_mut()
            .ok_or_else(|| {
                EnduranceCampaignError::Runtime(
                    "Headless realtime execution session is missing".to_owned(),
                )
            })?
            .finish_realtime()
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        Ok(())
    }

    /// Capture owner-derived gauges and terminal counters at a settled boundary.
    pub fn capture_facts(
        &self,
        app: &AppState,
    ) -> Result<EnduranceCaptureFacts, EnduranceCampaignError> {
        let realtime = self.realtime.as_ref().ok_or_else(|| {
            EnduranceCampaignError::Runtime(
                "Headless realtime execution session is missing".to_owned(),
            )
        })?;
        let snapshot = realtime
            .endurance_owner_snapshot(app)
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        Ok(EnduranceCaptureFacts::from_headless_owner_snapshot(
            snapshot,
        ))
    }

    /// Capture the selected realtime-phase owner projection without exposing raw owners.
    pub fn runtime_snapshot(
        &self,
        app: &AppState,
        phase_kind: EndurancePhaseKind,
    ) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError> {
        let reference_output = match phase_kind {
            EndurancePhaseKind::ContinuousExport => {
                return Err(EnduranceCampaignError::Runtime(
                    "continuous Export phases do not own Headless realtime execution".to_owned(),
                ));
            }
            EndurancePhaseKind::PlaybackReference | EndurancePhaseKind::ConcurrentRecovery => {
                app.reference_output_diagnostics().cloned().ok_or_else(|| {
                    EnduranceCampaignError::Runtime(
                        "required Reference Output owner is missing".to_owned(),
                    )
                })?
            }
        };
        Ok(EnduranceRuntimeSnapshot {
            phase_kind,
            playback: app.playback_evidence_report(),
            reference_output,
            export: app.export_endurance_snapshot(0),
            capture_facts: self.capture_facts(app)?,
        })
    }

    /// Stop Preview and the App State's actual Audio owner, then retire GPU.
    pub fn shutdown_and_wait(
        self,
        app: AppState,
        gpu_timeout: Duration,
    ) -> Result<EnduranceExecutionOwnerClosure, EnduranceCampaignError> {
        let deadline = Instant::now().checked_add(gpu_timeout).unwrap_or_else(Instant::now);
        self.shutdown_until(app, deadline)
    }

    /// Consume all realtime/App owners against one caller-owned absolute deadline.
    pub(crate) fn shutdown_until(
        mut self,
        mut app: AppState,
        deadline: Instant,
    ) -> Result<EnduranceExecutionOwnerClosure, EnduranceCampaignError> {
        let transport_shutdown_failed = app.is_playing() && app.pause().is_err();
        let Some(realtime) = self.realtime.take() else {
            app.begin_endurance_shutdown();
            let app = app.shutdown_for_endurance(deadline);
            let (app_background, terminal_projection_failure) =
                match app.background_terminal_snapshot() {
                    Ok(snapshot) => (Some(snapshot), None),
                    Err(error) => (
                        None,
                        Some(format!("capture App background terminal snapshot: {error}")),
                    ),
                };
            let preview = PreviewRuntimeShutdownEvidence {
                schema_version: 2,
                unverified_async_reaps: 1,
                ..PreviewRuntimeShutdownEvidence::default()
            };
            let gpu = EnduranceGpuShutdownEvidence {
                worker_started: false,
                worker_terminated: false,
                worker_panicked: false,
                timed_out: false,
                retirement_handoff_accepted: false,
                retirement_completed: false,
                device_loss_count: 0,
                fatal_error_count: 1,
            };
            let terminal_owner_snapshot = HeadlessEnduranceOwnerSnapshot::failed_capture()
                .fail_closed_after_shutdown(HeadlessEnduranceShutdownProjection {
                    playback_owner_consumed: true,
                    preview_closed: false,
                    audio_closed: app.audio.all_workers_terminated(),
                    app_residual_owners_closed: app.all_residual_owner_resources_released(),
                    app_background,
                    gpu_closed: false,
                    gpu_device_losses: 0,
                    gpu_fatal_errors: 1,
                    transport_shutdown_failed,
                });
            return Ok(EnduranceExecutionOwnerClosure {
                preview,
                app,
                gpu,
                owner_snapshot_failure: Some(
                    "Headless realtime execution session was missing during consuming shutdown"
                        .to_owned(),
                ),
                terminal_projection_failure,
                terminal_owner_snapshot,
            });
        };
        let (mut preview_owner, gpu_owner) = realtime.into_shutdown_owners();
        let (owner_snapshot, owner_snapshot_failure) =
            match capture_headless_endurance_owner_snapshot(&preview_owner, &gpu_owner, &app) {
                Ok(snapshot) => (snapshot, None),
                Err(error) => (
                    HeadlessEnduranceOwnerSnapshot::failed_capture(),
                    Some(error.to_string()),
                ),
            };
        preview_owner.begin_endurance_shutdown();
        app.begin_endurance_shutdown();
        let gpu = gpu_owner.shutdown_until(deadline);
        let preview = preview_owner.shutdown_until(deadline);
        let app = app.shutdown_for_endurance(deadline);
        let device_loss_count = u64::from(
            gpu.generation_terminal_kind == Some(ViewerGpuDeviceGenerationTerminalKind::DeviceLost),
        );
        let fatal_error_count = u64::from(
            gpu.generation_terminal_kind
                == Some(ViewerGpuDeviceGenerationTerminalKind::ProgressFailure),
        );
        let gpu = EnduranceGpuShutdownEvidence {
            worker_started: gpu.worker_started,
            worker_terminated: gpu.worker_terminated,
            worker_panicked: gpu.worker_panicked,
            timed_out: gpu.timed_out,
            retirement_handoff_accepted: gpu.retirement_handoff_accepted,
            retirement_completed: gpu.retirement_completed,
            device_loss_count,
            fatal_error_count,
        };
        let (app_background, background_projection_failure) =
            match app.background_terminal_snapshot() {
                Ok(snapshot) => (Some(snapshot), None),
                Err(error) => (
                    None,
                    Some(format!("capture App background terminal snapshot: {error}")),
                ),
            };
        let (terminal_owner_snapshot, terminal_projection_failure) =
            retain_terminal_owner_projection(
                owner_snapshot,
                HeadlessEnduranceShutdownProjection {
                    playback_owner_consumed: true,
                    preview_closed: preview.all_workers_terminated(),
                    audio_closed: app.audio.all_workers_terminated(),
                    app_residual_owners_closed: app.all_residual_owner_resources_released(),
                    app_background,
                    gpu_closed: gpu.all_resources_retired(),
                    gpu_device_losses: gpu.device_loss_count,
                    gpu_fatal_errors: gpu.fatal_error_count,
                    transport_shutdown_failed,
                },
                background_projection_failure,
            );
        Ok(EnduranceExecutionOwnerClosure {
            preview,
            app,
            gpu,
            owner_snapshot_failure,
            terminal_projection_failure,
            terminal_owner_snapshot,
        })
    }
}

fn cache_pressure_decision_sha256(
    decision: &super::execution_resource_coordination::ExecutionResourceDecisionSnapshot,
) -> Result<String, EnduranceCampaignError> {
    let frame_store = decision.preview.frame_store;
    let mut hasher = Sha256::new();
    hasher.update(b"mondrian.endurance.cache-pressure-decision.v1\0");
    hasher.update(decision.schema_version.to_le_bytes());
    hasher.update(decision.revision.to_le_bytes());
    hasher.update([match decision.pressure {
        ExecutionResourcePressure::Nominal => 0,
        ExecutionResourcePressure::Elevated => 1,
        ExecutionResourcePressure::Critical => 2,
    }]);
    hasher.update([match decision.pressure_source {
        ExecutionResourcePressureSource::Baseline => 0,
        ExecutionResourcePressureSource::Manual => 1,
        ExecutionResourcePressureSource::NativeMemory => 2,
    }]);
    hasher.update([match decision.preview.trim {
        ResourceTrimRequest::None => 0,
        ResourceTrimRequest::Speculative => 1,
        ResourceTrimRequest::Aggressive => 2,
    }]);
    for value in [
        frame_store.media_entry_capacity,
        frame_store.media_byte_budget,
        frame_store.media_resource_unit_budget,
        frame_store.current_media_working_set_entry_limit,
        frame_store.current_media_working_set_byte_limit,
        frame_store.current_media_working_set_resource_unit_limit,
        frame_store.viewer_entry_capacity,
        frame_store.viewer_byte_budget,
        frame_store.failure_entry_capacity,
    ] {
        hasher.update(
            u64::try_from(value)
                .map_err(|_| {
                    EnduranceCampaignError::Runtime(
                        "cache-pressure decision field exceeded u64".to_owned(),
                    )
                })?
                .to_le_bytes(),
        );
    }
    hasher.update([u8::from(decision.preview.viewer_gpu.clear_idle)]);
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Process-monotonic campaign clock. UTC is never duration authority.
pub trait EnduranceCampaignClock {
    /// Microseconds elapsed since the campaign process established its origin.
    fn elapsed_us(&self) -> u64;
}

/// Production clock backed by one process-local `Instant` origin.
#[derive(Debug)]
pub struct SystemEnduranceCampaignClock {
    origin: Instant,
}

impl SystemEnduranceCampaignClock {
    /// Establish one campaign origin before the first phase starts.
    pub fn new() -> Self {
        Self { origin: Instant::now() }
    }
}

impl Default for SystemEnduranceCampaignClock {
    fn default() -> Self {
        Self::new()
    }
}

impl EnduranceCampaignClock for SystemEnduranceCampaignClock {
    fn elapsed_us(&self) -> u64 {
        self.origin.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }
}

/// One typed semantic event emitted by the actual product workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnduranceCampaignEvent {
    /// Independent re-open/content verification of a published Export artifact.
    ExportArtifactVerified(VerifiedExportArtifactEvent),
    /// One exact operation in a controlled recovery cycle.
    RecoveryStepCompleted(VerifiedRecoveryStepEvent),
}

/// Sealed App projection of one independently verified Export artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExportArtifactEvent {
    completed_at_us: u64,
    artifact_id: String,
    artifact_sha256: String,
    validator_id: String,
    validation_report_sha256: String,
}

/// Sealed projection of one owner-derived, independently replayable receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRecoveryStepEvent {
    completed_at_us: u64,
    cycle_index: u32,
    step: EnduranceRecoveryStep,
    operation_receipt_json: String,
    operation_receipt_sha256: String,
}

impl EnduranceCampaignEvent {
    /// Construct an Export event only from a completed independent verifier receipt.
    pub fn export_artifact_verified(
        completed_at_us: u64,
        receipt: &mondrian_export::IndependentExportArtifactReceipt,
    ) -> Self {
        Self::ExportArtifactVerified(VerifiedExportArtifactEvent {
            completed_at_us,
            artifact_id: receipt.report().artifact_id.clone(),
            artifact_sha256: receipt.report().artifact_sha256.clone(),
            validator_id: receipt.report().validator_id.to_owned(),
            validation_report_sha256: receipt.validation_report_sha256().to_owned(),
        })
    }

    /// Construct a recovery event only from a sealed owner receipt.
    pub fn recovery_step_completed(
        completed_at_us: u64,
        receipt: &EnduranceRecoveryOperationReceipt,
    ) -> Self {
        Self::RecoveryStepCompleted(VerifiedRecoveryStepEvent {
            completed_at_us,
            cycle_index: receipt.cycle_index(),
            step: receipt.step(),
            operation_receipt_json: receipt.canonical_json().to_owned(),
            operation_receipt_sha256: receipt.sha256().to_owned(),
        })
    }

    #[cfg(test)]
    pub(super) fn test_export_artifact_verified(
        completed_at_us: u64,
        artifact_id: impl Into<String>,
        artifact_sha256: impl Into<String>,
        validator_id: impl Into<String>,
        validation_report_sha256: impl Into<String>,
    ) -> Self {
        Self::ExportArtifactVerified(VerifiedExportArtifactEvent {
            completed_at_us,
            artifact_id: artifact_id.into(),
            artifact_sha256: artifact_sha256.into(),
            validator_id: validator_id.into(),
            validation_report_sha256: validation_report_sha256.into(),
        })
    }

    #[cfg(test)]
    pub(super) fn test_recovery_receipt(&self) -> Option<(u32, EnduranceRecoveryStep, &str, &str)> {
        let Self::RecoveryStepCompleted(event) = self else {
            return None;
        };
        Some((
            event.cycle_index,
            event.step,
            &event.operation_receipt_json,
            &event.operation_receipt_sha256,
        ))
    }
}

/// Coordinator-bound projection of product-owned diagnostics at one cadence.
///
/// Each domain snapshot is internally consistent. The surrounding sample's
/// start/completion interval is the cross-domain capture envelope; independent
/// execution threads do not claim one global linearization instant.
#[derive(Debug, Clone)]
pub struct EnduranceRuntimeSnapshot {
    /// Phase provenance sealed by the concrete owner adapter.
    phase_kind: EndurancePhaseKind,
    /// Playback-owned evidence.
    playback: PlaybackEvidenceReport,
    /// Reference Output-owned diagnostics.
    reference_output: ReferenceOutputDiagnostics,
    /// Export-owned bounded diagnostics. The coordinator stamps publication time.
    export: ExportEnduranceSnapshot,
    /// Remaining App/runtime facts derived by the concrete product coordinator.
    capture_facts: EnduranceCaptureFacts,
}

impl EnduranceRuntimeSnapshot {
    /// Seal an Export-only phase without inventing realtime execution owners.
    pub fn continuous_export(
        playback: PlaybackEvidenceReport,
        reference_output: ReferenceOutputDiagnostics,
        export: ExportEnduranceSnapshot,
    ) -> Self {
        Self {
            phase_kind: EndurancePhaseKind::ContinuousExport,
            playback,
            reference_output,
            export,
            capture_facts: EnduranceCaptureFacts::for_continuous_export(),
        }
    }

    /// Replace live diagnostics with facts captured by the consuming terminal
    /// owners while preserving the phase identity and playback evidence.
    pub(crate) fn terminalize(
        mut self,
        reference_output: ReferenceOutputDiagnostics,
        export: ExportEnduranceSnapshot,
        capture_facts: EnduranceCaptureFacts,
    ) -> Self {
        self.reference_output = reference_output;
        self.export = export;
        self.capture_facts = capture_facts;
        self
    }

    pub(crate) fn with_capture_facts(mut self, capture_facts: EnduranceCaptureFacts) -> Self {
        self.capture_facts = capture_facts;
        self
    }
}

/// Typed terminal closure for the currently evidenced software and Export owners.
///
/// Reference Output contributes its final accounting snapshot separately. A
/// consuming vendor thread/device shutdown receipt remains a qualification
/// follow-on and is not implied by this projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceRuntimeClosure {
    /// Completed or failed; `NotRun` is only legal at admission.
    pub status: EndurancePhaseTerminalStatus,
    /// Whether Playback/Preview/Audio/GPU workers synchronously returned.
    pub playback_workers_terminated: bool,
    /// Supervised descendants still owned after shutdown.
    pub supervised_child_processes_remaining: u32,
    /// Export's consuming worker-shutdown receipt.
    pub export: ExportQueueShutdownEvidence,
}

impl EnduranceRuntimeClosure {
    fn proves_consuming_cleanup(self) -> bool {
        self.status != EndurancePhaseTerminalStatus::NotRun
            && self.playback_workers_terminated
            && self.supervised_child_processes_remaining == 0
            && self.export.all_resources_released()
    }

    fn incomplete_cleanup_error(self) -> EnduranceCampaignError {
        EnduranceCampaignError::IncompletePhaseCleanup {
            status: self.status,
            playback_workers_terminated: self.playback_workers_terminated,
            supervised_child_processes_remaining: self.supervised_child_processes_remaining,
            export: self.export,
        }
    }
}

/// Product-owned execution surface consumed by the serial supervisor.
pub trait EnduranceCampaignRuntime {
    /// Admit and start one exact checked-in workload.
    ///
    /// An error may follow partial owner creation. The supervisor will invoke
    /// `shutdown_phase` exactly once, so implementations must retain enough
    /// state to make that call consuming and idempotent for the failed start.
    fn begin_phase(
        &mut self,
        requirement: &EndurancePhaseRequirement,
        workload: &PreparedEnduranceWorkload,
        phase_started_at_run_us: u64,
    ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError>;

    /// Pump real product work until the absolute campaign deadline is reached.
    fn pump_until(
        &mut self,
        deadline_run_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, EnduranceCampaignError>;

    /// Snapshot all product owners inside one coordinator-bounded envelope.
    /// Snapshot adapters may refresh bounded diagnostic caches, but must not
    /// schedule, pump, or poll phase work through this call.
    fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError>;

    /// Stop work and synchronously return the currently evidenced software and
    /// Export workers/resources.
    ///
    /// Once called, this operation is consuming even if it returns an error;
    /// the implementation must still exhaust its in-scope cleanup path.
    /// Reference Output vendor-thread/device consumption is not yet represented
    /// by `EnduranceRuntimeClosure`.
    fn shutdown_phase(
        &mut self,
    ) -> Result<(EnduranceRuntimeClosure, Vec<EnduranceCampaignEvent>), EnduranceCampaignError>;
}

/// Immutable inputs for one complete serial campaign.
#[derive(Debug, Clone)]
pub struct EnduranceCampaignRequest {
    /// Compiled qualification profile.
    pub profile: EnduranceQualificationProfile,
    /// Exact release and machine identity.
    pub identity: EnduranceRunIdentity,
    /// Pre-issued capture authority manifest.
    pub capture_authority_manifest_path: PathBuf,
    /// Create-only evidence directory.
    pub evidence_directory: PathBuf,
    /// Create-only final run manifest path.
    pub output_manifest_path: PathBuf,
    /// Exact raw workload contract path for every profile phase.
    pub workload_contracts: BTreeMap<String, PathBuf>,
}

/// Execute one serial campaign through the provided real product runtime.
pub fn run_endurance_campaign<R, P, C>(
    request: EnduranceCampaignRequest,
    runtime: &mut R,
    process_memory: &P,
    clock: &C,
) -> Result<EnduranceRunManifest, EnduranceCampaignError>
where
    R: EnduranceCampaignRuntime,
    P: ProcessMemoryProbe,
    C: EnduranceCampaignClock,
{
    validate_workload_map(&request.profile, &request.workload_contracts)?;
    let profile = request.profile.clone();
    let mut capture = EnduranceRunCapture::new(
        request.profile,
        request.identity,
        &request.capture_authority_manifest_path,
    )?;

    for requirement in &profile.phases {
        let workload_path = request
            .workload_contracts
            .get(&requirement.phase_id)
            .ok_or_else(|| EnduranceCampaignError::MissingWorkload(requirement.phase_id.clone()))?;
        let workload = PreparedEnduranceWorkload::load(requirement, workload_path)?;
        let started_at_run_us = clock.elapsed_us();
        let phase = capture.begin_phase(
            &requirement.phase_id,
            started_at_run_us,
            &request.evidence_directory,
            workload_path,
        )?;
        let admission = match runtime.begin_phase(requirement, &workload, started_at_run_us) {
            Ok(admission) => admission,
            Err(primary @ EnduranceCampaignError::PreStartRuntime(_)) => return Err(primary),
            Err(primary) => return Err(cleanup_started_phase(runtime, primary)),
        };
        match admission {
            EndurancePhaseAdmission::NotRun(_) => {
                capture.commit_phase(phase.finish_not_run()?)?;
            }
            EndurancePhaseAdmission::Started => {
                let manifest = execute_started_phase(
                    requirement,
                    started_at_run_us,
                    phase,
                    runtime,
                    process_memory,
                    clock,
                    profile.sample_interval_us,
                )?;
                capture.commit_phase(manifest)?;
            }
        }
    }
    capture.seal_manifest(&request.output_manifest_path).map_err(Into::into)
}

#[allow(clippy::too_many_arguments)]
fn execute_started_phase<R, P, C>(
    requirement: &EndurancePhaseRequirement,
    started_at_run_us: u64,
    mut phase: EndurancePhaseCapture,
    runtime: &mut R,
    process_memory: &P,
    clock: &C,
    sample_interval_us: u64,
) -> Result<mondrian_platform::EndurancePhaseManifest, EnduranceCampaignError>
where
    R: EnduranceCampaignRuntime,
    P: ProcessMemoryProbe,
    C: EnduranceCampaignClock,
{
    let running = (|| {
        let mut sequence = 0_u64;
        let mut scheduled_at_us = 0_u64;
        while scheduled_at_us < requirement.minimum_duration_us {
            pump_and_record(runtime, &mut phase, started_at_run_us, scheduled_at_us)?;
            capture_one_sample(
                runtime,
                &mut phase,
                process_memory,
                clock,
                requirement.kind,
                started_at_run_us,
                sequence,
                scheduled_at_us,
            )?;
            sequence = sequence.checked_add(1).ok_or(EnduranceCampaignError::TimeOverflow)?;
            scheduled_at_us = scheduled_at_us
                .checked_add(sample_interval_us)
                .ok_or(EnduranceCampaignError::TimeOverflow)?;
        }

        let final_scheduled_at_us = requirement.minimum_duration_us;
        pump_and_record(
            runtime,
            &mut phase,
            started_at_run_us,
            final_scheduled_at_us,
        )?;
        Ok::<_, EnduranceCampaignError>((sequence, final_scheduled_at_us))
    })();
    let (sequence, final_scheduled_at_us) = match running {
        Ok(boundary) => boundary,
        Err(primary) => {
            return Err(cleanup_started_phase(runtime, primary));
        }
    };
    let (closure, events) = runtime.shutdown_phase()?;
    if closure.status == EndurancePhaseTerminalStatus::NotRun {
        return Err(EnduranceCampaignError::InvalidTerminalStatus);
    }
    if !closure.proves_consuming_cleanup() {
        return Err(closure.incomplete_cleanup_error());
    }
    record_events(&mut phase, events)?;
    let final_snapshot = capture_one_sample(
        runtime,
        &mut phase,
        process_memory,
        clock,
        requirement.kind,
        started_at_run_us,
        sequence,
        final_scheduled_at_us,
    )?;
    let completed_at_run_us = started_at_run_us
        .checked_add(final_snapshot.completed_at_us)
        .ok_or(EnduranceCampaignError::TimeOverflow)?;
    phase
        .finish(
            completed_at_run_us,
            closure.status,
            closure.playback_workers_terminated,
            closure.supervised_child_processes_remaining,
            &final_snapshot.reference_output,
            closure.export,
        )
        .map_err(Into::into)
}

fn cleanup_started_phase<R: EnduranceCampaignRuntime>(
    runtime: &mut R,
    primary: EnduranceCampaignError,
) -> EnduranceCampaignError {
    match runtime.shutdown_phase() {
        Ok((closure, _)) if closure.proves_consuming_cleanup() => primary,
        Ok((closure, _)) => EnduranceCampaignError::StartedPhaseCleanup {
            primary: Box::new(primary),
            cleanup: Box::new(closure.incomplete_cleanup_error()),
        },
        Err(cleanup) => EnduranceCampaignError::StartedPhaseCleanup {
            primary: Box::new(primary),
            cleanup: Box::new(cleanup),
        },
    }
}

fn pump_and_record<R: EnduranceCampaignRuntime>(
    runtime: &mut R,
    phase: &mut EndurancePhaseCapture,
    started_at_run_us: u64,
    scheduled_at_us: u64,
) -> Result<(), EnduranceCampaignError> {
    let deadline = started_at_run_us
        .checked_add(scheduled_at_us)
        .ok_or(EnduranceCampaignError::TimeOverflow)?;
    record_events(phase, runtime.pump_until(deadline)?)
}

fn record_events(
    phase: &mut EndurancePhaseCapture,
    events: Vec<EnduranceCampaignEvent>,
) -> Result<(), EnduranceCampaignError> {
    for event in events {
        match event {
            EnduranceCampaignEvent::ExportArtifactVerified(VerifiedExportArtifactEvent {
                completed_at_us,
                artifact_id,
                artifact_sha256,
                validator_id,
                validation_report_sha256,
            }) => phase.record_export_artifact_verified(
                completed_at_us,
                &artifact_id,
                &artifact_sha256,
                &validator_id,
                &validation_report_sha256,
            )?,
            EnduranceCampaignEvent::RecoveryStepCompleted(event) => phase
                .record_recovery_step_completed(
                    event.completed_at_us,
                    event.cycle_index,
                    event.step,
                    &event.operation_receipt_json,
                    &event.operation_receipt_sha256,
                )?,
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn capture_one_sample<R, P, C>(
    runtime: &mut R,
    phase: &mut EndurancePhaseCapture,
    process_memory: &P,
    clock: &C,
    expected_phase_kind: EndurancePhaseKind,
    started_at_run_us: u64,
    sequence: u64,
    scheduled_at_us: u64,
) -> Result<FinalSnapshot, EnduranceCampaignError>
where
    R: EnduranceCampaignRuntime,
    P: ProcessMemoryProbe,
    C: EnduranceCampaignClock,
{
    let started_at_us = clock
        .elapsed_us()
        .checked_sub(started_at_run_us)
        .ok_or(EnduranceCampaignError::TimeRegression)?;
    let memory = process_memory.process_memory(ProcessMemoryScope::ProductProcessTree);
    let mut snapshot = runtime.snapshot()?;
    if snapshot.phase_kind != expected_phase_kind {
        return Err(EnduranceCampaignError::SnapshotPhaseMismatch {
            expected: expected_phase_kind,
            actual: snapshot.phase_kind,
        });
    }
    let completed_at_us = clock
        .elapsed_us()
        .checked_sub(started_at_run_us)
        .ok_or(EnduranceCampaignError::TimeRegression)?;
    snapshot.export.observed_at_us = completed_at_us;
    snapshot.capture_facts.stamp_observed_at_us(completed_at_us);
    phase.capture_and_push(
        EnduranceSampleTiming {
            sequence,
            scheduled_at_us,
            started_at_us,
            completed_at_us,
        },
        &memory,
        &snapshot.playback,
        &snapshot.reference_output,
        snapshot.export,
        snapshot.capture_facts,
    )?;
    Ok(FinalSnapshot {
        completed_at_us,
        reference_output: snapshot.reference_output,
    })
}

struct FinalSnapshot {
    completed_at_us: u64,
    reference_output: ReferenceOutputDiagnostics,
}

fn validate_workload_map(
    profile: &EnduranceQualificationProfile,
    workloads: &BTreeMap<String, PathBuf>,
) -> Result<(), EnduranceCampaignError> {
    if workloads.len() != profile.phases.len()
        || profile.phases.iter().any(|phase| !workloads.contains_key(&phase.phase_id))
    {
        return Err(EnduranceCampaignError::WorkloadClosure);
    }
    Ok(())
}

/// Stable campaign coordination failure.
#[derive(Debug, Error)]
pub enum EnduranceCampaignError {
    /// Evidence capture/publication failed closed.
    #[error(transparent)]
    Capture(#[from] EnduranceCaptureError),
    /// Concrete product runtime rejected or failed one operation.
    #[error("endurance product runtime failed: {0}")]
    Runtime(String),
    /// Read-only machine preflight failed before any phase owner was created.
    #[error("endurance product pre-start inspection failed: {0}")]
    PreStartRuntime(String),
    /// A phase workload path was absent.
    #[error("endurance workload is missing for phase '{0}'")]
    MissingWorkload(String),
    /// Workload path map had a missing or extra phase.
    #[error("endurance workload path map does not exactly close over the profile")]
    WorkloadClosure,
    /// Campaign monotonic arithmetic overflowed.
    #[error("endurance campaign monotonic time overflowed")]
    TimeOverflow,
    /// Campaign clock regressed relative to the phase origin.
    #[error("endurance campaign monotonic clock regressed")]
    TimeRegression,
    /// A started phase failed and its consuming cleanup also failed.
    #[error("started endurance phase failed ({primary}) and cleanup failed ({cleanup})")]
    StartedPhaseCleanup {
        /// Original execution or capture failure.
        primary: Box<EnduranceCampaignError>,
        /// Failure returned by the consuming shutdown path.
        cleanup: Box<EnduranceCampaignError>,
    },
    /// A nominally successful cleanup receipt still retained owned execution.
    #[error(
        "endurance phase cleanup was incomplete: status={status:?}, playback_workers_terminated={playback_workers_terminated}, supervised_children={supervised_child_processes_remaining}, export={export:?}"
    )]
    IncompletePhaseCleanup {
        /// Terminal status returned by the runtime.
        status: EndurancePhaseTerminalStatus,
        /// Playback/Preview/Audio/GPU closure projection.
        playback_workers_terminated: bool,
        /// Supervised descendants still retained.
        supervised_child_processes_remaining: u32,
        /// Complete Export shutdown receipt, retained without lossy projection.
        export: ExportQueueShutdownEvidence,
    },
    /// A started runtime attempted to terminate as NotRun.
    #[error("a started endurance phase cannot terminate as not-run")]
    InvalidTerminalStatus,
    /// A runtime returned owner facts sealed for a different phase kind.
    #[error("endurance snapshot phase mismatch: expected {expected:?}, got {actual:?}")]
    SnapshotPhaseMismatch {
        /// Kind required by the active phase.
        expected: EndurancePhaseKind,
        /// Kind sealed into the returned snapshot.
        actual: EndurancePhaseKind,
    },
    /// Checked-in workload bytes did not compile into the exact typed contract.
    #[error(transparent)]
    Workload(#[from] EnduranceWorkloadError),
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mondrian_platform::{
        EnduranceQualificationStatus, PreparedEnduranceQualification, ProcessMemoryProbeBackend,
        ProcessMemoryProbeResult,
    };
    use mondrian_playback::{PlaybackEvidenceCollector, PlaybackEvidenceConfig};

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn empty_export_snapshot() -> ExportEnduranceSnapshot {
        ExportEnduranceSnapshot {
            schema_version: 2,
            observed_at_us: 0,
            shutdown_requested: false,
            worker_running: true,
            worker_terminated: false,
            activity_events: 0,
            admissions: 0,
            rejections: 0,
            completions: 0,
            failures: 0,
            cancellations: 0,
            rendered_frames: 0,
            durable_artifacts: 0,
            pending_jobs: 0,
            active_jobs: 0,
            worker_failed: false,
            audio_source_owners_started: 0,
            audio_source_owners_closed: 0,
            audio_source_owner_failures: 0,
            active_audio_source_owners: 0,
        }
    }

    #[test]
    fn continuous_export_snapshot_seals_zero_realtime_owner_facts() {
        let playback = PlaybackEvidenceCollector::new(PlaybackEvidenceConfig::default())
            .expect("playback collector")
            .report();
        let snapshot = EnduranceRuntimeSnapshot::continuous_export(
            playback,
            ReferenceOutputDiagnostics::default(),
            empty_export_snapshot(),
        );

        assert_eq!(snapshot.capture_facts, EnduranceCaptureFacts::default());
    }

    #[test]
    fn terminal_projection_failure_is_retained_without_discarding_owner_facts() {
        let running = HeadlessEnduranceOwnerSnapshot::test_fixture(0, 5, 7, 2, 3);
        let projection = HeadlessEnduranceShutdownProjection {
            playback_owner_consumed: true,
            preview_closed: true,
            audio_closed: true,
            app_residual_owners_closed: true,
            app_background: None,
            gpu_closed: true,
            gpu_device_losses: 2,
            gpu_fatal_errors: 0,
            transport_shutdown_failed: false,
        };

        let (terminal, failure) = retain_terminal_owner_projection(
            running,
            projection,
            Some("capture App background terminal snapshot: overflow".to_owned()),
        );

        assert_eq!(
            failure.as_deref(),
            Some("capture App background terminal snapshot: overflow")
        );
        assert_eq!(terminal.playback_pending(), 1);
        assert_eq!(terminal.other_queue_depth(), 5);
        assert_eq!(terminal.owned_resource_units(), 7);
        assert_eq!(terminal.gpu_device_losses(), 2);
        assert_eq!(terminal.fatal_errors(), 4);
    }

    #[test]
    #[ignore = "requires a real Headless GPU Adapter and native scheduling admission"]
    fn active_realtime_session_gates_raw_access_and_still_closes_all_owners() {
        let mut state = AppState::new();
        state.set_playback_frame_running(0);
        assert!(state.is_playing());
        let mut owners = EnduranceExecutionOwners::start().expect("start execution owners");
        {
            let realtime = owners.realtime.as_mut().expect("paired realtime session");
            realtime.begin_realtime(&state, None).expect("begin realtime residency");
            assert!(realtime.preview().is_err());
            assert!(realtime.gpu().is_err());
            assert!(realtime.gpu_mut().is_err());
            assert!(realtime.bound_resources().is_err());
        }

        let closure = owners
            .shutdown_and_wait(state, std::time::Duration::from_secs(30))
            .expect("close active execution owners");

        assert!(closure.preview.all_workers_terminated());
        assert!(closure.app.audio.all_workers_terminated());
        assert!(closure.app.all_resources_released());
        assert!(closure.gpu.all_resources_retired());
        assert!(closure.all_workers_terminated());
    }

    #[derive(Default)]
    struct FakeClock(AtomicU64);

    impl EnduranceCampaignClock for FakeClock {
        fn elapsed_us(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    struct FakeMemory<'a>(&'a FakeClock);

    impl ProcessMemoryProbe for FakeMemory<'_> {
        fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
            self.0 .0.fetch_add(1, Ordering::Relaxed);
            ProcessMemoryProbeResult::observed(
                scope,
                ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                1,
                1,
                100,
                100,
                100,
            )
        }
    }

    struct FakeRuntime<'a> {
        clock: &'a FakeClock,
        kind: Option<mondrian_platform::EndurancePhaseKind>,
        phase_started_at_us: u64,
        verified_exports: u64,
        shutdown: bool,
    }

    impl<'a> FakeRuntime<'a> {
        fn new(clock: &'a FakeClock) -> Self {
            Self {
                clock,
                kind: None,
                phase_started_at_us: 0,
                verified_exports: 0,
                shutdown: false,
            }
        }

        fn phase_elapsed_us(&self) -> u64 {
            self.clock.elapsed_us().saturating_sub(self.phase_started_at_us)
        }
    }

    impl EnduranceCampaignRuntime for FakeRuntime<'_> {
        fn begin_phase(
            &mut self,
            requirement: &EndurancePhaseRequirement,
            workload: &PreparedEnduranceWorkload,
            phase_started_at_run_us: u64,
        ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError> {
            self.kind = Some(requirement.kind);
            self.phase_started_at_us = phase_started_at_run_us;
            self.verified_exports = 0;
            self.shutdown = false;
            Ok(
                if requirement.kind == mondrian_platform::EndurancePhaseKind::ContinuousExport {
                    EndurancePhaseAdmission::Started
                } else {
                    workload.admit(&Default::default())
                },
            )
        }

        fn pump_until(
            &mut self,
            deadline_run_us: u64,
        ) -> Result<Vec<EnduranceCampaignEvent>, EnduranceCampaignError> {
            self.clock.0.store(deadline_run_us, Ordering::Relaxed);
            if self.kind != Some(mondrian_platform::EndurancePhaseKind::ContinuousExport) {
                return Ok(Vec::new());
            }
            let target = (self.phase_elapsed_us() / 3_600_000_000).min(24);
            let mut events = Vec::new();
            while self.verified_exports < target {
                self.verified_exports += 1;
                events.push(EnduranceCampaignEvent::test_export_artifact_verified(
                    self.verified_exports * 3_600_000_000,
                    format!("artifact-{}", self.verified_exports),
                    SHA,
                    "independent-validator-v1",
                    SHA,
                ));
            }
            Ok(events)
        }

        fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError> {
            let elapsed = self.phase_elapsed_us();
            let is_export =
                self.kind == Some(mondrian_platform::EndurancePhaseKind::ContinuousExport);
            Ok(EnduranceRuntimeSnapshot {
                phase_kind: self.kind.expect("started phase kind"),
                playback: PlaybackEvidenceCollector::new(PlaybackEvidenceConfig::default())
                    .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?
                    .report(),
                reference_output: ReferenceOutputDiagnostics::default(),
                export: ExportEnduranceSnapshot {
                    schema_version: 2,
                    observed_at_us: 0,
                    shutdown_requested: self.shutdown,
                    worker_running: !self.shutdown,
                    worker_terminated: self.shutdown,
                    activity_events: if is_export { elapsed / 60_000_000 } else { 0 },
                    admissions: self.verified_exports,
                    rejections: 0,
                    completions: self.verified_exports,
                    failures: 0,
                    cancellations: 0,
                    rendered_frames: if is_export {
                        u64::try_from(
                            u128::from(elapsed).saturating_mul(432_000)
                                / u128::from(86_400_000_000_u64),
                        )
                        .unwrap_or(u64::MAX)
                    } else {
                        0
                    },
                    durable_artifacts: self.verified_exports,
                    pending_jobs: 0,
                    active_jobs: 0,
                    worker_failed: false,
                    audio_source_owners_started: self.verified_exports,
                    audio_source_owners_closed: self.verified_exports,
                    audio_source_owner_failures: 0,
                    active_audio_source_owners: 0,
                },
                capture_facts: EnduranceCaptureFacts::default(),
            })
        }

        fn shutdown_phase(
            &mut self,
        ) -> Result<(EnduranceRuntimeClosure, Vec<EnduranceCampaignEvent>), EnduranceCampaignError>
        {
            self.shutdown = true;
            Ok((
                EnduranceRuntimeClosure {
                    status: EndurancePhaseTerminalStatus::Completed,
                    playback_workers_terminated: true,
                    supervised_child_processes_remaining: 0,
                    export: ExportQueueShutdownEvidence {
                        schema_version: 4,
                        worker_started: true,
                        worker_terminated: true,
                        worker_start_failed: false,
                        worker_panicked: false,
                        worker_timed_out: false,
                        worker_detached: false,
                        worker_owner_abandoned: false,
                        pending_jobs: 0,
                        active_jobs: 0,
                        activity_events: self.phase_elapsed_us() / 60_000_000,
                        audio_source_owners_started: self.verified_exports,
                        audio_source_owners_closed: self.verified_exports,
                        audio_source_owner_failures: 0,
                        active_audio_source_owners: 0,
                    },
                },
                Vec::new(),
            ))
        }
    }

    struct CleanupRuntime {
        shutdown_calls: u32,
        cleanup_fails: bool,
        cleanup_incomplete: bool,
    }

    impl EnduranceCampaignRuntime for CleanupRuntime {
        fn begin_phase(
            &mut self,
            _requirement: &EndurancePhaseRequirement,
            _workload: &PreparedEnduranceWorkload,
            _phase_started_at_run_us: u64,
        ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError> {
            unreachable!("cleanup test never admits a phase")
        }

        fn pump_until(
            &mut self,
            _deadline_run_us: u64,
        ) -> Result<Vec<EnduranceCampaignEvent>, EnduranceCampaignError> {
            unreachable!("cleanup test never pumps")
        }

        fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError> {
            unreachable!("cleanup test never snapshots")
        }

        fn shutdown_phase(
            &mut self,
        ) -> Result<(EnduranceRuntimeClosure, Vec<EnduranceCampaignEvent>), EnduranceCampaignError>
        {
            self.shutdown_calls += 1;
            if self.cleanup_fails {
                return Err(EnduranceCampaignError::Runtime("cleanup".to_owned()));
            }
            Ok((
                EnduranceRuntimeClosure {
                    status: EndurancePhaseTerminalStatus::Failed,
                    playback_workers_terminated: !self.cleanup_incomplete,
                    supervised_child_processes_remaining: 0,
                    export: ExportQueueShutdownEvidence {
                        schema_version: 4,
                        worker_started: true,
                        worker_terminated: true,
                        worker_start_failed: false,
                        worker_panicked: false,
                        worker_timed_out: false,
                        worker_detached: false,
                        worker_owner_abandoned: false,
                        pending_jobs: 0,
                        active_jobs: 0,
                        activity_events: 0,
                        audio_source_owners_started: 0,
                        audio_source_owners_closed: 0,
                        audio_source_owner_failures: 0,
                        active_audio_source_owners: 0,
                    },
                },
                Vec::new(),
            ))
        }
    }

    #[test]
    fn started_phase_failure_always_consumes_runtime_cleanup() {
        let mut runtime = CleanupRuntime {
            shutdown_calls: 0,
            cleanup_fails: false,
            cleanup_incomplete: false,
        };
        let error = cleanup_started_phase(
            &mut runtime,
            EnduranceCampaignError::Runtime("primary".to_owned()),
        );
        assert_eq!(runtime.shutdown_calls, 1);
        assert!(matches!(error, EnduranceCampaignError::Runtime(detail) if detail == "primary"));

        let mut runtime = CleanupRuntime {
            shutdown_calls: 0,
            cleanup_fails: true,
            cleanup_incomplete: false,
        };
        let error = cleanup_started_phase(
            &mut runtime,
            EnduranceCampaignError::Runtime("primary".to_owned()),
        );
        assert_eq!(runtime.shutdown_calls, 1);
        assert!(matches!(
            error,
            EnduranceCampaignError::StartedPhaseCleanup { primary, cleanup }
                if matches!(*primary, EnduranceCampaignError::Runtime(ref detail) if detail == "primary")
                    && matches!(*cleanup, EnduranceCampaignError::Runtime(ref detail) if detail == "cleanup")
        ));

        let mut runtime = CleanupRuntime {
            shutdown_calls: 0,
            cleanup_fails: false,
            cleanup_incomplete: true,
        };
        let error = cleanup_started_phase(
            &mut runtime,
            EnduranceCampaignError::Runtime("primary".to_owned()),
        );
        assert_eq!(runtime.shutdown_calls, 1);
        assert!(matches!(
            error,
            EnduranceCampaignError::StartedPhaseCleanup { cleanup, .. }
                if matches!(*cleanup, EnduranceCampaignError::IncompletePhaseCleanup {
                    playback_workers_terminated: false,
                    ..
                })
        ));
    }

    #[test]
    fn incomplete_cleanup_retains_joined_late_export_receipt() {
        let export = ExportQueueShutdownEvidence {
            schema_version: 4,
            worker_started: true,
            worker_start_failed: false,
            worker_terminated: true,
            worker_panicked: false,
            worker_timed_out: true,
            worker_detached: false,
            worker_owner_abandoned: false,
            pending_jobs: 0,
            active_jobs: 0,
            activity_events: 9,
            audio_source_owners_started: 1,
            audio_source_owners_closed: 1,
            audio_source_owner_failures: 0,
            active_audio_source_owners: 0,
        };
        let closure = EnduranceRuntimeClosure {
            status: EndurancePhaseTerminalStatus::Completed,
            playback_workers_terminated: true,
            supervised_child_processes_remaining: 0,
            export,
        };

        assert!(!closure.proves_consuming_cleanup());
        let error = closure.incomplete_cleanup_error();
        assert!(matches!(
            error,
            EnduranceCampaignError::IncompletePhaseCleanup {
                export: retained,
                ..
            } if retained == export
        ));
    }

    fn campaign_fixture() -> (
        tempfile::TempDir,
        EnduranceCampaignRequest,
        EnduranceQualificationProfile,
        PathBuf,
    ) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let profile: EnduranceQualificationProfile = serde_json::from_slice(
            &std::fs::read(root.join("tests/validation/commercial-endurance-qualification.json"))
                .expect("read profile"),
        )
        .expect("parse profile");
        let temporary = tempfile::tempdir().expect("temporary campaign root");
        let evidence = temporary.path().join("evidence");
        std::fs::create_dir(&evidence).expect("create evidence directory");
        let authority = temporary.path().join("authority.json");
        std::fs::write(
            &authority,
            br#"{"schema_version":1,"authority_id":"external-commercial-endurance-authority-v1","run_id":"campaign-test-run","single_use_challenge":"campaign-test-challenge"}"#,
        )
        .expect("write authority");
        let workloads = profile
            .phases
            .iter()
            .map(|phase| {
                let file_name = match phase.kind {
                    EndurancePhaseKind::PlaybackReference => "playback-reference-v1.json",
                    EndurancePhaseKind::ContinuousExport => "continuous-export-v1.json",
                    EndurancePhaseKind::ConcurrentRecovery => "concurrent-recovery-v1.json",
                };
                (
                    phase.phase_id.clone(),
                    root.join("tests/validation/endurance-workloads").join(file_name),
                )
            })
            .collect();
        let request = EnduranceCampaignRequest {
            profile: profile.clone(),
            identity: EnduranceRunIdentity {
                run_id: "campaign-test-run".to_owned(),
                source_revision: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                release_candidate_id: "mondrian-test-rc".to_owned(),
                product_artifact_sha256: SHA.to_owned(),
                runtime_image_sha256: SHA.to_owned(),
                build_provenance_sha256: SHA.to_owned(),
                machine_report_sha256: SHA.to_owned(),
                platform_cell_sha256: SHA.to_owned(),
                environment_before_sha256: SHA.to_owned(),
                environment_after_sha256: SHA.to_owned(),
            },
            capture_authority_manifest_path: authority,
            evidence_directory: evidence.clone(),
            output_manifest_path: temporary.path().join("run.json"),
            workload_contracts: workloads,
        };
        (temporary, request, profile, evidence)
    }

    #[derive(Clone, Copy)]
    enum CampaignFailpoint {
        PreStart,
        Begin,
        Pump,
        Event,
        Snapshot,
        PhaseProvenance,
        Memory,
        IncompleteShutdown,
    }

    struct FailpointRuntime<'a> {
        clock: &'a FakeClock,
        failpoint: CampaignFailpoint,
        begin_calls: u32,
        shutdown_calls: u32,
        snapshot_calls: u32,
        snapshots_at_shutdown: Option<u32>,
    }

    impl EnduranceCampaignRuntime for FailpointRuntime<'_> {
        fn begin_phase(
            &mut self,
            _requirement: &EndurancePhaseRequirement,
            _workload: &PreparedEnduranceWorkload,
            _phase_started_at_run_us: u64,
        ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError> {
            self.begin_calls += 1;
            match self.failpoint {
                CampaignFailpoint::PreStart => {
                    return Err(EnduranceCampaignError::PreStartRuntime(
                        "pre-start".to_owned(),
                    ));
                }
                CampaignFailpoint::Begin => {
                    return Err(EnduranceCampaignError::Runtime("begin".to_owned()));
                }
                _ => {}
            }
            Ok(EndurancePhaseAdmission::Started)
        }

        fn pump_until(
            &mut self,
            deadline_run_us: u64,
        ) -> Result<Vec<EnduranceCampaignEvent>, EnduranceCampaignError> {
            self.clock.0.store(deadline_run_us, Ordering::Relaxed);
            match self.failpoint {
                CampaignFailpoint::Pump => Err(EnduranceCampaignError::Runtime("pump".to_owned())),
                CampaignFailpoint::Event => {
                    Ok(vec![EnduranceCampaignEvent::test_export_artifact_verified(
                        0,
                        "wrong-phase-artifact",
                        SHA,
                        "independent-validator-v1",
                        SHA,
                    )])
                }
                _ => Ok(Vec::new()),
            }
        }

        fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError> {
            self.snapshot_calls += 1;
            if matches!(self.failpoint, CampaignFailpoint::Snapshot) {
                return Err(EnduranceCampaignError::Runtime("snapshot".to_owned()));
            }
            let playback = PlaybackEvidenceCollector::new(PlaybackEvidenceConfig::default())
                .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?
                .report();
            if matches!(self.failpoint, CampaignFailpoint::PhaseProvenance) {
                return Ok(EnduranceRuntimeSnapshot::continuous_export(
                    playback,
                    ReferenceOutputDiagnostics::default(),
                    empty_export_snapshot(),
                ));
            }
            Ok(EnduranceRuntimeSnapshot {
                phase_kind: EndurancePhaseKind::PlaybackReference,
                playback,
                reference_output: ReferenceOutputDiagnostics::default(),
                export: empty_export_snapshot(),
                capture_facts: EnduranceCaptureFacts::default(),
            })
        }

        fn shutdown_phase(
            &mut self,
        ) -> Result<(EnduranceRuntimeClosure, Vec<EnduranceCampaignEvent>), EnduranceCampaignError>
        {
            self.shutdown_calls += 1;
            self.snapshots_at_shutdown = Some(self.snapshot_calls);
            Ok((
                EnduranceRuntimeClosure {
                    status: EndurancePhaseTerminalStatus::Failed,
                    playback_workers_terminated: !matches!(
                        self.failpoint,
                        CampaignFailpoint::IncompleteShutdown
                    ),
                    supervised_child_processes_remaining: 0,
                    export: ExportQueueShutdownEvidence {
                        schema_version: 4,
                        worker_started: true,
                        worker_terminated: true,
                        worker_start_failed: false,
                        worker_panicked: false,
                        worker_timed_out: false,
                        worker_detached: false,
                        worker_owner_abandoned: false,
                        pending_jobs: 0,
                        active_jobs: 0,
                        activity_events: 0,
                        audio_source_owners_started: 0,
                        audio_source_owners_closed: 0,
                        audio_source_owner_failures: 0,
                        active_audio_source_owners: 0,
                    },
                },
                Vec::new(),
            ))
        }
    }

    struct FailedMemory;

    impl ProcessMemoryProbe for FailedMemory {
        fn process_memory(&self, scope: ProcessMemoryScope) -> ProcessMemoryProbeResult {
            ProcessMemoryProbeResult::failed(
                scope,
                ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                0,
                1,
                "memory probe failed",
            )
        }
    }

    #[test]
    fn coordinator_consumes_begin_pump_event_snapshot_and_probe_failures_once() {
        for failpoint in [
            CampaignFailpoint::Begin,
            CampaignFailpoint::Pump,
            CampaignFailpoint::Event,
            CampaignFailpoint::Snapshot,
            CampaignFailpoint::PhaseProvenance,
            CampaignFailpoint::Memory,
            CampaignFailpoint::IncompleteShutdown,
        ] {
            let (_temporary, request, _profile, _evidence) = campaign_fixture();
            let clock = FakeClock::default();
            let mut runtime = FailpointRuntime {
                clock: &clock,
                failpoint,
                begin_calls: 0,
                shutdown_calls: 0,
                snapshot_calls: 0,
                snapshots_at_shutdown: None,
            };
            let result = if matches!(failpoint, CampaignFailpoint::Memory) {
                run_endurance_campaign(request, &mut runtime, &FailedMemory, &clock)
            } else {
                run_endurance_campaign(request, &mut runtime, &FakeMemory(&clock), &clock)
            };
            assert!(result.is_err());
            assert_eq!(runtime.shutdown_calls, 1);
            if matches!(failpoint, CampaignFailpoint::Memory) {
                assert_eq!(runtime.snapshot_calls, 1);
            }
            if matches!(failpoint, CampaignFailpoint::IncompleteShutdown) {
                assert_eq!(
                    runtime.begin_calls, 1,
                    "must not admit the next serial phase"
                );
                assert_eq!(
                    runtime.snapshots_at_shutdown,
                    Some(runtime.snapshot_calls),
                    "must not capture a final sample after incomplete shutdown"
                );
                assert!(matches!(
                    &result,
                    Err(EnduranceCampaignError::IncompletePhaseCleanup {
                        playback_workers_terminated: false,
                        ..
                    })
                ));
            }
            if matches!(failpoint, CampaignFailpoint::PhaseProvenance) {
                assert!(matches!(
                    &result,
                    Err(EnduranceCampaignError::SnapshotPhaseMismatch {
                        expected: EndurancePhaseKind::PlaybackReference,
                        actual: EndurancePhaseKind::ContinuousExport,
                    })
                ));
            }
        }
    }

    #[test]
    fn coordinator_does_not_cleanup_a_pre_start_failure_without_owners() {
        let (_temporary, request, _profile, _evidence) = campaign_fixture();
        let clock = FakeClock::default();
        let mut runtime = FailpointRuntime {
            clock: &clock,
            failpoint: CampaignFailpoint::PreStart,
            begin_calls: 0,
            shutdown_calls: 0,
            snapshot_calls: 0,
            snapshots_at_shutdown: None,
        };

        let result = run_endurance_campaign(request, &mut runtime, &FakeMemory(&clock), &clock);

        assert!(matches!(
            result,
            Err(EnduranceCampaignError::PreStartRuntime(ref detail)) if detail == "pre-start"
        ));
        assert_eq!(runtime.begin_calls, 1);
        assert_eq!(runtime.shutdown_calls, 0);
        assert_eq!(runtime.snapshot_calls, 0);
    }

    #[test]
    fn coordinator_runs_serial_cadence_and_seals_not_run_hardware_phases() {
        let (_temporary, request, profile, evidence) = campaign_fixture();
        let clock = FakeClock::default();
        let mut runtime = FakeRuntime::new(&clock);
        let manifest = run_endurance_campaign(request, &mut runtime, &FakeMemory(&clock), &clock)
            .expect("run bounded campaign coordinator");
        assert_eq!(manifest.phases.len(), 3);
        assert_eq!(
            manifest.phases[0].terminal.status,
            EndurancePhaseTerminalStatus::NotRun
        );
        assert_eq!(
            manifest.phases[1].terminal.status,
            EndurancePhaseTerminalStatus::Completed
        );
        assert_eq!(
            manifest.phases[1].terminal.counters.export_artifacts_verified,
            24
        );
        assert_eq!(
            manifest.phases[2].terminal.status,
            EndurancePhaseTerminalStatus::NotRun
        );
        let prepared = PreparedEnduranceQualification::compile(profile).expect("compile profile");
        let report = prepared
            .evaluate(manifest, |receipt| {
                let bytes = std::fs::read(evidence.join(&receipt.file_name))
                    .map_err(|_| mondrian_platform::EnduranceQualificationError::EmptyChunk)?;
                serde_json::from_slice(&bytes)
                    .map_err(mondrian_platform::EnduranceQualificationError::Serialization)
            })
            .expect("evaluate campaign manifest");
        assert_eq!(report.status, EnduranceQualificationStatus::Incomplete);
        assert_eq!(
            report.phases[1].status,
            EnduranceQualificationStatus::Qualified
        );
    }
}
