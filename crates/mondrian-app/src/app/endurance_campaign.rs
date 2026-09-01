//! Validation-only serial coordinator for commercial endurance workloads.
//!
//! The coordinator owns cadence, native process-tree sampling, phase order,
//! bounded evidence publication, and fail-closed terminal capture. Concrete
//! product runtimes own Playback, Reference Output, Export, recovery, and
//! synchronous worker shutdown; this module never reinterprets their facts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mondrian_export::{ExportEnduranceSnapshot, ExportQueueShutdownEvidence};
use mondrian_platform::{
    EndurancePhaseRequirement, EndurancePhaseTerminalStatus, EnduranceQualificationProfile,
    EnduranceRunManifest, ProcessMemoryProbe, ProcessMemoryScope,
};
use mondrian_playback::PlaybackEvidenceReport;
use mondrian_reference_output::ReferenceOutputDiagnostics;
use thiserror::Error;

use super::endurance_qualification::{
    EnduranceCaptureError, EnduranceCaptureFacts, EndurancePhaseCapture, EnduranceRecoveryStep,
    EnduranceRunCapture, EnduranceRunIdentity, EnduranceSampleTiming,
};
use super::headless_realtime_playback::HeadlessRealtimePlaybackSession;
use super::preview_runtime::PreviewRuntimeShutdownEvidence;
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

/// Synchronous closure across the headless Preview, Audio, and GPU owners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceExecutionOwnerClosure {
    /// Complete Preview worker inventory.
    pub preview: PreviewRuntimeShutdownEvidence,
    /// Realtime PCM render and device-lifecycle inventory.
    pub audio: mondrian_media::AudioPlaybackShutdownEvidence,
    /// Bounded GPU progress and generation-retirement evidence.
    pub gpu: EnduranceGpuShutdownEvidence,
}

impl EnduranceExecutionOwnerClosure {
    /// Whether every software execution owner returned without panic or detach.
    pub const fn all_workers_terminated(self) -> bool {
        self.preview.all_workers_terminated()
            && self.audio.all_workers_terminated()
            && self.gpu.all_resources_retired()
    }
}

/// Validation owner group using the production Headless Preview/GPU and Audio paths.
pub struct EnduranceExecutionOwners {
    realtime: Option<HeadlessRealtimePlaybackSession>,
}

impl EnduranceExecutionOwners {
    /// Start real software execution owners without admitting a campaign phase.
    pub fn start() -> Result<Self, EnduranceCampaignError> {
        let realtime = HeadlessRealtimePlaybackSession::new()
            .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?;
        Ok(Self { realtime: Some(realtime) })
    }

    /// Stop Preview and the App State's actual Audio owner, then retire GPU.
    pub fn shutdown_and_wait(
        mut self,
        app: &mut AppState,
        gpu_timeout: Duration,
    ) -> Result<EnduranceExecutionOwnerClosure, EnduranceCampaignError> {
        if app.is_playing() {
            let _ = app.pause();
        }
        let (preview_owner, gpu_owner) = self
            .realtime
            .take()
            .ok_or_else(|| {
                EnduranceCampaignError::Runtime(
                    "Headless realtime execution session is missing".to_owned(),
                )
            })?
            .into_shutdown_owners();
        let preview = preview_owner.shutdown_and_wait();
        let audio = app.shutdown_audio_playback_and_wait();
        let gpu = gpu_owner.shutdown_and_wait(gpu_timeout);
        Ok(EnduranceExecutionOwnerClosure {
            preview,
            audio,
            gpu: EnduranceGpuShutdownEvidence {
                worker_started: gpu.worker_started,
                worker_terminated: gpu.worker_terminated,
                worker_panicked: gpu.worker_panicked,
                timed_out: gpu.timed_out,
                retirement_handoff_accepted: gpu.retirement_handoff_accepted,
                retirement_completed: gpu.retirement_completed,
            },
        })
    }
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
    RecoveryStepCompleted {
        /// Phase-local completion instant.
        completed_at_us: u64,
        /// Zero-based recovery cycle.
        cycle_index: u32,
        /// Exact ordered recovery step.
        step: EnduranceRecoveryStep,
        /// SHA-256 of the production owner's before/after operation receipt.
        operation_receipt_sha256: String,
    },
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

    #[cfg(test)]
    fn test_export_artifact_verified(
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
}

/// Result of attempting to admit one exact phase workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndurancePhaseAdmission {
    /// Required fixtures and product owners started successfully.
    Started,
    /// A required external prerequisite was absent; no product work started.
    NotRun,
}

/// Atomic projection of product-owned runtime diagnostics at one cadence.
#[derive(Debug, Clone)]
pub struct EnduranceRuntimeSnapshot {
    /// Playback-owned evidence.
    pub playback: PlaybackEvidenceReport,
    /// Reference Output-owned diagnostics.
    pub reference_output: ReferenceOutputDiagnostics,
    /// Export-owned bounded diagnostics. The coordinator stamps publication time.
    pub export: ExportEnduranceSnapshot,
    /// Remaining App/runtime facts derived by the concrete product coordinator.
    pub capture_facts: EnduranceCaptureFacts,
}

/// Typed synchronous terminal closure returned before the final sample.
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

/// Product-owned execution surface consumed by the serial supervisor.
pub trait EnduranceCampaignRuntime {
    /// Admit and start one exact checked-in workload.
    fn begin_phase(
        &mut self,
        requirement: &EndurancePhaseRequirement,
        workload_contract_path: &Path,
    ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError>;

    /// Pump real product work until the absolute campaign deadline is reached.
    fn pump_until(
        &mut self,
        deadline_run_us: u64,
    ) -> Result<Vec<EnduranceCampaignEvent>, EnduranceCampaignError>;

    /// Atomically snapshot all product owners after the native memory probe.
    fn snapshot(&mut self) -> Result<EnduranceRuntimeSnapshot, EnduranceCampaignError>;

    /// Stop work and synchronously return all phase-owned workers/resources.
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
        let started_at_run_us = clock.elapsed_us();
        let phase = capture.begin_phase(
            &requirement.phase_id,
            started_at_run_us,
            &request.evidence_directory,
            workload_path,
        )?;
        match runtime.begin_phase(requirement, workload_path)? {
            EndurancePhaseAdmission::NotRun => {
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
    let mut sequence = 0_u64;
    let mut scheduled_at_us = 0_u64;
    while scheduled_at_us < requirement.minimum_duration_us {
        pump_and_record(runtime, &mut phase, started_at_run_us, scheduled_at_us)?;
        capture_one_sample(
            runtime,
            &mut phase,
            process_memory,
            clock,
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
    let (closure, events) = runtime.shutdown_phase()?;
    if closure.status == EndurancePhaseTerminalStatus::NotRun {
        return Err(EnduranceCampaignError::InvalidTerminalStatus);
    }
    record_events(&mut phase, events)?;
    let final_snapshot = capture_one_sample(
        runtime,
        &mut phase,
        process_memory,
        clock,
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
            EnduranceCampaignEvent::RecoveryStepCompleted {
                completed_at_us,
                cycle_index,
                step,
                operation_receipt_sha256,
            } => phase.record_recovery_step_completed(
                completed_at_us,
                cycle_index,
                step,
                &operation_receipt_sha256,
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
    let completed_at_us = clock
        .elapsed_us()
        .checked_sub(started_at_run_us)
        .ok_or(EnduranceCampaignError::TimeRegression)?;
    snapshot.export.observed_at_us = completed_at_us;
    snapshot.capture_facts.observed_at_us = completed_at_us;
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
    /// A started runtime attempted to terminate as NotRun.
    #[error("a started endurance phase cannot terminate as not-run")]
    InvalidTerminalStatus,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use mondrian_platform::{
        EnduranceQualificationStatus, PreparedEnduranceQualification, ProcessMemoryProbeBackend,
        ProcessMemoryProbeResult,
    };
    use mondrian_playback::{PlaybackEvidenceCollector, PlaybackEvidenceConfig};

    use super::*;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

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
            .shutdown_and_wait(&mut state, std::time::Duration::from_secs(30))
            .expect("close active execution owners");

        assert!(closure.preview.all_workers_terminated());
        assert!(closure.audio.all_workers_terminated());
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
            _workload_contract_path: &Path,
        ) -> Result<EndurancePhaseAdmission, EnduranceCampaignError> {
            self.kind = Some(requirement.kind);
            self.phase_started_at_us = self.clock.elapsed_us();
            self.verified_exports = 0;
            self.shutdown = false;
            Ok(
                if requirement.kind == mondrian_platform::EndurancePhaseKind::ContinuousExport {
                    EndurancePhaseAdmission::Started
                } else {
                    EndurancePhaseAdmission::NotRun
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
                playback: PlaybackEvidenceCollector::new(PlaybackEvidenceConfig::default())
                    .map_err(|error| EnduranceCampaignError::Runtime(error.to_string()))?
                    .report(),
                reference_output: ReferenceOutputDiagnostics::default(),
                export: ExportEnduranceSnapshot {
                    schema_version: 1,
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
                        schema_version: 1,
                        worker_terminated: true,
                        pending_jobs: 0,
                        active_jobs: 0,
                        activity_events: self.phase_elapsed_us() / 60_000_000,
                    },
                },
                Vec::new(),
            ))
        }
    }

    #[test]
    fn coordinator_runs_serial_cadence_and_seals_not_run_hardware_phases() {
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
                    mondrian_platform::EndurancePhaseKind::PlaybackReference => {
                        "playback-reference-v1.json"
                    }
                    mondrian_platform::EndurancePhaseKind::ContinuousExport => {
                        "continuous-export-v1.json"
                    }
                    mondrian_platform::EndurancePhaseKind::ConcurrentRecovery => {
                        "concurrent-recovery-v1.json"
                    }
                };
                (
                    phase.phase_id.clone(),
                    root.join("tests/validation/endurance-workloads").join(file_name),
                )
            })
            .collect();
        let manifest_path = temporary.path().join("run.json");
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
            output_manifest_path: manifest_path,
            workload_contracts: workloads,
        };
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
