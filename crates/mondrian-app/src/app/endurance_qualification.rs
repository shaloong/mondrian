//! Validation Adapter from product-owned diagnostics to endurance samples.
//!
//! This module does not interpret qualification thresholds. It snapshots the
//! existing Playback, Reference Output, Export, and platform process-tree
//! authorities and writes fixed-capacity chunks for the platform-core replay
//! Module.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use mondrian_export::{ExportEnduranceSnapshot, ExportQueueShutdownEvidence};
use mondrian_platform::{
    EnduranceCounters, EnduranceGauges, EndurancePhaseChunkReceipt, EndurancePhaseKind,
    EndurancePhaseManifest, EndurancePhaseProducerEvidence, EndurancePhaseRequirement,
    EndurancePhaseTerminalEvidence, EndurancePhaseTerminalStatus, EnduranceProcessMemorySample,
    EnduranceQualificationProfile, EnduranceRunManifest, EnduranceSample, EnduranceSampleChunk,
    PreparedEnduranceQualification, ProcessMemoryProbeResult, ProcessMemoryScope,
};
use mondrian_playback::PlaybackEvidenceReport;
use mondrian_reference_output::{ReferenceOutputDiagnostics, ReferenceOutputState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::endurance_recovery::EnduranceRecoveryOperationReceipt;
use super::headless_realtime_playback::HeadlessEnduranceOwnerSnapshot;

/// Additional gauges and independently verified facts owned by the Headless
/// qualification driver rather than any single execution domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceCaptureFacts {
    /// Capture-fact schema version.
    schema_version: u32,
    /// Monotonic completion stamp for the containing capture envelope.
    observed_at_us: u64,
    /// Live Playback-current work bindings.
    playback_pending: u64,
    /// Additional bounded queue depth outside Reference Output and Export.
    other_queue_depth: u64,
    /// Selected Headless Preview/Audio/GPU leases and resources still owned.
    owned_resource_units: u64,
    /// Failed controlled recovery cycles.
    recovery_failures: u64,
    /// Renderer device-loss/reset terminals.
    gpu_device_losses: u64,
    /// Other crash, panic, or dead-worker terminals.
    fatal_errors: u64,
}

#[cfg(test)]
impl Default for EnduranceCaptureFacts {
    fn default() -> Self {
        Self {
            schema_version: 1,
            observed_at_us: 0,
            playback_pending: 0,
            other_queue_depth: 0,
            owned_resource_units: 0,
            recovery_failures: 0,
            gpu_device_losses: 0,
            fatal_errors: 0,
        }
    }
}

impl EnduranceCaptureFacts {
    /// Seal one settled Headless owner inventory into the capture vocabulary.
    pub(crate) const fn from_headless_owner_snapshot(
        snapshot: HeadlessEnduranceOwnerSnapshot,
    ) -> Self {
        Self {
            schema_version: 1,
            observed_at_us: 0,
            playback_pending: snapshot.playback_pending(),
            other_queue_depth: snapshot.other_queue_depth(),
            owned_resource_units: snapshot.owned_resource_units(),
            recovery_failures: 0,
            gpu_device_losses: snapshot.gpu_device_losses(),
            fatal_errors: snapshot.fatal_errors(),
        }
    }

    /// Construct the zero-realtime-owner facts required by an Export-only phase.
    pub(crate) const fn for_continuous_export() -> Self {
        Self {
            schema_version: 1,
            observed_at_us: 0,
            playback_pending: 0,
            other_queue_depth: 0,
            owned_resource_units: 0,
            recovery_failures: 0,
            gpu_device_losses: 0,
            fatal_errors: 0,
        }
    }

    /// Assign the supervisor's capture-envelope completion stamp.
    pub(crate) fn stamp_observed_at_us(&mut self, observed_at_us: u64) {
        self.observed_at_us = observed_at_us;
    }
}

/// One required operation inside a controlled recovery cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnduranceRecoveryStep {
    /// Seek through the production authoring/playback path.
    Seek,
    /// Close and reopen the production surface/device path.
    SurfaceDeviceReopen,
    /// Cancel one admitted Export and complete its retry.
    ExportCancelRetry,
    /// Apply and recover from the bounded cache-pressure workload.
    CachePressure,
}

const RECOVERY_STEPS: [EnduranceRecoveryStep; 4] = [
    EnduranceRecoveryStep::Seek,
    EnduranceRecoveryStep::SurfaceDeviceReopen,
    EnduranceRecoveryStep::ExportCancelRetry,
    EnduranceRecoveryStep::CachePressure,
];
const RECOVERY_STEP_COUNT: u64 = 4;

/// One typed, bounded producer event sealed outside the sample chunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EnduranceProducerEvent {
    /// Independent re-open/content verification of one published Export artifact.
    ExportArtifactVerified {
        sequence: u32,
        completed_at_us: u64,
        artifact_id: String,
        artifact_sha256: String,
        validator_id: String,
        validation_report_sha256: String,
    },
    /// One owner-receipted operation in an exact recovery-cycle sequence.
    RecoveryStepCompleted {
        sequence: u32,
        completed_at_us: u64,
        cycle_index: u32,
        step: EnduranceRecoveryStep,
        operation_receipt_json: String,
        operation_receipt_sha256: String,
    },
}

/// Raw, authority-bound semantic evidence generated by the App supervisor.
#[derive(Debug, Serialize)]
struct EnduranceProducerRawEvidence<'a> {
    schema_version: u32,
    phase_id: &'a str,
    run_id: &'a str,
    authority_challenge: &'a str,
    workload_sha256: &'a str,
    producer_owner: &'a str,
    producer_verifier_id: &'a str,
    events: &'a [EnduranceProducerEvent],
}

/// Normalized owner report generated from the typed raw event inventory.
#[derive(Debug, Serialize)]
struct EnduranceProducerReport<'a> {
    schema_version: u32,
    phase_id: &'a str,
    run_id: &'a str,
    authority_challenge: &'a str,
    workload_sha256: &'a str,
    producer_owner: &'a str,
    producer_verifier_id: &'a str,
    raw_evidence_sha256: &'a str,
    terminal_status: EndurancePhaseTerminalStatus,
    event_count: u32,
    verified_export_artifacts: u64,
    recovery_cycles: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct EnduranceSemanticCounters {
    verified_export_artifacts: u64,
    recovery_cycles: u64,
}

#[derive(Debug)]
struct EnduranceSemanticRecorder {
    phase_kind: EndurancePhaseKind,
    maximum_events: usize,
    events: Vec<EnduranceProducerEvent>,
    verified_export_artifacts: u64,
    recovery_steps: u64,
    recovery_operation_ids: BTreeSet<String>,
    last_completed_at_us: Option<u64>,
}

impl EnduranceSemanticRecorder {
    fn new(phase_kind: EndurancePhaseKind, maximum_events: u16) -> Self {
        Self {
            phase_kind,
            maximum_events: usize::from(maximum_events),
            events: Vec::with_capacity(usize::from(maximum_events)),
            verified_export_artifacts: 0,
            recovery_steps: 0,
            recovery_operation_ids: BTreeSet::new(),
            last_completed_at_us: None,
        }
    }

    fn counters(&self) -> EnduranceSemanticCounters {
        EnduranceSemanticCounters {
            verified_export_artifacts: self.verified_export_artifacts,
            recovery_cycles: self.recovery_steps / RECOVERY_STEP_COUNT,
        }
    }

    fn record_export_artifact_verified(
        &mut self,
        completed_at_us: u64,
        artifact_id: &str,
        artifact_sha256: &str,
        validator_id: &str,
        validation_report_sha256: &str,
    ) -> Result<(), EnduranceCaptureError> {
        if self.phase_kind == EndurancePhaseKind::PlaybackReference
            || !valid_evidence_token(artifact_id)
            || !valid_evidence_token(validator_id)
            || !valid_sha256(artifact_sha256)
            || !valid_sha256(validation_report_sha256)
        {
            return Err(EnduranceCaptureError::InvalidProducerEvent);
        }
        if self.events.iter().any(|event| {
            matches!(
                event,
                EnduranceProducerEvent::ExportArtifactVerified {
                    artifact_id: recorded_artifact_id,
                    ..
                } if recorded_artifact_id == artifact_id
            )
        }) {
            return Err(EnduranceCaptureError::InvalidProducerEvent);
        }
        self.prepare_event(completed_at_us)?;
        let sequence = u32::try_from(self.events.len())
            .map_err(|_| EnduranceCaptureError::ProducerEventLimitExceeded)?;
        self.events.push(EnduranceProducerEvent::ExportArtifactVerified {
            sequence,
            completed_at_us,
            artifact_id: artifact_id.to_owned(),
            artifact_sha256: artifact_sha256.to_owned(),
            validator_id: validator_id.to_owned(),
            validation_report_sha256: validation_report_sha256.to_owned(),
        });
        self.verified_export_artifacts = self
            .verified_export_artifacts
            .checked_add(1)
            .ok_or(EnduranceCaptureError::CounterOverflow)?;
        Ok(())
    }

    fn record_recovery_step_completed(
        &mut self,
        completed_at_us: u64,
        cycle_index: u32,
        step: EnduranceRecoveryStep,
        operation_receipt_json: &str,
        operation_receipt_sha256: &str,
    ) -> Result<(), EnduranceCaptureError> {
        let receipt = EnduranceRecoveryOperationReceipt::parse_and_validate(
            operation_receipt_json,
            operation_receipt_sha256,
        )
        .map_err(|_| EnduranceCaptureError::InvalidProducerEvent)?;
        let expected_cycle = self.recovery_steps / RECOVERY_STEP_COUNT;
        let expected_step =
            RECOVERY_STEPS[usize::try_from(self.recovery_steps % RECOVERY_STEP_COUNT)
                .map_err(|_| EnduranceCaptureError::InvalidProducerEvent)?];
        if self.phase_kind != EndurancePhaseKind::ConcurrentRecovery
            || u64::from(cycle_index) != expected_cycle
            || step != expected_step
            || receipt.cycle_index() != cycle_index
            || receipt.step() != step
            || self.recovery_operation_ids.contains(receipt.operation_id())
        {
            return Err(EnduranceCaptureError::InvalidProducerEvent);
        }
        self.prepare_event(completed_at_us)?;
        let sequence = u32::try_from(self.events.len())
            .map_err(|_| EnduranceCaptureError::ProducerEventLimitExceeded)?;
        self.events.push(EnduranceProducerEvent::RecoveryStepCompleted {
            sequence,
            completed_at_us,
            cycle_index,
            step,
            operation_receipt_json: operation_receipt_json.to_owned(),
            operation_receipt_sha256: operation_receipt_sha256.to_owned(),
        });
        self.recovery_operation_ids.insert(receipt.operation_id().to_owned());
        self.recovery_steps = self
            .recovery_steps
            .checked_add(1)
            .ok_or(EnduranceCaptureError::CounterOverflow)?;
        Ok(())
    }

    fn prepare_event(&mut self, completed_at_us: u64) -> Result<(), EnduranceCaptureError> {
        if self.events.len() == self.maximum_events {
            return Err(EnduranceCaptureError::ProducerEventLimitExceeded);
        }
        if self.last_completed_at_us.is_some_and(|previous| completed_at_us < previous) {
            return Err(EnduranceCaptureError::InvalidProducerEvent);
        }
        self.last_completed_at_us = Some(completed_at_us);
        Ok(())
    }

    fn validate_terminal(
        &self,
        last: &EnduranceSample,
        status: EndurancePhaseTerminalStatus,
    ) -> Result<(), EnduranceCaptureError> {
        let counters = self.counters();
        let has_partial_recovery = !self.recovery_steps.is_multiple_of(RECOVERY_STEP_COUNT);
        if status == EndurancePhaseTerminalStatus::NotRun
            || has_partial_recovery
            || counters.verified_export_artifacts != last.counters.export_artifacts_verified
            || counters.recovery_cycles != last.counters.recovery_cycles
            || (self.phase_kind == EndurancePhaseKind::ConcurrentRecovery
                && counters.recovery_cycles != last.counters.export_cancellations)
            || self
                .last_completed_at_us
                .is_some_and(|completed| completed > last.completed_at_us)
        {
            return Err(EnduranceCaptureError::InvalidProducerEvidence);
        }
        Ok(())
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_evidence_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Monotonic timing assigned by the capture scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnduranceSampleTiming {
    /// Contiguous phase-local sequence.
    pub sequence: u64,
    /// Intended sample instant relative to phase start.
    pub scheduled_at_us: u64,
    /// Native probe start relative to phase start.
    pub started_at_us: u64,
    /// Complete snapshot time relative to phase start.
    pub completed_at_us: u64,
}

/// Build one platform-neutral sample without duplicating domain policy.
fn capture_endurance_sample(
    timing: EnduranceSampleTiming,
    process_memory: &ProcessMemoryProbeResult,
    playback: &PlaybackEvidenceReport,
    reference: &ReferenceOutputDiagnostics,
    export: ExportEnduranceSnapshot,
    facts: EnduranceCaptureFacts,
    semantic: EnduranceSemanticCounters,
) -> Result<EnduranceSample, EnduranceCaptureError> {
    if playback.schema_version != mondrian_playback::PLAYBACK_EVIDENCE_SCHEMA_VERSION
        || reference.schema_version != 1
        || export.schema_version != 2
        || export.observed_at_us != timing.completed_at_us
        || facts.schema_version != 1
        || facts.observed_at_us != timing.completed_at_us
    {
        return Err(EnduranceCaptureError::OwnerSnapshotMismatch);
    }
    let process_memory = complete_process_memory(process_memory)?;
    let reference_hardware_maximum_gap_us = reference_hardware_gap_us(reference)?;
    let playback_failed_frames = playback
        .deliveries
        .blocked
        .checked_add(playback.deliveries.failed)
        .and_then(|value| value.checked_add(playback.deliveries.rejected))
        .ok_or(EnduranceCaptureError::CounterOverflow)?;
    let reference_terminal_failure = u64::from(matches!(
        reference.state,
        ReferenceOutputState::Failed | ReferenceOutputState::Blocked
    ));
    let fatal_errors = facts
        .fatal_errors
        .checked_add(u64::from(export.worker_failed))
        .and_then(|value| value.checked_add(export.audio_source_owner_failures))
        .and_then(|value| value.checked_add(reference_terminal_failure))
        .ok_or(EnduranceCaptureError::CounterOverflow)?;
    let queue_depth = facts
        .other_queue_depth
        .checked_add(reference.outstanding_frames)
        .and_then(|value| value.checked_add(export.pending_jobs))
        .and_then(|value| value.checked_add(export.active_jobs))
        .and_then(|value| value.checked_add(export.active_audio_source_owners))
        .ok_or(EnduranceCaptureError::CounterOverflow)?;
    Ok(EnduranceSample {
        sequence: timing.sequence,
        scheduled_at_us: timing.scheduled_at_us,
        started_at_us: timing.started_at_us,
        completed_at_us: timing.completed_at_us,
        process_memory,
        reference_output_hardware_backed: reference
            .provider
            .as_ref()
            .is_some_and(|provider| provider.hardware_backed),
        external_reference_locked: reference.reference_locked == Some(true),
        reference_hardware_maximum_gap_us,
        export_shutdown_requested: export.shutdown_requested,
        export_worker_running: export.worker_running,
        export_worker_terminated: export.worker_terminated,
        counters: EnduranceCounters {
            playback_presented_frames: playback.deliveries.ready,
            playback_late_frames: playback.deliveries.late,
            playback_failed_frames,
            audio_underruns: playback.audio_underrun_frames,
            reference_scheduled_frames: reference.scheduled_frames,
            reference_completed_frames: reference.completed_frames,
            reference_late_frames: reference.late_frames,
            reference_dropped_frames: reference.dropped_frames,
            reference_flushed_frames: reference.flushed_frames,
            reference_aborted_frames: reference.aborted_frames,
            reference_lock_losses: reference.reference_lock_losses,
            reference_hardware_timestamps: reference.hardware_timestamp_callbacks,
            reference_hardware_time_failures: reference.hardware_time_failures,
            export_admissions: export.admissions,
            export_rejections: export.rejections,
            export_completions: export.completions,
            export_failures: export.failures,
            export_cancellations: export.cancellations,
            export_frames: export.rendered_frames,
            export_durable_artifacts: export.durable_artifacts,
            export_activity_events: export.activity_events,
            export_artifacts_verified: semantic.verified_export_artifacts,
            recovery_cycles: semantic.recovery_cycles,
            recovery_failures: facts.recovery_failures,
            gpu_device_losses: facts.gpu_device_losses,
            fatal_errors,
        },
        gauges: EnduranceGauges {
            playback_pending: facts.playback_pending,
            reference_outstanding_frames: reference.outstanding_frames,
            export_pending_jobs: export.pending_jobs,
            export_active_jobs: export.active_jobs,
            queue_depth,
            owned_resource_units: facts.owned_resource_units,
        },
    })
}

fn reference_hardware_gap_us(
    reference: &ReferenceOutputDiagnostics,
) -> Result<u64, EnduranceCaptureError> {
    if reference.maximum_hardware_time_gap_ticks == 0 {
        return Ok(0);
    }
    let rate = reference
        .last_hardware_time
        .map(|time| time.ticks_per_second)
        .filter(|rate| *rate != 0)
        .ok_or(EnduranceCaptureError::InvalidHardwareTime)?;
    let scaled = u128::from(reference.maximum_hardware_time_gap_ticks)
        .checked_mul(1_000_000)
        .ok_or(EnduranceCaptureError::CounterOverflow)?;
    let rounded_up = scaled
        .checked_add(u128::from(rate) - 1)
        .ok_or(EnduranceCaptureError::CounterOverflow)?
        / u128::from(rate);
    u64::try_from(rounded_up).map_err(|_| EnduranceCaptureError::CounterOverflow)
}

fn complete_process_memory(
    sample: &ProcessMemoryProbeResult,
) -> Result<EnduranceProcessMemorySample, EnduranceCaptureError> {
    if sample.scope != ProcessMemoryScope::ProductProcessTree
        || !sample.discovery_available
        || !sample.inventory_complete
        || sample.observed_process_count == 0
        || sample.inventory_attempts == 0
        || sample.error.is_some()
    {
        return Err(EnduranceCaptureError::IncompleteProcessTreeMemory);
    }
    Ok(EnduranceProcessMemorySample {
        scope: sample.scope,
        backend: sample.backend.ok_or(EnduranceCaptureError::IncompleteProcessTreeMemory)?,
        metric: sample
            .private_memory_metric
            .ok_or(EnduranceCaptureError::IncompleteProcessTreeMemory)?,
        observed_process_count: sample.observed_process_count,
        inventory_attempts: sample.inventory_attempts,
        inventory_complete: sample.inventory_complete,
        private_memory_bytes: sample
            .private_memory_bytes
            .ok_or(EnduranceCaptureError::IncompleteProcessTreeMemory)?,
        resident_bytes: sample
            .resident_bytes
            .ok_or(EnduranceCaptureError::IncompleteProcessTreeMemory)?,
    })
}

/// Fixed-capacity phase-local chunk builder.
pub struct EnduranceChunkRecorder {
    phase_id: String,
    maximum_samples: usize,
    maximum_chunks: u16,
    next_chunk_index: u32,
    previous_chunk_sha256: Option<String>,
    next_sequence: u64,
    samples: Vec<EnduranceSample>,
}

impl EnduranceChunkRecorder {
    /// Construct a recorder using the compiled profile's per-chunk bound.
    pub fn new(
        phase_id: impl Into<String>,
        maximum_samples: usize,
        maximum_chunks: u16,
    ) -> Result<Self, EnduranceCaptureError> {
        let phase_id = phase_id.into();
        if phase_id.trim().is_empty()
            || maximum_samples == 0
            || maximum_samples > 256
            || maximum_chunks == 0
        {
            return Err(EnduranceCaptureError::InvalidChunkRecorder);
        }
        Ok(Self {
            phase_id,
            maximum_samples,
            maximum_chunks,
            next_chunk_index: 0,
            previous_chunk_sha256: None,
            next_sequence: 0,
            samples: Vec::with_capacity(maximum_samples),
        })
    }

    /// Append one exact sequence without exceeding fixed capacity.
    pub fn push(&mut self, sample: EnduranceSample) -> Result<(), EnduranceCaptureError> {
        if sample.sequence != self.next_sequence {
            return Err(EnduranceCaptureError::SampleSequence);
        }
        if self.samples.len() == self.maximum_samples {
            return Err(EnduranceCaptureError::ChunkFull);
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(EnduranceCaptureError::CounterOverflow)?;
        self.samples.push(sample);
        Ok(())
    }

    /// Whether the current create-only chunk reached its fixed capacity.
    pub fn is_full(&self) -> bool {
        self.samples.len() == self.maximum_samples
    }

    /// Seal and clear the current non-empty chunk while retaining only its hash.
    pub fn seal_current(&mut self) -> Result<EnduranceSampleChunk, EnduranceCaptureError> {
        if self.samples.is_empty() {
            return Err(EnduranceCaptureError::EmptyChunk);
        }
        if self.next_chunk_index >= u32::from(self.maximum_chunks) {
            return Err(EnduranceCaptureError::ChunkLimitExceeded);
        }
        let chunk = EnduranceSampleChunk {
            schema_version: 1,
            phase_id: self.phase_id.clone(),
            chunk_index: self.next_chunk_index,
            previous_chunk_sha256: self.previous_chunk_sha256.clone(),
            samples: std::mem::replace(&mut self.samples, Vec::with_capacity(self.maximum_samples)),
            chunk_sha256: String::new(),
        }
        .seal()
        .map_err(|error| EnduranceCaptureError::Seal(error.to_string()))?;
        self.next_chunk_index = self
            .next_chunk_index
            .checked_add(1)
            .ok_or(EnduranceCaptureError::CounterOverflow)?;
        self.previous_chunk_sha256 = Some(chunk.chunk_sha256.clone());
        Ok(chunk)
    }
}

/// Exact release and machine identity frozen before an endurance run starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnduranceRunIdentity {
    /// Unique campaign run identity.
    pub run_id: String,
    /// Clean lowercase Git source revision.
    pub source_revision: String,
    /// Exact release-candidate identity.
    pub release_candidate_id: String,
    /// Qualified package/artifact SHA-256.
    pub product_artifact_sha256: String,
    /// Actually executed product image SHA-256.
    pub runtime_image_sha256: String,
    /// Exact build provenance SHA-256.
    pub build_provenance_sha256: String,
    /// Machine inventory report SHA-256.
    pub machine_report_sha256: String,
    /// Admitted COL-046 platform/display row SHA-256.
    pub platform_cell_sha256: String,
    /// Environment identity captured before phase one.
    pub environment_before_sha256: String,
    /// Environment identity captured after phase three.
    pub environment_after_sha256: String,
}

#[derive(Debug, Deserialize)]
struct EnduranceCaptureAuthorityHeader {
    schema_version: u32,
    authority_id: String,
    run_id: String,
    single_use_challenge: String,
}

/// Create-only, fixed-space recorder for one real product phase.
pub struct EndurancePhaseCapture {
    requirement: EndurancePhaseRequirement,
    run_id: String,
    authority_challenge: String,
    started_at_run_us: u64,
    evidence_directory: PathBuf,
    chunks: EnduranceChunkRecorder,
    receipts: Vec<EndurancePhaseChunkReceipt>,
    last_sample: Option<EnduranceSample>,
    all_reference_samples_hardware_backed: bool,
    semantic: EnduranceSemanticRecorder,
}

impl EndurancePhaseCapture {
    fn new(
        requirement: EndurancePhaseRequirement,
        run_id: &str,
        authority_challenge: &str,
        started_at_run_us: u64,
        evidence_directory: &Path,
        workload_contract_path: &Path,
        maximum_samples_per_chunk: usize,
        maximum_chunks_per_phase: u16,
        maximum_producer_events_per_phase: u16,
    ) -> Result<Self, EnduranceCaptureError> {
        let evidence_directory = existing_real_directory(evidence_directory)?;
        let workload_contract = existing_regular_file(workload_contract_path)?;
        if file_sha256(&workload_contract)? != requirement.workload_sha256 {
            return Err(EnduranceCaptureError::WorkloadContractMismatch);
        }
        Ok(Self {
            chunks: EnduranceChunkRecorder::new(
                requirement.phase_id.clone(),
                maximum_samples_per_chunk,
                maximum_chunks_per_phase,
            )?,
            semantic: EnduranceSemanticRecorder::new(
                requirement.kind,
                maximum_producer_events_per_phase,
            ),
            requirement,
            run_id: run_id.to_owned(),
            authority_challenge: authority_challenge.to_owned(),
            started_at_run_us,
            evidence_directory,
            receipts: Vec::new(),
            last_sample: None,
            all_reference_samples_hardware_backed: true,
        })
    }

    /// Record one independently reopened and content-verified Export artifact.
    pub(crate) fn record_export_artifact_verified(
        &mut self,
        completed_at_us: u64,
        artifact_id: &str,
        artifact_sha256: &str,
        validator_id: &str,
        validation_report_sha256: &str,
    ) -> Result<(), EnduranceCaptureError> {
        self.semantic.record_export_artifact_verified(
            completed_at_us,
            artifact_id,
            artifact_sha256,
            validator_id,
            validation_report_sha256,
        )
    }

    /// Record one exact operation in the ordered controlled-recovery protocol.
    pub(crate) fn record_recovery_step_completed(
        &mut self,
        completed_at_us: u64,
        cycle_index: u32,
        step: EnduranceRecoveryStep,
        operation_receipt_json: &str,
        operation_receipt_sha256: &str,
    ) -> Result<(), EnduranceCaptureError> {
        self.semantic.record_recovery_step_completed(
            completed_at_us,
            cycle_index,
            step,
            operation_receipt_json,
            operation_receipt_sha256,
        )
    }

    /// Snapshot all product owners and append one bounded phase observation.
    #[allow(clippy::too_many_arguments)]
    pub fn capture_and_push(
        &mut self,
        timing: EnduranceSampleTiming,
        process_memory: &ProcessMemoryProbeResult,
        playback: &PlaybackEvidenceReport,
        reference: &ReferenceOutputDiagnostics,
        export: ExportEnduranceSnapshot,
        facts: EnduranceCaptureFacts,
    ) -> Result<(), EnduranceCaptureError> {
        let sample = capture_endurance_sample(
            timing,
            process_memory,
            playback,
            reference,
            export,
            facts,
            self.semantic.counters(),
        )?;
        self.push_sample(sample)
    }

    fn push_sample(&mut self, sample: EnduranceSample) -> Result<(), EnduranceCaptureError> {
        if self.chunks.is_full() {
            self.publish_current_chunk()?;
        }
        self.all_reference_samples_hardware_backed &= sample.reference_output_hardware_backed;
        self.last_sample = Some(sample.clone());
        self.chunks.push(sample)
    }

    /// Finish an executed phase from typed owner shutdown evidence.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        mut self,
        completed_at_run_us: u64,
        status: EndurancePhaseTerminalStatus,
        playback_workers_terminated: bool,
        supervised_child_processes_remaining: u32,
        reference: &ReferenceOutputDiagnostics,
        export: ExportQueueShutdownEvidence,
    ) -> Result<EndurancePhaseManifest, EnduranceCaptureError> {
        if status == EndurancePhaseTerminalStatus::NotRun {
            return Err(EnduranceCaptureError::InvalidTerminal);
        }
        let last = self.last_sample.as_ref().ok_or(EnduranceCaptureError::EmptyChunk)?;
        let duration = completed_at_run_us
            .checked_sub(self.started_at_run_us)
            .ok_or(EnduranceCaptureError::InvalidTerminal)?;
        if duration != last.completed_at_us
            || reference.schema_version != 1
            || reference.outstanding_frames != 0
            || !matches!(
                reference.state,
                ReferenceOutputState::Disabled | ReferenceOutputState::Stopped
            )
            || export.pending_jobs != 0
            || export.active_jobs != 0
            || export.schema_version != 4
            || !export.all_resources_released()
            || !last.export_shutdown_requested
            || last.export_worker_running
            || !last.export_worker_terminated
        {
            return Err(EnduranceCaptureError::InvalidTerminal);
        }
        self.semantic.validate_terminal(last, status)?;
        let terminal_counters = last.counters;
        let terminal_gauges = last.gauges;
        if !self.chunks.samples.is_empty() {
            self.publish_current_chunk()?;
        }
        let (report_file_name, report_sha256, raw_evidence_file_name, raw_evidence_sha256) =
            self.publish_producer_evidence(status)?;
        Ok(EndurancePhaseManifest {
            phase_id: self.requirement.phase_id.clone(),
            workload_sha256: self.requirement.workload_sha256.clone(),
            started_at_run_us: self.started_at_run_us,
            completed_at_run_us,
            producer: EndurancePhaseProducerEvidence {
                owner: self.requirement.producer_owner.clone(),
                verifier_id: self.requirement.producer_verifier_id.clone(),
                report_schema_version: self.requirement.producer_report_schema_version,
                report_sha256,
                report_file_name,
                raw_evidence_sha256,
                raw_evidence_file_name,
                reference_output_hardware_backed: self.all_reference_samples_hardware_backed,
                external_reference_required: self
                    .requirement
                    .counters
                    .require_external_reference_lock,
            },
            chunks: self.receipts,
            terminal: EndurancePhaseTerminalEvidence {
                status,
                counters: terminal_counters,
                gauges: terminal_gauges,
                workers_terminated: playback_workers_terminated && export.all_resources_released(),
                child_processes_reaped: supervised_child_processes_remaining == 0,
            },
        })
    }

    /// Record a required phase that could not start because an external prerequisite was absent.
    pub fn finish_not_run(self) -> Result<EndurancePhaseManifest, EnduranceCaptureError> {
        if self.last_sample.is_some()
            || !self.receipts.is_empty()
            || !self.chunks.samples.is_empty()
        {
            return Err(EnduranceCaptureError::InvalidTerminal);
        }
        if !self.semantic.events.is_empty() {
            return Err(EnduranceCaptureError::InvalidProducerEvidence);
        }
        let (report_file_name, report_sha256, raw_evidence_file_name, raw_evidence_sha256) =
            self.publish_producer_evidence(EndurancePhaseTerminalStatus::NotRun)?;
        Ok(EndurancePhaseManifest {
            phase_id: self.requirement.phase_id.clone(),
            workload_sha256: self.requirement.workload_sha256.clone(),
            started_at_run_us: self.started_at_run_us,
            completed_at_run_us: self.started_at_run_us,
            producer: EndurancePhaseProducerEvidence {
                owner: self.requirement.producer_owner.clone(),
                verifier_id: self.requirement.producer_verifier_id.clone(),
                report_schema_version: self.requirement.producer_report_schema_version,
                report_sha256,
                report_file_name,
                raw_evidence_sha256,
                raw_evidence_file_name,
                reference_output_hardware_backed: false,
                external_reference_required: self
                    .requirement
                    .counters
                    .require_external_reference_lock,
            },
            chunks: Vec::new(),
            terminal: EndurancePhaseTerminalEvidence {
                status: EndurancePhaseTerminalStatus::NotRun,
                counters: EnduranceCounters::default(),
                gauges: EnduranceGauges::default(),
                workers_terminated: true,
                child_processes_reaped: true,
            },
        })
    }

    fn publish_current_chunk(&mut self) -> Result<(), EnduranceCaptureError> {
        let chunk = self.chunks.seal_current()?;
        let file_name = format!("{}-{:04}.json", chunk.phase_id, chunk.chunk_index);
        let path = self.evidence_directory.join(&file_name);
        write_json_create_new(&path, &chunk)?;
        self.receipts.push(
            EndurancePhaseChunkReceipt::from_chunk(file_name, &chunk)
                .map_err(|error| EnduranceCaptureError::Seal(error.to_string()))?,
        );
        Ok(())
    }

    fn publish_producer_evidence(
        &self,
        terminal_status: EndurancePhaseTerminalStatus,
    ) -> Result<(String, String, String, String), EnduranceCaptureError> {
        let raw_evidence_file_name = format!("{}-producer-raw.json", self.requirement.phase_id);
        let raw_path = self.evidence_directory.join(&raw_evidence_file_name);
        let raw = EnduranceProducerRawEvidence {
            schema_version: 1,
            phase_id: &self.requirement.phase_id,
            run_id: &self.run_id,
            authority_challenge: &self.authority_challenge,
            workload_sha256: &self.requirement.workload_sha256,
            producer_owner: &self.requirement.producer_owner,
            producer_verifier_id: &self.requirement.producer_verifier_id,
            events: &self.semantic.events,
        };
        write_json_create_new(&raw_path, &raw)?;
        let raw_evidence_sha256 = file_sha256(&raw_path)?;
        let report_file_name = format!("{}-producer-report.json", self.requirement.phase_id);
        let report_path = self.evidence_directory.join(&report_file_name);
        let semantic = self.semantic.counters();
        let report = EnduranceProducerReport {
            schema_version: self.requirement.producer_report_schema_version,
            phase_id: &self.requirement.phase_id,
            run_id: &self.run_id,
            authority_challenge: &self.authority_challenge,
            workload_sha256: &self.requirement.workload_sha256,
            producer_owner: &self.requirement.producer_owner,
            producer_verifier_id: &self.requirement.producer_verifier_id,
            raw_evidence_sha256: &raw_evidence_sha256,
            terminal_status,
            event_count: u32::try_from(self.semantic.events.len())
                .map_err(|_| EnduranceCaptureError::ProducerEventLimitExceeded)?,
            verified_export_artifacts: semantic.verified_export_artifacts,
            recovery_cycles: semantic.recovery_cycles,
        };
        write_json_create_new(&report_path, &report)?;
        let report_sha256 = file_sha256(&report_path)?;
        Ok((
            report_file_name,
            report_sha256,
            raw_evidence_file_name,
            raw_evidence_sha256,
        ))
    }
}

/// Serial supervisor that admits exact profile phases and seals one run manifest.
pub struct EnduranceRunCapture {
    profile: EnduranceQualificationProfile,
    profile_sha256: String,
    identity: EnduranceRunIdentity,
    capture_authority_sha256: String,
    capture_authority_challenge: String,
    next_phase_index: usize,
    previous_phase_end_us: Option<u64>,
    phases: Vec<EndurancePhaseManifest>,
}

impl EnduranceRunCapture {
    /// Freeze an exact profile, release identity, and external capture authority.
    pub fn new(
        profile: EnduranceQualificationProfile,
        identity: EnduranceRunIdentity,
        capture_authority_manifest_path: &Path,
    ) -> Result<Self, EnduranceCaptureError> {
        let prepared = PreparedEnduranceQualification::compile(profile.clone())
            .map_err(|error| EnduranceCaptureError::Profile(error.to_string()))?;
        let capture_authority = existing_regular_file(capture_authority_manifest_path)?;
        let authority: EnduranceCaptureAuthorityHeader = read_bounded_json(&capture_authority)?;
        if authority.schema_version != 1
            || authority.authority_id != "external-commercial-endurance-authority-v1"
            || authority.run_id != identity.run_id
            || authority.single_use_challenge.trim().is_empty()
            || authority.single_use_challenge.contains("placeholder")
        {
            return Err(EnduranceCaptureError::CaptureAuthorityMismatch);
        }
        Ok(Self {
            profile,
            profile_sha256: prepared.profile_sha256().to_owned(),
            identity,
            capture_authority_sha256: file_sha256(&capture_authority)?,
            capture_authority_challenge: authority.single_use_challenge,
            next_phase_index: 0,
            previous_phase_end_us: None,
            phases: Vec::new(),
        })
    }

    /// Start exactly the next serial profile phase after checking workload bytes.
    pub fn begin_phase(
        &self,
        phase_id: &str,
        started_at_run_us: u64,
        evidence_directory: &Path,
        workload_contract_path: &Path,
    ) -> Result<EndurancePhaseCapture, EnduranceCaptureError> {
        let requirement = self
            .profile
            .phases
            .get(self.next_phase_index)
            .filter(|requirement| requirement.phase_id == phase_id)
            .cloned()
            .ok_or(EnduranceCaptureError::PhaseOrder)?;
        if self.previous_phase_end_us.is_some_and(|previous| started_at_run_us < previous) {
            return Err(EnduranceCaptureError::PhaseOrder);
        }
        EndurancePhaseCapture::new(
            requirement,
            &self.identity.run_id,
            &self.capture_authority_challenge,
            started_at_run_us,
            evidence_directory,
            workload_contract_path,
            usize::from(self.profile.maximum_samples_per_chunk),
            self.profile.maximum_chunks_per_phase,
            self.profile.maximum_producer_events_per_phase,
        )
    }

    /// Commit the exact next phase manifest after its product work has terminated.
    pub fn commit_phase(
        &mut self,
        phase: EndurancePhaseManifest,
    ) -> Result<(), EnduranceCaptureError> {
        let expected = self
            .profile
            .phases
            .get(self.next_phase_index)
            .ok_or(EnduranceCaptureError::PhaseOrder)?;
        if phase.phase_id != expected.phase_id
            || self
                .previous_phase_end_us
                .is_some_and(|previous| phase.started_at_run_us < previous)
        {
            return Err(EnduranceCaptureError::PhaseOrder);
        }
        self.previous_phase_end_us = Some(phase.completed_at_run_us);
        self.next_phase_index = self
            .next_phase_index
            .checked_add(1)
            .ok_or(EnduranceCaptureError::CounterOverflow)?;
        self.phases.push(phase);
        Ok(())
    }

    /// Create and fsync the exact run manifest after all required phases exist.
    pub fn seal_manifest(
        self,
        output_path: &Path,
    ) -> Result<EnduranceRunManifest, EnduranceCaptureError> {
        if self.next_phase_index != self.profile.phases.len() {
            return Err(EnduranceCaptureError::PhaseOrder);
        }
        let manifest = EnduranceRunManifest {
            schema_version: 1,
            run_id: self.identity.run_id,
            profile_sha256: self.profile_sha256,
            source_revision: self.identity.source_revision,
            release_candidate_id: self.identity.release_candidate_id,
            product_artifact_sha256: self.identity.product_artifact_sha256,
            runtime_image_sha256: self.identity.runtime_image_sha256,
            build_provenance_sha256: self.identity.build_provenance_sha256,
            machine_report_sha256: self.identity.machine_report_sha256,
            platform_cell_sha256: self.identity.platform_cell_sha256,
            capture_authority_sha256: self.capture_authority_sha256,
            environment_before_sha256: self.identity.environment_before_sha256,
            environment_after_sha256: self.identity.environment_after_sha256,
            phases: self.phases,
        };
        write_json_create_new(output_path, &manifest)?;
        Ok(manifest)
    }
}

fn write_json_create_new(path: &Path, value: &impl Serialize) -> Result<(), EnduranceCaptureError> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))
}

fn existing_real_directory(path: &Path) -> Result<PathBuf, EnduranceCaptureError> {
    let path = std::fs::canonicalize(path)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(EnduranceCaptureError::InvalidEvidencePath);
    }
    Ok(path)
}

fn existing_regular_file(path: &Path) -> Result<PathBuf, EnduranceCaptureError> {
    let absolute = std::fs::canonicalize(path)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    let metadata = std::fs::symlink_metadata(&absolute)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() == 0 {
        return Err(EnduranceCaptureError::InvalidEvidencePath);
    }
    Ok(absolute)
}

fn read_bounded_json<T>(path: &Path) -> Result<T, EnduranceCaptureError>
where
    T: for<'de> Deserialize<'de>,
{
    const MAXIMUM_JSON_BYTES: u64 = 1024 * 1024;
    let mut file =
        File::open(path).map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    if metadata.len() == 0 || metadata.len() > MAXIMUM_JSON_BYTES {
        return Err(EnduranceCaptureError::InvalidEvidencePath);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| EnduranceCaptureError::InvalidEvidencePath)?,
    );
    Read::by_ref(&mut file)
        .take(MAXIMUM_JSON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len() {
        return Err(EnduranceCaptureError::InvalidEvidencePath);
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))
}

fn file_sha256(path: &Path) -> Result<String, EnduranceCaptureError> {
    let mut file =
        File::open(path).map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| EnduranceCaptureError::Publication(error.to_string()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// Failure to capture one complete endurance observation or bounded chunk.
#[derive(Debug, Error)]
pub enum EnduranceCaptureError {
    /// Native product-process-tree evidence was incomplete.
    #[error("endurance capture requires complete native product-process-tree memory")]
    IncompleteProcessTreeMemory,
    /// A domain snapshot used the wrong schema or observation instant.
    #[error("endurance capture owner snapshots do not share the requested schema/instant")]
    OwnerSnapshotMismatch,
    /// Checked counter aggregation overflowed.
    #[error("endurance capture counter aggregation overflowed")]
    CounterOverflow,
    /// Reference Output hardware-clock evidence could not be normalized.
    #[error("endurance capture requires valid Reference Output hardware-clock evidence")]
    InvalidHardwareTime,
    /// Recorder identity or capacity was invalid.
    #[error("endurance chunk recorder configuration is invalid")]
    InvalidChunkRecorder,
    /// Samples must be contiguous.
    #[error("endurance sample sequence is not contiguous")]
    SampleSequence,
    /// Caller must seal a full chunk before adding another sample.
    #[error("endurance sample chunk is full")]
    ChunkFull,
    /// Empty chunks cannot be published.
    #[error("endurance sample chunk is empty")]
    EmptyChunk,
    /// A phase attempted to publish more chunks than its compiled profile permits.
    #[error("endurance phase exceeded its compiled chunk limit")]
    ChunkLimitExceeded,
    /// Platform-core chunk sealing failed.
    #[error("seal endurance chunk: {0}")]
    Seal(String),
    /// The checked-in workload bytes did not match the compiled profile.
    #[error("endurance workload contract differs from the compiled profile")]
    WorkloadContractMismatch,
    /// Serial phase order or completeness was invalid.
    #[error("endurance phases must be captured exactly once in compiled profile order")]
    PhaseOrder,
    /// Terminal owner evidence did not close against the final sample.
    #[error("endurance terminal owner evidence does not close")]
    InvalidTerminal,
    /// Evidence path was not a direct regular file or real directory.
    #[error("endurance evidence path is not a direct regular sealed entry")]
    InvalidEvidencePath,
    /// Capture authority did not bind this run or carry the approved protocol.
    #[error("endurance capture authority does not bind this run")]
    CaptureAuthorityMismatch,
    /// A typed producer event was malformed or out of protocol order.
    #[error("endurance producer event is invalid or out of order")]
    InvalidProducerEvent,
    /// Typed producer evidence exceeded its compiled fixed-space limit.
    #[error("endurance producer event limit exceeded")]
    ProducerEventLimitExceeded,
    /// Typed producer evidence did not close against the terminal sample.
    #[error("endurance producer evidence does not close against terminal counters")]
    InvalidProducerEvidence,
    /// Profile compilation failed.
    #[error("compile endurance profile: {0}")]
    Profile(String),
    /// Create-only durable evidence publication failed.
    #[error("publish endurance evidence: {0}")]
    Publication(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_export::ExportEnduranceSnapshot;
    use mondrian_platform::{ProcessMemoryProbeBackend, ProcessMemoryProbeResult};
    use mondrian_playback::PlaybackEvidenceConfig;
    use mondrian_reference_output::ReferenceOutputDiagnostics;

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn capture_facts_can_only_be_projected_from_the_sealed_owner_inventory() {
        let mut facts = EnduranceCaptureFacts::from_headless_owner_snapshot(
            HeadlessEnduranceOwnerSnapshot::test_fixture(1, 2, 3, 4, 5),
        );
        facts.stamp_observed_at_us(7);

        assert_eq!(facts.schema_version, 1);
        assert_eq!(facts.observed_at_us, 7);
        assert_eq!(facts.playback_pending, 1);
        assert_eq!(facts.other_queue_depth, 2);
        assert_eq!(facts.owned_resource_units, 3);
        assert_eq!(facts.gpu_device_losses, 4);
        assert_eq!(facts.fatal_errors, 5);
        assert_eq!(facts.recovery_failures, 0);
    }

    #[test]
    fn capture_maps_owner_counters_without_reinterpreting_thresholds() {
        let playback =
            mondrian_playback::PlaybackEvidenceCollector::new(PlaybackEvidenceConfig::default())
                .expect("collector")
                .report();
        let process_memory = ProcessMemoryProbeResult::observed(
            ProcessMemoryScope::ProductProcessTree,
            ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
            2,
            1,
            100,
            200,
            300,
        );
        let reference = ReferenceOutputDiagnostics::default();
        let export = ExportEnduranceSnapshot {
            schema_version: 2,
            observed_at_us: 1,
            shutdown_requested: false,
            worker_running: true,
            worker_terminated: false,
            activity_events: 4,
            admissions: 1,
            rejections: 0,
            completions: 1,
            failures: 0,
            cancellations: 0,
            rendered_frames: 10,
            durable_artifacts: 1,
            pending_jobs: 0,
            active_jobs: 0,
            worker_failed: false,
            audio_source_owners_started: 5,
            audio_source_owners_closed: 2,
            audio_source_owner_failures: 2,
            active_audio_source_owners: 3,
        };
        let sample = capture_endurance_sample(
            EnduranceSampleTiming {
                sequence: 0,
                scheduled_at_us: 0,
                started_at_us: 0,
                completed_at_us: 1,
            },
            &process_memory,
            &playback,
            &reference,
            export,
            EnduranceCaptureFacts {
                observed_at_us: 1,
                ..EnduranceCaptureFacts::default()
            },
            EnduranceSemanticCounters { verified_export_artifacts: 1, recovery_cycles: 0 },
        )
        .expect("capture");
        assert_eq!(sample.counters.export_frames, 10);
        assert_eq!(sample.counters.export_activity_events, 4);
        assert_eq!(sample.counters.export_artifacts_verified, 1);
        assert_eq!(sample.counters.fatal_errors, 2);
        assert_eq!(sample.gauges.queue_depth, 3);
        assert_eq!(sample.process_memory.observed_process_count, 2);
    }

    #[test]
    fn recorder_seals_a_hash_chain_in_fixed_space() {
        let mut recorder = EnduranceChunkRecorder::new("phase", 1, 2).expect("recorder");
        let sample = EnduranceSample {
            sequence: 0,
            scheduled_at_us: 0,
            started_at_us: 0,
            completed_at_us: 1,
            process_memory: EnduranceProcessMemorySample {
                scope: ProcessMemoryScope::ProductProcessTree,
                backend: ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                metric: mondrian_platform::ProcessPrivateMemoryMetric::WindowsPrivateCommit,
                observed_process_count: 1,
                inventory_attempts: 1,
                inventory_complete: true,
                private_memory_bytes: 1,
                resident_bytes: 1,
            },
            reference_output_hardware_backed: false,
            external_reference_locked: false,
            reference_hardware_maximum_gap_us: 0,
            export_shutdown_requested: false,
            export_worker_running: true,
            export_worker_terminated: false,
            counters: EnduranceCounters::default(),
            gauges: EnduranceGauges::default(),
        };
        recorder.push(sample.clone()).expect("push first");
        assert!(recorder.is_full());
        let mut overflow = sample.clone();
        overflow.sequence = 1;
        assert!(matches!(
            recorder.push(overflow),
            Err(EnduranceCaptureError::ChunkFull)
        ));
        let first = recorder.seal_current().expect("seal first");
        let mut next = first.samples[0].clone();
        next.sequence = 1;
        next.scheduled_at_us = 1;
        next.started_at_us = 1;
        next.completed_at_us = 2;
        recorder.push(next).expect("push second");
        let second = recorder.seal_current().expect("seal second");
        assert_eq!(
            second.previous_chunk_sha256.as_deref(),
            Some(first.chunk_sha256.as_str())
        );
        assert!(first.verify_evidence());
        assert!(second.verify_evidence());
        let mut beyond_profile = second.samples[0].clone();
        beyond_profile.sequence = 2;
        beyond_profile.scheduled_at_us = 2;
        beyond_profile.started_at_us = 2;
        beyond_profile.completed_at_us = 3;
        recorder.push(beyond_profile).expect("buffer final allowed sample");
        assert!(matches!(
            recorder.seal_current(),
            Err(EnduranceCaptureError::ChunkLimitExceeded)
        ));
    }

    #[test]
    fn producer_events_are_bounded_and_recovery_steps_are_exactly_ordered() {
        let mut export = EnduranceSemanticRecorder::new(EndurancePhaseKind::ContinuousExport, 2);
        export
            .record_export_artifact_verified(1, "artifact-0", SHA, "validator-v1", SHA)
            .expect("record verified artifact");
        assert!(matches!(
            export.record_export_artifact_verified(2, "artifact-0", SHA, "validator-v1", SHA),
            Err(EnduranceCaptureError::InvalidProducerEvent)
        ));
        export
            .record_export_artifact_verified(2, "artifact-1", SHA, "validator-v1", SHA)
            .expect("record second verified artifact");
        assert!(matches!(
            export.record_export_artifact_verified(3, "artifact-2", SHA, "validator-v1", SHA),
            Err(EnduranceCaptureError::ProducerEventLimitExceeded)
        ));
        assert_eq!(export.counters().verified_export_artifacts, 2);

        let mut recovery =
            EnduranceSemanticRecorder::new(EndurancePhaseKind::ConcurrentRecovery, 4);
        let seek_receipt = EnduranceRecoveryOperationReceipt::seek(
            0,
            "seek-0".to_owned(),
            SHA.to_owned(),
            1,
            2,
            3,
            4,
            true,
        )
        .expect("seek receipt");
        assert!(matches!(
            recovery.record_recovery_step_completed(
                1,
                0,
                EnduranceRecoveryStep::ExportCancelRetry,
                seek_receipt.canonical_json(),
                seek_receipt.sha256(),
            ),
            Err(EnduranceCaptureError::InvalidProducerEvent)
        ));
        let receipts = [
            seek_receipt,
            EnduranceRecoveryOperationReceipt::surface_device_reopen(
                0,
                "reopen-0".to_owned(),
                SHA.to_owned(),
                1,
                2,
                3,
                4,
                SHA.to_owned(),
                SHA.to_owned(),
            )
            .expect("reopen receipt"),
            EnduranceRecoveryOperationReceipt::export_cancel_retry(
                0,
                "export-0".to_owned(),
                "cancelled-job".to_owned(),
                "retry-job".to_owned(),
                0,
                1,
                SHA.to_owned(),
                SHA.to_owned(),
                SHA.to_owned(),
            )
            .expect("Export receipt"),
            EnduranceRecoveryOperationReceipt::cache_pressure(
                0,
                "cache-0".to_owned(),
                1,
                2,
                3,
                4096,
                1024,
                3072,
                0,
                true,
                true,
                0,
                0,
                0,
                0,
                0,
                0,
                SHA.to_owned(),
                SHA.to_owned(),
            )
            .expect("cache receipt"),
        ];
        for (index, receipt) in receipts.iter().enumerate() {
            recovery
                .record_recovery_step_completed(
                    u64::try_from(index + 1).expect("event time"),
                    0,
                    receipt.step(),
                    receipt.canonical_json(),
                    receipt.sha256(),
                )
                .expect("record ordered recovery step");
        }
        assert_eq!(recovery.counters().recovery_cycles, 1);
        assert!(matches!(
            recovery.record_recovery_step_completed(
                5,
                1,
                EnduranceRecoveryStep::Seek,
                receipts[0].canonical_json(),
                receipts[0].sha256(),
            ),
            Err(EnduranceCaptureError::InvalidProducerEvent)
        ));
    }

    #[test]
    fn run_capture_publishes_chunks_and_manifest_without_hand_authored_hashes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let profile: EnduranceQualificationProfile = serde_json::from_slice(
            &std::fs::read(root.join("tests/validation/commercial-endurance-qualification.json"))
                .expect("read profile"),
        )
        .expect("parse profile");
        let temporary = tempfile::tempdir().expect("temporary capture root");
        let evidence = temporary.path().join("evidence");
        std::fs::create_dir(&evidence).expect("create evidence directory");
        let authority = temporary.path().join("capture-authority.json");
        std::fs::write(
            &authority,
            br#"{"schema_version":1,"authority_id":"external-commercial-endurance-authority-v1","run_id":"capture-test-run","single_use_challenge":"test-challenge"}"#,
        )
        .expect("write authority");
        let mut capture = EnduranceRunCapture::new(
            profile,
            EnduranceRunIdentity {
                run_id: "capture-test-run".to_owned(),
                source_revision: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
                release_candidate_id: "mondrian-test-rc".to_owned(),
                product_artifact_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                runtime_image_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                build_provenance_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                machine_report_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                platform_cell_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                environment_before_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                environment_after_sha256:
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            },
            &authority,
        )
        .expect("start run capture");

        let mut first = capture
            .begin_phase(
                "01-playback-reference-24h",
                0,
                &evidence,
                &root.join("tests/validation/endurance-workloads/playback-reference-v1.json"),
            )
            .expect("begin executed phase");
        let terminal_sample = EnduranceSample {
            sequence: 0,
            scheduled_at_us: 0,
            started_at_us: 0,
            completed_at_us: 1,
            process_memory: EnduranceProcessMemorySample {
                scope: ProcessMemoryScope::ProductProcessTree,
                backend: ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
                metric: mondrian_platform::ProcessPrivateMemoryMetric::WindowsPrivateCommit,
                observed_process_count: 1,
                inventory_attempts: 1,
                inventory_complete: true,
                private_memory_bytes: 1,
                resident_bytes: 1,
            },
            reference_output_hardware_backed: true,
            external_reference_locked: true,
            reference_hardware_maximum_gap_us: 1,
            export_shutdown_requested: true,
            export_worker_running: false,
            export_worker_terminated: true,
            counters: EnduranceCounters::default(),
            gauges: EnduranceGauges::default(),
        };
        first.push_sample(terminal_sample.clone()).expect("capture sample");
        let reference = ReferenceOutputDiagnostics {
            state: ReferenceOutputState::Stopped,
            ..ReferenceOutputDiagnostics::default()
        };
        let mut late = capture
            .begin_phase(
                "01-playback-reference-24h",
                0,
                &evidence,
                &root.join("tests/validation/endurance-workloads/playback-reference-v1.json"),
            )
            .expect("begin late-receipt phase probe");
        late.push_sample(terminal_sample).expect("capture late-receipt sample");
        assert!(matches!(
            late.finish(
                1,
                EndurancePhaseTerminalStatus::Failed,
                true,
                0,
                &reference,
                ExportQueueShutdownEvidence {
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
                    activity_events: 1,
                    audio_source_owners_started: 1,
                    audio_source_owners_closed: 1,
                    audio_source_owner_failures: 0,
                    active_audio_source_owners: 0,
                },
            ),
            Err(EnduranceCaptureError::InvalidTerminal)
        ));
        let first = first
            .finish(
                1,
                EndurancePhaseTerminalStatus::Failed,
                true,
                0,
                &reference,
                ExportQueueShutdownEvidence {
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
                    activity_events: 1,
                    audio_source_owners_started: 1,
                    audio_source_owners_closed: 1,
                    audio_source_owner_failures: 0,
                    active_audio_source_owners: 0,
                },
            )
            .expect("finish executed phase");
        capture.commit_phase(first).expect("commit first phase");

        for (phase_id, workload_name, started_at) in [
            ("02-continuous-export-24h", "continuous-export-v1.json", 1),
            (
                "03-concurrent-recovery-24h",
                "concurrent-recovery-v1.json",
                2,
            ),
        ] {
            let phase = capture
                .begin_phase(
                    phase_id,
                    started_at,
                    &evidence,
                    &root.join("tests/validation/endurance-workloads").join(workload_name),
                )
                .expect("begin not-run phase");
            let phase = phase.finish_not_run().expect("finish not-run phase");
            capture.commit_phase(phase).expect("commit not-run phase");
        }

        let manifest_path = temporary.path().join("run-manifest.json");
        let manifest = capture.seal_manifest(&manifest_path).expect("seal manifest");
        assert_eq!(manifest.phases.len(), 3);
        assert_eq!(manifest.phases[0].chunks.len(), 1);
        assert!(evidence.join(&manifest.phases[0].chunks[0].file_name).is_file());
        for phase in &manifest.phases {
            assert!(evidence.join(&phase.producer.report_file_name).is_file());
            assert!(evidence.join(&phase.producer.raw_evidence_file_name).is_file());
        }
        assert!(manifest_path.is_file());
        assert!(OpenOptions::new().write(true).create_new(true).open(&manifest_path).is_err());
    }
}
