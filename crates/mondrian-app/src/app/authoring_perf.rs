//! Repeatable large-project authoring performance matrix.
//!
//! The ordinary test suite validates the matrix contract and runs every
//! operation against deliberately small active-heavy and project-heavy
//! fixtures. Full 5/30/120 minute measurements are ignored by default and
//! emit JSONL evidence. Reference budgets are versioned observations, not
//! unconditional wall-clock gates.

use super::*;
use crate::app::execution_resource_coordination::MachineResourceProfile;
use crate::app::project_runtime::claim_project_runtime_lease_for_test;
use crate::app::ui_actions::TimelineInsertAssetPayload;
use mondrian_core::{
    automation::{Keyframe, PropertyMutation, PropertyValue},
    AuthoringAllocationId, ProjectId,
};
use mondrian_editor_state::{AuthoringHistoryBudget, AuthoringSession};
use mondrian_platform::{ProcessMemoryProbe, ProcessMemoryProbeResult, SystemPlatformService};
use mondrian_project::ProjectDocument;
use mondrian_timeline::{
    clip::Transform2D, Clip, InsertAutomationPolicy, InsertTimelineStatePolicy,
    InsertTransitionPolicy, RangeEditKind, SequenceCollection,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

const AUTHORING_PERF_SCHEMA_VERSION: u32 = 10;
const AUTHORING_EVIDENCE_PROTOCOL_SCHEMA_VERSION: u32 = 1;
const AUTHORING_EVIDENCE_REPORT_COUNT: usize = 6;
const AUTHORING_REFERENCE_BUDGET_VERSION: u32 = 4;
const AUTHORING_SEQUENCE_LOCALITY_BUDGET_VERSION: u32 = 3;
const AUTHORING_PROCESS_MEMORY_BUDGET_VERSION: u32 = 1;
const AUTHORING_PROCESS_MEMORY_SAMPLE_INTERVAL: Duration = Duration::from_millis(2);
const AUTHORING_PROCESS_MEMORY_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(100);
const AUTHORING_PROCESS_MEMORY_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(1);
const AUTHORING_PROCESS_MEMORY_BASELINE_SAMPLES: usize = 3;
const AUTHORING_PROCESS_MEMORY_ABSOLUTE_PRIVATE_COMMIT_LIMIT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const AUTHORING_PROCESS_MEMORY_EVIDENCE_SCOPE: &str =
    "native_process_private_commit_sampled_before_fixture_construction_through_all_authoring_operations_and_history_depth_probe_with_current_resident_and_process_lifetime_peak_resident_values_diagnostic_only";
const AUTHORING_MEMORY_EVIDENCE_SCOPE: &str =
    "canonical_project_json_size_and_versioned_conservative_history_logical_author_payload_charge_from_post_operation_state_only_excluding_history_footprint_descriptor_allocations_history_footprint_index_allocations_history_descriptor_cache_allocations_history_stack_container_spare_capacity_transient_allocations_allocator_bytes_and_process_rss";
const AUTHORING_TIMING_EVIDENCE_SCOPE: &str =
    "declared_small_sample_plan_nearest_rank_percentiles_not_population_or_release_statistical_p95";
const AUTHORING_SEQUENCE_LOCALITY_COMPARISON_SCOPE: &str =
    "same_scale_serialized_size_and_workload_shape_equivalent_active_sequence_with_project_heavy_adding_only_unrelated_sequences_and_at_least_four_times_the_active_sequence_serialized_bytes_as_unrelated_payload_for_main_sequence_local_operations_using_sample_median_for_relative_locality_and_candidate_declared_small_sample_nearest_rank_p95_required_to_pass_its_existing_reference_budget";
const AUTHORING_OPTIMIZATION_CONTRACT: &str =
    "eligible_iff_debug_assertions_are_disabled_and_build_script_attested_cargo_rustc_opt_level_is_a_decimal_integer_at_least_2";
const AUTHORING_STRUCTURED_STATE_COMPARISON_SCOPE: &str =
    "complete_project_document_structural_equality_with_only_monotonic_sequence_revision_normalized_because_undo_and_redo_must_advance_execution_invalidation_revisions";
const AUTHORING_SOURCE_TREE_SCOPE: &str =
    "workspace_Cargo_toml_Cargo_lock_and_recursive_crates_files_with_rs_toml_lock_json_sql_proto_wgsl_glsl_hlsl_metal_extensions_excluding_symlinks_and_target_directories";
const AUTHORING_SOURCE_DIRTY_ATTESTATION: &str =
    "scm_cleanliness_not_claimed_current_source_tree_bytes_are_content_addressed_so_local_modifications_are_included_in_source_revision";
const FULL_HISTORY_DEPTH_PROBE_EDITS: usize = 200;
const LIGHT_HISTORY_DEPTH_PROBE_EDITS: usize = 4;
const HISTORY_DEPTH_DESCRIPTION: &str = "Authoring perf history depth probe";
const PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER: usize = 5;
const SEQUENCE_LOCALITY_MIN_UNRELATED_SERIALIZED_BYTES_MULTIPLIER: usize = 4;
const SEQUENCE_LOCALITY_RELATIVE_MEDIAN_MULTIPLIER: u64 = 2;
const SEQUENCE_LOCALITY_RELATIVE_MEDIAN_ALLOWANCE_US: u64 = 5_000;
const MEBIBYTE: u64 = 1024 * 1024;

#[derive(Debug, Default)]
struct SerializedSizeWriter {
    bytes: usize,
}

impl Write for SerializedSizeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.checked_add(buffer.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::FileTooLarge,
                "serialized JSON size overflowed",
            )
        })?;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_json_size(value: &(impl Serialize + ?Sized)) -> anyhow::Result<usize> {
    let mut writer = SerializedSizeWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuthoringProcessMemoryBudget {
    scale_name: &'static str,
    active_heavy_max_delta_bytes: u64,
    project_heavy_max_delta_bytes: u64,
}

const AUTHORING_PROCESS_MEMORY_BUDGETS: [AuthoringProcessMemoryBudget; 4] = [
    AuthoringProcessMemoryBudget {
        scale_name: "ci_light",
        active_heavy_max_delta_bytes: 256 * MEBIBYTE,
        project_heavy_max_delta_bytes: 384 * MEBIBYTE,
    },
    AuthoringProcessMemoryBudget {
        scale_name: "program_5m",
        active_heavy_max_delta_bytes: 512 * MEBIBYTE,
        project_heavy_max_delta_bytes: 768 * MEBIBYTE,
    },
    AuthoringProcessMemoryBudget {
        scale_name: "program_30m",
        active_heavy_max_delta_bytes: 1024 * MEBIBYTE,
        project_heavy_max_delta_bytes: 1536 * MEBIBYTE,
    },
    AuthoringProcessMemoryBudget {
        scale_name: "program_120m",
        active_heavy_max_delta_bytes: 2048 * MEBIBYTE,
        project_heavy_max_delta_bytes: 3072 * MEBIBYTE,
    },
];

const SEQUENCE_LOCALITY_OPERATIONS: [&str; 8] = [
    "sequence.move_clip",
    "sequence.keyframe",
    "sequence.split",
    "sequence.insert",
    "sequence.lift",
    "sequence.extract",
    "history.sequence_undo",
    "history.sequence_redo",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthoringFixtureProfile {
    ActiveHeavy,
    ProjectHeavy,
}

impl AuthoringFixtureProfile {
    const ALL: [Self; 2] = [Self::ActiveHeavy, Self::ProjectHeavy];

    const fn name(self) -> &'static str {
        match self {
            Self::ActiveHeavy => "active-heavy",
            Self::ProjectHeavy => "project-heavy",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct AuthoringScale {
    name: &'static str,
    duration_minutes: u32,
    video_tracks: usize,
    audio_tracks: usize,
    active_payload_clips: usize,
    active_effects: usize,
    active_keyframes: usize,
    nesting_depth: usize,
    project_heavy_unrelated_sequences: usize,
    interactive_history_samples: usize,
    structural_samples: usize,
}

const AUTHORING_SCALE_MATRIX: [AuthoringScale; 3] = [
    AuthoringScale {
        name: "program_5m",
        duration_minutes: 5,
        video_tracks: 4,
        audio_tracks: 4,
        active_payload_clips: 256,
        active_effects: 128,
        active_keyframes: 512,
        nesting_depth: 2,
        project_heavy_unrelated_sequences: 8,
        interactive_history_samples: 5,
        structural_samples: 3,
    },
    AuthoringScale {
        name: "program_30m",
        duration_minutes: 30,
        video_tracks: 8,
        audio_tracks: 8,
        active_payload_clips: 1_536,
        active_effects: 768,
        active_keyframes: 3_072,
        nesting_depth: 4,
        project_heavy_unrelated_sequences: 32,
        interactive_history_samples: 5,
        structural_samples: 3,
    },
    AuthoringScale {
        name: "program_120m",
        duration_minutes: 120,
        video_tracks: 16,
        audio_tracks: 16,
        active_payload_clips: 6_144,
        active_effects: 3_072,
        active_keyframes: 12_288,
        nesting_depth: 8,
        project_heavy_unrelated_sequences: 128,
        interactive_history_samples: 5,
        structural_samples: 3,
    },
];

const LIGHT_AUTHORING_SCALE: AuthoringScale = AuthoringScale {
    name: "ci_light",
    duration_minutes: 1,
    video_tracks: 2,
    audio_tracks: 2,
    active_payload_clips: 24,
    active_effects: 12,
    active_keyframes: 48,
    nesting_depth: 1,
    project_heavy_unrelated_sequences: 2,
    interactive_history_samples: 1,
    structural_samples: 1,
};

#[derive(Debug, Clone, Serialize)]
struct AuthoringFixtureEvidence {
    profile: AuthoringFixtureProfile,
    project_json_bytes: usize,
    active_sequence_json_bytes: usize,
    unrelated_project_json_bytes: usize,
    sequence_count: usize,
    primary_video_tracks: usize,
    primary_audio_tracks: usize,
    active_payload_clips: usize,
    unrelated_payload_clips: usize,
    total_payload_clips: usize,
    active_effects: usize,
    unrelated_effects: usize,
    total_effects: usize,
    active_keyframes: usize,
    unrelated_keyframes: usize,
    total_keyframes: usize,
    nesting_depth: usize,
    proxy_mode_asset_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthoringOperationClass {
    Interactive,
    Structural,
    ProjectStructural,
    ProjectSetting,
    History,
    SnapshotCapture,
    SnapshotPublish,
}

#[derive(Debug, Clone, Copy)]
struct AuthoringSamplePlan {
    warmup_iterations: usize,
    measured_iterations: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum AuthoringHistoryCommandScope {
    Sequence,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryDepthProbeKind {
    LightweightSequenceMetadata,
    ClipContainerCowDetach,
}

impl HistoryDepthProbeKind {
    const fn for_index(index: usize) -> Self {
        if index.is_multiple_of(2) {
            Self::LightweightSequenceMetadata
        } else {
            Self::ClipContainerCowDetach
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuthoringBuildMachineAttestation {
    target_os: &'static str,
    target_arch: &'static str,
    target_pointer_width_bits: usize,
    debug_assertions_enabled: bool,
    timing_gate_optimized_build_eligible: bool,
    optimization_contract: &'static str,
    timing_gate_eligibility_reason: &'static str,
    cargo_profile: Option<&'static str>,
    cargo_rustc_opt_level: Option<&'static str>,
    parsed_cargo_rustc_opt_level: Option<u8>,
    logical_cpu_count: usize,
    installed_physical_memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringMemoryEvidence {
    measurement_scope: &'static str,
    observations: usize,
    peak_document_json_bytes_after_operation: usize,
    peak_history_logical_retained_charge_bytes_after_operation: usize,
    peak_history_entries_after_operation: usize,
    peak_amortized_history_logical_charge_per_entry_after_operation: usize,
    history_budget_max_entries: usize,
    history_budget_max_logical_retained_charge_bytes: usize,
    every_observation_within_history_budget: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringProcessMemoryEvidence {
    measurement_scope: &'static str,
    budget_version: u32,
    sample_interval_us: u64,
    gate_required: bool,
    discovery_available: bool,
    backend: Option<String>,
    attempted_samples: u64,
    observed_private_samples: u64,
    baseline_private_samples: u64,
    background_private_samples: u64,
    final_private_sample_observed: bool,
    background_sampling_started: bool,
    background_sampling_stopped: bool,
    probe_errors: u64,
    last_probe_error: Option<String>,
    baseline_private_committed_bytes: Option<u64>,
    final_private_committed_bytes: Option<u64>,
    peak_private_committed_bytes: Option<u64>,
    peak_private_committed_delta_bytes: Option<u64>,
    peak_observed_resident_bytes: Option<u64>,
    os_process_lifetime_peak_resident_bytes: Option<u64>,
    max_peak_private_committed_delta_bytes: u64,
    max_peak_private_committed_bytes: u64,
    evidence_complete: bool,
    within_delta_budget: bool,
    within_absolute_budget: bool,
    within_budget: bool,
    passed: bool,
}

#[derive(Debug, Clone, Default)]
struct AuthoringProcessMemoryAccumulator {
    discovery_available: bool,
    backend: Option<String>,
    attempted_samples: u64,
    observed_private_samples: u64,
    baseline_private_samples: u64,
    background_private_samples: u64,
    final_private_sample_observed: bool,
    background_sampling_started: bool,
    background_sampling_stopped: bool,
    backend_consistent: bool,
    probe_errors: u64,
    last_probe_error: Option<String>,
    baseline_private_committed_bytes: Option<u64>,
    final_private_committed_bytes: Option<u64>,
    peak_private_committed_bytes: Option<u64>,
    peak_observed_resident_bytes: Option<u64>,
    os_process_lifetime_peak_resident_bytes: Option<u64>,
}

struct AuthoringProcessMemorySampler {
    stop: Arc<AtomicBool>,
    accumulator: Arc<Mutex<AuthoringProcessMemoryAccumulator>>,
    worker: Option<JoinHandle<()>>,
    scale: AuthoringScale,
    profile: AuthoringFixtureProfile,
    gate_required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthoringProcessMemorySamplePhase {
    Baseline,
    Background,
    Final,
}

impl AuthoringProcessMemoryAccumulator {
    fn new() -> Self {
        Self { backend_consistent: true, ..Self::default() }
    }

    fn observe(
        &mut self,
        sample: ProcessMemoryProbeResult,
        phase: AuthoringProcessMemorySamplePhase,
    ) {
        self.attempted_samples = self.attempted_samples.saturating_add(1);
        self.discovery_available |= sample.discovery_available;
        let sample_reported_error = sample.error.is_some();

        if let Some(backend) = sample.backend {
            let backend = backend.as_str();
            match self.backend.clone() {
                None => self.backend = Some(backend.to_owned()),
                Some(existing) if existing != backend => {
                    self.backend_consistent = false;
                    self.record_probe_error(format!(
                        "process-memory backend changed from {existing} to {backend}"
                    ));
                }
                Some(_) => {}
            }
        } else if sample.discovery_available {
            self.record_probe_error(
                "process-memory discovery was available but the sample had no backend",
            );
        }

        if let Some(error) = sample.error {
            self.record_probe_error(error);
        }

        if let Some(private_committed_bytes) = sample.private_committed_bytes {
            self.observed_private_samples = self.observed_private_samples.saturating_add(1);
            self.peak_private_committed_bytes = Some(
                self.peak_private_committed_bytes
                    .unwrap_or(private_committed_bytes)
                    .max(private_committed_bytes),
            );
            match phase {
                AuthoringProcessMemorySamplePhase::Baseline => {
                    self.baseline_private_samples = self.baseline_private_samples.saturating_add(1);
                    self.baseline_private_committed_bytes = Some(
                        self.baseline_private_committed_bytes
                            .unwrap_or(private_committed_bytes)
                            .min(private_committed_bytes),
                    );
                }
                AuthoringProcessMemorySamplePhase::Background => {
                    self.background_private_samples =
                        self.background_private_samples.saturating_add(1);
                }
                AuthoringProcessMemorySamplePhase::Final => {
                    self.final_private_committed_bytes = Some(private_committed_bytes);
                    self.final_private_sample_observed = true;
                }
            }
        } else if sample.discovery_available && !sample_reported_error {
            self.record_probe_error(
                "process-memory sample did not include private committed bytes",
            );
        }

        if let Some(resident_bytes) = sample.resident_bytes {
            self.peak_observed_resident_bytes = Some(
                self.peak_observed_resident_bytes.unwrap_or(resident_bytes).max(resident_bytes),
            );
        }
        if let Some(peak_resident_bytes) = sample.peak_resident_bytes {
            self.os_process_lifetime_peak_resident_bytes = Some(
                self.os_process_lifetime_peak_resident_bytes
                    .unwrap_or(peak_resident_bytes)
                    .max(peak_resident_bytes),
            );
        }
    }

    fn record_probe_error(&mut self, error: impl Into<String>) {
        self.probe_errors = self.probe_errors.saturating_add(1);
        self.last_probe_error = Some(error.into());
    }

    fn evidence(
        &self,
        scale: AuthoringScale,
        profile: AuthoringFixtureProfile,
        gate_required: bool,
    ) -> AuthoringProcessMemoryEvidence {
        let max_peak_private_committed_delta_bytes =
            authoring_process_memory_budget_bytes(scale, profile).unwrap_or(0);
        let peak_private_committed_delta_bytes = self
            .baseline_private_committed_bytes
            .zip(self.peak_private_committed_bytes)
            .map(|(baseline, peak)| peak.saturating_sub(baseline));
        let required_baseline_samples =
            u64::try_from(AUTHORING_PROCESS_MEMORY_BASELINE_SAMPLES).unwrap_or(u64::MAX);
        let evidence_complete = authoring_process_memory_budget_bytes(scale, profile).is_some()
            && self.discovery_available
            && self.backend.is_some()
            && self.backend_consistent
            && self.probe_errors == 0
            && self.baseline_private_samples >= required_baseline_samples
            && self.baseline_private_committed_bytes.is_some()
            && self.background_private_samples >= 1
            && self.final_private_sample_observed
            && self.final_private_committed_bytes.is_some()
            && self.peak_private_committed_bytes.is_some()
            && self.background_sampling_started
            && self.background_sampling_stopped;
        let within_delta_budget = evidence_complete
            && peak_private_committed_delta_bytes
                .is_some_and(|delta| delta <= max_peak_private_committed_delta_bytes);
        let within_absolute_budget = evidence_complete
            && self.peak_private_committed_bytes.is_some_and(|peak| {
                peak <= AUTHORING_PROCESS_MEMORY_ABSOLUTE_PRIVATE_COMMIT_LIMIT_BYTES
            });
        let within_budget = within_delta_budget && within_absolute_budget;
        AuthoringProcessMemoryEvidence {
            measurement_scope: AUTHORING_PROCESS_MEMORY_EVIDENCE_SCOPE,
            budget_version: AUTHORING_PROCESS_MEMORY_BUDGET_VERSION,
            sample_interval_us: u64::try_from(AUTHORING_PROCESS_MEMORY_SAMPLE_INTERVAL.as_micros())
                .unwrap_or(u64::MAX),
            gate_required,
            discovery_available: self.discovery_available,
            backend: self.backend.clone(),
            attempted_samples: self.attempted_samples,
            observed_private_samples: self.observed_private_samples,
            baseline_private_samples: self.baseline_private_samples,
            background_private_samples: self.background_private_samples,
            final_private_sample_observed: self.final_private_sample_observed,
            background_sampling_started: self.background_sampling_started,
            background_sampling_stopped: self.background_sampling_stopped,
            probe_errors: self.probe_errors,
            last_probe_error: self.last_probe_error.clone(),
            baseline_private_committed_bytes: self.baseline_private_committed_bytes,
            final_private_committed_bytes: self.final_private_committed_bytes,
            peak_private_committed_bytes: self.peak_private_committed_bytes,
            peak_private_committed_delta_bytes,
            peak_observed_resident_bytes: self.peak_observed_resident_bytes,
            os_process_lifetime_peak_resident_bytes: self.os_process_lifetime_peak_resident_bytes,
            max_peak_private_committed_delta_bytes,
            max_peak_private_committed_bytes:
                AUTHORING_PROCESS_MEMORY_ABSOLUTE_PRIVATE_COMMIT_LIMIT_BYTES,
            evidence_complete,
            within_delta_budget,
            within_absolute_budget,
            within_budget,
            passed: !gate_required || within_budget,
        }
    }
}

impl AuthoringProcessMemorySampler {
    fn start(
        scale: AuthoringScale,
        profile: AuthoringFixtureProfile,
        timing_gate_requested: bool,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let accumulator = Arc::new(Mutex::new(AuthoringProcessMemoryAccumulator::new()));
        let probe = SystemPlatformService;
        for _ in 0..AUTHORING_PROCESS_MEMORY_BASELINE_SAMPLES {
            observe_process_memory(
                &accumulator,
                probe.current_process_memory(),
                AuthoringProcessMemorySamplePhase::Baseline,
            );
            if !process_memory_discovery_available(&accumulator) {
                break;
            }
        }

        let worker = if process_memory_discovery_available(&accumulator) {
            let worker_stop = Arc::clone(&stop);
            let worker_accumulator = Arc::clone(&accumulator);
            let worker_name = format!("authoring-memory-{}-{}", scale.name, profile.name());
            match std::thread::Builder::new().name(worker_name).spawn(move || {
                let probe = SystemPlatformService;
                while !worker_stop.load(Ordering::Acquire) {
                    observe_process_memory(
                        &worker_accumulator,
                        probe.current_process_memory(),
                        AuthoringProcessMemorySamplePhase::Background,
                    );
                    std::thread::park_timeout(AUTHORING_PROCESS_MEMORY_SAMPLE_INTERVAL);
                }
            }) {
                Ok(worker) => {
                    with_process_memory_accumulator(&accumulator, |accumulator| {
                        accumulator.background_sampling_started = true;
                    });
                    Some(worker)
                }
                Err(error) => {
                    with_process_memory_accumulator(&accumulator, |accumulator| {
                        accumulator.background_sampling_stopped = true;
                        accumulator.record_probe_error(format!(
                            "failed to start process-memory sampler: {error}"
                        ));
                    });
                    None
                }
            }
        } else {
            with_process_memory_accumulator(&accumulator, |accumulator| {
                accumulator.background_sampling_stopped = true;
            });
            None
        };

        Self {
            stop,
            accumulator,
            worker,
            scale,
            profile,
            gate_required: timing_gate_requested && cfg!(target_os = "windows"),
        }
    }

    fn finish(mut self) -> AuthoringProcessMemoryEvidence {
        self.stop_worker();
        observe_process_memory(
            &self.accumulator,
            SystemPlatformService.current_process_memory(),
            AuthoringProcessMemorySamplePhase::Final,
        );
        let accumulator =
            with_process_memory_accumulator(&self.accumulator, |accumulator| accumulator.clone());
        accumulator.evidence(self.scale, self.profile, self.gate_required)
    }

    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return;
        };
        worker.thread().unpark();
        let deadline = Instant::now() + AUTHORING_PROCESS_MEMORY_SHUTDOWN_TIMEOUT;
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(AUTHORING_PROCESS_MEMORY_SHUTDOWN_POLL_INTERVAL);
        }
        if worker.is_finished() {
            let join_result = worker.join();
            with_process_memory_accumulator(&self.accumulator, |accumulator| {
                accumulator.background_sampling_stopped = true;
                if join_result.is_err() {
                    accumulator.record_probe_error("process-memory sampler panicked");
                }
            });
        } else {
            with_process_memory_accumulator(&self.accumulator, |accumulator| {
                accumulator.record_probe_error(format!(
                    "process-memory sampler did not stop within {} ms and was detached",
                    AUTHORING_PROCESS_MEMORY_SHUTDOWN_TIMEOUT.as_millis()
                ));
            });
            drop(worker);
        }
    }
}

impl Drop for AuthoringProcessMemorySampler {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

fn observe_process_memory(
    accumulator: &Arc<Mutex<AuthoringProcessMemoryAccumulator>>,
    sample: ProcessMemoryProbeResult,
    phase: AuthoringProcessMemorySamplePhase,
) {
    with_process_memory_accumulator(accumulator, |accumulator| {
        accumulator.observe(sample, phase);
    });
}

fn process_memory_discovery_available(
    accumulator: &Arc<Mutex<AuthoringProcessMemoryAccumulator>>,
) -> bool {
    with_process_memory_accumulator(accumulator, |accumulator| accumulator.discovery_available)
}

fn with_process_memory_accumulator<T>(
    accumulator: &Arc<Mutex<AuthoringProcessMemoryAccumulator>>,
    operation: impl FnOnce(&mut AuthoringProcessMemoryAccumulator) -> T,
) -> T {
    let mut accumulator = accumulator.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    operation(&mut accumulator)
}

fn authoring_process_memory_budget_bytes(
    scale: AuthoringScale,
    profile: AuthoringFixtureProfile,
) -> Option<u64> {
    let budget = AUTHORING_PROCESS_MEMORY_BUDGETS
        .iter()
        .find(|budget| budget.scale_name == scale.name)?;
    Some(match profile {
        AuthoringFixtureProfile::ActiveHeavy => budget.active_heavy_max_delta_bytes,
        AuthoringFixtureProfile::ProjectHeavy => budget.project_heavy_max_delta_bytes,
    })
}

impl AuthoringMemoryEvidence {
    fn merge_peak(&mut self, observation: Self) {
        debug_assert_eq!(
            self.history_budget_max_entries,
            observation.history_budget_max_entries
        );
        debug_assert_eq!(
            self.history_budget_max_logical_retained_charge_bytes,
            observation.history_budget_max_logical_retained_charge_bytes
        );
        self.observations = self.observations.saturating_add(observation.observations);
        self.peak_document_json_bytes_after_operation = self
            .peak_document_json_bytes_after_operation
            .max(observation.peak_document_json_bytes_after_operation);
        self.peak_history_logical_retained_charge_bytes_after_operation = self
            .peak_history_logical_retained_charge_bytes_after_operation
            .max(observation.peak_history_logical_retained_charge_bytes_after_operation);
        self.peak_history_entries_after_operation = self
            .peak_history_entries_after_operation
            .max(observation.peak_history_entries_after_operation);
        self.peak_amortized_history_logical_charge_per_entry_after_operation = self
            .peak_amortized_history_logical_charge_per_entry_after_operation
            .max(observation.peak_amortized_history_logical_charge_per_entry_after_operation);
        self.every_observation_within_history_budget &=
            observation.every_observation_within_history_budget;
    }
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringOperationReport {
    operation: &'static str,
    operation_class: AuthoringOperationClass,
    history_command_scope: Option<AuthoringHistoryCommandScope>,
    reversibility: Option<AuthoringReversibilityEvidence>,
    warmup_iterations: usize,
    iterations: usize,
    samples_us: Vec<u64>,
    sample_median_us: u64,
    sample_nearest_rank_p95_us: u64,
    sample_max_us: u64,
    reference_sample_p95_budget_us: u64,
    within_reference_budget: bool,
    memory: AuthoringMemoryEvidence,
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringReversibilityEvidence {
    comparison_scope: &'static str,
    verified_iterations: usize,
    every_after_state_differed_from_before: bool,
    every_roundtrip_restored_before_state: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringHistoryDepthEvidence {
    requested_probe_edits: usize,
    committed_probe_edits: usize,
    verified_lightweight_sequence_metadata_probe_edits: usize,
    verified_clip_container_cow_detach_probe_edits: usize,
    undo_entries_before_probe: usize,
    redo_entries_before_probe: usize,
    undo_entries_after_probe_commits: usize,
    redo_entries_after_probe_undo: usize,
    probe_edits_undone: usize,
    history_logical_retained_charge_bytes_after_probe_commits: usize,
    history_budget_max_entries: usize,
    history_budget_max_logical_retained_charge_bytes: usize,
    budget_evicted_entries_during_probe: u64,
    budget_evicted_logical_charge_bytes_during_probe: u64,
    retention_disabled_entries_during_probe: u64,
    oversize_dropped_entries_during_probe: u64,
    branch_discarded_entries_during_probe: u64,
    barrier_discarded_entries_during_probe: u64,
    post_probe_commit_state_within_history_budget: bool,
    budget_evictions_confined_to_pre_probe_entries: bool,
    all_probe_edits_retained_and_undone: bool,
    complete_structured_project_document_restored_after_probe_undo: bool,
    structured_state_comparison_scope: &'static str,
    edit_samples_us: Vec<u64>,
    edit_sample_median_us: u64,
    edit_sample_nearest_rank_p95_us: u64,
    edit_sample_max_us: u64,
    undo_samples_us: Vec<u64>,
    undo_sample_median_us: u64,
    undo_sample_nearest_rank_p95_us: u64,
    undo_sample_max_us: u64,
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringSequenceLocalityOperationEvidence {
    operation: &'static str,
    active_heavy_sample_median_us: u64,
    project_heavy_sample_median_us: u64,
    active_heavy_sample_nearest_rank_p95_us: u64,
    project_heavy_sample_nearest_rank_p95_us: u64,
    relative_median_multiplier: u64,
    relative_median_allowance_us: u64,
    relative_median_budget_us: u64,
    project_heavy_reference_sample_p95_budget_us: u64,
    within_relative_median_budget: bool,
    within_project_heavy_reference_budget: bool,
    passed: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AuthoringSequenceLocalityEvidence {
    budget_version: u32,
    baseline_profile: AuthoringFixtureProfile,
    candidate_profile: AuthoringFixtureProfile,
    comparison_scope: &'static str,
    active_sequence_serialized_size_bytes: usize,
    active_sequence_serialized_size_equal: bool,
    active_sequence_workload_shape_equivalent: bool,
    candidate_additional_sequence_count: usize,
    minimum_unrelated_serialized_bytes_multiplier: usize,
    required_unrelated_project_json_bytes: usize,
    candidate_unrelated_project_json_bytes: usize,
    candidate_unrelated_serialized_bytes_multiplier_floor: usize,
    fixture_strength_satisfied: bool,
    operations: Vec<AuthoringSequenceLocalityOperationEvidence>,
    all_within_budget: bool,
}

#[derive(Debug, Serialize)]
struct AuthoringScaleReport {
    schema_version: u32,
    reference_budget_version: u32,
    scenario: &'static str,
    profile: AuthoringFixtureProfile,
    scale: AuthoringScale,
    fixture: AuthoringFixtureEvidence,
    cases: Vec<AuthoringOperationReport>,
    all_within_reference_budget: bool,
    timing_evidence_scope: &'static str,
    memory_evidence_scope: &'static str,
    all_history_payloads_within_budget: bool,
    process_memory: AuthoringProcessMemoryEvidence,
    timing_gate_requested: bool,
    build_machine_attestation: AuthoringBuildMachineAttestation,
    sequence_locality: Option<AuthoringSequenceLocalityEvidence>,
    history_depth: AuthoringHistoryDepthEvidence,
    history: mondrian_editor_state::AuthoringHistoryDiagnostics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AuthoringSourceTreeAttestation {
    source_revision: String,
    revision_kind: &'static str,
    source_tree_scope: &'static str,
    files_hashed: usize,
    bytes_hashed: u64,
    source_dirty: Option<bool>,
    dirty_attestation: &'static str,
}

#[derive(Serialize)]
struct AuthoringEvidenceReportRecord<'a, T> {
    record_type: &'static str,
    protocol_schema_version: u32,
    run_id: &'a str,
    ordinal: usize,
    expected_report_count: usize,
    source: &'a AuthoringSourceTreeAttestation,
    report: &'a T,
}

#[derive(Serialize)]
struct AuthoringEvidenceCompletionRecord<'a> {
    record_type: &'static str,
    protocol_schema_version: u32,
    run_id: &'a str,
    completed_report_count: usize,
    expected_report_count: usize,
    last_report_ordinal: usize,
    source: &'a AuthoringSourceTreeAttestation,
    source_unchanged_during_run: bool,
    started_unix_time_ns: u128,
    completed_unix_time_ns: u128,
}

struct AuthoringEvidenceRun {
    run_id: String,
    source: AuthoringSourceTreeAttestation,
    started_unix_time_ns: u128,
    emitted_reports: usize,
    writer: Option<FreshJsonlEvidenceWriter>,
}

#[derive(Debug, Clone, Copy)]
struct FixtureHandles {
    sequence_id: SequenceId,
    first_track_id: TrackId,
    first_clip_id: ClipId,
    first_clip_position_frame: i64,
    first_clip_duration_frames: i64,
    second_clip_id: ClipId,
    insert_asset_id: AssetId,
    proxy_toggle_asset_id: AssetId,
}

struct UnrelatedPayloadBuild {
    sequences: Vec<Sequence>,
    effects_written: usize,
    keyframes_written: usize,
}

fn partition_fixture_count(total: usize, index: usize, partitions: usize) -> usize {
    if partitions == 0 {
        return 0;
    }
    total / partitions + usize::from(index < total % partitions)
}

fn build_unrelated_payload_sequences(
    sequence_count: usize,
    payload_clips: usize,
    effect_target: usize,
    keyframe_target: usize,
    visual_assets: &[AssetId],
    offline_audio_asset: AssetId,
) -> anyhow::Result<UnrelatedPayloadBuild> {
    if sequence_count == 0 {
        if payload_clips != 0 || effect_target != 0 || keyframe_target != 0 {
            anyhow::bail!("active-heavy fixture cannot retain unrelated payload");
        }
        return Ok(UnrelatedPayloadBuild {
            sequences: Vec::new(),
            effects_written: 0,
            keyframes_written: 0,
        });
    }
    if visual_assets.is_empty() {
        anyhow::bail!("project-heavy fixture requires at least one visual Asset");
    }

    let effect_types = [
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::BasicCorrection,
        EffectType::Vignette,
        EffectType::ChromaKey,
        EffectType::LumaKey,
    ];
    let mut sequences = Vec::with_capacity(sequence_count);
    let mut effects_written = 0usize;
    let mut keyframes_written = 0usize;
    for sequence_index in 0..sequence_count {
        let local_clip_count =
            partition_fixture_count(payload_clips, sequence_index, sequence_count);
        let local_effect_target =
            partition_fixture_count(effect_target, sequence_index, sequence_count);
        let local_keyframe_target =
            partition_fixture_count(keyframe_target, sequence_index, sequence_count);
        let mut sequence = Sequence::new(format!("Unrelated {:04}", sequence_index + 1));
        let time_base = sequence.time_base();
        let video_clip_count = (local_clip_count.saturating_mul(3) / 4)
            .max(usize::from(local_clip_count > 0))
            .min(local_clip_count);
        let audio_clip_count = local_clip_count.saturating_sub(video_clip_count);
        if video_clip_count == 0 && (local_effect_target > 0 || local_keyframe_target > 0) {
            anyhow::bail!(
                "project-heavy payload partition {sequence_index} has effects or keyframes without a picture Clip"
            );
        }
        let effects_per_clip = local_effect_target / video_clip_count.max(1);
        let effects_remainder = local_effect_target % video_clip_count.max(1);
        let keys_per_clip = local_keyframe_target / video_clip_count.max(1);
        let keys_remainder = local_keyframe_target % video_clip_count.max(1);
        let clip_duration_frames = 12i64;
        let clip_slot_frames = 16i64;

        for clip_index in 0..video_clip_count {
            let position_frame = i64::try_from(clip_index)?.saturating_mul(clip_slot_frames);
            let mut clip = Clip::new_solid_color(
                visual_assets[(sequence_index + clip_index) % visual_assets.len()],
                Color::from_hex(
                    0x18324A
                        + (((sequence_index.saturating_mul(131) + clip_index) as u32 * 977)
                            & 0x003F3F),
                ),
                timeline_time_from_frame(position_frame, time_base)?,
                timeline_time_from_frame(clip_duration_frames, time_base)?,
            )?;
            clip.label = Some(format!(
                "Unrelated {:04}/{:05}",
                sequence_index + 1,
                clip_index + 1
            ));
            let local_effects = effects_per_clip + usize::from(clip_index < effects_remainder);
            for effect_index in 0..local_effects {
                let effect_type = effect_types
                    [(sequence_index + clip_index + effect_index) % effect_types.len()]
                .clone();
                clip.add_effect_node(EffectNode::with_defaults(effect_type));
                effects_written = effects_written.saturating_add(1);
            }
            let local_keys = keys_per_clip + usize::from(clip_index < keys_remainder);
            for key_index in 0..local_keys {
                let numerator = i64::try_from(key_index + 1)?;
                let denominator = i64::try_from(local_keys + 1)?;
                let local_frame = (clip_duration_frames.saturating_mul(numerator) / denominator)
                    .clamp(0, clip_duration_frames - 1);
                clip.apply_property_mutation(PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_owned(),
                    keyframe: Keyframe::linear(
                        timeline_time_from_frame(local_frame, time_base)?,
                        PropertyValue::Float(if key_index % 2 == 0 { 0.4 } else { 0.85 }),
                    ),
                })?;
                keyframes_written = keyframes_written.saturating_add(1);
            }
            sequence.video_tracks[0].add_clip(clip)?;
        }
        for clip_index in 0..audio_clip_count {
            let mut clip = Clip::new(
                offline_audio_asset,
                timeline_time_from_frame(
                    i64::try_from(clip_index)?.saturating_mul(clip_slot_frames),
                    time_base,
                )?,
                timeline_time_from_frame(clip_duration_frames, time_base)?,
            )?;
            clip.label = Some(format!(
                "Unrelated Audio {:04}/{:05}",
                sequence_index + 1,
                clip_index + 1
            ));
            let track_id = sequence.audio_tracks[0].id;
            sequence.add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())?;
        }
        sequences.push(sequence);
    }
    Ok(UnrelatedPayloadBuild { sequences, effects_written, keyframes_written })
}

fn build_authoring_fixture(
    root: &Path,
    scale: AuthoringScale,
    profile: AuthoringFixtureProfile,
) -> anyhow::Result<(AppState, FixtureHandles, AuthoringFixtureEvidence)> {
    let profile_tag = match profile {
        AuthoringFixtureProfile::ActiveHeavy => "a",
        AuthoringFixtureProfile::ProjectHeavy => "p",
    };
    // Keep the Windows test path short enough for SQLite's native VFS while
    // the runtime directory and durable-publication temp names add their own
    // collision-resistant identities.
    let project_file = root.join(format!("{profile_tag}.mdp"));
    let project_id = ProjectId::new();
    let runtime_lease =
        claim_project_runtime_lease_for_test(&root.join("r"), &project_file, project_id)
            .map_err(anyhow::Error::msg)?;
    let runtime_root = runtime_lease.runtime_root().to_path_buf();
    fs::create_dir_all(runtime_root.join("library"))?;
    let library = AssetLibrary::open(runtime_root.join("library"))?;
    let visual_asset_count = scale.video_tracks.clamp(1, 16);
    let mut visual_assets = Vec::with_capacity(visual_asset_count);
    for index in 0..visual_asset_count {
        visual_assets
            .push(library.create_solid_color_asset(Some(&format!("Authoring Plate {index:02}")))?);
    }
    let insert_asset_id = library.create_solid_color_asset(Some("Authoring Insert"))?;
    // Both profiles deliberately build the same active Sequence. ProjectHeavy
    // adds unrelated author payload below so paired timings isolate Project
    // locality instead of conflating it with a smaller active aggregate.
    let active_payload_clips = scale.active_payload_clips;
    let active_effect_target = scale.active_effects;
    let active_keyframe_target = scale.active_keyframes;

    let mut nested_sequences = (0..scale.nesting_depth)
        .map(|index| Sequence::new(format!("Nested {:02}", index + 1)))
        .collect::<Vec<_>>();
    for index in (0..nested_sequences.len()).rev() {
        let time_base = nested_sequences[index].time_base();
        let duration = timeline_time_from_frame(120, time_base)?;
        let clip = if let Some(next) = nested_sequences.get(index + 1) {
            Clip::new_nested_sequence(
                next.id,
                TimelineTime::ZERO,
                duration,
                Some(next.name.clone()),
            )?
        } else {
            Clip::new_solid_color(
                visual_assets[index % visual_assets.len()],
                Color::from_hex(0x26405C),
                TimelineTime::ZERO,
                duration,
            )?
        };
        nested_sequences[index].video_tracks[0].add_clip(clip)?;
    }

    let mut primary = Sequence::new(format!("{} authoring", scale.name));
    primary.video_tracks.clear();
    primary.audio_tracks.clear();
    for index in 0..scale.video_tracks {
        primary.video_tracks.push(Track::new_video(format!("V{}", index + 1)));
    }
    for index in 0..scale.audio_tracks {
        primary.audio_tracks.push(Track::new_audio(format!("A{}", index + 1)));
    }
    primary.audio_program = mondrian_timeline::AudioProgram::for_tracks(
        primary.audio_tracks.iter().map(|track| track.id),
    );

    let time_base = primary.time_base();
    let total_frames = i64::from(scale.duration_minutes)
        .checked_mul(60)
        .and_then(|seconds| seconds.checked_mul(25))
        .ok_or_else(|| anyhow::anyhow!("authoring fixture duration overflow"))?;
    let video_clip_count = active_payload_clips.saturating_mul(3) / 4;
    let audio_clip_count = active_payload_clips.saturating_sub(video_clip_count);
    let video_lanes = video_clip_count.div_ceil(scale.video_tracks.max(1));
    let audio_lanes = audio_clip_count.div_ceil(scale.audio_tracks.max(1));
    let video_slot = (total_frames / i64::try_from(video_lanes.max(1))?).max(16);
    let audio_slot = (total_frames / i64::try_from(audio_lanes.max(1))?).max(16);
    let video_duration = (video_slot.saturating_mul(3) / 4).max(8);
    let audio_duration = (audio_slot.saturating_mul(3) / 4).max(8);
    let effect_types = [
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::BasicCorrection,
        EffectType::Vignette,
        EffectType::ChromaKey,
        EffectType::LumaKey,
    ];

    let mut effects_written = 0usize;
    let mut keyframes_written = 0usize;
    let effects_per_clip = active_effect_target / video_clip_count.max(1);
    let effects_remainder = active_effect_target % video_clip_count.max(1);
    let keys_per_clip = active_keyframe_target / video_clip_count.max(1);
    let keys_remainder = active_keyframe_target % video_clip_count.max(1);
    let mut first_track_clips = Vec::new();

    for index in 0..video_clip_count {
        let track_index = index % scale.video_tracks;
        let lane = index / scale.video_tracks;
        let position_frame = i64::try_from(lane)?.saturating_mul(video_slot);
        let position = timeline_time_from_frame(position_frame, time_base)?;
        let duration = timeline_time_from_frame(video_duration, time_base)?;
        let mut clip = if index == 0 && !nested_sequences.is_empty() {
            let nested = &nested_sequences[0];
            Clip::new_nested_sequence(nested.id, position, duration, Some(nested.name.clone()))?
        } else {
            Clip::new_solid_color(
                visual_assets[index % visual_assets.len()],
                Color::from_hex(0x203C60 + ((index as u32 * 977) & 0x003F3F)),
                position,
                duration,
            )?
        };
        clip.label = Some(format!("Picture {index:05}"));

        let local_effects = effects_per_clip + usize::from(index < effects_remainder);
        for effect_index in 0..local_effects {
            let effect_type = effect_types[(index + effect_index) % effect_types.len()].clone();
            clip.add_effect_node(EffectNode::with_defaults(effect_type));
            effects_written += 1;
        }

        let local_keys = keys_per_clip + usize::from(index < keys_remainder);
        for key_index in 0..local_keys {
            let numerator = i64::try_from(key_index + 1)?;
            let denominator = i64::try_from(local_keys + 1)?;
            let local_frame = (video_duration.saturating_mul(numerator) / denominator)
                .clamp(0, video_duration - 1);
            clip.apply_property_mutation(PropertyMutation::SetKeyframe {
                path: Transform2D::OPACITY_PATH.to_owned(),
                keyframe: Keyframe::linear(
                    timeline_time_from_frame(local_frame, time_base)?,
                    PropertyValue::Float(if key_index % 2 == 0 { 0.35 } else { 0.9 }),
                ),
            })?;
            keyframes_written += 1;
        }

        if track_index == 0 && first_track_clips.len() < 2 {
            first_track_clips.push((clip.id, position_frame, video_duration));
        }
        primary.video_tracks[track_index].add_clip(clip)?;
    }

    let offline_audio_asset = AssetId::new();
    for index in 0..audio_clip_count {
        let track_index = index % scale.audio_tracks;
        let lane = index / scale.audio_tracks;
        let position_frame = i64::try_from(lane)?.saturating_mul(audio_slot);
        let mut clip = Clip::new(
            offline_audio_asset,
            timeline_time_from_frame(position_frame, time_base)?,
            timeline_time_from_frame(audio_duration, time_base)?,
        )?;
        clip.label = Some(format!("Offline Audio {index:05}"));
        let track_id = primary.audio_tracks[track_index].id;
        primary.add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())?;
    }

    let first = first_track_clips
        .first()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("authoring fixture has no primary picture Clip"))?;
    let second = first_track_clips
        .get(1)
        .copied()
        .ok_or_else(|| anyhow::anyhow!("authoring fixture needs two Clips on V1"))?;
    primary.mark_in(timeline_time_from_frame(first.1 + 1, time_base)?);
    primary.mark_out(timeline_time_from_frame(
        first.1 + (first.2 / 2).max(2),
        time_base,
    )?);
    primary.playhead = TimelineTime::ZERO;

    let primary_sequence_id = primary.id;
    let first_track_id = primary.video_tracks[0].id;
    let mut sequences = SequenceCollection::new(primary);
    for nested in nested_sequences.into_iter().rev() {
        sequences.add_sequence(nested)?;
    }
    let unrelated_payload_clips = match profile {
        AuthoringFixtureProfile::ActiveHeavy => 0,
        AuthoringFixtureProfile::ProjectHeavy => scale
            .active_payload_clips
            .checked_mul(PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER)
            .ok_or_else(|| anyhow::anyhow!("project-heavy Clip cardinality overflow"))?,
    };
    let unrelated_effect_target = match profile {
        AuthoringFixtureProfile::ActiveHeavy => 0,
        AuthoringFixtureProfile::ProjectHeavy => scale
            .active_effects
            .checked_mul(PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER)
            .ok_or_else(|| anyhow::anyhow!("project-heavy effect cardinality overflow"))?,
    };
    let unrelated_keyframe_target = match profile {
        AuthoringFixtureProfile::ActiveHeavy => 0,
        AuthoringFixtureProfile::ProjectHeavy => scale
            .active_keyframes
            .checked_mul(PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER)
            .ok_or_else(|| anyhow::anyhow!("project-heavy keyframe cardinality overflow"))?,
    };
    let unrelated_sequence_count = match profile {
        AuthoringFixtureProfile::ActiveHeavy => 0,
        AuthoringFixtureProfile::ProjectHeavy => scale.project_heavy_unrelated_sequences,
    };
    let unrelated = build_unrelated_payload_sequences(
        unrelated_sequence_count,
        unrelated_payload_clips,
        unrelated_effect_target,
        unrelated_keyframe_target,
        &visual_assets,
        offline_audio_asset,
    )?;
    let unrelated_project_json_bytes =
        unrelated.sequences.iter().try_fold(0usize, |total, sequence| {
            total
                .checked_add(serialized_json_size(sequence)?)
                .ok_or_else(|| anyhow::anyhow!("unrelated Project JSON size overflowed"))
        })?;
    effects_written = effects_written.saturating_add(unrelated.effects_written);
    keyframes_written = keyframes_written.saturating_add(unrelated.keyframes_written);
    for sequence in unrelated.sequences {
        sequences.add_sequence(sequence)?;
    }
    let expected_effects = active_effect_target.saturating_add(unrelated_effect_target);
    let expected_keyframes = active_keyframe_target.saturating_add(unrelated_keyframe_target);
    if effects_written != expected_effects || keyframes_written != expected_keyframes {
        anyhow::bail!(
            "authoring fixture cardinality mismatch: effects {effects_written}/{}, keyframes {keyframes_written}/{}",
            expected_effects,
            expected_keyframes
        );
    }
    let mut document = ProjectDocument::new(
        format!("{} Authoring Project", scale.name),
        sequences,
        mondrian_core::ProjectColorEnvironment::default(),
        SequenceSettings::default(),
        ProjectSettings::default(),
    );
    document.project_id = project_id;
    let proxy_mode_asset_count = scale.active_payload_clips.saturating_mul(4).max(64);
    document
        .proxy_mode_assets
        .extend((0..proxy_mode_asset_count).map(|_| AssetId::new()));
    let proxy_toggle_asset_id = insert_asset_id;
    let project_json_bytes = serialized_json_size(&document)?;
    let active_sequence_json_bytes = serialized_json_size(
        document
            .sequences
            .sequence(primary_sequence_id)
            .ok_or_else(|| anyhow::anyhow!("authoring fixture lost active Sequence"))?,
    )?;
    let evidence = AuthoringFixtureEvidence {
        profile,
        project_json_bytes,
        active_sequence_json_bytes,
        unrelated_project_json_bytes,
        sequence_count: document.sequences.sequences.len(),
        primary_video_tracks: scale.video_tracks,
        primary_audio_tracks: scale.audio_tracks,
        active_payload_clips,
        unrelated_payload_clips,
        total_payload_clips: active_payload_clips.saturating_add(unrelated_payload_clips),
        active_effects: active_effect_target,
        unrelated_effects: unrelated_effect_target,
        total_effects: effects_written,
        active_keyframes: active_keyframe_target,
        unrelated_keyframes: unrelated_keyframe_target,
        total_keyframes: keyframes_written,
        nesting_depth: scale.nesting_depth,
        proxy_mode_asset_count,
    };
    let session = AuthoringSession::new_unsaved(document, project_file, runtime_root, library)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut state = AppState::new();
    state.authoring = Some(session);
    state.project_runtime_lease = Some(runtime_lease);
    state.reconcile_timeline_targeting();

    Ok((
        state,
        FixtureHandles {
            sequence_id: primary_sequence_id,
            first_track_id,
            first_clip_id: first.0,
            first_clip_position_frame: first.1,
            first_clip_duration_frames: first.2,
            second_clip_id: second.0,
            insert_asset_id,
            proxy_toggle_asset_id,
        },
        evidence,
    ))
}

fn run_authoring_scale(
    root: &Path,
    scale: AuthoringScale,
    profile: AuthoringFixtureProfile,
    timing_gate_requested: bool,
) -> anyhow::Result<AuthoringScaleReport> {
    let process_memory_sampler =
        AuthoringProcessMemorySampler::start(scale, profile, timing_gate_requested);
    let (mut state, handles, fixture) = build_authoring_fixture(root, scale, profile)?;
    let mut cases = Vec::new();
    let selection = SelectedClipRef {
        track_id: handles.first_track_id,
        is_video_track: true,
        clip_id: handles.first_clip_id,
    };

    cases.push(measure_reversible(
        &mut state,
        scale,
        "sequence.move_clip",
        AuthoringOperationClass::Interactive,
        |state| {
            state.move_clip_in_track_with_mode(
                handles.first_track_id,
                true,
                handles.first_clip_id,
                handles.first_clip_position_frame + 1,
                ClipOverlapMode::Overwrite,
            )
        },
    )?);

    cases.push(measure_reversible(
        &mut state,
        scale,
        "sequence.keyframe",
        AuthoringOperationClass::Interactive,
        |state| {
            state
                .mutate_clip_property(
                    selection,
                    PropertyMutation::SetKeyframe {
                        path: Transform2D::OPACITY_PATH.to_owned(),
                        keyframe: Keyframe::linear(
                            timeline_time_from_frame(
                                handles.first_clip_duration_frames - 1,
                                state.active_sequence().expect("active Sequence").time_base(),
                            )?,
                            PropertyValue::Float(0.72),
                        ),
                    },
                    "Authoring perf keyframe",
                )
                .map(|_| ())
        },
    )?);

    cases.push(measure_reversible(
        &mut state,
        scale,
        "sequence.split",
        AuthoringOperationClass::Structural,
        |state| {
            state
                .split_clip_at_frame(
                    handles.first_track_id,
                    true,
                    handles.first_clip_id,
                    handles.first_clip_position_frame + handles.first_clip_duration_frames / 2,
                )
                .and_then(|outcome| {
                    outcome.map(|_| ()).ok_or_else(|| {
                        mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "authoring_perf_split".to_owned(),
                            reason: "fixture split produced no edit".to_owned(),
                        }
                    })
                })
        },
    )?);

    let ripple_tracks = state
        .active_sequence()
        .expect("active Sequence")
        .video_tracks
        .iter()
        .chain(&state.active_sequence().expect("active Sequence").audio_tracks)
        .map(|track| track.id)
        .collect::<Vec<_>>();
    cases.push(measure_reversible(
        &mut state,
        scale,
        "sequence.insert",
        AuthoringOperationClass::Structural,
        |state| {
            state
                .insert_asset_from_ui(TimelineInsertAssetPayload {
                    asset_id: handles.insert_asset_id,
                    insert_frame: handles.first_clip_position_frame
                        + handles.first_clip_duration_frames,
                    source_in_frame: 0,
                    duration_frames: 12,
                    video_target_track_id: Some(handles.first_track_id),
                    audio_target_track_id: None,
                    ripple_track_ids: ripple_tracks.clone(),
                    automation_policy: InsertAutomationPolicy::FollowEditorialContent,
                    transition_policy: InsertTransitionPolicy::RemoveAffected,
                    timeline_state_policy: InsertTimelineStatePolicy::PreserveSequenceTime,
                })
                .map(|_| ())
        },
    )?);

    cases.push(measure_reversible(
        &mut state,
        scale,
        "sequence.lift",
        AuthoringOperationClass::Structural,
        |state| state.apply_timeline_range_edit(RangeEditKind::Lift).map(|_| ()),
    )?);
    cases.push(measure_reversible(
        &mut state,
        scale,
        "sequence.extract",
        AuthoringOperationClass::Structural,
        |state| state.apply_timeline_range_edit(RangeEditKind::Extract).map(|_| ()),
    )?);

    let precompose_selection = vec![
        (handles.first_track_id, true, handles.first_clip_id),
        (handles.first_track_id, true, handles.second_clip_id),
    ];
    cases.push(measure_reversible(
        &mut state,
        scale,
        "project.precompose",
        AuthoringOperationClass::ProjectStructural,
        |state| {
            state
                .precompose_clips_as_sequence(&precompose_selection, "Authoring Perf Precompose")
                .map(|_| ())
        },
    )?);

    cases.push(measure_reversible(
        &mut state,
        scale,
        "project.setting",
        AuthoringOperationClass::ProjectSetting,
        |state| {
            let mut settings = state.new_sequence_defaults().clone();
            settings.resolution.width = settings.resolution.width.saturating_add(2);
            state.update_new_sequence_defaults(settings)
        },
    )?);

    cases.push(measure_reversible(
        &mut state,
        scale,
        "project.proxy_mode_toggle",
        AuthoringOperationClass::ProjectSetting,
        |state| {
            state.set_asset_proxy_mode(handles.proxy_toggle_asset_id, true);
            if state.is_asset_proxy_mode(handles.proxy_toggle_asset_id) {
                Ok(())
            } else {
                Err(mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "authoring_perf_proxy_mode_toggle".to_owned(),
                    reason: "proxy-mode authoring transaction did not commit".to_owned(),
                })
            }
        },
    )?);

    let sequence_history_id = handles.sequence_id;
    let sequence_name_before = state
        .active_sequence()
        .ok_or_else(|| anyhow::anyhow!("active Sequence unavailable for History seed"))?
        .name
        .clone();
    let sequence_name_after = format!("{sequence_name_before} edited");
    state.commit_active_sequence_edit("Authoring perf Sequence Undo/Redo seed", |sequence| {
        sequence.name.clone_from(&sequence_name_after);
        Ok(())
    })?;
    let sequence_history_reports = measure_history_roundtrip(
        &mut state,
        scale,
        AuthoringHistoryCommandScope::Sequence,
        "history.sequence_undo",
        "history.sequence_redo",
        |state| {
            let sequence = state.active_sequence().ok_or_else(|| {
                anyhow::anyhow!("active Sequence unavailable after Sequence Undo")
            })?;
            if sequence.id != sequence_history_id || sequence.name != sequence_name_before {
                anyhow::bail!("Sequence Undo did not restore the exact Sequence seed state");
            }
            Ok(())
        },
        |state| {
            let sequence = state.active_sequence().ok_or_else(|| {
                anyhow::anyhow!("active Sequence unavailable after Sequence Redo")
            })?;
            if sequence.id != sequence_history_id || sequence.name != sequence_name_after {
                anyhow::bail!("Sequence Redo did not restore the exact Sequence seed state");
            }
            Ok(())
        },
    )?;
    cases.extend(sequence_history_reports);

    let project_defaults_before = state.new_sequence_defaults().clone();
    let mut project_defaults_after = project_defaults_before.clone();
    project_defaults_after.resolution.width =
        project_defaults_after.resolution.width.saturating_add(4);
    state.update_new_sequence_defaults(project_defaults_after.clone())?;
    let project_history_reports = measure_history_roundtrip(
        &mut state,
        scale,
        AuthoringHistoryCommandScope::Project,
        "history.project_undo",
        "history.project_redo",
        |state| {
            if state.new_sequence_defaults() != &project_defaults_before {
                anyhow::bail!("Project Undo did not restore the exact Project setting seed");
            }
            Ok(())
        },
        |state| {
            if state.new_sequence_defaults() != &project_defaults_after {
                anyhow::bail!("Project Redo did not restore the exact Project setting seed");
            }
            Ok(())
        },
    )?;
    cases.extend(project_history_reports);

    cases.push(measure_observation(
        &mut state,
        scale,
        "snapshot.manual_capture",
        AuthoringOperationClass::SnapshotCapture,
        |state| {
            state
                .authoring
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("authoring Session closed"))?
                .snapshot()
                .map(|_| ())
                .map_err(|error| anyhow::anyhow!(error.to_string()))
        },
    )?);
    cases.push(measure_observation(
        &mut state,
        scale,
        "snapshot.manual_publish",
        AuthoringOperationClass::SnapshotPublish,
        AppState::save_project_file,
    )?);
    cases.push(measure_observation(
        &mut state,
        scale,
        "snapshot.autosave_publish",
        AuthoringOperationClass::SnapshotPublish,
        |state| state.write_autosave_snapshot(2, 7).map(|_| ()),
    )?);
    let history_depth = measure_history_depth(&mut state, scale)?;
    let process_memory = process_memory_sampler.finish();

    if state.active_sequence_id() != Some(handles.sequence_id) {
        anyhow::bail!("authoring perf changed the active Sequence unexpectedly");
    }
    let all_within_reference_budget = cases.iter().all(|case| case.within_reference_budget);
    let all_history_payloads_within_budget = history_depth
        .post_probe_commit_state_within_history_budget
        && history_depth.budget_evictions_confined_to_pre_probe_entries
        && history_depth.all_probe_edits_retained_and_undone
        && cases.iter().all(|case| case.memory.every_observation_within_history_budget);
    if !all_history_payloads_within_budget {
        let failed = cases
            .iter()
            .filter(|case| !case.memory.every_observation_within_history_budget)
            .map(|case| case.operation)
            .collect::<Vec<_>>();
        anyhow::bail!(
            "authoring retained-history invariant failed: cases={failed:?}, committed={}, undone={}/{}, pre_probe_entries={}, evicted={}, retention_disabled={}, oversize={}, barriers={}",
            history_depth.committed_probe_edits,
            history_depth.probe_edits_undone,
            history_depth.requested_probe_edits,
            history_depth
                .undo_entries_before_probe
                .saturating_add(history_depth.redo_entries_before_probe),
            history_depth.budget_evicted_entries_during_probe,
            history_depth.retention_disabled_entries_during_probe,
            history_depth.oversize_dropped_entries_during_probe,
            history_depth.barrier_discarded_entries_during_probe
        );
    }
    let history = state
        .authoring_history()
        .ok_or_else(|| anyhow::anyhow!("authoring history unavailable"))?
        .diagnostics();
    Ok(AuthoringScaleReport {
        schema_version: AUTHORING_PERF_SCHEMA_VERSION,
        reference_budget_version: AUTHORING_REFERENCE_BUDGET_VERSION,
        scenario: "large_project_authoring_matrix",
        profile,
        scale,
        fixture,
        cases,
        all_within_reference_budget,
        timing_evidence_scope: AUTHORING_TIMING_EVIDENCE_SCOPE,
        memory_evidence_scope: AUTHORING_MEMORY_EVIDENCE_SCOPE,
        all_history_payloads_within_budget,
        process_memory,
        timing_gate_requested,
        build_machine_attestation: authoring_build_machine_attestation(),
        sequence_locality: None,
        history_depth,
        history,
    })
}

fn authoring_sample_plan(
    scale: AuthoringScale,
    operation_class: AuthoringOperationClass,
) -> AuthoringSamplePlan {
    let measured_iterations = match operation_class {
        AuthoringOperationClass::Interactive | AuthoringOperationClass::History => {
            scale.interactive_history_samples
        }
        AuthoringOperationClass::Structural
        | AuthoringOperationClass::ProjectStructural
        | AuthoringOperationClass::ProjectSetting
        | AuthoringOperationClass::SnapshotCapture => scale.structural_samples,
        AuthoringOperationClass::SnapshotPublish => 1,
    }
    .max(1);
    let is_full_matrix = scale.interactive_history_samples >= 5 && scale.structural_samples >= 3;
    let warmup_iterations =
        usize::from(is_full_matrix && operation_class != AuthoringOperationClass::SnapshotPublish);
    AuthoringSamplePlan { warmup_iterations, measured_iterations }
}

fn structured_authoring_state(state: &AppState) -> anyhow::Result<ProjectDocument> {
    let document = state
        .authoring
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("authoring Session closed during state comparison"))?
        .document()
        .clone();
    Ok(document)
}

fn structured_authoring_state_eq(left: &ProjectDocument, right: &ProjectDocument) -> bool {
    let ProjectDocument {
        schema_version: left_schema_version,
        project_id: left_project_id,
        document_revision: left_document_revision,
        meta: left_meta,
        settings: left_settings,
        color_environment: left_color_environment,
        new_sequence_defaults: left_new_sequence_defaults,
        sequences: left_sequences,
        proxy_mode_assets: left_proxy_mode_assets,
    } = left;
    let ProjectDocument {
        schema_version: right_schema_version,
        project_id: right_project_id,
        document_revision: right_document_revision,
        meta: right_meta,
        settings: right_settings,
        color_environment: right_color_environment,
        new_sequence_defaults: right_new_sequence_defaults,
        sequences: right_sequences,
        proxy_mode_assets: right_proxy_mode_assets,
    } = right;
    left_schema_version == right_schema_version
        && left_project_id == right_project_id
        && left_document_revision == right_document_revision
        && left_meta == right_meta
        && left_settings == right_settings
        && left_color_environment == right_color_environment
        && left_new_sequence_defaults == right_new_sequence_defaults
        && left_proxy_mode_assets == right_proxy_mode_assets
        && left_sequences.default_sequence_id == right_sequences.default_sequence_id
        && left_sequences.active_sequence_id == right_sequences.active_sequence_id
        && left_sequences.sequences.len() == right_sequences.sequences.len()
        && left_sequences
            .sequences
            .iter()
            .zip(&right_sequences.sequences)
            .all(|(left, right)| left.author_state_eq_ignoring_revision(right))
}

fn reversibility_evidence(verified_iterations: usize) -> AuthoringReversibilityEvidence {
    AuthoringReversibilityEvidence {
        comparison_scope: AUTHORING_STRUCTURED_STATE_COMPARISON_SCOPE,
        verified_iterations,
        every_after_state_differed_from_before: true,
        every_roundtrip_restored_before_state: true,
    }
}

fn measure_history_roundtrip(
    state: &mut AppState,
    scale: AuthoringScale,
    command_scope: AuthoringHistoryCommandScope,
    undo_operation: &'static str,
    redo_operation: &'static str,
    mut verify_after_undo: impl FnMut(&AppState) -> anyhow::Result<()>,
    mut verify_after_redo: impl FnMut(&AppState) -> anyhow::Result<()>,
) -> anyhow::Result<[AuthoringOperationReport; 2]> {
    let operation_class = AuthoringOperationClass::History;
    let plan = authoring_sample_plan(scale, operation_class);
    let total_iterations = plan.warmup_iterations.saturating_add(plan.measured_iterations);
    let mut undo_samples = Vec::with_capacity(plan.measured_iterations);
    let mut redo_samples = Vec::with_capacity(plan.measured_iterations);
    let mut undo_memory = None;
    let mut redo_memory = None;
    let mut verified_iterations = 0usize;

    for iteration in 0..total_iterations {
        let measured = iteration >= plan.warmup_iterations;
        let before_undo = structured_authoring_state(state)?;

        let undo_started = Instant::now();
        if !state.undo_timeline()? {
            anyhow::bail!("{undo_operation} had no seeded command to Undo");
        }
        let undo_elapsed = duration_us(undo_started.elapsed());
        let after_undo = structured_authoring_state(state)?;
        if structured_authoring_state_eq(&after_undo, &before_undo) {
            anyhow::bail!(
                "{undo_operation} did not structurally change the complete authoring state"
            );
        }
        verify_after_undo(state)?;
        if measured {
            undo_samples.push(undo_elapsed);
            merge_memory_observation(&mut undo_memory, authoring_memory_evidence(state)?);
        }

        let redo_started = Instant::now();
        if !state.redo_timeline()? {
            anyhow::bail!("{redo_operation} had no command to Redo");
        }
        let redo_elapsed = duration_us(redo_started.elapsed());
        let after_redo = structured_authoring_state(state)?;
        if !structured_authoring_state_eq(&after_redo, &before_undo) {
            anyhow::bail!(
                "{redo_operation} did not structurally restore the exact pre-Undo authoring state"
            );
        }
        verify_after_redo(state)?;
        verified_iterations = verified_iterations.saturating_add(1);
        if measured {
            redo_samples.push(redo_elapsed);
            merge_memory_observation(&mut redo_memory, authoring_memory_evidence(state)?);
        }
    }

    Ok([
        operation_report(
            scale,
            undo_operation,
            operation_class,
            Some(command_scope),
            Some(reversibility_evidence(verified_iterations)),
            plan,
            undo_samples,
            undo_memory.ok_or_else(|| {
                anyhow::anyhow!("{undo_operation} produced no authoring memory evidence")
            })?,
        ),
        operation_report(
            scale,
            redo_operation,
            operation_class,
            Some(command_scope),
            Some(reversibility_evidence(verified_iterations)),
            plan,
            redo_samples,
            redo_memory.ok_or_else(|| {
                anyhow::anyhow!("{redo_operation} produced no authoring memory evidence")
            })?,
        ),
    ])
}

fn measure_reversible(
    state: &mut AppState,
    scale: AuthoringScale,
    operation: &'static str,
    operation_class: AuthoringOperationClass,
    mut operation_fn: impl FnMut(&mut AppState) -> mondrian_core::Result<()>,
) -> anyhow::Result<AuthoringOperationReport> {
    let plan = authoring_sample_plan(scale, operation_class);
    let mut samples = Vec::with_capacity(plan.measured_iterations);
    let mut memory = None;
    let mut verified_iterations = 0usize;
    for iteration in 0..plan.warmup_iterations.saturating_add(plan.measured_iterations) {
        let measured = iteration >= plan.warmup_iterations;
        let before = structured_authoring_state(state)?;
        let started = Instant::now();
        operation_fn(state).map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let elapsed = duration_us(started.elapsed());
        let after = structured_authoring_state(state)?;
        if structured_authoring_state_eq(&after, &before) {
            anyhow::bail!(
                "{operation} completed without changing the complete structured authoring state"
            );
        }
        if measured {
            samples.push(elapsed);
            merge_memory_observation(&mut memory, authoring_memory_evidence(state)?);
        }
        if !state.undo_timeline().map_err(|error| anyhow::anyhow!(error.to_string()))? {
            anyhow::bail!("{operation} did not retain an Undo entry");
        }
        let after_undo = structured_authoring_state(state)?;
        if !structured_authoring_state_eq(&after_undo, &before) {
            anyhow::bail!(
                "{operation} Undo did not restore the exact complete structured authoring state"
            );
        }
        verified_iterations = verified_iterations.saturating_add(1);
    }
    Ok(operation_report(
        scale,
        operation,
        operation_class,
        None,
        Some(reversibility_evidence(verified_iterations)),
        plan,
        samples,
        memory
            .ok_or_else(|| anyhow::anyhow!("{operation} produced no authoring memory evidence"))?,
    ))
}

fn measure_observation(
    state: &mut AppState,
    scale: AuthoringScale,
    operation: &'static str,
    operation_class: AuthoringOperationClass,
    mut operation_fn: impl FnMut(&mut AppState) -> anyhow::Result<()>,
) -> anyhow::Result<AuthoringOperationReport> {
    let plan = authoring_sample_plan(scale, operation_class);
    let mut samples = Vec::with_capacity(plan.measured_iterations);
    let mut memory = None;
    for iteration in 0..plan.warmup_iterations.saturating_add(plan.measured_iterations) {
        let measured = iteration >= plan.warmup_iterations;
        let started = Instant::now();
        operation_fn(state)?;
        let elapsed = duration_us(started.elapsed());
        if measured {
            samples.push(elapsed);
            merge_memory_observation(&mut memory, authoring_memory_evidence(state)?);
        }
    }
    Ok(operation_report(
        scale,
        operation,
        operation_class,
        None,
        None,
        plan,
        samples,
        memory
            .ok_or_else(|| anyhow::anyhow!("{operation} produced no authoring memory evidence"))?,
    ))
}

fn measure_history_depth(
    state: &mut AppState,
    scale: AuthoringScale,
) -> anyhow::Result<AuthoringHistoryDepthEvidence> {
    let requested_probe_edits = if scale.interactive_history_samples >= 5 {
        FULL_HISTORY_DEPTH_PROBE_EDITS
    } else {
        LIGHT_HISTORY_DEPTH_PROBE_EDITS
    };
    measure_history_depth_with_count(state, requested_probe_edits)
}

fn measure_history_depth_with_count(
    state: &mut AppState,
    requested_probe_edits: usize,
) -> anyhow::Result<AuthoringHistoryDepthEvidence> {
    anyhow::ensure!(
        requested_probe_edits > 0,
        "history-depth probe must request at least one reversible edit"
    );
    let structured_before = structured_authoring_state(state)?;
    let before = state
        .authoring_history()
        .ok_or_else(|| anyhow::anyhow!("authoring history unavailable before depth probe"))?
        .diagnostics();
    let mut edit_samples_us = Vec::with_capacity(requested_probe_edits);
    let mut committed_probe_edits = 0usize;
    let mut verified_lightweight_sequence_metadata_probe_edits = 0usize;
    let mut verified_clip_container_cow_detach_probe_edits = 0usize;
    for index in 0..requested_probe_edits {
        let kind = HistoryDepthProbeKind::for_index(index);
        let clip_allocation_before = history_depth_probe_clip_allocation(state)?;
        let started = Instant::now();
        state.commit_active_sequence_edit(HISTORY_DEPTH_DESCRIPTION, |sequence| {
            apply_history_depth_probe_edit(sequence, index, kind)
        })?;
        edit_samples_us.push(duration_us(started.elapsed()));
        let clip_allocation_after = history_depth_probe_clip_allocation(state)?;
        committed_probe_edits = committed_probe_edits.saturating_add(1);
        match kind {
            HistoryDepthProbeKind::LightweightSequenceMetadata => {
                if clip_allocation_after != clip_allocation_before {
                    anyhow::bail!(
                        "lightweight Sequence metadata probe unexpectedly detached the first Clip container"
                    );
                }
                verified_lightweight_sequence_metadata_probe_edits =
                    verified_lightweight_sequence_metadata_probe_edits.saturating_add(1);
            }
            HistoryDepthProbeKind::ClipContainerCowDetach => {
                if clip_allocation_after == clip_allocation_before {
                    anyhow::bail!("Clip label probe did not detach the first Clip container");
                }
                verified_clip_container_cow_detach_probe_edits =
                    verified_clip_container_cow_detach_probe_edits.saturating_add(1);
            }
        }
    }
    let structured_after_commits = structured_authoring_state(state)?;
    if structured_authoring_state_eq(&structured_after_commits, &structured_before) {
        anyhow::bail!(
            "history-depth probe commits did not change the complete structured ProjectDocument"
        );
    }
    let after_probe_commits = state
        .authoring_history()
        .ok_or_else(|| anyhow::anyhow!("authoring history unavailable after depth probe"))?
        .diagnostics();

    let mut probe_edits_undone = 0usize;
    let mut undo_samples_us = Vec::with_capacity(requested_probe_edits);
    while probe_edits_undone < requested_probe_edits {
        let probe_is_next =
            state.authoring_history().and_then(|history| history.undo_description())
                == Some(HISTORY_DEPTH_DESCRIPTION);
        if !probe_is_next {
            break;
        }
        let started = Instant::now();
        if !state.undo_timeline()? {
            anyhow::bail!("retained history-depth probe entry could not be undone");
        }
        undo_samples_us.push(duration_us(started.elapsed()));
        probe_edits_undone = probe_edits_undone.saturating_add(1);
    }
    let after_probe_undo = state
        .authoring_history()
        .ok_or_else(|| anyhow::anyhow!("authoring history unavailable after depth-probe Undo"))?
        .diagnostics();
    let structured_after_undo = structured_authoring_state(state)?;
    let complete_structured_project_document_restored_after_probe_undo =
        structured_authoring_state_eq(&structured_after_undo, &structured_before);
    if !complete_structured_project_document_restored_after_probe_undo {
        anyhow::bail!(
            "history-depth probe Undo did not restore the exact complete structured ProjectDocument"
        );
    }

    edit_samples_us.sort_unstable();
    undo_samples_us.sort_unstable();
    let edit_sample_median_us = percentile(&edit_samples_us, 50);
    let edit_sample_nearest_rank_p95_us = percentile(&edit_samples_us, 95);
    let edit_sample_max_us = edit_samples_us.last().copied().unwrap_or(0);
    let undo_sample_median_us = percentile(&undo_samples_us, 50);
    let undo_sample_nearest_rank_p95_us = percentile(&undo_samples_us, 95);
    let undo_sample_max_us = undo_samples_us.last().copied().unwrap_or(0);
    let budget_evicted_entries_during_probe = after_probe_commits
        .budget_evicted_entries
        .saturating_sub(before.budget_evicted_entries);
    let budget_evicted_logical_charge_bytes_during_probe = after_probe_commits
        .budget_evicted_bytes
        .saturating_sub(before.budget_evicted_bytes);
    let oversize_dropped_entries_during_probe = after_probe_commits
        .oversize_dropped_entries
        .saturating_sub(before.oversize_dropped_entries);
    let retention_disabled_entries_during_probe = after_probe_commits
        .retention_disabled_entries
        .saturating_sub(before.retention_disabled_entries);
    let branch_discarded_entries_during_probe = after_probe_commits
        .branch_discarded_entries
        .saturating_sub(before.branch_discarded_entries);
    let barrier_discarded_entries_during_probe = after_probe_commits
        .barrier_discarded_entries
        .saturating_sub(before.barrier_discarded_entries);
    let post_probe_commit_state_within_history_budget = after_probe_commits
        .undo_entries
        .saturating_add(after_probe_commits.redo_entries)
        <= after_probe_commits.budget.max_entries
        && after_probe_commits.retained_bytes <= after_probe_commits.budget.max_retained_bytes;
    let all_probe_edits_retained_and_undone = committed_probe_edits == requested_probe_edits
        && probe_edits_undone == requested_probe_edits
        && after_probe_undo.redo_entries == requested_probe_edits
        && retention_disabled_entries_during_probe == 0
        && oversize_dropped_entries_during_probe == 0
        && barrier_discarded_entries_during_probe == 0
        && complete_structured_project_document_restored_after_probe_undo;
    let pre_probe_entries = before.undo_entries.saturating_add(before.redo_entries);
    let pre_probe_entry_ceiling = u64::try_from(pre_probe_entries).unwrap_or(u64::MAX);
    let budget_evictions_confined_to_pre_probe_entries = all_probe_edits_retained_and_undone
        && budget_evicted_entries_during_probe <= pre_probe_entry_ceiling;
    Ok(AuthoringHistoryDepthEvidence {
        requested_probe_edits,
        committed_probe_edits,
        verified_lightweight_sequence_metadata_probe_edits,
        verified_clip_container_cow_detach_probe_edits,
        undo_entries_before_probe: before.undo_entries,
        redo_entries_before_probe: before.redo_entries,
        undo_entries_after_probe_commits: after_probe_commits.undo_entries,
        redo_entries_after_probe_undo: after_probe_undo.redo_entries,
        probe_edits_undone,
        history_logical_retained_charge_bytes_after_probe_commits: after_probe_commits
            .retained_bytes,
        history_budget_max_entries: after_probe_commits.budget.max_entries,
        history_budget_max_logical_retained_charge_bytes: after_probe_commits
            .budget
            .max_retained_bytes,
        budget_evicted_entries_during_probe,
        budget_evicted_logical_charge_bytes_during_probe,
        retention_disabled_entries_during_probe,
        oversize_dropped_entries_during_probe,
        branch_discarded_entries_during_probe,
        barrier_discarded_entries_during_probe,
        post_probe_commit_state_within_history_budget,
        budget_evictions_confined_to_pre_probe_entries,
        all_probe_edits_retained_and_undone,
        complete_structured_project_document_restored_after_probe_undo,
        structured_state_comparison_scope: AUTHORING_STRUCTURED_STATE_COMPARISON_SCOPE,
        edit_samples_us,
        edit_sample_median_us,
        edit_sample_nearest_rank_p95_us,
        edit_sample_max_us,
        undo_samples_us,
        undo_sample_median_us,
        undo_sample_nearest_rank_p95_us,
        undo_sample_max_us,
    })
}

fn history_depth_probe_clip_allocation(state: &AppState) -> anyhow::Result<AuthoringAllocationId> {
    let sequence = state
        .active_sequence()
        .ok_or_else(|| anyhow::anyhow!("active Sequence unavailable during history-depth probe"))?;
    let first_video_track = sequence
        .video_tracks
        .first()
        .ok_or_else(|| anyhow::anyhow!("fixture has no video Track during history-depth probe"))?;
    Ok(first_video_track.clips.allocation_id())
}

fn apply_history_depth_probe_edit(
    sequence: &mut Sequence,
    index: usize,
    kind: HistoryDepthProbeKind,
) -> mondrian_core::Result<()> {
    match kind {
        HistoryDepthProbeKind::LightweightSequenceMetadata => {
            let name = if (index / 2).is_multiple_of(2) {
                "History Depth Sequence A"
            } else {
                "History Depth Sequence B"
            };
            sequence.name = name.to_owned();
        }
        HistoryDepthProbeKind::ClipContainerCowDetach => {
            let first_video_track = sequence.video_tracks.first_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "authoring_perf_history_depth".to_owned(),
                    reason: "fixture has no video Track for Clip-container COW probe".to_owned(),
                }
            })?;
            let first_clip = first_video_track.clips.first_mut().ok_or_else(|| {
                mondrian_core::MondrianError::WorkflowStepFailed {
                    step_id: "authoring_perf_history_depth".to_owned(),
                    reason: "fixture has no Clip for Clip-container COW probe".to_owned(),
                }
            })?;
            let label = if (index / 2).is_multiple_of(2) {
                "history-depth-cow-a"
            } else {
                "history-depth-cow-b"
            };
            first_clip.label = Some(label.to_owned());
        }
    }
    Ok(())
}

fn operation_report(
    scale: AuthoringScale,
    operation: &'static str,
    operation_class: AuthoringOperationClass,
    history_command_scope: Option<AuthoringHistoryCommandScope>,
    reversibility: Option<AuthoringReversibilityEvidence>,
    plan: AuthoringSamplePlan,
    mut samples_us: Vec<u64>,
    memory: AuthoringMemoryEvidence,
) -> AuthoringOperationReport {
    samples_us.sort_unstable();
    let iterations = samples_us.len();
    let sample_median_us = percentile(&samples_us, 50);
    let sample_nearest_rank_p95_us = percentile(&samples_us, 95);
    let sample_max_us = samples_us.last().copied().unwrap_or(0);
    let reference_sample_p95_budget_us = reference_budget_us(scale, operation_class);
    AuthoringOperationReport {
        operation,
        operation_class,
        history_command_scope,
        reversibility,
        warmup_iterations: plan.warmup_iterations,
        iterations,
        samples_us,
        sample_median_us,
        sample_nearest_rank_p95_us,
        sample_max_us,
        reference_sample_p95_budget_us,
        within_reference_budget: sample_nearest_rank_p95_us <= reference_sample_p95_budget_us,
        memory,
    }
}

fn percentile(samples: &[u64], percentile: usize) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let percentile = percentile.clamp(1, 100);
    let nearest_rank = samples.len().saturating_mul(percentile).div_ceil(100);
    let index = nearest_rank.saturating_sub(1);
    samples[index.min(samples.len() - 1)]
}

fn reference_budget_us(scale: AuthoringScale, operation_class: AuthoringOperationClass) -> u64 {
    let profile = match scale.duration_minutes {
        0..=5 => [30_000, 50_000, 150_000, 80_000, 50_000, 100_000, 8_000_000],
        6..=30 => [
            45_000, 90_000, 300_000, 150_000, 90_000, 200_000, 12_000_000,
        ],
        _ => [
            75_000, 150_000, 600_000, 250_000, 150_000, 400_000, 20_000_000,
        ],
    };
    profile[match operation_class {
        AuthoringOperationClass::Interactive => 0,
        AuthoringOperationClass::Structural => 1,
        AuthoringOperationClass::ProjectStructural => 2,
        AuthoringOperationClass::ProjectSetting => 3,
        AuthoringOperationClass::History => 4,
        AuthoringOperationClass::SnapshotCapture => 5,
        AuthoringOperationClass::SnapshotPublish => 6,
    }]
}

fn authoring_build_machine_attestation() -> AuthoringBuildMachineAttestation {
    let machine = MachineResourceProfile::default();
    let debug_assertions_enabled = cfg!(debug_assertions);
    let cargo_rustc_opt_level = option_env!("MONDRIAN_BUILD_RUSTC_OPT_LEVEL");
    let optimization =
        assess_timing_gate_optimization(debug_assertions_enabled, cargo_rustc_opt_level);
    AuthoringBuildMachineAttestation {
        target_os: std::env::consts::OS,
        target_arch: std::env::consts::ARCH,
        target_pointer_width_bits: usize::BITS as usize,
        debug_assertions_enabled,
        timing_gate_optimized_build_eligible: optimization.eligible,
        optimization_contract: AUTHORING_OPTIMIZATION_CONTRACT,
        timing_gate_eligibility_reason: optimization.reason,
        cargo_profile: option_env!("MONDRIAN_BUILD_CARGO_PROFILE"),
        cargo_rustc_opt_level,
        parsed_cargo_rustc_opt_level: optimization.parsed_opt_level,
        logical_cpu_count: machine.logical_cpu_count,
        installed_physical_memory_bytes: machine.installed_memory_bytes,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimingGateOptimizationAssessment {
    parsed_opt_level: Option<u8>,
    eligible: bool,
    reason: &'static str,
}

fn assess_timing_gate_optimization(
    debug_assertions_enabled: bool,
    cargo_rustc_opt_level: Option<&str>,
) -> TimingGateOptimizationAssessment {
    let parsed_opt_level = cargo_rustc_opt_level.and_then(|value| value.trim().parse::<u8>().ok());
    let (eligible, reason) = if debug_assertions_enabled {
        (false, "debug_assertions_enabled")
    } else {
        match (cargo_rustc_opt_level, parsed_opt_level) {
            (None, _) => (false, "cargo_rustc_opt_level_missing"),
            (Some(_), None) => (false, "cargo_rustc_opt_level_not_decimal_integer"),
            (Some(_), Some(0 | 1)) => (false, "cargo_rustc_opt_level_below_2"),
            (Some(_), Some(_)) => (
                true,
                "debug_assertions_disabled_and_cargo_rustc_opt_level_at_least_2",
            ),
        }
    };
    TimingGateOptimizationAssessment { parsed_opt_level, eligible, reason }
}

fn evaluate_sequence_locality(
    active_heavy: &AuthoringScaleReport,
    project_heavy: &AuthoringScaleReport,
) -> anyhow::Result<AuthoringSequenceLocalityEvidence> {
    if active_heavy.scale != project_heavy.scale {
        anyhow::bail!(
            "Sequence-locality evidence requires the same scale, got {} and {}",
            active_heavy.scale.name,
            project_heavy.scale.name
        );
    }
    if active_heavy.profile != AuthoringFixtureProfile::ActiveHeavy
        || project_heavy.profile != AuthoringFixtureProfile::ProjectHeavy
    {
        anyhow::bail!(
            "Sequence-locality evidence requires active-heavy then project-heavy reports"
        );
    }
    if active_heavy.build_machine_attestation != project_heavy.build_machine_attestation {
        anyhow::bail!("Sequence-locality evidence requires the same build and machine attestation");
    }
    let active_sequence_workload_shape_equivalent = active_heavy.fixture.primary_video_tracks
        == project_heavy.fixture.primary_video_tracks
        && active_heavy.fixture.primary_audio_tracks == project_heavy.fixture.primary_audio_tracks
        && active_heavy.fixture.active_payload_clips == project_heavy.fixture.active_payload_clips
        && active_heavy.fixture.active_effects == project_heavy.fixture.active_effects
        && active_heavy.fixture.active_keyframes == project_heavy.fixture.active_keyframes
        && active_heavy.fixture.nesting_depth == project_heavy.fixture.nesting_depth
        && active_heavy.fixture.proxy_mode_asset_count
            == project_heavy.fixture.proxy_mode_asset_count;
    let active_sequence_serialized_size_equal = active_heavy.fixture.active_sequence_json_bytes
        == project_heavy.fixture.active_sequence_json_bytes;
    if !active_sequence_workload_shape_equivalent || !active_sequence_serialized_size_equal {
        anyhow::bail!(
            "Sequence-locality evidence requires serialized-size and workload-shape equivalent active Sequences"
        );
    }
    if active_heavy.fixture.unrelated_payload_clips != 0
        || active_heavy.fixture.unrelated_effects != 0
        || active_heavy.fixture.unrelated_keyframes != 0
        || active_heavy.fixture.unrelated_project_json_bytes != 0
    {
        anyhow::bail!("active-heavy locality baseline unexpectedly contains unrelated payload");
    }
    if project_heavy.fixture.unrelated_payload_clips == 0
        || project_heavy.fixture.unrelated_effects == 0
        || project_heavy.fixture.unrelated_keyframes == 0
        || project_heavy.fixture.unrelated_project_json_bytes == 0
        || project_heavy.fixture.sequence_count <= active_heavy.fixture.sequence_count
        || project_heavy.fixture.project_json_bytes <= active_heavy.fixture.project_json_bytes
    {
        anyhow::bail!(
            "project-heavy locality candidate must add measurable unrelated Project payload"
        );
    }
    let candidate_additional_sequence_count =
        project_heavy.fixture.sequence_count - active_heavy.fixture.sequence_count;
    let fixture_strength = assess_sequence_locality_fixture_strength(
        active_heavy.fixture.active_sequence_json_bytes,
        project_heavy.fixture.unrelated_project_json_bytes,
    );

    let mut operations = Vec::with_capacity(SEQUENCE_LOCALITY_OPERATIONS.len());
    for operation in SEQUENCE_LOCALITY_OPERATIONS {
        let active_case = unique_operation_case(active_heavy, operation)?;
        let project_case = unique_operation_case(project_heavy, operation)?;
        if active_case.iterations == 0
            || active_case.iterations != project_case.iterations
            || active_case.iterations.is_multiple_of(2)
        {
            anyhow::bail!(
                "Sequence-locality operation {operation} requires equal nonzero odd sample counts, got active-heavy={} and project-heavy={}",
                active_case.iterations,
                project_case.iterations
            );
        }
        operations.push(sequence_locality_operation_evidence(
            operation,
            active_case.sample_median_us,
            project_case.sample_median_us,
            active_case.sample_nearest_rank_p95_us,
            project_case.sample_nearest_rank_p95_us,
            project_case.reference_sample_p95_budget_us,
        ));
    }
    let all_within_budget =
        fixture_strength.satisfied && operations.iter().all(|operation| operation.passed);
    Ok(AuthoringSequenceLocalityEvidence {
        budget_version: AUTHORING_SEQUENCE_LOCALITY_BUDGET_VERSION,
        baseline_profile: AuthoringFixtureProfile::ActiveHeavy,
        candidate_profile: AuthoringFixtureProfile::ProjectHeavy,
        comparison_scope: AUTHORING_SEQUENCE_LOCALITY_COMPARISON_SCOPE,
        active_sequence_serialized_size_bytes: active_heavy.fixture.active_sequence_json_bytes,
        active_sequence_serialized_size_equal,
        active_sequence_workload_shape_equivalent,
        candidate_additional_sequence_count,
        minimum_unrelated_serialized_bytes_multiplier:
            SEQUENCE_LOCALITY_MIN_UNRELATED_SERIALIZED_BYTES_MULTIPLIER,
        required_unrelated_project_json_bytes: fixture_strength.required_unrelated_bytes,
        candidate_unrelated_project_json_bytes: project_heavy.fixture.unrelated_project_json_bytes,
        candidate_unrelated_serialized_bytes_multiplier_floor: fixture_strength
            .candidate_multiplier_floor,
        fixture_strength_satisfied: fixture_strength.satisfied,
        operations,
        all_within_budget,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SequenceLocalityFixtureStrength {
    required_unrelated_bytes: usize,
    candidate_multiplier_floor: usize,
    satisfied: bool,
}

fn assess_sequence_locality_fixture_strength(
    active_sequence_json_bytes: usize,
    candidate_unrelated_project_json_bytes: usize,
) -> SequenceLocalityFixtureStrength {
    let required = active_sequence_json_bytes
        .checked_mul(SEQUENCE_LOCALITY_MIN_UNRELATED_SERIALIZED_BYTES_MULTIPLIER);
    let candidate_multiplier_floor = candidate_unrelated_project_json_bytes
        .checked_div(active_sequence_json_bytes)
        .unwrap_or(0);
    SequenceLocalityFixtureStrength {
        required_unrelated_bytes: required.unwrap_or(usize::MAX),
        candidate_multiplier_floor,
        satisfied: required
            .is_some_and(|required| candidate_unrelated_project_json_bytes >= required)
            && active_sequence_json_bytes > 0,
    }
}

fn sequence_locality_operation_evidence(
    operation: &'static str,
    active_heavy_sample_median_us: u64,
    project_heavy_sample_median_us: u64,
    active_heavy_sample_nearest_rank_p95_us: u64,
    project_heavy_sample_nearest_rank_p95_us: u64,
    project_heavy_reference_sample_p95_budget_us: u64,
) -> AuthoringSequenceLocalityOperationEvidence {
    let relative_median_budget_us = active_heavy_sample_median_us
        .saturating_mul(SEQUENCE_LOCALITY_RELATIVE_MEDIAN_MULTIPLIER)
        .saturating_add(SEQUENCE_LOCALITY_RELATIVE_MEDIAN_ALLOWANCE_US);
    let within_relative_median_budget = project_heavy_sample_median_us <= relative_median_budget_us;
    let within_project_heavy_reference_budget =
        project_heavy_sample_nearest_rank_p95_us <= project_heavy_reference_sample_p95_budget_us;
    AuthoringSequenceLocalityOperationEvidence {
        operation,
        active_heavy_sample_median_us,
        project_heavy_sample_median_us,
        active_heavy_sample_nearest_rank_p95_us,
        project_heavy_sample_nearest_rank_p95_us,
        relative_median_multiplier: SEQUENCE_LOCALITY_RELATIVE_MEDIAN_MULTIPLIER,
        relative_median_allowance_us: SEQUENCE_LOCALITY_RELATIVE_MEDIAN_ALLOWANCE_US,
        relative_median_budget_us,
        project_heavy_reference_sample_p95_budget_us,
        within_relative_median_budget,
        within_project_heavy_reference_budget,
        passed: within_relative_median_budget && within_project_heavy_reference_budget,
    }
}

fn unique_operation_case<'a>(
    report: &'a AuthoringScaleReport,
    operation: &'static str,
) -> anyhow::Result<&'a AuthoringOperationReport> {
    let mut matching = report.cases.iter().filter(|case| case.operation == operation);
    let case = matching.next().ok_or_else(|| {
        anyhow::anyhow!(
            "{} {} report is missing required Sequence-locality operation {operation}",
            report.scale.name,
            report.profile.name()
        )
    })?;
    if matching.next().is_some() {
        anyhow::bail!(
            "{} {} report contains duplicate Sequence-locality operation {operation}",
            report.scale.name,
            report.profile.name()
        );
    }
    Ok(case)
}

fn enforce_authoring_performance_pair(
    active_heavy: &AuthoringScaleReport,
    project_heavy: &AuthoringScaleReport,
) -> anyhow::Result<()> {
    if !active_heavy.build_machine_attestation.timing_gate_optimized_build_eligible
        || !project_heavy.build_machine_attestation.timing_gate_optimized_build_eligible
    {
        anyhow::bail!(
            "enforced authoring wall-clock gates require {}; active-heavy reason={}, profile={:?}, OPT_LEVEL={:?}, parsed={:?}; project-heavy reason={}, profile={:?}, OPT_LEVEL={:?}, parsed={:?}",
            AUTHORING_OPTIMIZATION_CONTRACT,
            active_heavy.build_machine_attestation.timing_gate_eligibility_reason,
            active_heavy.build_machine_attestation.cargo_profile,
            active_heavy.build_machine_attestation.cargo_rustc_opt_level,
            active_heavy.build_machine_attestation.parsed_cargo_rustc_opt_level,
            project_heavy.build_machine_attestation.timing_gate_eligibility_reason,
            project_heavy.build_machine_attestation.cargo_profile,
            project_heavy.build_machine_attestation.cargo_rustc_opt_level,
            project_heavy.build_machine_attestation.parsed_cargo_rustc_opt_level,
        );
    }
    if cfg!(target_os = "windows")
        && (!active_heavy.process_memory.gate_required
            || !project_heavy.process_memory.gate_required)
    {
        anyhow::bail!(
            "enforced Windows authoring reports must arm native process-memory gate v{}",
            AUTHORING_PROCESS_MEMORY_BUDGET_VERSION
        );
    }
    let failed_process_memory = [active_heavy, project_heavy]
        .into_iter()
        .filter(|report| !report.process_memory.passed)
        .map(|report| {
            format!(
                "{}: complete={}, private_delta={:?}, max_private_delta={}, peak_private={:?}, max_peak_private={}, errors={}, last_error={:?}",
                report.profile.name(),
                report.process_memory.evidence_complete,
                report.process_memory.peak_private_committed_delta_bytes,
                report.process_memory.max_peak_private_committed_delta_bytes,
                report.process_memory.peak_private_committed_bytes,
                report.process_memory.max_peak_private_committed_bytes,
                report.process_memory.probe_errors,
                report.process_memory.last_probe_error
            )
        })
        .collect::<Vec<_>>();
    if !failed_process_memory.is_empty() {
        anyhow::bail!(
            "authoring native process-memory budget v{} failed: {failed_process_memory:?}",
            AUTHORING_PROCESS_MEMORY_BUDGET_VERSION
        );
    }
    let failed_reference_cases = active_heavy
        .cases
        .iter()
        .filter(|case| !case.within_reference_budget)
        .map(|case| format!("{}/{}", active_heavy.profile.name(), case.operation))
        .chain(
            project_heavy
                .cases
                .iter()
                .filter(|case| !case.within_reference_budget)
                .map(|case| format!("{}/{}", project_heavy.profile.name(), case.operation)),
        )
        .collect::<Vec<_>>();
    if !failed_reference_cases.is_empty() {
        anyhow::bail!(
            "authoring reference budget v{} exceeded by {failed_reference_cases:?}",
            AUTHORING_REFERENCE_BUDGET_VERSION
        );
    }
    let locality = project_heavy.sequence_locality.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "enforced project-heavy authoring report is missing Sequence-locality evidence"
        )
    })?;
    if !locality.fixture_strength_satisfied {
        anyhow::bail!(
            "authoring Sequence-locality fixture is too weak: unrelated serialized payload {} bytes, required at least {} bytes ({}x active Sequence)",
            locality.candidate_unrelated_project_json_bytes,
            locality.required_unrelated_project_json_bytes,
            locality.minimum_unrelated_serialized_bytes_multiplier
        );
    }
    if !locality.all_within_budget {
        let failed = locality
            .operations
            .iter()
            .filter(|operation| !operation.passed)
            .map(|operation| {
                format!(
                    "{}(active_median={}us, project_median={}us, relative_median_budget={}us, project_nearest_rank_p95={}us, project_reference_budget={}us, relative_pass={}, reference_pass={})",
                    operation.operation,
                    operation.active_heavy_sample_median_us,
                    operation.project_heavy_sample_median_us,
                    operation.relative_median_budget_us,
                    operation.project_heavy_sample_nearest_rank_p95_us,
                    operation.project_heavy_reference_sample_p95_budget_us,
                    operation.within_relative_median_budget,
                    operation.within_project_heavy_reference_budget
                )
            })
            .collect::<Vec<_>>();
        anyhow::bail!(
            "authoring Sequence-locality budget v{} exceeded by {failed:?}",
            locality.budget_version
        );
    }
    Ok(())
}

fn authoring_memory_evidence(state: &AppState) -> anyhow::Result<AuthoringMemoryEvidence> {
    let session = state
        .authoring
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("authoring Session closed"))?;
    let document_json_bytes = serialized_json_size(session.document())?;
    let diagnostics = session.history().diagnostics();
    let entries = diagnostics.undo_entries.saturating_add(diagnostics.redo_entries);
    // The retained charge is a deduplicated union shared by every command,
    // so division is an amortized diagnostic, not an independently owned
    // per-command allocation estimate.
    let amortized_charge_per_entry = if entries == 0 {
        0
    } else {
        diagnostics.retained_bytes.div_ceil(entries)
    };
    Ok(AuthoringMemoryEvidence {
        measurement_scope: AUTHORING_MEMORY_EVIDENCE_SCOPE,
        observations: 1,
        peak_document_json_bytes_after_operation: document_json_bytes,
        peak_history_logical_retained_charge_bytes_after_operation: diagnostics.retained_bytes,
        peak_history_entries_after_operation: entries,
        peak_amortized_history_logical_charge_per_entry_after_operation: amortized_charge_per_entry,
        history_budget_max_entries: diagnostics.budget.max_entries,
        history_budget_max_logical_retained_charge_bytes: diagnostics.budget.max_retained_bytes,
        every_observation_within_history_budget: entries <= diagnostics.budget.max_entries
            && diagnostics.retained_bytes <= diagnostics.budget.max_retained_bytes,
    })
}

fn merge_memory_observation(
    aggregate: &mut Option<AuthoringMemoryEvidence>,
    observation: AuthoringMemoryEvidence,
) {
    if let Some(aggregate) = aggregate {
        aggregate.merge_peak(observation);
    } else {
        *aggregate = Some(observation);
    }
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn timeline_time_from_frame(
    frame: i64,
    time_base: Rational,
) -> mondrian_core::Result<TimelineTime> {
    Ok(TimelineTime::from_frame_position(FramePosition::new(
        frame, time_base,
    ))?)
}

fn parse_timing_gate_value(value: Option<&str>) -> anyhow::Result<bool> {
    let Some(value) = value else {
        return Ok(false);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => {
            anyhow::bail!("MONDRIAN_AUTHORING_PERF_ENFORCE must be one of 1/true/yes or 0/false/no")
        }
    }
}

fn timing_gate_from_env() -> anyhow::Result<bool> {
    match std::env::var("MONDRIAN_AUTHORING_PERF_ENFORCE") {
        Ok(value) => parse_timing_gate_value(Some(&value)),
        Err(std::env::VarError::NotPresent) => parse_timing_gate_value(None),
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("MONDRIAN_AUTHORING_PERF_ENFORCE is not valid Unicode")
        }
    }
}

fn unix_time_ns() -> anyhow::Result<u128> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| anyhow::anyhow!("system clock precedes Unix epoch: {error}"))?
        .as_nanos())
}

fn collect_authoring_source_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(directory)
        .with_context(|| format!("read source-tree directory {}", directory.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("enumerate source-tree directory {}", directory.display()))?;
    entries.sort_unstable_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry
            .file_type()
            .with_context(|| format!("read source-tree entry type {}", entry.path().display()))?;
        let path = entry.path();
        if file_type.is_dir() {
            if entry.file_name() != "target" {
                collect_authoring_source_files(&path, files)?;
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let included =
            path.extension().and_then(std::ffi::OsStr::to_str).is_some_and(|extension| {
                matches!(
                    extension,
                    "rs" | "toml"
                        | "lock"
                        | "json"
                        | "sql"
                        | "proto"
                        | "wgsl"
                        | "glsl"
                        | "hlsl"
                        | "metal"
                )
            });
        if included {
            files.push(path);
        }
    }
    Ok(())
}

fn capture_authoring_source_tree_attestation() -> anyhow::Result<AuthoringSourceTreeAttestation> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().and_then(Path::parent).ok_or_else(|| {
        anyhow::anyhow!(
            "cannot resolve workspace root from CARGO_MANIFEST_DIR {}",
            manifest_dir.display()
        )
    })?;
    let mut files = ["Cargo.toml", "Cargo.lock"]
        .into_iter()
        .map(|name| workspace_root.join(name))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    collect_authoring_source_files(&workspace_root.join("crates"), &mut files)?;
    files.sort_unstable();
    files.dedup();
    anyhow::ensure!(
        !files.is_empty(),
        "source-tree attestation did not discover any source files"
    );

    let mut hasher = Sha256::new();
    let mut bytes_hashed = 0u64;
    for path in &files {
        let relative = path.strip_prefix(workspace_root).with_context(|| {
            format!(
                "source-tree file {} is outside workspace {}",
                path.display(),
                workspace_root.display()
            )
        })?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        let contents =
            fs::read(path).with_context(|| format!("read source-tree file {}", path.display()))?;
        bytes_hashed = bytes_hashed
            .checked_add(u64::try_from(contents.len()).context("source file length exceeds u64")?)
            .ok_or_else(|| anyhow::anyhow!("source-tree attestation byte count overflow"))?;
        let relative_bytes = relative.as_bytes();
        hasher.update(
            u64::try_from(relative_bytes.len())
                .context("source path length exceeds u64")?
                .to_le_bytes(),
        );
        hasher.update(relative_bytes);
        hasher.update(
            u64::try_from(contents.len())
                .context("source file length exceeds u64")?
                .to_le_bytes(),
        );
        hasher.update(&contents);
    }

    Ok(AuthoringSourceTreeAttestation {
        source_revision: format!("sha256:{:x}", hasher.finalize()),
        revision_kind: "content_addressed_source_tree",
        source_tree_scope: AUTHORING_SOURCE_TREE_SCOPE,
        files_hashed: files.len(),
        bytes_hashed,
        source_dirty: None,
        dirty_attestation: AUTHORING_SOURCE_DIRTY_ATTESTATION,
    })
}

impl AuthoringEvidenceRun {
    fn from_environment(enforce: bool) -> anyhow::Result<Self> {
        let output_path = perf_output_path();
        if enforce && output_path.is_none() {
            anyhow::bail!(
                "MONDRIAN_AUTHORING_PERF_ENFORCE requires an explicit MONDRIAN_PERF_OUTPUT path"
            );
        }
        Self::new(
            output_path,
            enforce,
            uuid::Uuid::new_v4().to_string(),
            capture_authoring_source_tree_attestation()?,
            unix_time_ns()?,
        )
    }

    fn new(
        output_path: Option<PathBuf>,
        enforce: bool,
        run_id: String,
        source: AuthoringSourceTreeAttestation,
        started_unix_time_ns: u128,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !run_id.trim().is_empty(),
            "authoring evidence run ID is empty"
        );
        if enforce && output_path.is_none() {
            anyhow::bail!(
                "MONDRIAN_AUTHORING_PERF_ENFORCE requires an explicit MONDRIAN_PERF_OUTPUT path"
            );
        }
        let writer = output_path.map(FreshJsonlEvidenceWriter::create).transpose()?;
        Ok(Self {
            run_id,
            source,
            started_unix_time_ns,
            emitted_reports: 0,
            writer,
        })
    }

    fn emit_report<T: Serialize>(&mut self, report: &T) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.emitted_reports < AUTHORING_EVIDENCE_REPORT_COUNT,
            "authoring evidence run {} already emitted all {} reports",
            self.run_id,
            AUTHORING_EVIDENCE_REPORT_COUNT
        );
        let ordinal = self.emitted_reports.saturating_add(1);
        let record = AuthoringEvidenceReportRecord {
            record_type: "authoring_scale_report",
            protocol_schema_version: AUTHORING_EVIDENCE_PROTOCOL_SCHEMA_VERSION,
            run_id: &self.run_id,
            ordinal,
            expected_report_count: AUTHORING_EVIDENCE_REPORT_COUNT,
            source: &self.source,
            report,
        };
        let record_json = serde_json::to_string(&record)?;
        eprintln!("MONDRIAN_AUTHORING_PERF_JSON={record_json}");
        if let Some(writer) = &mut self.writer {
            writer.write_json_line(&record_json)?;
        }
        self.emitted_reports = ordinal;
        Ok(())
    }

    fn complete(mut self, final_source: AuthoringSourceTreeAttestation) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.emitted_reports == AUTHORING_EVIDENCE_REPORT_COUNT,
            "authoring evidence run {} is incomplete: emitted {}/{} reports",
            self.run_id,
            self.emitted_reports,
            AUTHORING_EVIDENCE_REPORT_COUNT
        );
        anyhow::ensure!(
            final_source == self.source,
            "authoring source tree changed during evidence run {}",
            self.run_id
        );
        let record = AuthoringEvidenceCompletionRecord {
            record_type: "authoring_run_completion",
            protocol_schema_version: AUTHORING_EVIDENCE_PROTOCOL_SCHEMA_VERSION,
            run_id: &self.run_id,
            completed_report_count: self.emitted_reports,
            expected_report_count: AUTHORING_EVIDENCE_REPORT_COUNT,
            last_report_ordinal: self.emitted_reports,
            source: &self.source,
            source_unchanged_during_run: true,
            started_unix_time_ns: self.started_unix_time_ns,
            completed_unix_time_ns: unix_time_ns()?,
        };
        let record_json = serde_json::to_string(&record)?;
        eprintln!("MONDRIAN_AUTHORING_PERF_JSON={record_json}");
        if let Some(mut writer) = self.writer.take() {
            writer.write_json_line(&record_json)?;
            writer.finish()?;
        }
        Ok(())
    }
}

fn unique_authoring_perf_root(scale: AuthoringScale, profile: AuthoringFixtureProfile) -> PathBuf {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let profile_tag = match profile {
        AuthoringFixtureProfile::ActiveHeavy => "a",
        AuthoringFixtureProfile::ProjectHeavy => "p",
    };
    std::env::temp_dir().join(format!(
        "map-{}-{profile_tag}-{:x}-{unique:x}",
        scale.duration_minutes,
        std::process::id(),
    ))
}

#[test]
fn authoring_scale_matrix_contract_is_fixed_and_monotonic() {
    assert_eq!(
        AUTHORING_SCALE_MATRIX.len() * AuthoringFixtureProfile::ALL.len(),
        AUTHORING_EVIDENCE_REPORT_COUNT,
        "the evidence protocol requires exactly six ordered Scale/Profile reports"
    );
    let mut previous: Option<AuthoringScale> = None;
    for scale in AUTHORING_SCALE_MATRIX {
        assert!(scale.video_tracks >= 2);
        assert!(scale.audio_tracks >= 2);
        assert!(scale.active_payload_clips >= scale.video_tracks * 2);
        assert!(scale.active_effects > 0);
        assert!(scale.active_keyframes >= scale.active_effects);
        assert!(scale.nesting_depth > 0);
        assert!(scale.project_heavy_unrelated_sequences > 0);
        assert!(scale.interactive_history_samples >= 5);
        assert!(scale.structural_samples >= 3);
        if let Some(previous) = previous {
            assert!(scale.duration_minutes > previous.duration_minutes);
            assert!(scale.video_tracks > previous.video_tracks);
            assert!(scale.audio_tracks > previous.audio_tracks);
            assert!(scale.active_payload_clips > previous.active_payload_clips);
            assert!(scale.active_effects > previous.active_effects);
            assert!(scale.active_keyframes > previous.active_keyframes);
            assert!(scale.nesting_depth > previous.nesting_depth);
            assert!(
                scale.project_heavy_unrelated_sequences
                    > previous.project_heavy_unrelated_sequences
            );
        }
        previous = Some(scale);
    }
    assert_eq!(
        AUTHORING_SCALE_MATRIX.map(|scale| scale.duration_minutes),
        [5, 30, 120]
    );
}

#[test]
fn authoring_process_memory_budgets_are_fixed_by_scale_and_profile() {
    let expected_mib = [
        (LIGHT_AUTHORING_SCALE, [256, 384]),
        (AUTHORING_SCALE_MATRIX[0], [512, 768]),
        (AUTHORING_SCALE_MATRIX[1], [1024, 1536]),
        (AUTHORING_SCALE_MATRIX[2], [2048, 3072]),
    ];
    for (scale, [active_mib, project_mib]) in expected_mib {
        assert_eq!(
            authoring_process_memory_budget_bytes(scale, AuthoringFixtureProfile::ActiveHeavy),
            Some(active_mib * MEBIBYTE)
        );
        assert_eq!(
            authoring_process_memory_budget_bytes(scale, AuthoringFixtureProfile::ProjectHeavy),
            Some(project_mib * MEBIBYTE)
        );
    }
    assert!(AUTHORING_SCALE_MATRIX.windows(2).all(|scales| {
        AuthoringFixtureProfile::ALL.into_iter().all(|profile| {
            authoring_process_memory_budget_bytes(scales[0], profile)
                < authoring_process_memory_budget_bytes(scales[1], profile)
        })
    }));
}

#[test]
fn authoring_process_memory_gate_uses_private_commit_delta_and_absolute_peak_only() {
    use mondrian_platform::ProcessMemoryProbeBackend;

    let scale = AUTHORING_SCALE_MATRIX[0];
    let profile = AuthoringFixtureProfile::ActiveHeavy;
    let mut within = AuthoringProcessMemoryAccumulator::new();
    within.background_sampling_started = true;
    within.background_sampling_stopped = true;
    for private_mib in [120, 100, 110] {
        within.observe(
            ProcessMemoryProbeResult::observed(
                mondrian_platform::ProcessMemoryScope::CurrentProcess,
                ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
                1,
                1,
                private_mib * MEBIBYTE,
                8 * 1024 * MEBIBYTE,
                12 * 1024 * MEBIBYTE,
            ),
            AuthoringProcessMemorySamplePhase::Baseline,
        );
    }
    within.observe(
        ProcessMemoryProbeResult::observed(
            mondrian_platform::ProcessMemoryScope::CurrentProcess,
            ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
            1,
            1,
            125 * MEBIBYTE,
            14 * 1024 * MEBIBYTE,
            15 * 1024 * MEBIBYTE,
        ),
        AuthoringProcessMemorySamplePhase::Background,
    );
    within.observe(
        ProcessMemoryProbeResult::observed(
            mondrian_platform::ProcessMemoryScope::CurrentProcess,
            ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
            1,
            1,
            115 * MEBIBYTE,
            13 * 1024 * MEBIBYTE,
            15 * 1024 * MEBIBYTE,
        ),
        AuthoringProcessMemorySamplePhase::Final,
    );
    let evidence = within.evidence(scale, profile, true);
    assert_eq!(
        evidence.peak_private_committed_delta_bytes,
        Some(25 * MEBIBYTE)
    );
    assert!(evidence.evidence_complete);
    assert!(evidence.within_delta_budget);
    assert!(evidence.within_absolute_budget);
    assert!(evidence.within_budget);
    assert!(evidence.passed);
    assert!(
        evidence.peak_observed_resident_bytes
            > Some(evidence.max_peak_private_committed_delta_bytes)
    );
    assert!(
        evidence.os_process_lifetime_peak_resident_bytes
            > Some(evidence.max_peak_private_committed_delta_bytes)
    );

    let mut exceeded = within;
    exceeded.observe(
        ProcessMemoryProbeResult::observed(
            mondrian_platform::ProcessMemoryScope::CurrentProcess,
            ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
            1,
            1,
            100 * MEBIBYTE + evidence.max_peak_private_committed_delta_bytes + 1,
            MEBIBYTE,
            MEBIBYTE,
        ),
        AuthoringProcessMemorySamplePhase::Background,
    );
    let exceeded = exceeded.evidence(scale, profile, true);
    assert!(exceeded.evidence_complete);
    assert!(!exceeded.within_delta_budget);
    assert!(exceeded.within_absolute_budget);
    assert!(!exceeded.within_budget);
    assert!(!exceeded.passed);

    let mut absolute_exceeded = AuthoringProcessMemoryAccumulator::new();
    absolute_exceeded.background_sampling_started = true;
    absolute_exceeded.background_sampling_stopped = true;
    for _ in 0..AUTHORING_PROCESS_MEMORY_BASELINE_SAMPLES {
        absolute_exceeded.observe(
            ProcessMemoryProbeResult::observed(
                mondrian_platform::ProcessMemoryScope::CurrentProcess,
                ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
                1,
                1,
                AUTHORING_PROCESS_MEMORY_ABSOLUTE_PRIVATE_COMMIT_LIMIT_BYTES + 1,
                MEBIBYTE,
                MEBIBYTE,
            ),
            AuthoringProcessMemorySamplePhase::Baseline,
        );
    }
    for phase in [
        AuthoringProcessMemorySamplePhase::Background,
        AuthoringProcessMemorySamplePhase::Final,
    ] {
        absolute_exceeded.observe(
            ProcessMemoryProbeResult::observed(
                mondrian_platform::ProcessMemoryScope::CurrentProcess,
                ProcessMemoryProbeBackend::WindowsCurrentProcessStatus,
                1,
                1,
                AUTHORING_PROCESS_MEMORY_ABSOLUTE_PRIVATE_COMMIT_LIMIT_BYTES + 1,
                MEBIBYTE,
                MEBIBYTE,
            ),
            phase,
        );
    }
    let absolute_exceeded = absolute_exceeded.evidence(scale, profile, true);
    assert!(absolute_exceeded.evidence_complete);
    assert!(absolute_exceeded.within_delta_budget);
    assert!(!absolute_exceeded.within_absolute_budget);
    assert!(!absolute_exceeded.within_budget);
    assert!(!absolute_exceeded.passed);
}

#[test]
fn unavailable_process_memory_does_not_fail_an_unarmed_gate() {
    let mut accumulator = AuthoringProcessMemoryAccumulator::new();
    accumulator.background_sampling_stopped = true;
    accumulator.observe(
        ProcessMemoryProbeResult::unsupported(
            mondrian_platform::ProcessMemoryScope::CurrentProcess,
            "not implemented for test platform",
        ),
        AuthoringProcessMemorySamplePhase::Baseline,
    );
    accumulator.observe(
        ProcessMemoryProbeResult::unsupported(
            mondrian_platform::ProcessMemoryScope::CurrentProcess,
            "not implemented for test platform",
        ),
        AuthoringProcessMemorySamplePhase::Final,
    );
    let evidence = accumulator.evidence(
        LIGHT_AUTHORING_SCALE,
        AuthoringFixtureProfile::ActiveHeavy,
        false,
    );
    assert!(!evidence.evidence_complete);
    assert!(!evidence.within_budget);
    assert!(evidence.passed);
}

#[test]
fn authoring_percentile_uses_nearest_rank_for_small_sample_sets() {
    assert_eq!(percentile(&[7], 95), 7);
    assert_eq!(percentile(&[1, 2, 3], 95), 3);
    assert_eq!(percentile(&[1, 2, 3, 4, 5], 95), 5);
    assert_eq!(percentile(&[1, 2, 3], 50), 2);
}

#[test]
fn full_authoring_matrix_uses_repeatable_sample_plans() {
    let interactive_and_history = [
        AuthoringOperationClass::Interactive,
        AuthoringOperationClass::History,
    ];
    let structural_and_capture = [
        AuthoringOperationClass::Structural,
        AuthoringOperationClass::ProjectStructural,
        AuthoringOperationClass::ProjectSetting,
        AuthoringOperationClass::SnapshotCapture,
    ];
    for scale in AUTHORING_SCALE_MATRIX {
        for operation_class in interactive_and_history {
            let plan = authoring_sample_plan(scale, operation_class);
            assert_eq!(plan.warmup_iterations, 1);
            assert!(
                plan.measured_iterations >= 5,
                "{} must retain the declared small-sample observation plan for {operation_class:?}",
                scale.name
            );
        }
        for operation_class in structural_and_capture {
            let plan = authoring_sample_plan(scale, operation_class);
            assert_eq!(plan.warmup_iterations, 1);
            assert!(
                plan.measured_iterations >= 3,
                "{} must retain repeatable structural evidence for {operation_class:?}",
                scale.name
            );
        }
        let publish = authoring_sample_plan(scale, AuthoringOperationClass::SnapshotPublish);
        assert_eq!(publish.warmup_iterations, 0);
        assert_eq!(publish.measured_iterations, 1);
    }

    for operation_class in interactive_and_history
        .into_iter()
        .chain(structural_and_capture)
        .chain([AuthoringOperationClass::SnapshotPublish])
    {
        let plan = authoring_sample_plan(LIGHT_AUTHORING_SCALE, operation_class);
        assert_eq!(plan.warmup_iterations, 0);
        assert_eq!(plan.measured_iterations, 1);
    }
}

#[test]
fn authoring_history_depth_and_evidence_scopes_are_explicit_contracts() {
    assert_eq!(
        FULL_HISTORY_DEPTH_PROBE_EDITS,
        AuthoringHistoryBudget::default().max_entries
    );
    assert_eq!(LIGHT_HISTORY_DEPTH_PROBE_EDITS, 4);
    for probe_edits in [
        LIGHT_HISTORY_DEPTH_PROBE_EDITS,
        FULL_HISTORY_DEPTH_PROBE_EDITS,
    ] {
        let kinds = (0..probe_edits).map(HistoryDepthProbeKind::for_index).collect::<Vec<_>>();
        assert!(kinds.windows(2).all(|pair| pair[0] != pair[1]));
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == HistoryDepthProbeKind::LightweightSequenceMetadata)
                .count(),
            probe_edits / 2
        );
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == HistoryDepthProbeKind::ClipContainerCowDetach)
                .count(),
            probe_edits / 2
        );
    }
    for excluded in [
        "descriptor",
        "index",
        "cache",
        "container_spare_capacity",
        "transient_allocations",
        "allocator_bytes",
        "process_rss",
    ] {
        assert!(
            AUTHORING_MEMORY_EVIDENCE_SCOPE.contains(excluded),
            "memory evidence scope must exclude {excluded}"
        );
    }
    assert!(AUTHORING_TIMING_EVIDENCE_SCOPE.contains("not_population_or_release_statistical_p95"));
}

#[test]
fn serialized_size_evidence_does_not_require_a_payload_buffer() -> anyhow::Result<()> {
    let value = serde_json::json!({
        "name": "authoring-size-evidence",
        "values": [1, 2, 3, 5, 8, 13],
    });
    assert_eq!(
        serialized_json_size(&value)?,
        serde_json::to_vec(&value)?.len()
    );
    Ok(())
}

#[test]
fn authoring_build_machine_attestation_is_observable_and_conservative() {
    let attestation = authoring_build_machine_attestation();
    assert_eq!(attestation.target_os, std::env::consts::OS);
    assert_eq!(attestation.target_arch, std::env::consts::ARCH);
    assert_eq!(attestation.target_pointer_width_bits, usize::BITS as usize);
    assert!(attestation.logical_cpu_count >= 1);
    assert_eq!(
        attestation.timing_gate_optimized_build_eligible,
        !attestation.debug_assertions_enabled
            && attestation.parsed_cargo_rustc_opt_level.is_some_and(|level| level >= 2)
    );
    assert_eq!(
        attestation.optimization_contract,
        AUTHORING_OPTIMIZATION_CONTRACT
    );
    assert!(!attestation.timing_gate_eligibility_reason.is_empty());
    assert_eq!(
        attestation.cargo_profile,
        option_env!("MONDRIAN_BUILD_CARGO_PROFILE")
    );
    assert_eq!(
        attestation.cargo_rustc_opt_level,
        option_env!("MONDRIAN_BUILD_RUSTC_OPT_LEVEL")
    );
    assert_eq!(
        attestation.parsed_cargo_rustc_opt_level,
        attestation
            .cargo_rustc_opt_level
            .and_then(|value| value.trim().parse::<u8>().ok())
    );
}

#[test]
fn authoring_timing_gate_optimization_attestation_fails_closed() {
    for opt_level in [None, Some(""), Some("fast"), Some("0"), Some("1")] {
        assert!(
            !assess_timing_gate_optimization(false, opt_level).eligible,
            "{opt_level:?} must not prove an optimized build"
        );
    }
    for opt_level in [Some("2"), Some("3")] {
        let assessment = assess_timing_gate_optimization(false, opt_level);
        assert!(
            assessment.eligible,
            "{opt_level:?} must prove opt-level >= 2"
        );
        assert!(assessment.parsed_opt_level.is_some_and(|level| level >= 2));
    }
    let debug_assessment = assess_timing_gate_optimization(true, Some("3"));
    assert!(!debug_assessment.eligible);
    assert_eq!(debug_assessment.reason, "debug_assertions_enabled");
    assert_eq!(
        assess_timing_gate_optimization(false, None).reason,
        "cargo_rustc_opt_level_missing"
    );
    assert_eq!(
        assess_timing_gate_optimization(false, Some("s")).reason,
        "cargo_rustc_opt_level_not_decimal_integer"
    );
    assert_eq!(
        assess_timing_gate_optimization(false, Some("1")).reason,
        "cargo_rustc_opt_level_below_2"
    );
}

#[test]
fn authoring_timing_gate_configuration_is_strict() -> anyhow::Result<()> {
    assert!(!parse_timing_gate_value(None)?);
    for value in ["1", "true", "TRUE", " yes "] {
        assert!(parse_timing_gate_value(Some(value))?);
    }
    for value in ["0", "false", "FALSE", " no "] {
        assert!(!parse_timing_gate_value(Some(value))?);
    }
    for value in ["", "enabled", "tru", "2"] {
        assert!(parse_timing_gate_value(Some(value))
            .expect_err("invalid enforcement value must fail closed")
            .to_string()
            .contains("must be one of"));
    }
    Ok(())
}

#[test]
fn sequence_locality_budget_is_versioned_unique_relative_and_reuses_reference_tail_budget() {
    assert_eq!(AUTHORING_PERF_SCHEMA_VERSION, 10);
    assert_eq!(AUTHORING_REFERENCE_BUDGET_VERSION, 4);
    assert_eq!(AUTHORING_SEQUENCE_LOCALITY_BUDGET_VERSION, 3);
    let operations = SEQUENCE_LOCALITY_OPERATIONS.iter().copied().collect::<BTreeSet<_>>();
    assert_eq!(operations.len(), SEQUENCE_LOCALITY_OPERATIONS.len());

    let relative_limited = sequence_locality_operation_evidence(
        SEQUENCE_LOCALITY_OPERATIONS[0],
        5_000,
        50_000,
        8_000,
        60_000,
        100_000,
    );
    assert_eq!(relative_limited.relative_median_budget_us, 15_000);
    assert_eq!(
        relative_limited.project_heavy_reference_sample_p95_budget_us,
        100_000
    );
    assert!(!relative_limited.within_relative_median_budget);
    assert!(relative_limited.within_project_heavy_reference_budget);
    assert!(!relative_limited.passed);

    let reference_tail_limited = sequence_locality_operation_evidence(
        SEQUENCE_LOCALITY_OPERATIONS[0],
        60_000,
        110_000,
        80_000,
        110_000,
        100_000,
    );
    assert_eq!(reference_tail_limited.relative_median_budget_us, 125_000);
    assert!(reference_tail_limited.within_relative_median_budget);
    assert!(!reference_tail_limited.within_project_heavy_reference_budget);
    assert!(!reference_tail_limited.passed);
    assert!(AUTHORING_SEQUENCE_LOCALITY_COMPARISON_SCOPE
        .contains("at_least_four_times_the_active_sequence_serialized_bytes"));
    assert!(AUTHORING_SEQUENCE_LOCALITY_COMPARISON_SCOPE
        .contains("sample_median_for_relative_locality"));
    assert!(AUTHORING_SEQUENCE_LOCALITY_COMPARISON_SCOPE
        .contains("nearest_rank_p95_required_to_pass_its_existing_reference_budget"));
}

#[test]
fn sequence_locality_fixture_strength_requires_four_active_sequences_of_payload() {
    let weak = assess_sequence_locality_fixture_strength(1_000, 3_999);
    assert_eq!(weak.required_unrelated_bytes, 4_000);
    assert_eq!(weak.candidate_multiplier_floor, 3);
    assert!(!weak.satisfied);

    let exact = assess_sequence_locality_fixture_strength(1_000, 4_000);
    assert_eq!(exact.required_unrelated_bytes, 4_000);
    assert_eq!(exact.candidate_multiplier_floor, 4);
    assert!(exact.satisfied);

    assert!(!assess_sequence_locality_fixture_strength(0, usize::MAX).satisfied);
    assert!(
        !assess_sequence_locality_fixture_strength(usize::MAX, usize::MAX).satisfied,
        "overflowing the required byte count must fail closed"
    );
}

#[test]
fn authoring_reference_budgets_are_explicit_sublinear_and_capped() {
    let classes = [
        AuthoringOperationClass::Interactive,
        AuthoringOperationClass::Structural,
        AuthoringOperationClass::ProjectStructural,
        AuthoringOperationClass::ProjectSetting,
        AuthoringOperationClass::History,
        AuthoringOperationClass::SnapshotCapture,
        AuthoringOperationClass::SnapshotPublish,
    ];
    assert_eq!(
        classes.map(|class| reference_budget_us(AUTHORING_SCALE_MATRIX[0], class)),
        [30_000, 50_000, 150_000, 80_000, 50_000, 100_000, 8_000_000]
    );
    assert_eq!(
        classes.map(|class| reference_budget_us(AUTHORING_SCALE_MATRIX[1], class)),
        [45_000, 90_000, 300_000, 150_000, 90_000, 200_000, 12_000_000]
    );
    assert_eq!(
        classes.map(|class| reference_budget_us(AUTHORING_SCALE_MATRIX[2], class)),
        [75_000, 150_000, 600_000, 250_000, 150_000, 400_000, 20_000_000]
    );
    let beyond_matrix = AuthoringScale { duration_minutes: 240, ..AUTHORING_SCALE_MATRIX[2] };
    for class in classes {
        let five_minutes = reference_budget_us(AUTHORING_SCALE_MATRIX[0], class);
        let thirty_minutes = reference_budget_us(AUTHORING_SCALE_MATRIX[1], class);
        let one_hundred_twenty_minutes = reference_budget_us(AUTHORING_SCALE_MATRIX[2], class);
        assert!(five_minutes <= thirty_minutes);
        assert!(thirty_minutes <= one_hundred_twenty_minutes);
        assert!(thirty_minutes < five_minutes.saturating_mul(6));
        assert!(one_hundred_twenty_minutes < five_minutes.saturating_mul(24));
    }
    assert_eq!(
        classes.map(|class| reference_budget_us(beyond_matrix, class)),
        classes.map(|class| reference_budget_us(AUTHORING_SCALE_MATRIX[2], class))
    );
}

#[test]
fn authoring_perf_harness_light_smoke_covers_every_operation() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let mut paired_reports = Vec::with_capacity(AuthoringFixtureProfile::ALL.len());
    let expected_operations = BTreeSet::from([
        "history.project_redo",
        "history.project_undo",
        "history.sequence_redo",
        "history.sequence_undo",
        "project.precompose",
        "project.proxy_mode_toggle",
        "project.setting",
        "sequence.extract",
        "sequence.insert",
        "sequence.keyframe",
        "sequence.lift",
        "sequence.move_clip",
        "sequence.split",
        "snapshot.autosave_publish",
        "snapshot.manual_capture",
        "snapshot.manual_publish",
    ]);
    for profile in AuthoringFixtureProfile::ALL {
        let root = unique_authoring_perf_root(LIGHT_AUTHORING_SCALE, profile);
        let result = run_authoring_scale(&root, LIGHT_AUTHORING_SCALE, profile, false);
        let _ = fs::remove_dir_all(&root);
        let report = result?;
        let operations = report.cases.iter().map(|case| case.operation).collect::<BTreeSet<_>>();
        assert_eq!(operations, expected_operations);
        assert_eq!(report.profile, profile);
        assert_eq!(report.fixture.profile, profile);
        let expected_unrelated_clips = match profile {
            AuthoringFixtureProfile::ActiveHeavy => 0,
            AuthoringFixtureProfile::ProjectHeavy => LIGHT_AUTHORING_SCALE
                .active_payload_clips
                .saturating_mul(PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER),
        };
        let expected_unrelated_effects = match profile {
            AuthoringFixtureProfile::ActiveHeavy => 0,
            AuthoringFixtureProfile::ProjectHeavy => LIGHT_AUTHORING_SCALE
                .active_effects
                .saturating_mul(PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER),
        };
        let expected_unrelated_keyframes = match profile {
            AuthoringFixtureProfile::ActiveHeavy => 0,
            AuthoringFixtureProfile::ProjectHeavy => LIGHT_AUTHORING_SCALE
                .active_keyframes
                .saturating_mul(PROJECT_HEAVY_UNRELATED_SHAPE_MULTIPLIER),
        };
        assert_eq!(
            report.fixture.total_payload_clips,
            LIGHT_AUTHORING_SCALE
                .active_payload_clips
                .saturating_add(expected_unrelated_clips)
        );
        assert_eq!(
            report.fixture.total_effects,
            LIGHT_AUTHORING_SCALE.active_effects.saturating_add(expected_unrelated_effects)
        );
        assert_eq!(
            report.fixture.total_keyframes,
            LIGHT_AUTHORING_SCALE
                .active_keyframes
                .saturating_add(expected_unrelated_keyframes)
        );
        assert_eq!(
            report.fixture.nesting_depth,
            LIGHT_AUTHORING_SCALE.nesting_depth
        );
        assert_eq!(
            report.fixture.proxy_mode_asset_count,
            LIGHT_AUTHORING_SCALE.active_payload_clips.saturating_mul(4).max(64)
        );
        assert!(report.fixture.active_sequence_json_bytes > 0);
        match profile {
            AuthoringFixtureProfile::ActiveHeavy => {
                assert_eq!(
                    report.fixture.active_payload_clips,
                    LIGHT_AUTHORING_SCALE.active_payload_clips
                );
                assert_eq!(report.fixture.unrelated_payload_clips, 0);
                assert_eq!(report.fixture.unrelated_project_json_bytes, 0);
            }
            AuthoringFixtureProfile::ProjectHeavy => {
                assert_eq!(
                    report.fixture.active_payload_clips,
                    LIGHT_AUTHORING_SCALE.active_payload_clips
                );
                assert_eq!(
                    report.fixture.unrelated_payload_clips,
                    expected_unrelated_clips
                );
                assert_eq!(report.fixture.unrelated_effects, expected_unrelated_effects);
                assert_eq!(
                    report.fixture.unrelated_keyframes,
                    expected_unrelated_keyframes
                );
                assert!(report.fixture.unrelated_project_json_bytes > 0);
                assert!(
                    report.fixture.sequence_count
                        > LIGHT_AUTHORING_SCALE.nesting_depth.saturating_add(1)
                );
            }
        }
        assert_eq!(
            report.memory_evidence_scope,
            AUTHORING_MEMORY_EVIDENCE_SCOPE
        );
        assert_eq!(
            report.timing_evidence_scope,
            AUTHORING_TIMING_EVIDENCE_SCOPE
        );
        assert!(!report.timing_gate_requested);
        assert_eq!(
            report.process_memory.measurement_scope,
            AUTHORING_PROCESS_MEMORY_EVIDENCE_SCOPE
        );
        assert_eq!(
            report.process_memory.budget_version,
            AUTHORING_PROCESS_MEMORY_BUDGET_VERSION
        );
        assert_eq!(
            report.process_memory.max_peak_private_committed_delta_bytes,
            authoring_process_memory_budget_bytes(LIGHT_AUTHORING_SCALE, profile)
                .expect("light process-memory budget")
        );
        assert_eq!(
            report.process_memory.max_peak_private_committed_bytes,
            AUTHORING_PROCESS_MEMORY_ABSOLUTE_PRIVATE_COMMIT_LIMIT_BYTES
        );
        assert!(!report.process_memory.gate_required);
        assert!(report.process_memory.passed);
        assert!(report.process_memory.attempted_samples >= 2);
        if report.process_memory.evidence_complete {
            assert!(report.process_memory.discovery_available);
            assert!(report.process_memory.backend.is_some());
            assert!(
                report.process_memory.baseline_private_samples
                    >= u64::try_from(AUTHORING_PROCESS_MEMORY_BASELINE_SAMPLES)
                        .expect("baseline sample count")
            );
            assert!(report.process_memory.background_private_samples >= 1);
            assert!(report.process_memory.final_private_sample_observed);
            assert!(report.process_memory.background_sampling_started);
            assert!(report.process_memory.background_sampling_stopped);
        }
        assert!(report.sequence_locality.is_none());
        assert!(report.build_machine_attestation.logical_cpu_count >= 1);
        assert!(report.all_history_payloads_within_budget);
        assert_eq!(
            report.history_depth.requested_probe_edits,
            LIGHT_HISTORY_DEPTH_PROBE_EDITS
        );
        assert_eq!(
            report.history_depth.committed_probe_edits,
            LIGHT_HISTORY_DEPTH_PROBE_EDITS
        );
        assert_eq!(
            report.history_depth.verified_lightweight_sequence_metadata_probe_edits,
            LIGHT_HISTORY_DEPTH_PROBE_EDITS / 2
        );
        assert_eq!(
            report.history_depth.verified_clip_container_cow_detach_probe_edits,
            LIGHT_HISTORY_DEPTH_PROBE_EDITS / 2
        );
        assert_eq!(
            report.history_depth.probe_edits_undone,
            LIGHT_HISTORY_DEPTH_PROBE_EDITS
        );
        assert_eq!(
            report.history_depth.redo_entries_after_probe_undo,
            LIGHT_HISTORY_DEPTH_PROBE_EDITS
        );
        assert_eq!(
            report.history_depth.retention_disabled_entries_during_probe,
            0
        );
        assert_eq!(
            report.history_depth.oversize_dropped_entries_during_probe,
            0
        );
        assert_eq!(
            report.history_depth.barrier_discarded_entries_during_probe,
            0
        );
        assert!(report.history_depth.post_probe_commit_state_within_history_budget);
        assert!(report.history_depth.budget_evictions_confined_to_pre_probe_entries);
        assert!(report.history_depth.all_probe_edits_retained_and_undone);
        assert!(
            report
                .history_depth
                .complete_structured_project_document_restored_after_probe_undo
        );
        assert_eq!(
            report.history_depth.structured_state_comparison_scope,
            AUTHORING_STRUCTURED_STATE_COMPARISON_SCOPE
        );
        assert!(report.cases.iter().all(|case| {
            case.warmup_iterations == 0
                && case.memory.measurement_scope == AUTHORING_MEMORY_EVIDENCE_SCOPE
                && case.memory.observations == case.iterations
                && case.memory.every_observation_within_history_budget
        }));
        for case in &report.cases {
            let expected_scope = match case.operation {
                "history.sequence_undo" | "history.sequence_redo" => {
                    Some(AuthoringHistoryCommandScope::Sequence)
                }
                "history.project_undo" | "history.project_redo" => {
                    Some(AuthoringHistoryCommandScope::Project)
                }
                _ => None,
            };
            assert_eq!(case.history_command_scope, expected_scope);
            if case.operation.starts_with("snapshot.") {
                assert!(case.reversibility.is_none());
            } else {
                let reversibility =
                    case.reversibility.as_ref().expect("reversible authoring operation evidence");
                assert_eq!(
                    reversibility.verified_iterations,
                    case.warmup_iterations.saturating_add(case.iterations)
                );
                assert!(reversibility.every_after_state_differed_from_before);
                assert!(reversibility.every_roundtrip_restored_before_state);
                assert_eq!(
                    reversibility.comparison_scope,
                    AUTHORING_STRUCTURED_STATE_COMPARISON_SCOPE
                );
            }
        }
        paired_reports.push(report);
    }
    let locality = evaluate_sequence_locality(&paired_reports[0], &paired_reports[1])?;
    assert_eq!(
        locality.operations.len(),
        SEQUENCE_LOCALITY_OPERATIONS.len()
    );
    assert!(locality.active_sequence_serialized_size_equal);
    assert!(locality.active_sequence_workload_shape_equivalent);
    assert_eq!(
        locality.active_sequence_serialized_size_bytes,
        paired_reports[0].fixture.active_sequence_json_bytes
    );
    assert_eq!(
        paired_reports[0].fixture.active_sequence_json_bytes,
        paired_reports[1].fixture.active_sequence_json_bytes
    );
    assert_eq!(
        locality.candidate_additional_sequence_count,
        LIGHT_AUTHORING_SCALE.project_heavy_unrelated_sequences
    );
    assert_eq!(
        locality.minimum_unrelated_serialized_bytes_multiplier,
        SEQUENCE_LOCALITY_MIN_UNRELATED_SERIALIZED_BYTES_MULTIPLIER
    );
    assert_eq!(
        locality.required_unrelated_project_json_bytes,
        locality
            .active_sequence_serialized_size_bytes
            .saturating_mul(SEQUENCE_LOCALITY_MIN_UNRELATED_SERIALIZED_BYTES_MULTIPLIER)
    );
    assert!(locality.fixture_strength_satisfied);
    assert!(
        locality.candidate_unrelated_project_json_bytes
            >= locality.required_unrelated_project_json_bytes
    );
    assert!(
        locality.candidate_unrelated_serialized_bytes_multiplier_floor
            >= SEQUENCE_LOCALITY_MIN_UNRELATED_SERIALIZED_BYTES_MULTIPLIER
    );
    Ok(())
}

#[test]
fn history_depth_200_undo_restores_complete_structured_project_document() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let root =
        unique_authoring_perf_root(LIGHT_AUTHORING_SCALE, AuthoringFixtureProfile::ActiveHeavy);
    let result = (|| {
        let (mut state, _, _) = build_authoring_fixture(
            &root,
            LIGHT_AUTHORING_SCALE,
            AuthoringFixtureProfile::ActiveHeavy,
        )?;
        let before = structured_authoring_state(&state)?;
        let evidence =
            measure_history_depth_with_count(&mut state, FULL_HISTORY_DEPTH_PROBE_EDITS)?;

        assert_eq!(
            evidence.requested_probe_edits,
            FULL_HISTORY_DEPTH_PROBE_EDITS
        );
        assert_eq!(
            evidence.committed_probe_edits,
            FULL_HISTORY_DEPTH_PROBE_EDITS
        );
        assert_eq!(evidence.probe_edits_undone, FULL_HISTORY_DEPTH_PROBE_EDITS);
        assert_eq!(
            evidence.redo_entries_after_probe_undo,
            FULL_HISTORY_DEPTH_PROBE_EDITS
        );
        assert!(evidence.all_probe_edits_retained_and_undone);
        assert!(evidence.complete_structured_project_document_restored_after_probe_undo);
        assert!(structured_authoring_state_eq(
            &structured_authoring_state(&state)?,
            &before
        ));
        Ok::<(), anyhow::Error>(())
    })();
    let _ = fs::remove_dir_all(root);
    result
}

fn test_source_attestation(revision: &str) -> AuthoringSourceTreeAttestation {
    AuthoringSourceTreeAttestation {
        source_revision: revision.to_owned(),
        revision_kind: "test_source_tree",
        source_tree_scope: "test_scope",
        files_hashed: 2,
        bytes_hashed: 42,
        source_dirty: Some(true),
        dirty_attestation: "test_dirty_attestation",
    }
}

#[test]
fn authoring_evidence_protocol_is_fresh_ordinal_complete_and_source_bound() -> anyhow::Result<()> {
    let root = std::env::temp_dir().join(format!(
        "mondrian-authoring-evidence-protocol-{}-{}",
        std::process::id(),
        unix_time_ns()?
    ));
    let path = root.join("authoring.jsonl");
    fs::create_dir_all(&root)?;
    fs::write(&path, b"{\"stale\":true}\n")?;
    let source = test_source_attestation("sha256:test-run-source");
    let mut run = AuthoringEvidenceRun::new(
        Some(path.clone()),
        true,
        "test-authoring-run".to_owned(),
        source.clone(),
        123,
    )?;
    for ordinal in 1..=AUTHORING_EVIDENCE_REPORT_COUNT {
        run.emit_report(&serde_json::json!({"payload_ordinal": ordinal}))?;
    }
    run.complete(source.clone())?;

    let records = fs::read_to_string(&path)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<serde_json::Result<Vec<_>>>()?;
    assert_eq!(records.len(), AUTHORING_EVIDENCE_REPORT_COUNT + 1);
    for (index, record) in records[..AUTHORING_EVIDENCE_REPORT_COUNT].iter().enumerate() {
        assert_eq!(record["record_type"], "authoring_scale_report");
        assert_eq!(record["protocol_schema_version"], 1);
        assert_eq!(record["run_id"], "test-authoring-run");
        assert_eq!(record["ordinal"], index + 1);
        assert_eq!(
            record["expected_report_count"],
            AUTHORING_EVIDENCE_REPORT_COUNT
        );
        assert_eq!(record["source"]["source_revision"], source.source_revision);
        assert_eq!(record["source"]["source_dirty"], true);
        assert_eq!(record["report"]["payload_ordinal"], index + 1);
        assert!(record.get("stale").is_none());
    }
    let completion = records.last().expect("completion record");
    assert_eq!(completion["record_type"], "authoring_run_completion");
    assert_eq!(completion["run_id"], "test-authoring-run");
    assert_eq!(
        completion["completed_report_count"],
        AUTHORING_EVIDENCE_REPORT_COUNT
    );
    assert_eq!(
        completion["expected_report_count"],
        AUTHORING_EVIDENCE_REPORT_COUNT
    );
    assert_eq!(
        completion["last_report_ordinal"],
        AUTHORING_EVIDENCE_REPORT_COUNT
    );
    assert_eq!(
        completion["source"]["source_revision"],
        source.source_revision
    );
    assert_eq!(completion["source_unchanged_during_run"], true);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn authoring_evidence_protocol_fails_closed_on_missing_or_incomplete_authority(
) -> anyhow::Result<()> {
    let source = test_source_attestation("sha256:test-run-source");
    let missing_path =
        AuthoringEvidenceRun::new(None, true, "missing-output".to_owned(), source.clone(), 1);
    assert!(missing_path
        .err()
        .is_some_and(|error| error.to_string().contains("requires an explicit")));

    let mut incomplete =
        AuthoringEvidenceRun::new(None, false, "incomplete".to_owned(), source.clone(), 1)?;
    for ordinal in 1..AUTHORING_EVIDENCE_REPORT_COUNT {
        incomplete.emit_report(&serde_json::json!({"ordinal": ordinal}))?;
    }
    assert!(incomplete
        .complete(source.clone())
        .expect_err("incomplete evidence must not complete")
        .to_string()
        .contains("is incomplete"));

    let mut overrun =
        AuthoringEvidenceRun::new(None, false, "overrun".to_owned(), source.clone(), 1)?;
    for ordinal in 1..=AUTHORING_EVIDENCE_REPORT_COUNT {
        overrun.emit_report(&serde_json::json!({"ordinal": ordinal}))?;
    }
    assert!(overrun
        .emit_report(&serde_json::json!({"ordinal": AUTHORING_EVIDENCE_REPORT_COUNT + 1}))
        .expect_err("a seventh report must be rejected")
        .to_string()
        .contains("already emitted all"));
    overrun.complete(source.clone())?;

    let mut changed_source =
        AuthoringEvidenceRun::new(None, false, "changed-source".to_owned(), source.clone(), 1)?;
    for ordinal in 1..=AUTHORING_EVIDENCE_REPORT_COUNT {
        changed_source.emit_report(&serde_json::json!({"ordinal": ordinal}))?;
    }
    assert!(changed_source
        .complete(test_source_attestation("sha256:changed"))
        .expect_err("changed source must not complete")
        .to_string()
        .contains("source tree changed"));
    Ok(())
}

#[test]
#[ignore = "development 5/30/120 minute authoring matrix; emits versioned JSONL"]
fn large_project_authoring_perf_matrix() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");
    let enforce = timing_gate_from_env()?;
    let mut evidence_run = AuthoringEvidenceRun::from_environment(enforce)?;
    for scale in AUTHORING_SCALE_MATRIX {
        let active_root = unique_authoring_perf_root(scale, AuthoringFixtureProfile::ActiveHeavy);
        let active_result = run_authoring_scale(
            &active_root,
            scale,
            AuthoringFixtureProfile::ActiveHeavy,
            enforce,
        );
        let _ = fs::remove_dir_all(&active_root);
        let active_heavy = active_result?;

        let project_root = unique_authoring_perf_root(scale, AuthoringFixtureProfile::ProjectHeavy);
        let project_result = run_authoring_scale(
            &project_root,
            scale,
            AuthoringFixtureProfile::ProjectHeavy,
            enforce,
        );
        let _ = fs::remove_dir_all(&project_root);
        let mut project_heavy = project_result?;
        project_heavy.sequence_locality =
            Some(evaluate_sequence_locality(&active_heavy, &project_heavy)?);

        evidence_run.emit_report(&active_heavy)?;
        evidence_run.emit_report(&project_heavy)?;
        if enforce {
            enforce_authoring_performance_pair(&active_heavy, &project_heavy)?;
        }
    }
    evidence_run.complete(capture_authoring_source_tree_attestation()?)
}
