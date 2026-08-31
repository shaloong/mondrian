//! Commercial long-duration qualification contracts and deterministic replay.
//!
//! The Module is deliberately platform-neutral. Product domains publish
//! cumulative typed snapshots, platform Adapters publish complete process-tree
//! memory observations, and a supervisor stores samples in create-only chained
//! chunks. This Module alone interprets the frozen profile. It performs no OS,
//! filesystem, playback, reference-output, or export work.

use crate::{ProcessMemoryProbeBackend, ProcessMemoryScope, ProcessPrivateMemoryMetric};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

const PROFILE_SCHEMA_VERSION: u32 = 1;
const RUN_SCHEMA_VERSION: u32 = 1;
const CHUNK_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;
const HARD_MAX_PHASES: usize = 8;
const HARD_MAX_CHUNKS_PER_PHASE: usize = 2_048;
const HARD_MAX_SAMPLES_PER_CHUNK: usize = 256;
const HOUR_MICROSECONDS: i128 = 3_600_000_000;

/// Stable aggregate qualification state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnduranceQualificationStatus {
    /// Every declared phase ran and passed.
    Qualified,
    /// At least one executed phase violated a gate.
    Failed,
    /// Required execution or evidence was absent.
    Incomplete,
}

/// Product workload family owned by one serial endurance phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndurancePhaseKind {
    /// Realtime playback and physical reference output run together.
    PlaybackReference,
    /// Repeated complete exports, validation, and durable publication.
    ContinuousExport,
    /// Playback, reference output, exports, and controlled recovery overlap.
    ConcurrentRecovery,
}

/// Terminal state asserted by the phase-owning producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndurancePhaseTerminalStatus {
    /// The producer reached its requested terminal and quiescence sequence.
    Completed,
    /// Execution started but ended in a terminal failure.
    Failed,
    /// Required hardware, fixture, or execution was unavailable.
    NotRun,
}

/// Platform-native memory gate for one phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceMemoryRequirement {
    /// Exact native process-tree backend required by the profile.
    pub backend: ProcessMemoryProbeBackend,
    /// Exact non-interchangeable private-footprint metric.
    pub metric: ProcessPrivateMemoryMetric,
    /// Absolute private-footprint ceiling after phase start.
    pub maximum_private_memory_bytes: u64,
    /// Maximum positive first-to-last settled growth after warmup.
    pub maximum_settled_growth_bytes: u64,
    /// Maximum positive integer least-squares slope after warmup.
    pub maximum_slope_bytes_per_hour: u64,
}

/// Cumulative and terminal budgets for one phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceCounterRequirement {
    /// Minimum successfully presented playback pictures.
    pub minimum_playback_presented_frames: u64,
    /// Maximum late playback pictures.
    pub maximum_playback_late_frames: u64,
    /// Maximum terminal playback failures.
    pub maximum_playback_failed_frames: u64,
    /// Maximum realtime audio underruns.
    pub maximum_audio_underruns: u64,
    /// Minimum physical reference-output completions.
    pub minimum_reference_completed_frames: u64,
    /// Maximum physical reference-output late callbacks.
    pub maximum_reference_late_frames: u64,
    /// Maximum physical reference-output dropped callbacks.
    pub maximum_reference_dropped_frames: u64,
    /// Maximum provider-flushed Reference Output frames.
    pub maximum_reference_flushed_frames: u64,
    /// Maximum Reference Output frames aborted by a terminal transition.
    pub maximum_reference_aborted_frames: u64,
    /// Minimum callbacks carrying valid provider hardware time.
    pub minimum_reference_hardware_timestamps: u64,
    /// Maximum adjacent provider hardware-clock gap after rate normalization.
    pub maximum_reference_hardware_time_gap_us: u64,
    /// Require a physical provider rather than the simulated Adapter.
    pub require_hardware_reference_output: bool,
    /// Require external-reference lock for the complete active interval.
    pub require_external_reference_lock: bool,
    /// Minimum durably published and independently verified exports.
    pub minimum_verified_exports: u64,
    /// Minimum rendered export frames across verified exports.
    pub minimum_export_frames: u64,
    /// Minimum controlled Export cancellations completed by the workload.
    pub minimum_export_cancellations: u64,
    /// Maximum canceled admitted Export jobs.
    pub maximum_export_cancellations: u64,
    /// Maximum rejected Export admission requests.
    pub maximum_export_rejections: u64,
    /// Minimum successfully completed controlled recovery cycles.
    pub minimum_recovery_cycles: u64,
    /// Maximum allowed aggregate queue depth.
    pub maximum_queue_depth: u64,
    /// Require every phase-owned queue, lease, and job gauge to end at zero.
    pub require_quiescent_terminal: bool,
    /// Require the phase producer and its owned workers to confirm shutdown.
    pub require_worker_shutdown: bool,
}

/// One exact serial workload in a commercial qualification profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndurancePhaseRequirement {
    /// Stable phase identity.
    pub phase_id: String,
    /// Product workload family.
    pub kind: EndurancePhaseKind,
    /// Exact workload contract digest admitted by this profile edition.
    pub workload_sha256: String,
    /// Exact product composition authority expected to capture the phase.
    pub producer_owner: String,
    /// Exact producer/verifier implementation identity.
    pub producer_verifier_id: String,
    /// Exact producer report schema accepted by this profile edition.
    pub producer_report_schema_version: u32,
    /// Minimum monotonic elapsed execution duration.
    pub minimum_duration_us: u64,
    /// Initial interval excluded from leak-slope and settled-growth gates.
    pub warmup_duration_us: u64,
    /// Minimum complete native samples.
    pub minimum_samples: u32,
    /// Longest permitted interval without Playback progress; zero disables it.
    pub maximum_playback_progress_gap_us: u64,
    /// Longest permitted interval without Reference Output progress; zero disables it.
    pub maximum_reference_progress_gap_us: u64,
    /// Longest permitted interval without Export progress; zero disables it.
    pub maximum_export_progress_gap_us: u64,
    /// Longest permitted interval without recovery-cycle progress; zero disables it.
    pub maximum_recovery_progress_gap_us: u64,
    /// Native process-tree memory policy.
    pub memory: EnduranceMemoryRequirement,
    /// Product-domain cumulative and terminal budgets.
    pub counters: EnduranceCounterRequirement,
}

/// Versioned, bounded commercial endurance profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceQualificationProfile {
    /// Contract schema. Version 1 is required.
    pub schema_version: u32,
    /// Stable qualification identity.
    pub qualification_id: String,
    /// Immutable profile edition.
    pub edition: String,
    /// Nominal capture cadence.
    pub sample_interval_us: u64,
    /// Largest accepted gap between scheduled samples.
    pub maximum_sample_gap_us: u64,
    /// Largest accepted native-probe completion latency.
    pub maximum_probe_latency_us: u64,
    /// Maximum samples stored in one create-only chunk.
    pub maximum_samples_per_chunk: u16,
    /// Maximum chunks stored for one phase.
    pub maximum_chunks_per_phase: u16,
    /// Maximum typed producer events retained for one phase.
    pub maximum_producer_events_per_phase: u16,
    /// Required serial phases.
    pub phases: Vec<EndurancePhaseRequirement>,
}

/// Canonically ordered, validated qualification policy.
#[derive(Debug, Clone)]
pub struct PreparedEnduranceQualification {
    profile: EnduranceQualificationProfile,
    profile_sha256: String,
}

impl PreparedEnduranceQualification {
    /// Validate, canonicalize, and hash one exact profile.
    pub fn compile(
        profile: EnduranceQualificationProfile,
    ) -> Result<Self, EnduranceQualificationError> {
        validate_profile(&profile)?;
        let profile_sha256 = digest_serializable(&profile)?;
        Ok(Self { profile, profile_sha256 })
    }

    /// Canonical SHA-256 of the compiled profile.
    pub fn profile_sha256(&self) -> &str {
        &self.profile_sha256
    }

    /// Evaluate one manifest while loading each declared chunk exactly once.
    pub fn evaluate<F>(
        &self,
        mut run: EnduranceRunManifest,
        mut load_chunk: F,
    ) -> Result<EnduranceQualificationReport, EnduranceQualificationError>
    where
        F: FnMut(
            &EndurancePhaseChunkReceipt,
        ) -> Result<EnduranceSampleChunk, EnduranceQualificationError>,
    {
        validate_run_header(&run, &self.profile_sha256)?;
        run.phases.sort_by(|left, right| left.phase_id.cmp(&right.phase_id));
        let mut observed = BTreeMap::new();
        for phase in run.phases {
            let phase_id = phase.phase_id.clone();
            if observed.insert(phase_id.clone(), phase).is_some() {
                return Err(EnduranceQualificationError::DuplicatePhase { phase_id });
            }
        }
        let expected = self
            .profile
            .phases
            .iter()
            .map(|phase| phase.phase_id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(phase_id) =
            observed.keys().find(|phase_id| !expected.contains(phase_id.as_str()))
        {
            return Err(EnduranceQualificationError::UnexpectedPhase {
                phase_id: phase_id.clone(),
            });
        }
        let mut evidence_file_names = BTreeSet::new();
        for phase in observed.values() {
            for file_name in std::iter::once(&phase.producer.report_file_name)
                .chain(std::iter::once(&phase.producer.raw_evidence_file_name))
                .chain(phase.chunks.iter().map(|receipt| &receipt.file_name))
            {
                validate_chunk_file_name(file_name)?;
                if !evidence_file_names.insert(file_name.clone()) {
                    return Err(EnduranceQualificationError::DuplicateEvidenceFileName {
                        file_name: file_name.clone(),
                    });
                }
            }
        }

        let mut missing_phases = Vec::new();
        let mut reports = Vec::with_capacity(self.profile.phases.len());
        let mut any_failed = false;
        let mut any_incomplete = false;
        let mut prior_phase_end = None;
        for requirement in &self.profile.phases {
            let Some(phase) = observed.get(&requirement.phase_id) else {
                missing_phases.push(requirement.phase_id.clone());
                any_incomplete = true;
                continue;
            };
            if let Some(prior_end) = prior_phase_end
                && phase.started_at_run_us < prior_end
            {
                return Err(EnduranceQualificationError::PhaseOverlap {
                    phase_id: phase.phase_id.clone(),
                });
            }
            prior_phase_end = Some(phase.completed_at_run_us);
            let report = evaluate_phase(requirement, phase, &self.profile, &mut load_chunk)?;
            any_failed |= report.status == EnduranceQualificationStatus::Failed;
            any_incomplete |= report.status == EnduranceQualificationStatus::Incomplete;
            reports.push(report);
        }
        let status = if any_failed {
            EnduranceQualificationStatus::Failed
        } else if any_incomplete {
            EnduranceQualificationStatus::Incomplete
        } else {
            EnduranceQualificationStatus::Qualified
        };
        let mut report = EnduranceQualificationReport {
            schema_version: REPORT_SCHEMA_VERSION,
            qualification_id: self.profile.qualification_id.clone(),
            edition: self.profile.edition.clone(),
            profile_sha256: self.profile_sha256.clone(),
            run_id: run.run_id,
            source_revision: run.source_revision,
            release_candidate_id: run.release_candidate_id,
            product_artifact_sha256: run.product_artifact_sha256,
            runtime_image_sha256: run.runtime_image_sha256,
            build_provenance_sha256: run.build_provenance_sha256,
            machine_report_sha256: run.machine_report_sha256,
            platform_cell_sha256: run.platform_cell_sha256,
            capture_authority_sha256: run.capture_authority_sha256,
            status,
            missing_phases,
            phases: reports,
            evidence_sha256: String::new(),
        };
        report.evidence_sha256 = report_digest(&report)?;
        Ok(report)
    }
}

/// Complete native process-tree fact stored in an endurance sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceProcessMemorySample {
    /// Requested and proved ownership scope.
    pub scope: ProcessMemoryScope,
    /// Exact native backend.
    pub backend: ProcessMemoryProbeBackend,
    /// Exact private-footprint metric.
    pub metric: ProcessPrivateMemoryMetric,
    /// Number of fully queried process-tree members.
    pub observed_process_count: u32,
    /// Bounded inventory attempts used by the Adapter.
    pub inventory_attempts: u32,
    /// Whether both inventories matched and every member query succeeded.
    pub inventory_complete: bool,
    /// Aggregate platform-native private footprint.
    pub private_memory_bytes: u64,
    /// Aggregate resident bytes retained for diagnosis only.
    pub resident_bytes: u64,
}

/// Monotonic cumulative domain counters. Every field must never decrease.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceCounters {
    /// Playback pictures presented Ready.
    pub playback_presented_frames: u64,
    /// Playback pictures presented late.
    pub playback_late_frames: u64,
    /// Playback picture attempts ending Failed/Blocked/Rejected.
    pub playback_failed_frames: u64,
    /// Realtime audio underrun callbacks.
    pub audio_underruns: u64,
    /// Complete frames accepted by Reference Output.
    pub reference_scheduled_frames: u64,
    /// Reference Output completion callbacks.
    pub reference_completed_frames: u64,
    /// Reference Output late callbacks.
    pub reference_late_frames: u64,
    /// Reference Output dropped callbacks.
    pub reference_dropped_frames: u64,
    /// Reference Output flushed callbacks.
    pub reference_flushed_frames: u64,
    /// Scheduled frames explicitly aborted by a terminal output transition.
    pub reference_aborted_frames: u64,
    /// Active external-reference lock losses.
    pub reference_lock_losses: u64,
    /// Completion callbacks carrying a valid provider hardware timestamp.
    pub reference_hardware_timestamps: u64,
    /// Non-monotonic or incompatible provider hardware timestamps.
    pub reference_hardware_time_failures: u64,
    /// Export submissions admitted.
    pub export_admissions: u64,
    /// Export submissions rejected before admission.
    pub export_rejections: u64,
    /// Successfully published export jobs.
    pub export_completions: u64,
    /// Failed admitted export jobs.
    pub export_failures: u64,
    /// Canceled admitted export jobs.
    pub export_cancellations: u64,
    /// Timeline frames rendered into export encoders.
    pub export_frames: u64,
    /// Export artifacts durably published by the queue authority.
    pub export_durable_artifacts: u64,
    /// Accepted export lifecycle/progress updates.
    pub export_activity_events: u64,
    /// Published artifacts independently re-opened and verified.
    pub export_artifacts_verified: u64,
    /// Completed controlled recovery cycles.
    pub recovery_cycles: u64,
    /// Failed controlled recovery cycles.
    pub recovery_failures: u64,
    /// GPU device-loss or reset terminals.
    pub gpu_device_losses: u64,
    /// Crash, panic, dead worker, or other correctness terminal.
    pub fatal_errors: u64,
}

impl EnduranceCounters {
    fn values(self) -> [u64; 26] {
        [
            self.playback_presented_frames,
            self.playback_late_frames,
            self.playback_failed_frames,
            self.audio_underruns,
            self.reference_scheduled_frames,
            self.reference_completed_frames,
            self.reference_late_frames,
            self.reference_dropped_frames,
            self.reference_flushed_frames,
            self.reference_aborted_frames,
            self.reference_lock_losses,
            self.reference_hardware_timestamps,
            self.reference_hardware_time_failures,
            self.export_admissions,
            self.export_rejections,
            self.export_completions,
            self.export_failures,
            self.export_cancellations,
            self.export_frames,
            self.export_durable_artifacts,
            self.export_activity_events,
            self.export_artifacts_verified,
            self.recovery_cycles,
            self.recovery_failures,
            self.gpu_device_losses,
            self.fatal_errors,
        ]
    }

    fn checked_delta(self, baseline: Self) -> Option<Self> {
        let current = self.values();
        let baseline_values = baseline.values();
        if !current.iter().zip(baseline_values).all(|(now, before)| *now >= before) {
            return None;
        }
        Some(Self {
            playback_presented_frames: self.playback_presented_frames
                - baseline.playback_presented_frames,
            playback_late_frames: self.playback_late_frames - baseline.playback_late_frames,
            playback_failed_frames: self.playback_failed_frames - baseline.playback_failed_frames,
            audio_underruns: self.audio_underruns - baseline.audio_underruns,
            reference_scheduled_frames: self.reference_scheduled_frames
                - baseline.reference_scheduled_frames,
            reference_completed_frames: self.reference_completed_frames
                - baseline.reference_completed_frames,
            reference_late_frames: self.reference_late_frames - baseline.reference_late_frames,
            reference_dropped_frames: self.reference_dropped_frames
                - baseline.reference_dropped_frames,
            reference_flushed_frames: self.reference_flushed_frames
                - baseline.reference_flushed_frames,
            reference_aborted_frames: self.reference_aborted_frames
                - baseline.reference_aborted_frames,
            reference_lock_losses: self.reference_lock_losses - baseline.reference_lock_losses,
            reference_hardware_timestamps: self.reference_hardware_timestamps
                - baseline.reference_hardware_timestamps,
            reference_hardware_time_failures: self.reference_hardware_time_failures
                - baseline.reference_hardware_time_failures,
            export_admissions: self.export_admissions - baseline.export_admissions,
            export_rejections: self.export_rejections - baseline.export_rejections,
            export_completions: self.export_completions - baseline.export_completions,
            export_failures: self.export_failures - baseline.export_failures,
            export_cancellations: self.export_cancellations - baseline.export_cancellations,
            export_frames: self.export_frames - baseline.export_frames,
            export_durable_artifacts: self.export_durable_artifacts
                - baseline.export_durable_artifacts,
            export_activity_events: self.export_activity_events - baseline.export_activity_events,
            export_artifacts_verified: self.export_artifacts_verified
                - baseline.export_artifacts_verified,
            recovery_cycles: self.recovery_cycles - baseline.recovery_cycles,
            recovery_failures: self.recovery_failures - baseline.recovery_failures,
            gpu_device_losses: self.gpu_device_losses - baseline.gpu_device_losses,
            fatal_errors: self.fatal_errors - baseline.fatal_errors,
        })
    }

    fn playback_progress(self) -> u64 {
        self.playback_presented_frames
            .saturating_add(self.playback_late_frames)
            .saturating_add(self.playback_failed_frames)
    }

    fn reference_progress(self) -> u64 {
        self.reference_completed_frames
            .saturating_add(self.reference_late_frames)
            .saturating_add(self.reference_dropped_frames)
            .saturating_add(self.reference_flushed_frames)
            .saturating_add(self.reference_aborted_frames)
    }

    fn export_progress(self) -> u64 {
        self.export_frames
            .saturating_add(self.export_completions)
            .saturating_add(self.export_durable_artifacts)
            .saturating_add(self.export_artifacts_verified)
    }
}

/// Point-in-time gauges. These may rise and fall but terminal closure is exact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceGauges {
    /// Playback work with a live presentation binding.
    pub playback_pending: u64,
    /// Scheduled Reference Output frames awaiting a callback.
    pub reference_outstanding_frames: u64,
    /// Pending export jobs.
    pub export_pending_jobs: u64,
    /// Running, cancelling, or committing export jobs.
    pub export_active_jobs: u64,
    /// Aggregate current queue depth across phase-owned bounded queues.
    pub queue_depth: u64,
    /// Aggregate owned leases/resources that must be released at terminal.
    pub owned_resource_units: u64,
}

impl EnduranceGauges {
    fn is_quiescent(self) -> bool {
        self == Self::default()
    }
}

/// One fixed-cadence monotonic observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceSample {
    /// Contiguous sequence starting at zero for each phase.
    pub sequence: u64,
    /// Intended sample instant relative to phase start.
    pub scheduled_at_us: u64,
    /// Actual native probe start relative to phase start.
    pub started_at_us: u64,
    /// Complete sample publication relative to phase start.
    pub completed_at_us: u64,
    /// Complete platform-native process-tree memory fact.
    pub process_memory: EnduranceProcessMemorySample,
    /// Whether this observation came from a physical Reference Output provider.
    pub reference_output_hardware_backed: bool,
    /// Whether required external reference was positively locked at this observation.
    pub external_reference_locked: bool,
    /// Largest adjacent provider hardware-clock gap observed so far, in microseconds.
    pub reference_hardware_maximum_gap_us: u64,
    /// Whether the Export queue had entered permanent shutdown at this observation.
    pub export_shutdown_requested: bool,
    /// Whether the Export worker was alive at this observation.
    pub export_worker_running: bool,
    /// Whether the Export worker had returned from its loop.
    pub export_worker_terminated: bool,
    /// Monotonic product-domain counters.
    pub counters: EnduranceCounters,
    /// Current bounded-resource gauges.
    pub gauges: EnduranceGauges,
}

/// Create-only sample segment linked to its predecessor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceSampleChunk {
    /// Chunk schema. Version 1 is required.
    pub schema_version: u32,
    /// Owning phase identity.
    pub phase_id: String,
    /// Contiguous zero-based chunk index.
    pub chunk_index: u32,
    /// Digest of the preceding chunk; absent only for index zero.
    pub previous_chunk_sha256: Option<String>,
    /// Ordered samples.
    pub samples: Vec<EnduranceSample>,
    /// SHA-256 over every preceding field with this value empty.
    pub chunk_sha256: String,
}

impl EnduranceSampleChunk {
    /// Seal a newly produced chunk.
    pub fn seal(mut self) -> Result<Self, EnduranceQualificationError> {
        self.chunk_sha256.clear();
        self.chunk_sha256 = chunk_digest(&self)?;
        Ok(self)
    }

    /// Verify the deterministic chunk digest.
    pub fn verify_evidence(&self) -> bool {
        chunk_digest(self).is_ok_and(|digest| digest == self.chunk_sha256)
    }
}

/// Bounded manifest receipt for one sample chunk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndurancePhaseChunkReceipt {
    /// Link-free file name inside the sealed phase directory.
    pub file_name: String,
    /// Contiguous zero-based chunk index.
    pub chunk_index: u32,
    /// Exact chunk SHA-256.
    pub chunk_sha256: String,
    /// Exact sample count in the chunk.
    pub sample_count: u16,
    /// First sample sequence.
    pub first_sequence: u64,
    /// Last sample sequence.
    pub last_sequence: u64,
}

impl EndurancePhaseChunkReceipt {
    /// Build a manifest receipt from one sealed non-empty chunk.
    pub fn from_chunk(
        file_name: impl Into<String>,
        chunk: &EnduranceSampleChunk,
    ) -> Result<Self, EnduranceQualificationError> {
        let first = chunk.samples.first().ok_or(EnduranceQualificationError::EmptyChunk)?;
        let last = chunk.samples.last().ok_or(EnduranceQualificationError::EmptyChunk)?;
        let sample_count = u16::try_from(chunk.samples.len())
            .map_err(|_| EnduranceQualificationError::ChunkSampleLimitExceeded)?;
        Ok(Self {
            file_name: file_name.into(),
            chunk_index: chunk.chunk_index,
            chunk_sha256: chunk.chunk_sha256.clone(),
            sample_count,
            first_sequence: first.sequence,
            last_sequence: last.sequence,
        })
    }
}

/// Owner and runtime facts frozen for one phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndurancePhaseProducerEvidence {
    /// Module responsible for the product workload.
    pub owner: String,
    /// Exact producer/verifier identity.
    pub verifier_id: String,
    /// Positive producer report schema.
    pub report_schema_version: u32,
    /// SHA-256 of the independently produced phase report.
    pub report_sha256: String,
    /// Link-free phase report file inside the sealed evidence directory.
    pub report_file_name: String,
    /// SHA-256 of bounded raw producer evidence outside sample chunks.
    pub raw_evidence_sha256: String,
    /// Link-free raw evidence file inside the sealed evidence directory.
    pub raw_evidence_file_name: String,
    /// Whether Reference Output used a physical provider.
    pub reference_output_hardware_backed: bool,
    /// Whether the open output contract required external reference lock.
    pub external_reference_required: bool,
}

/// Phase-owned terminal closure evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndurancePhaseTerminalEvidence {
    /// Producer terminal state.
    pub status: EndurancePhaseTerminalStatus,
    /// Final counters, required to match the last sample exactly.
    pub counters: EnduranceCounters,
    /// Final gauges, required to match the last sample exactly.
    pub gauges: EnduranceGauges,
    /// Whether every phase-owned worker returned before final process sampling.
    pub workers_terminated: bool,
    /// Whether supervised descendant-process ownership was empty at terminal.
    pub child_processes_reaped: bool,
}

/// One phase entry in a sealed endurance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndurancePhaseManifest {
    /// Profile phase identity.
    pub phase_id: String,
    /// Exact workload/corpus/configuration SHA-256.
    pub workload_sha256: String,
    /// Phase start relative to the run's monotonic origin.
    pub started_at_run_us: u64,
    /// Phase completion relative to the run's monotonic origin.
    pub completed_at_run_us: u64,
    /// Product owner evidence.
    pub producer: EndurancePhaseProducerEvidence,
    /// Ordered create-only chunk receipts.
    pub chunks: Vec<EndurancePhaseChunkReceipt>,
    /// Exact terminal closure.
    pub terminal: EndurancePhaseTerminalEvidence,
}

/// Exact-source, exact-runtime manifest for one serial qualification run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceRunManifest {
    /// Run schema. Version 1 is required.
    pub schema_version: u32,
    /// Unique run identity.
    pub run_id: String,
    /// Canonical profile SHA-256.
    pub profile_sha256: String,
    /// Clean lowercase Git source revision.
    pub source_revision: String,
    /// Exact release-candidate identity.
    pub release_candidate_id: String,
    /// SHA-256 of the qualified package/artifact.
    pub product_artifact_sha256: String,
    /// SHA-256 of the executable image actually run.
    pub runtime_image_sha256: String,
    /// SHA-256 of exact build provenance.
    pub build_provenance_sha256: String,
    /// SHA-256 of the machine inventory report.
    pub machine_report_sha256: String,
    /// SHA-256 of the admitted COL-046 platform/display row.
    pub platform_cell_sha256: String,
    /// SHA-256 of the externally approved single-use capture authority manifest.
    pub capture_authority_sha256: String,
    /// Environment identity before the first phase.
    pub environment_before_sha256: String,
    /// Environment identity after the last phase.
    pub environment_after_sha256: String,
    /// Serial phase manifests.
    pub phases: Vec<EndurancePhaseManifest>,
}

/// Deterministic measured values for one present phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndurancePhaseReport {
    /// Phase identity.
    pub phase_id: String,
    /// Workload family.
    pub kind: EndurancePhaseKind,
    /// Exact workload contract digest replayed for this phase.
    pub workload_sha256: String,
    /// Product authority that captured this phase.
    pub producer_owner: String,
    /// Producer/verifier implementation identity.
    pub producer_verifier_id: String,
    /// Producer report schema version.
    pub producer_report_schema_version: u32,
    /// Exact normalized producer report digest.
    pub producer_report_sha256: String,
    /// Link-free normalized producer report file.
    pub producer_report_file_name: String,
    /// Exact sealed raw-evidence digest.
    pub producer_raw_evidence_sha256: String,
    /// Link-free sealed raw-evidence file.
    pub producer_raw_evidence_file_name: String,
    /// Whether the producer proved a physical Reference Output provider.
    pub reference_output_hardware_backed: bool,
    /// Whether the workload required external reference.
    pub external_reference_required: bool,
    /// Terminal phase verdict.
    pub status: EnduranceQualificationStatus,
    /// Complete samples replayed.
    pub sample_count: u64,
    /// Monotonic observed duration.
    pub observed_duration_us: u64,
    /// Largest scheduled-sample gap.
    pub maximum_sample_gap_us: u64,
    /// Largest native-probe completion latency.
    pub maximum_probe_latency_us: u64,
    /// Largest workload progress gap.
    pub maximum_progress_gap_us: u64,
    /// Largest Playback-only progress gap.
    pub maximum_playback_progress_gap_us: u64,
    /// Largest Reference Output-only progress gap.
    pub maximum_reference_progress_gap_us: u64,
    /// Largest Export-only progress gap.
    pub maximum_export_progress_gap_us: u64,
    /// Largest recovery-cycle-only progress gap.
    pub maximum_recovery_progress_gap_us: u64,
    /// Largest normalized adjacent provider hardware-clock gap.
    pub maximum_reference_hardware_time_gap_us: u64,
    /// Largest aggregate queue depth observed during the phase.
    pub maximum_queue_depth: u64,
    /// Peak platform-native private footprint.
    pub peak_private_memory_bytes: u64,
    /// Positive settled first-to-last memory growth.
    pub settled_growth_bytes: u64,
    /// Positive settled least-squares slope in bytes/hour.
    pub positive_slope_bytes_per_hour: u64,
    /// Cumulative deltas across the phase.
    pub counter_deltas: EnduranceCounters,
    /// Final gauges.
    pub final_gauges: EnduranceGauges,
    /// Deterministically ordered failed or incomplete gate identifiers.
    pub failed_checks: Vec<String>,
}

/// Self-verifying aggregate endurance report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnduranceQualificationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Qualification profile identity.
    pub qualification_id: String,
    /// Profile edition.
    pub edition: String,
    /// Canonical profile SHA-256.
    pub profile_sha256: String,
    /// Run identity.
    pub run_id: String,
    /// Exact source revision.
    pub source_revision: String,
    /// Release-candidate identity.
    pub release_candidate_id: String,
    /// Product artifact SHA-256.
    pub product_artifact_sha256: String,
    /// Executed runtime image SHA-256.
    pub runtime_image_sha256: String,
    /// Build provenance SHA-256.
    pub build_provenance_sha256: String,
    /// Machine report SHA-256.
    pub machine_report_sha256: String,
    /// Admitted platform/display row SHA-256.
    pub platform_cell_sha256: String,
    /// Approved single-use capture authority manifest SHA-256.
    pub capture_authority_sha256: String,
    /// Aggregate status.
    pub status: EnduranceQualificationStatus,
    /// Required phases absent from the manifest.
    pub missing_phases: Vec<String>,
    /// Present phase reports in canonical order.
    pub phases: Vec<EndurancePhaseReport>,
    /// SHA-256 over every preceding report field.
    pub evidence_sha256: String,
}

impl EnduranceQualificationReport {
    /// Verify the report's deterministic evidence digest.
    pub fn verify_evidence(&self) -> bool {
        report_digest(self).is_ok_and(|digest| digest == self.evidence_sha256)
    }
}

#[derive(Default)]
struct ProgressTracker {
    last_value: Option<u64>,
    last_progress_at_us: Option<u64>,
    maximum_gap_us: u64,
}

impl ProgressTracker {
    fn observe(&mut self, value: u64, observed_at_us: u64) {
        match self.last_value {
            None => {
                self.last_value = Some(value);
                self.last_progress_at_us = Some(observed_at_us);
            }
            Some(previous) if value > previous => {
                if let Some(last_progress_at_us) = self.last_progress_at_us {
                    self.maximum_gap_us =
                        self.maximum_gap_us.max(observed_at_us.saturating_sub(last_progress_at_us));
                }
                self.last_value = Some(value);
                self.last_progress_at_us = Some(observed_at_us);
            }
            Some(_) => {}
        }
    }

    fn finish(&mut self, observed_at_us: u64) -> u64 {
        if let Some(last_progress_at_us) = self.last_progress_at_us {
            self.maximum_gap_us =
                self.maximum_gap_us.max(observed_at_us.saturating_sub(last_progress_at_us));
        }
        self.maximum_gap_us
    }
}

#[derive(Default)]
struct MemoryRegression {
    count: i128,
    sum_x: i128,
    sum_y: i128,
    sum_xx: i128,
    sum_xy: i128,
}

impl MemoryRegression {
    fn observe(&mut self, elapsed_us: u64, bytes: u64) -> Result<(), EnduranceQualificationError> {
        let x = i128::from(elapsed_us);
        let y = i128::from(bytes);
        self.count = self
            .count
            .checked_add(1)
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        self.sum_x = self
            .sum_x
            .checked_add(x)
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        self.sum_y = self
            .sum_y
            .checked_add(y)
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        self.sum_xx = self
            .sum_xx
            .checked_add(x.checked_mul(x).ok_or(EnduranceQualificationError::ArithmeticOverflow)?)
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        self.sum_xy = self
            .sum_xy
            .checked_add(x.checked_mul(y).ok_or(EnduranceQualificationError::ArithmeticOverflow)?)
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        Ok(())
    }

    fn positive_slope_bytes_per_hour(&self) -> Result<u64, EnduranceQualificationError> {
        if self.count < 2 {
            return Ok(0);
        }
        let numerator = self
            .count
            .checked_mul(self.sum_xy)
            .and_then(|value| value.checked_sub(self.sum_x.checked_mul(self.sum_y)?))
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        if numerator <= 0 {
            return Ok(0);
        }
        let denominator = self
            .count
            .checked_mul(self.sum_xx)
            .and_then(|value| value.checked_sub(self.sum_x.checked_mul(self.sum_x)?))
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        if denominator <= 0 {
            return Ok(0);
        }
        let scaled = numerator
            .checked_mul(HOUR_MICROSECONDS)
            .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
        u64::try_from(scaled / denominator)
            .map_err(|_| EnduranceQualificationError::ArithmeticOverflow)
    }
}

fn evaluate_phase<F>(
    requirement: &EndurancePhaseRequirement,
    phase: &EndurancePhaseManifest,
    profile: &EnduranceQualificationProfile,
    load_chunk: &mut F,
) -> Result<EndurancePhaseReport, EnduranceQualificationError>
where
    F: FnMut(
        &EndurancePhaseChunkReceipt,
    ) -> Result<EnduranceSampleChunk, EnduranceQualificationError>,
{
    validate_phase_manifest(requirement, phase, profile)?;
    let mut previous_chunk_sha256 = None;
    let mut previous_sample: Option<EnduranceSample> = None;
    let mut baseline_counters = None;
    let mut first_scheduled = None;
    let mut playback_progress = ProgressTracker::default();
    let mut reference_progress = ProgressTracker::default();
    let mut export_progress = ProgressTracker::default();
    let mut recovery_progress = ProgressTracker::default();
    let mut maximum_sample_gap_us = 0;
    let mut maximum_probe_latency_us = 0;
    let mut maximum_reference_hardware_time_gap_us = 0;
    let mut all_reference_samples_hardware_backed = true;
    let mut all_reference_samples_locked = true;
    let mut maximum_queue_depth = 0;
    let mut peak_private_memory_bytes = 0;
    let mut settled_first_memory = None;
    let mut settled_last_memory = None;
    let mut regression = MemoryRegression::default();
    let mut sample_count = 0_u64;

    for (expected_chunk_index, receipt) in phase.chunks.iter().enumerate() {
        let expected_chunk_index = u32::try_from(expected_chunk_index)
            .map_err(|_| EnduranceQualificationError::ChunkLimitExceeded)?;
        if receipt.chunk_index != expected_chunk_index {
            return Err(EnduranceQualificationError::ChunkOrder {
                phase_id: phase.phase_id.clone(),
            });
        }
        validate_chunk_file_name(&receipt.file_name)?;
        validate_sha256("chunk_sha256", &receipt.chunk_sha256)?;
        let chunk = load_chunk(receipt)?;
        validate_chunk(
            phase,
            receipt,
            &chunk,
            previous_chunk_sha256.as_deref(),
            profile,
        )?;
        previous_chunk_sha256 = Some(chunk.chunk_sha256.clone());
        for sample in chunk.samples {
            validate_sample(requirement, &sample)?;
            if let Some(previous) = previous_sample.as_ref() {
                if sample.sequence != previous.sequence.saturating_add(1)
                    || sample.scheduled_at_us <= previous.scheduled_at_us
                {
                    return Err(EnduranceQualificationError::SampleOrder {
                        phase_id: phase.phase_id.clone(),
                    });
                }
                let gap = sample.scheduled_at_us - previous.scheduled_at_us;
                maximum_sample_gap_us = maximum_sample_gap_us.max(gap);
                if gap > profile.maximum_sample_gap_us {
                    return Err(EnduranceQualificationError::SampleGap {
                        phase_id: phase.phase_id.clone(),
                    });
                }
                if sample.counters.checked_delta(previous.counters).is_none() {
                    return Err(EnduranceQualificationError::CounterRegression {
                        phase_id: phase.phase_id.clone(),
                    });
                }
            } else if sample.sequence != 0 {
                return Err(EnduranceQualificationError::SampleOrder {
                    phase_id: phase.phase_id.clone(),
                });
            }
            let first = *first_scheduled.get_or_insert(sample.scheduled_at_us);
            baseline_counters.get_or_insert(sample.counters);
            let latency = sample.completed_at_us - sample.scheduled_at_us;
            maximum_probe_latency_us = maximum_probe_latency_us.max(latency);
            if latency > profile.maximum_probe_latency_us {
                return Err(EnduranceQualificationError::ProbeLatency {
                    phase_id: phase.phase_id.clone(),
                });
            }
            peak_private_memory_bytes =
                peak_private_memory_bytes.max(sample.process_memory.private_memory_bytes);
            maximum_reference_hardware_time_gap_us = maximum_reference_hardware_time_gap_us
                .max(sample.reference_hardware_maximum_gap_us);
            all_reference_samples_hardware_backed &= sample.reference_output_hardware_backed;
            all_reference_samples_locked &= sample.external_reference_locked;
            maximum_queue_depth = maximum_queue_depth.max(sample.gauges.queue_depth);
            let elapsed = sample.scheduled_at_us - first;
            if elapsed >= requirement.warmup_duration_us {
                settled_first_memory.get_or_insert(sample.process_memory.private_memory_bytes);
                settled_last_memory = Some(sample.process_memory.private_memory_bytes);
                regression.observe(
                    elapsed - requirement.warmup_duration_us,
                    sample.process_memory.private_memory_bytes,
                )?;
            }
            playback_progress.observe(sample.counters.playback_progress(), sample.completed_at_us);
            reference_progress
                .observe(sample.counters.reference_progress(), sample.completed_at_us);
            export_progress.observe(sample.counters.export_progress(), sample.completed_at_us);
            recovery_progress.observe(sample.counters.recovery_cycles, sample.completed_at_us);
            sample_count = sample_count
                .checked_add(1)
                .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
            previous_sample = Some(sample);
        }
    }

    let Some(first_scheduled) = first_scheduled else {
        return phase_report_without_samples(requirement, phase);
    };
    let last = previous_sample.as_ref().ok_or(EnduranceQualificationError::EmptyChunk)?;
    if phase.terminal.counters != last.counters || phase.terminal.gauges != last.gauges {
        return Err(EnduranceQualificationError::TerminalMismatch {
            phase_id: phase.phase_id.clone(),
        });
    }
    let observed_duration_us = last.completed_at_us - first_scheduled;
    let declared_duration_us = phase.completed_at_run_us - phase.started_at_run_us;
    if declared_duration_us != last.completed_at_us {
        return Err(EnduranceQualificationError::PhaseClockMismatch {
            phase_id: phase.phase_id.clone(),
        });
    }
    let maximum_playback_progress_gap_us = playback_progress.finish(last.completed_at_us);
    let maximum_reference_progress_gap_us = reference_progress.finish(last.completed_at_us);
    let maximum_export_progress_gap_us = export_progress.finish(last.completed_at_us);
    let maximum_recovery_progress_gap_us = recovery_progress.finish(last.completed_at_us);
    let maximum_progress_gap_us = [
        maximum_playback_progress_gap_us,
        maximum_reference_progress_gap_us,
        maximum_export_progress_gap_us,
        maximum_recovery_progress_gap_us,
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    let baseline = baseline_counters.ok_or(EnduranceQualificationError::EmptyChunk)?;
    let counter_deltas = last.counters.checked_delta(baseline).ok_or_else(|| {
        EnduranceQualificationError::CounterRegression { phase_id: phase.phase_id.clone() }
    })?;
    let settled_growth_bytes = settled_last_memory
        .zip(settled_first_memory)
        .map_or(0, |(last, first)| last.saturating_sub(first));
    let positive_slope_bytes_per_hour = regression.positive_slope_bytes_per_hour()?;
    let mut failed = Vec::new();
    let mut incomplete = Vec::new();
    match phase.terminal.status {
        EndurancePhaseTerminalStatus::Completed => {}
        EndurancePhaseTerminalStatus::Failed => failed.push("producer_terminal"),
        EndurancePhaseTerminalStatus::NotRun => incomplete.push("producer_not_run"),
    }
    check_min(
        sample_count,
        u64::from(requirement.minimum_samples),
        "sample_count",
        &mut failed,
    );
    check_min(
        observed_duration_us,
        requirement.minimum_duration_us,
        "duration",
        &mut failed,
    );
    check_optional_progress_gap(
        maximum_playback_progress_gap_us,
        requirement.maximum_playback_progress_gap_us,
        "playback_progress_gap",
        &mut failed,
    );
    check_optional_progress_gap(
        maximum_reference_progress_gap_us,
        requirement.maximum_reference_progress_gap_us,
        "reference_progress_gap",
        &mut failed,
    );
    check_optional_progress_gap(
        maximum_export_progress_gap_us,
        requirement.maximum_export_progress_gap_us,
        "export_progress_gap",
        &mut failed,
    );
    check_optional_progress_gap(
        maximum_recovery_progress_gap_us,
        requirement.maximum_recovery_progress_gap_us,
        "recovery_progress_gap",
        &mut failed,
    );
    check_max(
        peak_private_memory_bytes,
        requirement.memory.maximum_private_memory_bytes,
        "memory_peak",
        &mut failed,
    );
    check_max(
        settled_growth_bytes,
        requirement.memory.maximum_settled_growth_bytes,
        "memory_growth",
        &mut failed,
    );
    check_max(
        positive_slope_bytes_per_hour,
        requirement.memory.maximum_slope_bytes_per_hour,
        "memory_slope",
        &mut failed,
    );
    evaluate_counter_gates(
        requirement,
        phase,
        counter_deltas,
        last.gauges,
        maximum_queue_depth,
        maximum_reference_hardware_time_gap_us,
        all_reference_samples_hardware_backed,
        all_reference_samples_locked,
        &mut failed,
    );
    if requirement.counters.require_worker_shutdown
        && (!phase.terminal.workers_terminated
            || !phase.terminal.child_processes_reaped
            || !last.export_shutdown_requested
            || last.export_worker_running
            || !last.export_worker_terminated)
    {
        failed.push("worker_shutdown");
    }
    failed.sort_unstable();
    failed.dedup();
    incomplete.sort_unstable();
    incomplete.dedup();
    let status = if !failed.is_empty() {
        EnduranceQualificationStatus::Failed
    } else if !incomplete.is_empty() {
        EnduranceQualificationStatus::Incomplete
    } else {
        EnduranceQualificationStatus::Qualified
    };
    let mut failed_checks = failed.into_iter().map(str::to_owned).collect::<Vec<_>>();
    failed_checks.extend(incomplete.into_iter().map(str::to_owned));
    Ok(EndurancePhaseReport {
        phase_id: phase.phase_id.clone(),
        kind: requirement.kind,
        workload_sha256: phase.workload_sha256.clone(),
        producer_owner: phase.producer.owner.clone(),
        producer_verifier_id: phase.producer.verifier_id.clone(),
        producer_report_schema_version: phase.producer.report_schema_version,
        producer_report_sha256: phase.producer.report_sha256.clone(),
        producer_report_file_name: phase.producer.report_file_name.clone(),
        producer_raw_evidence_sha256: phase.producer.raw_evidence_sha256.clone(),
        producer_raw_evidence_file_name: phase.producer.raw_evidence_file_name.clone(),
        reference_output_hardware_backed: phase.producer.reference_output_hardware_backed,
        external_reference_required: phase.producer.external_reference_required,
        status,
        sample_count,
        observed_duration_us,
        maximum_sample_gap_us,
        maximum_probe_latency_us,
        maximum_progress_gap_us,
        maximum_playback_progress_gap_us,
        maximum_reference_progress_gap_us,
        maximum_export_progress_gap_us,
        maximum_recovery_progress_gap_us,
        maximum_reference_hardware_time_gap_us,
        maximum_queue_depth,
        peak_private_memory_bytes,
        settled_growth_bytes,
        positive_slope_bytes_per_hour,
        counter_deltas,
        final_gauges: last.gauges,
        failed_checks,
    })
}

fn phase_report_without_samples(
    requirement: &EndurancePhaseRequirement,
    phase: &EndurancePhaseManifest,
) -> Result<EndurancePhaseReport, EnduranceQualificationError> {
    if phase.terminal.status != EndurancePhaseTerminalStatus::NotRun {
        return Err(EnduranceQualificationError::EmptyPhase { phase_id: phase.phase_id.clone() });
    }
    Ok(EndurancePhaseReport {
        phase_id: phase.phase_id.clone(),
        kind: requirement.kind,
        workload_sha256: phase.workload_sha256.clone(),
        producer_owner: phase.producer.owner.clone(),
        producer_verifier_id: phase.producer.verifier_id.clone(),
        producer_report_schema_version: phase.producer.report_schema_version,
        producer_report_sha256: phase.producer.report_sha256.clone(),
        producer_report_file_name: phase.producer.report_file_name.clone(),
        producer_raw_evidence_sha256: phase.producer.raw_evidence_sha256.clone(),
        producer_raw_evidence_file_name: phase.producer.raw_evidence_file_name.clone(),
        reference_output_hardware_backed: phase.producer.reference_output_hardware_backed,
        external_reference_required: phase.producer.external_reference_required,
        status: EnduranceQualificationStatus::Incomplete,
        sample_count: 0,
        observed_duration_us: 0,
        maximum_sample_gap_us: 0,
        maximum_probe_latency_us: 0,
        maximum_progress_gap_us: 0,
        maximum_playback_progress_gap_us: 0,
        maximum_reference_progress_gap_us: 0,
        maximum_export_progress_gap_us: 0,
        maximum_recovery_progress_gap_us: 0,
        maximum_reference_hardware_time_gap_us: 0,
        maximum_queue_depth: 0,
        peak_private_memory_bytes: 0,
        settled_growth_bytes: 0,
        positive_slope_bytes_per_hour: 0,
        counter_deltas: EnduranceCounters::default(),
        final_gauges: phase.terminal.gauges,
        failed_checks: vec!["producer_not_run".to_owned()],
    })
}

fn evaluate_counter_gates(
    requirement: &EndurancePhaseRequirement,
    phase: &EndurancePhaseManifest,
    counters: EnduranceCounters,
    gauges: EnduranceGauges,
    maximum_queue_depth: u64,
    maximum_reference_hardware_time_gap_us: u64,
    all_reference_samples_hardware_backed: bool,
    all_reference_samples_locked: bool,
    failed: &mut Vec<&'static str>,
) {
    let gate = &requirement.counters;
    check_min(
        counters.playback_presented_frames,
        gate.minimum_playback_presented_frames,
        "playback_presented",
        failed,
    );
    check_max(
        counters.playback_late_frames,
        gate.maximum_playback_late_frames,
        "playback_late",
        failed,
    );
    check_max(
        counters.playback_failed_frames,
        gate.maximum_playback_failed_frames,
        "playback_failed",
        failed,
    );
    check_max(
        counters.audio_underruns,
        gate.maximum_audio_underruns,
        "audio_underrun",
        failed,
    );
    check_min(
        counters.reference_completed_frames,
        gate.minimum_reference_completed_frames,
        "reference_completed",
        failed,
    );
    check_max(
        counters.reference_late_frames,
        gate.maximum_reference_late_frames,
        "reference_late",
        failed,
    );
    check_max(
        counters.reference_dropped_frames,
        gate.maximum_reference_dropped_frames,
        "reference_dropped",
        failed,
    );
    check_max(
        counters.reference_flushed_frames,
        gate.maximum_reference_flushed_frames,
        "reference_flushed",
        failed,
    );
    check_max(
        counters.reference_aborted_frames,
        gate.maximum_reference_aborted_frames,
        "reference_aborted",
        failed,
    );
    check_min(
        counters.reference_hardware_timestamps,
        gate.minimum_reference_hardware_timestamps,
        "reference_hardware_time",
        failed,
    );
    check_max(
        maximum_reference_hardware_time_gap_us,
        gate.maximum_reference_hardware_time_gap_us,
        "reference_hardware_time_gap",
        failed,
    );
    check_min(
        counters.export_artifacts_verified,
        gate.minimum_verified_exports,
        "verified_exports",
        failed,
    );
    check_min(
        counters.export_frames,
        gate.minimum_export_frames,
        "export_frames",
        failed,
    );
    check_max(
        counters.export_cancellations,
        gate.maximum_export_cancellations,
        "export_cancellations",
        failed,
    );
    check_max(
        counters.export_rejections,
        gate.maximum_export_rejections,
        "export_rejections",
        failed,
    );
    check_min(
        counters.export_cancellations,
        gate.minimum_export_cancellations,
        "export_cancellation_cycles",
        failed,
    );
    check_min(
        counters.recovery_cycles,
        gate.minimum_recovery_cycles,
        "recovery_cycles",
        failed,
    );
    check_max(
        maximum_queue_depth,
        gate.maximum_queue_depth,
        "queue_depth",
        failed,
    );
    if gate.require_hardware_reference_output
        && (!phase.producer.reference_output_hardware_backed
            || !all_reference_samples_hardware_backed)
    {
        failed.push("physical_reference_output");
    }
    if gate.require_external_reference_lock
        && (!phase.producer.external_reference_required
            || !all_reference_samples_locked
            || counters.reference_lock_losses != 0)
    {
        failed.push("external_reference_lock");
    }
    if counters.reference_hardware_time_failures != 0
        || counters.export_failures != 0
        || counters.recovery_failures != 0
        || counters.gpu_device_losses != 0
        || counters.fatal_errors != 0
    {
        failed.push("terminal_correctness");
    }
    if counters.export_completions != counters.export_durable_artifacts
        || counters.export_completions != counters.export_artifacts_verified
    {
        failed.push("export_artifact_closure");
    }
    if gate.require_quiescent_terminal && !gauges.is_quiescent() {
        failed.push("terminal_quiescence");
    }
}

fn check_min(actual: u64, minimum: u64, name: &'static str, failed: &mut Vec<&'static str>) {
    if actual < minimum {
        failed.push(name);
    }
}

fn check_max(actual: u64, maximum: u64, name: &'static str, failed: &mut Vec<&'static str>) {
    if actual > maximum {
        failed.push(name);
    }
}

fn check_optional_progress_gap(
    actual: u64,
    maximum: u64,
    name: &'static str,
    failed: &mut Vec<&'static str>,
) {
    if maximum != 0 && actual > maximum {
        failed.push(name);
    }
}

fn validate_profile(
    profile: &EnduranceQualificationProfile,
) -> Result<(), EnduranceQualificationError> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        return Err(EnduranceQualificationError::UnsupportedProfileSchema {
            actual: profile.schema_version,
        });
    }
    validate_identity("qualification_id", &profile.qualification_id)?;
    validate_identity("edition", &profile.edition)?;
    if profile.sample_interval_us == 0
        || profile.maximum_sample_gap_us < profile.sample_interval_us
        || profile.maximum_probe_latency_us == 0
        || profile.maximum_samples_per_chunk == 0
        || usize::from(profile.maximum_samples_per_chunk) > HARD_MAX_SAMPLES_PER_CHUNK
        || profile.maximum_chunks_per_phase == 0
        || usize::from(profile.maximum_chunks_per_phase) > HARD_MAX_CHUNKS_PER_PHASE
        || profile.maximum_producer_events_per_phase == 0
        || profile.maximum_producer_events_per_phase > 4_096
        || profile.phases.is_empty()
        || profile.phases.len() > HARD_MAX_PHASES
    {
        return Err(EnduranceQualificationError::InvalidLimits);
    }
    let mut ids = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let sample_capacity = u128::from(profile.maximum_samples_per_chunk)
        * u128::from(profile.maximum_chunks_per_phase);
    for phase in &profile.phases {
        validate_identity("phase_id", &phase.phase_id)?;
        validate_sha256("phase_workload_sha256", &phase.workload_sha256)?;
        validate_identity("phase_producer_owner", &phase.producer_owner)?;
        validate_identity("phase_producer_verifier_id", &phase.producer_verifier_id)?;
        if !ids.insert(phase.phase_id.clone()) {
            return Err(EnduranceQualificationError::DuplicatePhase {
                phase_id: phase.phase_id.clone(),
            });
        }
        if !kinds.insert(phase.kind) {
            return Err(EnduranceQualificationError::DuplicatePhaseKind);
        }
        let duration_samples = u128::from(phase.minimum_duration_us)
            .div_ceil(u128::from(profile.maximum_sample_gap_us))
            .saturating_add(1);
        if phase.minimum_duration_us == 0
            || phase.warmup_duration_us >= phase.minimum_duration_us
            || phase.minimum_samples < 2
            || phase.producer_report_schema_version == 0
            || phase.memory.backend.scope() != ProcessMemoryScope::ProductProcessTree
            || phase.memory.backend.private_memory_metric() != phase.memory.metric
            || phase.memory.maximum_private_memory_bytes == 0
            || sample_capacity < u128::from(phase.minimum_samples)
            || sample_capacity < duration_samples
        {
            return Err(EnduranceQualificationError::InvalidPhase {
                phase_id: phase.phase_id.clone(),
            });
        }
        validate_phase_closure(phase)?;
    }
    for required in [
        EndurancePhaseKind::PlaybackReference,
        EndurancePhaseKind::ContinuousExport,
        EndurancePhaseKind::ConcurrentRecovery,
    ] {
        if !kinds.contains(&required) {
            return Err(EnduranceQualificationError::IncompletePhaseCoverage);
        }
    }
    Ok(())
}

fn validate_phase_closure(
    phase: &EndurancePhaseRequirement,
) -> Result<(), EnduranceQualificationError> {
    let gate = &phase.counters;
    let valid = match phase.kind {
        EndurancePhaseKind::PlaybackReference => {
            gate.minimum_playback_presented_frames > 0
                && gate.minimum_reference_completed_frames > 0
                && gate.minimum_reference_hardware_timestamps > 0
                && gate.require_hardware_reference_output
                && gate.require_external_reference_lock
                && phase.maximum_playback_progress_gap_us > 0
                && phase.maximum_reference_progress_gap_us > 0
                && phase.maximum_export_progress_gap_us == 0
                && phase.maximum_recovery_progress_gap_us == 0
        }
        EndurancePhaseKind::ContinuousExport => {
            gate.minimum_verified_exports > 0
                && gate.minimum_export_frames > 0
                && phase.maximum_playback_progress_gap_us == 0
                && phase.maximum_reference_progress_gap_us == 0
                && phase.maximum_export_progress_gap_us > 0
                && phase.maximum_recovery_progress_gap_us == 0
        }
        EndurancePhaseKind::ConcurrentRecovery => {
            gate.minimum_playback_presented_frames > 0
                && gate.minimum_reference_completed_frames > 0
                && gate.minimum_verified_exports > 0
                && gate.minimum_recovery_cycles > 0
                && gate.require_hardware_reference_output
                && gate.require_external_reference_lock
                && phase.maximum_playback_progress_gap_us > 0
                && phase.maximum_reference_progress_gap_us > 0
                && phase.maximum_export_progress_gap_us > 0
                && phase.maximum_recovery_progress_gap_us > 0
        }
    };
    if !valid || !gate.require_quiescent_terminal || !gate.require_worker_shutdown {
        return Err(EnduranceQualificationError::InvalidPhase { phase_id: phase.phase_id.clone() });
    }
    Ok(())
}

fn validate_run_header(
    run: &EnduranceRunManifest,
    profile_sha256: &str,
) -> Result<(), EnduranceQualificationError> {
    if run.schema_version != RUN_SCHEMA_VERSION {
        return Err(EnduranceQualificationError::UnsupportedRunSchema {
            actual: run.schema_version,
        });
    }
    validate_identity("run_id", &run.run_id)?;
    validate_identity("release_candidate_id", &run.release_candidate_id)?;
    validate_source_revision(&run.source_revision)?;
    for (field, digest) in [
        ("profile_sha256", &run.profile_sha256),
        ("product_artifact_sha256", &run.product_artifact_sha256),
        ("runtime_image_sha256", &run.runtime_image_sha256),
        ("build_provenance_sha256", &run.build_provenance_sha256),
        ("machine_report_sha256", &run.machine_report_sha256),
        ("platform_cell_sha256", &run.platform_cell_sha256),
        ("capture_authority_sha256", &run.capture_authority_sha256),
        ("environment_before_sha256", &run.environment_before_sha256),
        ("environment_after_sha256", &run.environment_after_sha256),
    ] {
        validate_sha256(field, digest)?;
    }
    if run.profile_sha256 != profile_sha256 {
        return Err(EnduranceQualificationError::ProfileMismatch);
    }
    if run.environment_before_sha256 != run.environment_after_sha256 {
        return Err(EnduranceQualificationError::EnvironmentDrift);
    }
    Ok(())
}

fn validate_phase_manifest(
    requirement: &EndurancePhaseRequirement,
    phase: &EndurancePhaseManifest,
    profile: &EnduranceQualificationProfile,
) -> Result<(), EnduranceQualificationError> {
    validate_identity("phase_id", &phase.phase_id)?;
    validate_sha256("workload_sha256", &phase.workload_sha256)?;
    validate_identity("producer_owner", &phase.producer.owner)?;
    validate_identity("producer_verifier_id", &phase.producer.verifier_id)?;
    validate_sha256("producer_report_sha256", &phase.producer.report_sha256)?;
    validate_chunk_file_name(&phase.producer.report_file_name)?;
    validate_sha256(
        "producer_raw_evidence_sha256",
        &phase.producer.raw_evidence_sha256,
    )?;
    validate_chunk_file_name(&phase.producer.raw_evidence_file_name)?;
    if phase.producer.report_file_name == phase.producer.raw_evidence_file_name {
        return Err(EnduranceQualificationError::DuplicateEvidenceFileName {
            file_name: phase.producer.report_file_name.clone(),
        });
    }
    if phase.producer.report_schema_version == 0
        || phase.completed_at_run_us < phase.started_at_run_us
        || phase.chunks.len() > usize::from(profile.maximum_chunks_per_phase)
    {
        return Err(EnduranceQualificationError::InvalidPhase { phase_id: phase.phase_id.clone() });
    }
    if phase.workload_sha256 != requirement.workload_sha256
        || phase.producer.owner != requirement.producer_owner
        || phase.producer.verifier_id != requirement.producer_verifier_id
        || phase.producer.report_schema_version != requirement.producer_report_schema_version
    {
        return Err(EnduranceQualificationError::PhaseContractMismatch {
            phase_id: phase.phase_id.clone(),
        });
    }
    Ok(())
}

fn validate_chunk(
    phase: &EndurancePhaseManifest,
    receipt: &EndurancePhaseChunkReceipt,
    chunk: &EnduranceSampleChunk,
    expected_previous_sha256: Option<&str>,
    profile: &EnduranceQualificationProfile,
) -> Result<(), EnduranceQualificationError> {
    if chunk.schema_version != CHUNK_SCHEMA_VERSION
        || chunk.phase_id != phase.phase_id
        || chunk.chunk_index != receipt.chunk_index
        || chunk.previous_chunk_sha256.as_deref() != expected_previous_sha256
        || chunk.samples.is_empty()
        || chunk.samples.len() > usize::from(profile.maximum_samples_per_chunk)
        || usize::from(receipt.sample_count) != chunk.samples.len()
        || chunk.samples.first().map(|sample| sample.sequence) != Some(receipt.first_sequence)
        || chunk.samples.last().map(|sample| sample.sequence) != Some(receipt.last_sequence)
        || chunk.chunk_sha256 != receipt.chunk_sha256
        || !chunk.verify_evidence()
    {
        return Err(EnduranceQualificationError::ChunkMismatch {
            phase_id: phase.phase_id.clone(),
        });
    }
    Ok(())
}

fn validate_sample(
    requirement: &EndurancePhaseRequirement,
    sample: &EnduranceSample,
) -> Result<(), EnduranceQualificationError> {
    if sample.scheduled_at_us > sample.started_at_us
        || sample.started_at_us > sample.completed_at_us
        || sample.completed_at_us - sample.scheduled_at_us == 0
    {
        return Err(EnduranceQualificationError::InvalidSampleTime {
            phase_id: requirement.phase_id.clone(),
        });
    }
    let memory = &sample.process_memory;
    if memory.scope != ProcessMemoryScope::ProductProcessTree
        || memory.backend != requirement.memory.backend
        || memory.metric != requirement.memory.metric
        || memory.backend.private_memory_metric() != memory.metric
        || !memory.inventory_complete
        || memory.observed_process_count == 0
        || memory.inventory_attempts == 0
    {
        return Err(EnduranceQualificationError::IncompleteProcessTreeMemory {
            phase_id: requirement.phase_id.clone(),
        });
    }
    let accounted_reference = sample
        .counters
        .reference_completed_frames
        .checked_add(sample.counters.reference_late_frames)
        .and_then(|value| value.checked_add(sample.counters.reference_dropped_frames))
        .and_then(|value| value.checked_add(sample.counters.reference_flushed_frames))
        .and_then(|value| value.checked_add(sample.counters.reference_aborted_frames))
        .and_then(|value| value.checked_add(sample.gauges.reference_outstanding_frames))
        .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
    if sample.counters.reference_scheduled_frames != accounted_reference {
        return Err(EnduranceQualificationError::ReferenceAccounting {
            phase_id: requirement.phase_id.clone(),
        });
    }
    let accounted_exports = sample
        .counters
        .export_completions
        .checked_add(sample.counters.export_failures)
        .and_then(|value| value.checked_add(sample.counters.export_cancellations))
        .and_then(|value| value.checked_add(sample.gauges.export_pending_jobs))
        .and_then(|value| value.checked_add(sample.gauges.export_active_jobs))
        .ok_or(EnduranceQualificationError::ArithmeticOverflow)?;
    if sample.counters.export_admissions != accounted_exports {
        return Err(EnduranceQualificationError::ExportAccounting {
            phase_id: requirement.phase_id.clone(),
        });
    }
    if sample.counters.export_completions != sample.counters.export_durable_artifacts {
        return Err(EnduranceQualificationError::ExportArtifactAccounting {
            phase_id: requirement.phase_id.clone(),
        });
    }
    Ok(())
}

fn validate_chunk_file_name(value: &str) -> Result<(), EnduranceQualificationError> {
    if value.is_empty()
        || value.len() > 128
        || value == "."
        || value == ".."
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(EnduranceQualificationError::InvalidChunkFileName);
    }
    Ok(())
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), EnduranceQualificationError> {
    if value.trim().is_empty() || value.len() > 160 || value.contains("placeholder") {
        return Err(EnduranceQualificationError::InvalidIdentity { field });
    }
    Ok(())
}

fn validate_sha256(field: &'static str, value: &str) -> Result<(), EnduranceQualificationError> {
    if value.len() != 64
        || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(EnduranceQualificationError::InvalidSha256 { field });
    }
    Ok(())
}

fn validate_source_revision(value: &str) -> Result<(), EnduranceQualificationError> {
    if value.len() != 40
        || !value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(EnduranceQualificationError::InvalidSourceRevision);
    }
    Ok(())
}

fn chunk_digest(chunk: &EnduranceSampleChunk) -> Result<String, EnduranceQualificationError> {
    let mut canonical = chunk.clone();
    canonical.chunk_sha256.clear();
    digest_serializable(&canonical)
}

fn report_digest(
    report: &EnduranceQualificationReport,
) -> Result<String, EnduranceQualificationError> {
    let mut canonical = report.clone();
    canonical.evidence_sha256.clear();
    digest_serializable(&canonical)
}

fn digest_serializable<T: Serialize>(value: &T) -> Result<String, EnduranceQualificationError> {
    let bytes = serde_json::to_vec(value).map_err(EnduranceQualificationError::Serialization)?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Structural failure while compiling or replaying endurance evidence.
#[derive(Debug, Error)]
pub enum EnduranceQualificationError {
    /// Unsupported profile schema.
    #[error("unsupported endurance profile schema {actual}; expected 1")]
    UnsupportedProfileSchema { actual: u32 },
    /// Unsupported run schema.
    #[error("unsupported endurance run schema {actual}; expected 1")]
    UnsupportedRunSchema { actual: u32 },
    /// Empty or placeholder identity.
    #[error("invalid endurance identity field '{field}'")]
    InvalidIdentity { field: &'static str },
    /// Invalid lowercase SHA-256.
    #[error("endurance field '{field}' must be a lowercase SHA-256")]
    InvalidSha256 { field: &'static str },
    /// Invalid lowercase Git revision.
    #[error("endurance source revision must be a lowercase 40-hex Git SHA")]
    InvalidSourceRevision,
    /// Invalid profile resource limits.
    #[error("endurance profile resource limits are invalid")]
    InvalidLimits,
    /// Invalid phase policy or manifest.
    #[error("invalid endurance phase '{phase_id}'")]
    InvalidPhase { phase_id: String },
    /// Phase workload or producer contract differed from the compiled profile.
    #[error("endurance phase '{phase_id}' differs from its compiled workload contract")]
    PhaseContractMismatch { phase_id: String },
    /// Duplicate phase identity.
    #[error("duplicate endurance phase '{phase_id}'")]
    DuplicatePhase { phase_id: String },
    /// Duplicate phase kind.
    #[error("endurance profile contains a duplicate phase kind")]
    DuplicatePhaseKind,
    /// Missing required workload family.
    #[error("endurance profile must cover playback/reference, export, and concurrent recovery")]
    IncompletePhaseCoverage,
    /// Manifest contains an undeclared phase.
    #[error("unexpected endurance phase '{phase_id}'")]
    UnexpectedPhase { phase_id: String },
    /// Serial phases overlap.
    #[error("endurance phase '{phase_id}' overlaps its predecessor")]
    PhaseOverlap { phase_id: String },
    /// Phase-local monotonic timestamps did not close against the run clock.
    #[error("endurance phase '{phase_id}' clock closure does not match its final sample")]
    PhaseClockMismatch { phase_id: String },
    /// Profile digest mismatch.
    #[error("endurance run profile digest does not match")]
    ProfileMismatch,
    /// Environment changed across the serial run.
    #[error("endurance environment identity changed during the run")]
    EnvironmentDrift,
    /// Chunk count exceeded the compiled bound.
    #[error("endurance chunk count exceeded the compiled bound")]
    ChunkLimitExceeded,
    /// Chunk sample count exceeded the compiled bound.
    #[error("endurance chunk sample count exceeded the compiled bound")]
    ChunkSampleLimitExceeded,
    /// Chunk had no samples.
    #[error("endurance sample chunk is empty")]
    EmptyChunk,
    /// Chunk file name was not a link-free leaf name.
    #[error("endurance chunk file name is invalid")]
    InvalidChunkFileName,
    /// Two sealed evidence entries reused one leaf name.
    #[error("duplicate endurance evidence file name '{file_name}'")]
    DuplicateEvidenceFileName { file_name: String },
    /// Chunk order was not contiguous.
    #[error("endurance chunks are not contiguous for phase '{phase_id}'")]
    ChunkOrder { phase_id: String },
    /// Chunk bytes or receipt did not match.
    #[error("endurance chunk evidence mismatch for phase '{phase_id}'")]
    ChunkMismatch { phase_id: String },
    /// Phase had no samples despite claiming execution.
    #[error("executed endurance phase '{phase_id}' has no samples")]
    EmptyPhase { phase_id: String },
    /// Sample sequence or monotonic schedule was invalid.
    #[error("endurance samples are out of order for phase '{phase_id}'")]
    SampleOrder { phase_id: String },
    /// Sample schedule contained an excessive gap.
    #[error("endurance sample gap exceeded policy for phase '{phase_id}'")]
    SampleGap { phase_id: String },
    /// Native probe completion latency exceeded policy.
    #[error("endurance native probe latency exceeded policy for phase '{phase_id}'")]
    ProbeLatency { phase_id: String },
    /// Sample timestamps were incoherent.
    #[error("endurance sample timestamps are invalid for phase '{phase_id}'")]
    InvalidSampleTime { phase_id: String },
    /// Process-tree memory was incomplete or used the wrong metric.
    #[error("endurance process-tree memory evidence is incomplete for phase '{phase_id}'")]
    IncompleteProcessTreeMemory { phase_id: String },
    /// A cumulative counter decreased.
    #[error("endurance cumulative counter regressed for phase '{phase_id}'")]
    CounterRegression { phase_id: String },
    /// Reference Output accounting did not close.
    #[error("endurance Reference Output accounting does not close for phase '{phase_id}'")]
    ReferenceAccounting { phase_id: String },
    /// Export job accounting did not close.
    #[error("endurance Export accounting does not close for phase '{phase_id}'")]
    ExportAccounting { phase_id: String },
    /// Export durable-publication accounting did not close.
    #[error("endurance Export artifact accounting does not close for phase '{phase_id}'")]
    ExportArtifactAccounting { phase_id: String },
    /// Terminal counters/gauges differ from the final sample.
    #[error("endurance terminal evidence differs from the final sample for phase '{phase_id}'")]
    TerminalMismatch { phase_id: String },
    /// Checked integer arithmetic overflowed.
    #[error("endurance qualification arithmetic overflowed")]
    ArithmeticOverflow,
    /// Canonical JSON serialization failed.
    #[error("serialize endurance qualification evidence: {0}")]
    Serialization(serde_json::Error),
}
