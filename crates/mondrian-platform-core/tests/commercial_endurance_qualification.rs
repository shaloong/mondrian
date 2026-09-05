use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use mondrian_platform_core::{
    EnduranceCounterRequirement, EnduranceCounters, EnduranceGauges, EnduranceMemoryRequirement,
    EndurancePhaseChunkReceipt, EndurancePhaseKind, EndurancePhaseManifest,
    EndurancePhaseProducerEvidence, EndurancePhaseRequirement, EndurancePhaseTerminalEvidence,
    EndurancePhaseTerminalStatus, EnduranceProcessMemorySample, EnduranceQualificationError,
    EnduranceQualificationProfile, EnduranceQualificationStatus, EnduranceRunManifest,
    EnduranceRunOwnerClosureEvidence, EnduranceSample, EnduranceSampleChunk,
    PreparedEnduranceQualification, ProcessEventLoopOwnerClosureEvidence,
    ProcessMemoryProbeBackend, ProcessMemoryScope, ProcessPrivateMemoryMetric,
};
use sha2::{Digest, Sha256};

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SOURCE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn file_sha256(path: &Path) -> String {
    format!(
        "{:x}",
        Sha256::digest(std::fs::read(path).expect("read digest input"))
    )
}

fn counters_for(kind: EndurancePhaseKind, progress: u64) -> EnduranceCounters {
    match kind {
        EndurancePhaseKind::PlaybackReference => EnduranceCounters {
            playback_presented_frames: progress,
            reference_scheduled_frames: progress,
            reference_completed_frames: progress,
            reference_hardware_timestamps: progress,
            ..EnduranceCounters::default()
        },
        EndurancePhaseKind::ContinuousExport => EnduranceCounters {
            export_admissions: progress,
            export_completions: progress,
            export_frames: progress.saturating_mul(10),
            export_durable_artifacts: progress,
            export_artifacts_verified: progress,
            ..EnduranceCounters::default()
        },
        EndurancePhaseKind::ConcurrentRecovery => EnduranceCounters {
            playback_presented_frames: progress,
            reference_scheduled_frames: progress,
            reference_completed_frames: progress,
            reference_hardware_timestamps: progress,
            export_admissions: progress.saturating_mul(2),
            export_completions: progress,
            export_frames: progress.saturating_mul(10),
            export_durable_artifacts: progress,
            export_artifacts_verified: progress,
            export_cancellations: progress,
            recovery_cycles: progress,
            ..EnduranceCounters::default()
        },
    }
}

fn counter_requirement(kind: EndurancePhaseKind) -> EnduranceCounterRequirement {
    EnduranceCounterRequirement {
        minimum_playback_presented_frames: u64::from(kind != EndurancePhaseKind::ContinuousExport),
        maximum_playback_late_frames: 0,
        maximum_playback_failed_frames: 0,
        maximum_audio_underruns: 0,
        minimum_reference_completed_frames: u64::from(kind != EndurancePhaseKind::ContinuousExport),
        maximum_reference_late_frames: 0,
        maximum_reference_dropped_frames: 0,
        maximum_reference_flushed_frames: 0,
        maximum_reference_aborted_frames: 0,
        minimum_reference_hardware_timestamps: u64::from(
            kind != EndurancePhaseKind::ContinuousExport,
        ),
        maximum_reference_hardware_time_gap_us: if kind == EndurancePhaseKind::ContinuousExport {
            0
        } else {
            50_000
        },
        require_hardware_reference_output: kind != EndurancePhaseKind::ContinuousExport,
        require_external_reference_lock: kind != EndurancePhaseKind::ContinuousExport,
        minimum_verified_exports: u64::from(kind != EndurancePhaseKind::PlaybackReference),
        minimum_export_frames: 10 * u64::from(kind != EndurancePhaseKind::PlaybackReference),
        minimum_export_cancellations: u64::from(kind == EndurancePhaseKind::ConcurrentRecovery),
        maximum_export_cancellations: if kind == EndurancePhaseKind::ConcurrentRecovery {
            2
        } else {
            0
        },
        maximum_export_rejections: 0,
        minimum_recovery_cycles: u64::from(kind == EndurancePhaseKind::ConcurrentRecovery),
        maximum_queue_depth: 8,
        require_quiescent_terminal: true,
        require_worker_shutdown: true,
    }
}

fn profile() -> EnduranceQualificationProfile {
    let phases = [
        (
            "01-playback-reference",
            EndurancePhaseKind::PlaybackReference,
        ),
        ("02-continuous-export", EndurancePhaseKind::ContinuousExport),
        (
            "03-concurrent-recovery",
            EndurancePhaseKind::ConcurrentRecovery,
        ),
    ]
    .into_iter()
    .map(|(phase_id, kind)| EndurancePhaseRequirement {
        phase_id: phase_id.to_owned(),
        kind,
        workload_sha256: SHA.to_owned(),
        producer_owner: "mondrian-app".to_owned(),
        producer_verifier_id: "mondrian-app-endurance-capture-v1".to_owned(),
        producer_report_schema_version: 1,
        minimum_duration_us: 20,
        warmup_duration_us: 5,
        minimum_samples: 3,
        maximum_playback_progress_gap_us: if kind == EndurancePhaseKind::ContinuousExport {
            0
        } else {
            12
        },
        maximum_reference_progress_gap_us: if kind == EndurancePhaseKind::ContinuousExport {
            0
        } else {
            12
        },
        maximum_export_progress_gap_us: if kind == EndurancePhaseKind::PlaybackReference {
            0
        } else {
            12
        },
        maximum_recovery_progress_gap_us: if kind == EndurancePhaseKind::ConcurrentRecovery {
            12
        } else {
            0
        },
        memory: EnduranceMemoryRequirement {
            backend: ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
            metric: ProcessPrivateMemoryMetric::WindowsPrivateCommit,
            maximum_private_memory_bytes: 1_000,
            maximum_settled_growth_bytes: 10,
            maximum_slope_bytes_per_hour: 10,
        },
        counters: counter_requirement(kind),
    })
    .collect();
    EnduranceQualificationProfile {
        schema_version: 1,
        qualification_id: "commercial-endurance-v1".to_owned(),
        edition: "2026-08-31".to_owned(),
        sample_interval_us: 10,
        maximum_sample_gap_us: 12,
        maximum_probe_latency_us: 2,
        maximum_samples_per_chunk: 16,
        maximum_chunks_per_phase: 16,
        maximum_producer_events_per_phase: 32,
        phases,
    }
}

fn memory(bytes: u64) -> EnduranceProcessMemorySample {
    EnduranceProcessMemorySample {
        scope: ProcessMemoryScope::ProductProcessTree,
        backend: ProcessMemoryProbeBackend::WindowsToolhelpProcessTree,
        metric: ProcessPrivateMemoryMetric::WindowsPrivateCommit,
        observed_process_count: 2,
        inventory_attempts: 1,
        inventory_complete: true,
        private_memory_bytes: bytes,
        resident_bytes: bytes,
    }
}

fn phase(
    phase_id: &str,
    kind: EndurancePhaseKind,
    started_at_run_us: u64,
) -> (EndurancePhaseManifest, EnduranceSampleChunk) {
    let samples = (0_u64..3)
        .map(|sequence| EnduranceSample {
            sequence,
            scheduled_at_us: sequence * 10,
            started_at_us: sequence * 10,
            completed_at_us: sequence * 10 + 1,
            process_memory: memory(100),
            reference_output_hardware_backed: kind != EndurancePhaseKind::ContinuousExport,
            external_reference_locked: kind != EndurancePhaseKind::ContinuousExport,
            reference_hardware_maximum_gap_us: if kind == EndurancePhaseKind::ContinuousExport {
                0
            } else {
                40_000
            },
            export_shutdown_requested: true,
            export_worker_running: false,
            export_worker_terminated: true,
            counters: counters_for(kind, sequence),
            gauges: EnduranceGauges::default(),
        })
        .collect();
    let chunk = EnduranceSampleChunk {
        schema_version: 1,
        phase_id: phase_id.to_owned(),
        chunk_index: 0,
        previous_chunk_sha256: None,
        samples,
        chunk_sha256: String::new(),
    }
    .seal()
    .expect("seal chunk");
    let file_name = format!("{phase_id}-0000.json");
    let receipt = EndurancePhaseChunkReceipt::from_chunk(file_name, &chunk).expect("chunk receipt");
    let final_counters = counters_for(kind, 2);
    (
        EndurancePhaseManifest {
            phase_id: phase_id.to_owned(),
            workload_sha256: SHA.to_owned(),
            started_at_run_us,
            completed_at_run_us: started_at_run_us + 21,
            producer: EndurancePhaseProducerEvidence {
                owner: "mondrian-app".to_owned(),
                verifier_id: "mondrian-app-endurance-capture-v1".to_owned(),
                report_schema_version: 1,
                report_sha256: SHA.to_owned(),
                report_file_name: format!("{phase_id}-report.json"),
                raw_evidence_sha256: SHA.to_owned(),
                raw_evidence_file_name: format!("{phase_id}-raw.json"),
                reference_output_hardware_backed: kind != EndurancePhaseKind::ContinuousExport,
                external_reference_required: kind != EndurancePhaseKind::ContinuousExport,
            },
            chunks: vec![receipt],
            terminal: EndurancePhaseTerminalEvidence {
                status: EndurancePhaseTerminalStatus::Completed,
                counters: final_counters,
                gauges: EnduranceGauges::default(),
                workers_terminated: true,
                child_processes_reaped: true,
            },
        },
        chunk,
    )
}

fn qualified_fixture() -> (
    PreparedEnduranceQualification,
    EnduranceRunManifest,
    BTreeMap<String, EnduranceSampleChunk>,
) {
    let prepared = PreparedEnduranceQualification::compile(profile()).expect("compile profile");
    let phases = [
        (
            "01-playback-reference",
            EndurancePhaseKind::PlaybackReference,
            0,
        ),
        (
            "02-continuous-export",
            EndurancePhaseKind::ContinuousExport,
            30,
        ),
        (
            "03-concurrent-recovery",
            EndurancePhaseKind::ConcurrentRecovery,
            60,
        ),
    ];
    let mut manifests = Vec::new();
    let mut chunks = BTreeMap::new();
    for (phase_id, kind, started) in phases {
        let (manifest, chunk) = phase(phase_id, kind, started);
        chunks.insert(manifest.chunks[0].file_name.clone(), chunk);
        manifests.push(manifest);
    }
    let run = EnduranceRunManifest {
        schema_version: 3,
        run_id: "commercial-run-001".to_owned(),
        profile_sha256: prepared.profile_sha256().to_owned(),
        source_revision: SOURCE.to_owned(),
        release_candidate_id: "mondrian-0.2.0-rc1".to_owned(),
        product_artifact_sha256: SHA.to_owned(),
        runtime_image_sha256: SHA.to_owned(),
        build_provenance_sha256: SHA.to_owned(),
        machine_report_sha256: SHA.to_owned(),
        platform_cell_sha256: SHA.to_owned(),
        machine_plan_sha256: SHA.to_owned(),
        capture_authority_sha256: SHA.to_owned(),
        environment_before_sha256: SHA.to_owned(),
        environment_after_sha256: SHA.to_owned(),
        owner_closure: EnduranceRunOwnerClosureEvidence::event_loop(
            ProcessEventLoopOwnerClosureEvidence::after_rust_owner_drop(),
        ),
        phases: manifests,
    };
    (prepared, run, chunks)
}

#[test]
fn complete_serial_campaign_qualifies_and_self_verifies() {
    let (prepared, run, chunks) = qualified_fixture();
    let report = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate run");

    assert_eq!(report.status, EnduranceQualificationStatus::Qualified);
    assert_eq!(report.phases.len(), 3);
    assert!(report.phases.iter().all(|phase| phase.failed_checks.is_empty()));
    assert!(report.verify_evidence());
}

#[test]
fn schema_three_owner_closure_and_machine_plan_are_hashed_into_report() {
    let (prepared, mut run, chunks) = qualified_fixture();
    let expected_machine_plan = run.machine_plan_sha256.clone();
    let mut report = prepared
        .evaluate(run.clone(), |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate schema-three run");
    assert_eq!(report.schema_version, 3);
    assert!(report.owner_closure.all_owned_authority_released());
    assert_eq!(report.machine_plan_sha256, expected_machine_plan);
    report.machine_plan_sha256 = "b".repeat(64);
    assert!(!report.verify_evidence());

    run.schema_version = 1;
    assert!(matches!(
        prepared.evaluate(run, |_| Err(EnduranceQualificationError::EmptyChunk)),
        Err(EnduranceQualificationError::UnsupportedRunSchema { actual: 1 })
    ));
}

#[test]
fn started_concurrent_recovery_rejects_not_applicable_run_owner_closure() {
    let (prepared, mut run, chunks) = qualified_fixture();
    run.owner_closure = EnduranceRunOwnerClosureEvidence::not_applicable();

    assert!(matches!(
        prepared.evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        }),
        Err(EnduranceQualificationError::InvalidRunOwnerClosure)
    ));
}

#[test]
fn malformed_event_loop_owner_closure_fails_before_phase_replay() {
    let (prepared, run, _) = qualified_fixture();
    let mut value = serde_json::to_value(run).expect("serialize run");
    value["owner_closure"]["closure"]["physical_native_termination_verified"] =
        serde_json::json!(true);
    let run = serde_json::from_value(value).expect("deserialize malformed closure");

    assert!(matches!(
        prepared.evaluate(run, |_| Err(EnduranceQualificationError::EmptyChunk)),
        Err(EnduranceQualificationError::InvalidRunOwnerClosure)
    ));
}

#[test]
fn malformed_machine_plan_digest_is_rejected_before_phase_replay() {
    let (prepared, mut run, _) = qualified_fixture();
    run.machine_plan_sha256 = "A".repeat(64);
    assert!(matches!(
        prepared.evaluate(run, |_| Err(EnduranceQualificationError::EmptyChunk)),
        Err(EnduranceQualificationError::InvalidSha256 { field: "machine_plan_sha256" })
    ));
}

#[test]
fn missing_phase_is_incomplete_not_qualified() {
    let (prepared, mut run, chunks) = qualified_fixture();
    run.phases.pop();
    let report = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate incomplete run");

    assert_eq!(report.status, EnduranceQualificationStatus::Incomplete);
    assert_eq!(report.missing_phases, vec!["03-concurrent-recovery"]);
}

#[test]
fn not_run_phase_remains_explicitly_incomplete() {
    let (prepared, mut run, chunks) = qualified_fixture();
    let phase = &mut run.phases[0];
    phase.chunks.clear();
    phase.completed_at_run_us = phase.started_at_run_us;
    phase.terminal.status = EndurancePhaseTerminalStatus::NotRun;
    phase.terminal.counters = EnduranceCounters::default();
    let report = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate not-run phase");

    assert_eq!(report.status, EnduranceQualificationStatus::Incomplete);
    assert_eq!(report.phases[0].failed_checks, vec!["producer_not_run"]);
}

#[test]
fn sustained_memory_growth_fails_the_phase() {
    let (prepared, run, mut chunks) = qualified_fixture();
    let chunk = chunks.get_mut("01-playback-reference-0000.json").expect("chunk");
    chunk.samples[1].process_memory.private_memory_bytes = 110;
    chunk.samples[2].process_memory.private_memory_bytes = 130;
    *chunk = chunk.clone().seal().expect("reseal");
    let mut run = run;
    run.phases[0].chunks[0] =
        EndurancePhaseChunkReceipt::from_chunk("01-playback-reference-0000.json", chunk)
            .expect("receipt");
    let report = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate leak");

    assert_eq!(report.status, EnduranceQualificationStatus::Failed);
    assert!(report.phases[0].failed_checks.contains(&"memory_growth".to_owned()));
    assert!(report.phases[0].failed_checks.contains(&"memory_slope".to_owned()));
}

#[test]
fn tampered_chunk_is_rejected_before_policy_evaluation() {
    let (prepared, run, mut chunks) = qualified_fixture();
    chunks.get_mut("02-continuous-export-0000.json").expect("chunk").samples[1]
        .counters
        .export_frames = 999;
    let error = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect_err("tamper must fail");

    assert!(matches!(
        error,
        EnduranceQualificationError::ChunkMismatch { .. }
    ));
}

#[test]
fn reference_and_export_accounting_fail_closed() {
    let (prepared, run, mut chunks) = qualified_fixture();
    let chunk = chunks.get_mut("03-concurrent-recovery-0000.json").expect("chunk");
    chunk.samples[1].counters.reference_scheduled_frames = 3;
    *chunk = chunk.clone().seal().expect("reseal");
    let mut run = run;
    run.phases[2].chunks[0] =
        EndurancePhaseChunkReceipt::from_chunk("03-concurrent-recovery-0000.json", chunk)
            .expect("receipt");
    let error = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect_err("accounting must fail");

    assert!(matches!(
        error,
        EnduranceQualificationError::ReferenceAccounting { .. }
    ));
}

#[test]
fn counter_regression_is_rejected_even_when_terminal_is_large_enough() {
    let (prepared, run, mut chunks) = qualified_fixture();
    let chunk = chunks.get_mut("02-continuous-export-0000.json").expect("chunk");
    chunk.samples[1].counters.export_frames = 20;
    chunk.samples[2].counters.export_frames = 10;
    *chunk = chunk.clone().seal().expect("reseal");
    let mut run = run;
    run.phases[1].chunks[0] =
        EndurancePhaseChunkReceipt::from_chunk("02-continuous-export-0000.json", chunk)
            .expect("receipt");
    let error = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect_err("counter regression must fail");

    assert!(matches!(
        error,
        EnduranceQualificationError::CounterRegression { .. }
    ));
}

#[test]
fn one_busy_domain_cannot_hide_a_stalled_export_domain() {
    let (prepared, mut run, mut chunks) = qualified_fixture();
    let chunk = chunks.get_mut("03-concurrent-recovery-0000.json").expect("chunk");
    for sample in &mut chunk.samples {
        sample.counters.export_admissions = 0;
        sample.counters.export_completions = 0;
        sample.counters.export_frames = 0;
        sample.counters.export_durable_artifacts = 0;
        sample.counters.export_activity_events = sample.sequence.saturating_add(1);
        sample.counters.export_artifacts_verified = 0;
        sample.counters.export_cancellations = 0;
    }
    *chunk = chunk.clone().seal().expect("reseal");
    run.phases[2].chunks[0] =
        EndurancePhaseChunkReceipt::from_chunk("03-concurrent-recovery-0000.json", chunk)
            .expect("receipt");
    run.phases[2].terminal.counters = chunk.samples.last().expect("last").counters;
    let report = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate stalled export");

    assert_eq!(report.status, EnduranceQualificationStatus::Failed);
    assert!(report.phases[2].failed_checks.contains(&"export_progress_gap".to_owned()));
}

#[test]
fn physical_reference_and_worker_closure_are_proved_by_the_final_samples() {
    let (prepared, mut run, mut chunks) = qualified_fixture();
    let chunk = chunks.get_mut("01-playback-reference-0000.json").expect("chunk");
    chunk.samples[1].reference_output_hardware_backed = false;
    let last = chunk.samples.last_mut().expect("last");
    last.export_shutdown_requested = false;
    last.export_worker_terminated = false;
    *chunk = chunk.clone().seal().expect("reseal");
    run.phases[0].chunks[0] =
        EndurancePhaseChunkReceipt::from_chunk("01-playback-reference-0000.json", chunk)
            .expect("receipt");
    let report = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect("evaluate false closure");

    assert_eq!(report.status, EnduranceQualificationStatus::Failed);
    assert!(report.phases[0].failed_checks.contains(&"physical_reference_output".to_owned()));
    assert!(report.phases[0].failed_checks.contains(&"worker_shutdown".to_owned()));
}

#[test]
fn manifest_cannot_substitute_an_unapproved_phase_producer() {
    let (prepared, mut run, chunks) = qualified_fixture();
    run.phases[0].producer.verifier_id = "substitute-producer".to_owned();
    let error = prepared
        .evaluate(run, |receipt| {
            chunks
                .get(&receipt.file_name)
                .cloned()
                .ok_or(EnduranceQualificationError::EmptyChunk)
        })
        .expect_err("producer substitution must fail");
    assert!(matches!(
        error,
        EnduranceQualificationError::PhaseContractMismatch { .. }
    ));
}

#[test]
fn checked_in_commercial_profile_compiles_as_a_72_hour_serial_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let profile: EnduranceQualificationProfile = serde_json::from_slice(
        &std::fs::read(root.join("tests/validation/commercial-endurance-qualification.json"))
            .expect("read commercial endurance profile"),
    )
    .expect("parse commercial endurance profile");
    let total_duration = profile.phases.iter().map(|phase| phase.minimum_duration_us).sum::<u64>();
    let prepared = PreparedEnduranceQualification::compile(profile).expect("compile profile");

    assert_eq!(total_duration, 72 * 60 * 60 * 1_000_000);
    assert_eq!(prepared.profile_sha256().len(), 64);
}

#[test]
fn replay_binary_reads_the_sealed_closure_and_creates_a_verified_report() {
    let (_prepared, run, chunks) = qualified_fixture();
    let temporary = tempfile::tempdir().expect("temporary replay directory");
    let profile_path = temporary.path().join("profile.json");
    let manifest_path = temporary.path().join("run.json");
    let chunk_directory = temporary.path().join("chunks");
    let report_path = temporary.path().join("report.json");
    std::fs::create_dir(&chunk_directory).expect("create chunk directory");
    std::fs::write(
        &profile_path,
        serde_json::to_vec_pretty(&profile()).expect("serialize profile"),
    )
    .expect("write profile");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&run).expect("serialize run"),
    )
    .expect("write run");
    for (file_name, chunk) in chunks {
        std::fs::write(
            chunk_directory.join(file_name),
            serde_json::to_vec_pretty(&chunk).expect("serialize chunk"),
        )
        .expect("write chunk");
    }

    let status = Command::new(env!("CARGO_BIN_EXE_mondrian-endurance-replay"))
        .arg(&profile_path)
        .arg(&manifest_path)
        .arg(&chunk_directory)
        .arg(&report_path)
        .status()
        .expect("run replay binary");
    assert!(status.success());

    let report: mondrian_platform_core::EnduranceQualificationReport =
        serde_json::from_slice(&std::fs::read(&report_path).expect("read report"))
            .expect("parse report");
    assert_eq!(report.status, EnduranceQualificationStatus::Qualified);
    assert!(report.verify_evidence());
}

#[cfg(windows)]
#[test]
fn powershell_verifier_checks_authority_and_complete_owner_evidence_closure() {
    let (_prepared, mut run, chunks) = qualified_fixture();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    let temporary = tempfile::tempdir().expect("temporary verifier directory");
    let evidence_directory = temporary.path().join("evidence");
    std::fs::create_dir(&evidence_directory).expect("create evidence directory");
    let authority_challenge = "test-only-unique-challenge";
    let run_id = run.run_id.clone();
    for phase in &mut run.phases {
        let report_path = evidence_directory.join(&phase.producer.report_file_name);
        let raw_path = evidence_directory.join(&phase.producer.raw_evidence_file_name);
        let mut events = Vec::new();
        for artifact in 0..phase.terminal.counters.export_artifacts_verified {
            events.push(serde_json::json!({
                "kind": "export_artifact_verified",
                "sequence": events.len(),
                "completed_at_us": artifact + 1,
                "artifact_id": format!("artifact-{artifact}"),
                "artifact_sha256": SHA,
                "validator_id": "independent-test-validator-v1",
                "validation_report_sha256": SHA,
            }));
        }
        let steps = [
            "seek",
            "surface_device_reopen",
            "export_cancel_retry",
            "cache_pressure",
        ];
        for cycle in 0..phase.terminal.counters.recovery_cycles {
            for step in steps {
                let operation_id = format!("{step}-{cycle}");
                let receipt = match step {
                    "seek" => serde_json::json!({
                        "step": step,
                        "schema_version": 3,
                        "cycle_index": cycle,
                        "operation_id": operation_id,
                        "sequence_binding_sha256": SHA,
                        "from_frame": 1,
                        "target_frame": 2,
                        "before_epoch": 1,
                        "after_epoch": 2,
                        "exact_picture_ready": true,
                    }),
                    "surface_device_reopen" => {
                        let shutdown_receipt_json = serde_json::to_string(&serde_json::json!({
                            "schema_version": 3,
                            "surface_generation": cycle + 1,
                            "device_generation": cycle + 3,
                            "worker_started": true,
                            "worker_terminated": true,
                            "worker_panicked": false,
                            "timed_out": false,
                            "retirement_requested": true,
                            "retirement_handoff_accepted": true,
                            "retirement_completed": true,
                            "renderer_retirement": {
                                "cpu_yuv_upload": "returned",
                                "native_device_removed": false,
                            },
                            "generation_terminal_kind": null,
                        }))
                        .expect("serialize Surface shutdown receipt");
                        let shutdown_receipt_sha256 =
                            format!("{:x}", Sha256::digest(shutdown_receipt_json.as_bytes()));
                        let reopened_picture_json = serde_json::to_string(&serde_json::json!({
                            "sequence_id": "test-sequence",
                            "frame": cycle,
                            "width": 1920,
                            "height": 1080,
                            "output_target": "Display",
                            "output_color_space": "Srgb",
                            "monitor_color_space": "Srgb",
                            "tone_map": false,
                            "display_view": null,
                            "frame_residency": {
                                "decode_residency": "ProceduralGpuNative",
                                "working_residency": "GpuWorkingCompositeExecuted",
                                "input_transform_path": "GpuNativeProcedural",
                                "execution_observed": true,
                                "zero_copy": true,
                                "low_copy": false,
                                "upload_count": 0,
                                "native_bridge_copy_count": 0,
                                "readback_count": 0,
                                "reason": "test fixture",
                            },
                            "display_contract_sha256": SHA,
                        }))
                        .expect("serialize reopened Surface picture");
                        let reopened_picture_sha256 =
                            format!("{:x}", Sha256::digest(reopened_picture_json.as_bytes()));
                        let reopened_contract_json = serde_json::to_string(&serde_json::json!({
                            "schema_version": 2,
                            "surface_generation": cycle + 2,
                            "device_generation": cycle + 4,
                            "actual_surface_presented": true,
                            "original_picture_sha256": reopened_picture_sha256,
                            "reopened_picture_json": reopened_picture_json,
                            "reopened_picture_sha256": reopened_picture_sha256,
                        }))
                        .expect("serialize reopened Surface contract");
                        let reopened_contract_sha256 =
                            format!("{:x}", Sha256::digest(reopened_contract_json.as_bytes()));
                        serde_json::json!({
                            "step": step,
                            "schema_version": 4,
                            "cycle_index": cycle,
                            "operation_id": operation_id,
                            "sequence_binding_sha256": SHA,
                            "surface_generation_before": cycle + 1,
                            "surface_generation_after": cycle + 2,
                            "device_generation_before": cycle + 3,
                            "device_generation_after": cycle + 4,
                            "shutdown_receipt_json": shutdown_receipt_json,
                            "shutdown_receipt_sha256": shutdown_receipt_sha256,
                            "reopened_contract_json": reopened_contract_json,
                            "reopened_contract_sha256": reopened_contract_sha256,
                        })
                    }
                    "export_cancel_retry" => serde_json::json!({
                        "step": step,
                        "schema_version": 3,
                        "cycle_index": cycle,
                        "operation_id": operation_id,
                        "cancelled_job_id": format!("cancelled-{cycle}"),
                        "retry_job_id": format!("retry-{cycle}"),
                        "cancellation_count_before": cycle,
                        "cancellation_count_after": cycle + 1,
                        "cancelled_terminal_sha256": SHA,
                        "retry_artifact_sha256": SHA,
                        "retry_validation_report_sha256": SHA,
                    }),
                    "cache_pressure" => serde_json::json!({
                        "step": step,
                        "schema_version": 3,
                        "cycle_index": cycle,
                        "operation_id": operation_id,
                        "decision_generation_before": 1,
                        "pressure_decision_generation": 2,
                        "recovered_decision_generation": 3,
                        "cache_bytes_before_pressure": 4096,
                        "cache_bytes_after_pressure": 1024,
                        "pressure_trimmed_bytes": 3072,
                        "residual_owned_resources": 0,
                        "recovered_nominal": true,
                        "exact_picture_ready": true,
                        "gpu_device_losses_before": 0,
                        "gpu_device_losses_after": 0,
                        "fatal_errors_before": 0,
                        "fatal_errors_after": 0,
                        "export_failures_before": 0,
                        "export_failures_after": 0,
                        "pressure_decision_sha256": SHA,
                        "recovered_decision_sha256": SHA,
                    }),
                    _ => unreachable!("fixed recovery step"),
                };
                let operation_receipt_json =
                    serde_json::to_string(&receipt).expect("serialize recovery receipt");
                let operation_receipt_sha256 =
                    format!("{:x}", Sha256::digest(operation_receipt_json.as_bytes()));
                let window_run_receipt = (step == "surface_device_reopen").then(|| {
                    let runtime = r#"{"supervisor":"terminated"}"#;
                    let host = r#"{"services":"returned"}"#;
                    let gpu = serde_json::to_string(&serde_json::json!({
                        "surface_generation": cycle + 2,
                        "device_generation": cycle + 4,
                        "publication_cleanup": { "Ok": null },
                        "retirement": {
                            "retired": {
                                "worker_started": true,
                                "worker_terminated": true,
                                "worker_panicked": false,
                                "timed_out": false,
                                "retirement_requested": true,
                                "retirement_handoff_accepted": true,
                                "retirement_completed": true,
                                "renderer_retirement": {
                                    "cpu_yuv_upload": "returned",
                                    "native_device_removed": false,
                                },
                                "generation_terminal_kind": null,
                            }
                        }
                    }))
                    .expect("serialize final GPU shutdown");
                    let native = r#"{"event_loop_borrow_returned":true,"window_owner_scope_exited":true,"physical_native_termination":"unverified"}"#;
                    let outer = serde_json::json!({
                        "schema_version": 2,
                        "outcome": "active_exited",
                        "recovery_receipt_json": operation_receipt_json.clone(),
                        "recovery_receipt_sha256": operation_receipt_sha256.clone(),
                        "runtime_shutdown_json": runtime,
                        "runtime_shutdown_sha256": format!("{:x}", Sha256::digest(runtime.as_bytes())),
                        "host_shutdown_json": host,
                        "host_shutdown_sha256": format!("{:x}", Sha256::digest(host.as_bytes())),
                        "gpu_shutdown_json": gpu.clone(),
                        "gpu_shutdown_sha256": format!("{:x}", Sha256::digest(gpu.as_bytes())),
                        "native_return_json": native,
                        "native_return_sha256": format!("{:x}", Sha256::digest(native.as_bytes())),
                    });
                    let json = serde_json::to_string(&outer).expect("serialize Window-run receipt");
                    let sha256 = format!("{:x}", Sha256::digest(json.as_bytes()));
                    (json, sha256)
                });
                events.push(serde_json::json!({
                    "kind": "recovery_step_completed",
                    "sequence": events.len(),
                    "completed_at_us": events.len() + 1,
                    "cycle_index": cycle,
                    "step": step,
                    "operation_receipt_json": operation_receipt_json,
                    "operation_receipt_sha256": operation_receipt_sha256,
                    "window_run_receipt_json": window_run_receipt.as_ref().map(|receipt| &receipt.0),
                    "window_run_receipt_sha256": window_run_receipt.as_ref().map(|receipt| &receipt.1),
                }));
            }
        }
        let raw_evidence = serde_json::json!({
            "schema_version": 2,
            "phase_id": phase.phase_id,
            "run_id": run_id,
            "authority_challenge": authority_challenge,
            "workload_sha256": phase.workload_sha256,
            "producer_owner": phase.producer.owner,
            "producer_verifier_id": phase.producer.verifier_id,
            "events": events,
        });
        std::fs::write(
            &raw_path,
            serde_json::to_vec_pretty(&raw_evidence).expect("serialize raw producer evidence"),
        )
        .expect("write raw producer evidence");
        phase.producer.raw_evidence_sha256 = file_sha256(&raw_path);
        let producer_report = serde_json::json!({
            "schema_version": phase.producer.report_schema_version,
            "phase_id": phase.phase_id,
            "run_id": run_id,
            "authority_challenge": authority_challenge,
            "workload_sha256": phase.workload_sha256,
            "producer_owner": phase.producer.owner,
            "producer_verifier_id": phase.producer.verifier_id,
            "raw_evidence_sha256": phase.producer.raw_evidence_sha256,
            "terminal_status": phase.terminal.status,
            "event_count": events.len(),
            "verified_export_artifacts": phase.terminal.counters.export_artifacts_verified,
            "recovery_cycles": phase.terminal.counters.recovery_cycles,
        });
        std::fs::write(
            &report_path,
            serde_json::to_vec_pretty(&producer_report).expect("serialize producer report"),
        )
        .expect("write producer report");
        phase.producer.report_sha256 = file_sha256(&report_path);
    }
    for (file_name, chunk) in chunks {
        std::fs::write(
            evidence_directory.join(file_name),
            serde_json::to_vec_pretty(&chunk).expect("serialize chunk"),
        )
        .expect("write chunk");
    }
    let profile_path = temporary.path().join("profile.json");
    std::fs::write(
        &profile_path,
        serde_json::to_vec_pretty(&profile()).expect("serialize profile"),
    )
    .expect("write profile");
    let machine_plan_path = temporary.path().join("machine-plan.json");
    std::fs::write(
        &machine_plan_path,
        br#"{"schema_version":2,"plan_id":"test-machine-plan"}"#,
    )
    .expect("write machine plan");
    run.machine_plan_sha256 = file_sha256(&machine_plan_path);
    let authority_path = temporary.path().join("capture-authority.json");
    let authority_phases = run
        .phases
        .iter()
        .map(|phase| {
            serde_json::json!({
                "phase_id": phase.phase_id,
                "workload_sha256": phase.workload_sha256,
                "producer_owner": phase.producer.owner,
                "producer_verifier_id": phase.producer.verifier_id,
            })
        })
        .collect::<Vec<_>>();
    let authority = serde_json::json!({
        "schema_version": 2,
        "authority_id": "external-commercial-endurance-authority-v2",
        "run_id": run.run_id,
        "profile_file_sha256": file_sha256(&profile_path),
        "source_revision": run.source_revision,
        "release_candidate_id": run.release_candidate_id,
        "product_artifact_sha256": run.product_artifact_sha256,
        "runtime_image_sha256": run.runtime_image_sha256,
        "build_provenance_sha256": run.build_provenance_sha256,
        "machine_report_sha256": run.machine_report_sha256,
        "platform_cell_sha256": run.platform_cell_sha256,
        "machine_plan_sha256": run.machine_plan_sha256,
        "single_use_challenge": authority_challenge,
        "phases": authority_phases,
    });
    std::fs::write(
        &authority_path,
        serde_json::to_vec_pretty(&authority).expect("serialize authority"),
    )
    .expect("write authority");
    run.capture_authority_sha256 = file_sha256(&authority_path);
    let manifest_path = temporary.path().join("run.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&run).expect("serialize run"),
    )
    .expect("write run");
    let replay_binary = Path::new(env!("CARGO_BIN_EXE_mondrian-endurance-replay"));
    let output_path = temporary.path().join("report.json");
    let mut verifier = Command::new("pwsh");
    verifier
        .arg("-NoProfile")
        .arg("-File")
        .arg(root.join("scripts/validation/verify-commercial-endurance-qualification.ps1"))
        .arg("-ProfilePath")
        .arg(&profile_path)
        .arg("-RunManifestPath")
        .arg(&manifest_path)
        .arg("-ChunkDirectory")
        .arg(&evidence_directory)
        .arg("-ReplayBinaryPath")
        .arg(replay_binary)
        .arg("-ReplayBinarySha256")
        .arg(file_sha256(replay_binary))
        .arg("-ExpectedProfileFileSha256")
        .arg(file_sha256(&profile_path))
        .arg("-ExpectedCaptureAuthorityPath")
        .arg(&authority_path)
        .arg("-ExpectedCaptureAuthoritySha256")
        .arg(file_sha256(&authority_path))
        .arg("-ExpectedMachinePlanPath")
        .arg(&machine_plan_path)
        .arg("-ExpectedMachinePlanSha256")
        .arg(file_sha256(&machine_plan_path))
        .arg("-ExpectedSourceRevision")
        .arg(SOURCE)
        .arg("-ExpectedReleaseCandidateId")
        .arg("mondrian-0.2.0-rc1")
        .arg("-ExpectedProductArtifactSha256")
        .arg(SHA)
        .arg("-ExpectedRuntimeImageSha256")
        .arg(SHA)
        .arg("-ExpectedBuildProvenanceSha256")
        .arg(SHA)
        .arg("-ExpectedMachineReportSha256")
        .arg(SHA)
        .arg("-ExpectedPlatformCellSha256")
        .arg(SHA)
        .arg("-OutputPath")
        .arg(&output_path);
    let status = verifier.status().expect("run PowerShell verifier");
    assert!(status.success());
    assert!(output_path.is_file());

    std::fs::remove_file(&output_path).expect("remove first test report");
    let machine_plan_bytes = std::fs::read(&machine_plan_path).expect("read machine plan");
    let mut tampered_machine_plan = machine_plan_bytes.clone();
    tampered_machine_plan.push(b'\n');
    std::fs::write(&machine_plan_path, tampered_machine_plan).expect("tamper machine plan");
    let status = verifier.status().expect("rerun verifier with tampered machine plan");
    assert!(!status.success(), "machine-plan byte drift must fail");
    assert!(
        !output_path.exists(),
        "rejected plan drift must not publish a report"
    );
    std::fs::write(&machine_plan_path, machine_plan_bytes).expect("restore machine plan");

    let baseline_manifest_bytes =
        serde_json::to_vec_pretty(&run).expect("serialize baseline manifest");
    let mut promoted_closure = serde_json::to_value(&run).expect("serialize closure tamper");
    promoted_closure["owner_closure"]["closure"]["physical_native_termination_verified"] =
        serde_json::json!(true);
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&promoted_closure).expect("serialize promoted closure"),
    )
    .expect("write promoted closure");
    let status = verifier.status().expect("rerun verifier with promoted closure");
    assert!(
        !status.success(),
        "invented physical native closure must fail"
    );
    assert!(!output_path.exists());

    let mut absent_closure = serde_json::to_value(&run).expect("serialize closure substitution");
    absent_closure["owner_closure"] = serde_json::json!({ "kind": "not_applicable" });
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&absent_closure).expect("serialize absent closure"),
    )
    .expect("write absent closure");
    let status = verifier.status().expect("rerun verifier with absent closure");
    assert!(
        !status.success(),
        "started Concurrent Recovery cannot use NotApplicable closure"
    );
    assert!(!output_path.exists());
    std::fs::write(&manifest_path, baseline_manifest_bytes).expect("restore run manifest");

    let recovery_phase = run
        .phases
        .iter()
        .find(|phase| phase.terminal.counters.recovery_cycles != 0)
        .expect("concurrent recovery phase");
    let raw_path = evidence_directory.join(&recovery_phase.producer.raw_evidence_file_name);
    let baseline_raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&raw_path).expect("read raw evidence"))
            .expect("parse raw evidence");
    let report_path = evidence_directory.join(&recovery_phase.producer.report_file_name);
    let baseline_producer_report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report_path).expect("read producer report"))
            .expect("parse producer report");
    let baseline_run = run.clone();
    let mut assert_rehashed_tamper_rejected = |description: &str, raw: &serde_json::Value| {
        let mut tampered_run = baseline_run.clone();
        let phase = tampered_run
            .phases
            .iter_mut()
            .find(|phase| phase.terminal.counters.recovery_cycles != 0)
            .expect("concurrent recovery phase");
        std::fs::write(
            &raw_path,
            serde_json::to_vec_pretty(raw).expect("serialize tampered raw evidence"),
        )
        .expect("write tampered raw evidence");
        phase.producer.raw_evidence_sha256 = file_sha256(&raw_path);
        let mut producer_report = baseline_producer_report.clone();
        producer_report["raw_evidence_sha256"] =
            serde_json::json!(phase.producer.raw_evidence_sha256);
        producer_report["event_count"] =
            serde_json::json!(raw["events"].as_array().expect("producer events array").len());
        std::fs::write(
            &report_path,
            serde_json::to_vec_pretty(&producer_report)
                .expect("serialize tampered producer report"),
        )
        .expect("write tampered producer report");
        phase.producer.report_sha256 = file_sha256(&report_path);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&tampered_run).expect("serialize tampered run"),
        )
        .expect("write tampered run");
        let status = verifier.status().expect("rerun PowerShell verifier");
        assert!(!status.success(), "{description}");
        assert!(
            !output_path.exists(),
            "a rejected verifier attempt must not publish a report: {description}"
        );
    };
    let reseal_receipt = |event: &mut serde_json::Value, receipt: &serde_json::Value| {
        let receipt_json = serde_json::to_string(receipt).expect("serialize tampered receipt");
        event["operation_receipt_sha256"] =
            serde_json::json!(format!("{:x}", Sha256::digest(receipt_json.as_bytes())));
        event["operation_receipt_json"] = serde_json::json!(receipt_json);
    };
    let reseal_reopened_picture = |receipt: &mut serde_json::Value, picture_json: String| {
        let picture_sha256 = format!("{:x}", Sha256::digest(picture_json.as_bytes()));
        let mut reopened: serde_json::Value = serde_json::from_str(
            receipt["reopened_contract_json"].as_str().expect("reopened contract"),
        )
        .expect("parse reopened contract");
        reopened["reopened_picture_json"] = serde_json::json!(picture_json);
        reopened["reopened_picture_sha256"] = serde_json::json!(picture_sha256.clone());
        reopened["original_picture_sha256"] = serde_json::json!(picture_sha256);
        let reopened_json = serde_json::to_string(&reopened).expect("serialize reopened contract");
        receipt["reopened_contract_json"] = serde_json::json!(reopened_json.clone());
        receipt["reopened_contract_sha256"] =
            serde_json::json!(format!("{:x}", Sha256::digest(reopened_json.as_bytes())));
    };
    let reseal_window_recovery_binding = |event: &mut serde_json::Value| {
        let mut window_run: serde_json::Value = serde_json::from_str(
            event["window_run_receipt_json"].as_str().expect("Window-run receipt"),
        )
        .expect("parse Window-run receipt");
        window_run["recovery_receipt_json"] = event["operation_receipt_json"].clone();
        window_run["recovery_receipt_sha256"] = event["operation_receipt_sha256"].clone();
        let window_run_json =
            serde_json::to_string(&window_run).expect("serialize Window-run receipt");
        event["window_run_receipt_sha256"] =
            serde_json::json!(format!("{:x}", Sha256::digest(window_run_json.as_bytes())));
        event["window_run_receipt_json"] = serde_json::json!(window_run_json);
    };

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| {
            event["kind"] == "recovery_step_completed" && event["step"] == "export_cancel_retry"
        })
        .expect("Export cancel/retry recovery event");
    let mut receipt: serde_json::Value = serde_json::from_str(
        event["operation_receipt_json"]
            .as_str()
            .expect("embedded Export recovery receipt"),
    )
    .expect("parse embedded Export recovery receipt");
    receipt["cancellation_count_after"] = receipt["cancellation_count_before"].clone();
    reseal_receipt(event, &receipt);
    assert_rehashed_tamper_rejected(
        "rehash-consistent Export cancellation leaf substitution must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["kind"] == "recovery_step_completed")
        .expect("recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse receipt");
    receipt["operation_id"] = serde_json::json!("x".repeat(4_097));
    reseal_receipt(event, &receipt);
    assert_rehashed_tamper_rejected("an outer recovery receipt above 4 KiB must fail", &raw);

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let oversized_nested = format!(
        "{}{}",
        receipt["shutdown_receipt_json"].as_str().expect("shutdown receipt"),
        " ".repeat(4_097)
    );
    receipt["shutdown_receipt_json"] = serde_json::json!(oversized_nested);
    receipt["shutdown_receipt_sha256"] =
        serde_json::json!(format!("{:x}", Sha256::digest(oversized_nested.as_bytes())));
    reseal_receipt(event, &receipt);
    assert_rehashed_tamper_rejected("nested Surface evidence above 4 KiB must fail", &raw);

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let mut reopened: serde_json::Value = serde_json::from_str(
        receipt["reopened_contract_json"].as_str().expect("reopened contract"),
    )
    .expect("parse reopened contract");
    reopened["original_picture_sha256"] = serde_json::json!(SHA);
    let reopened_json =
        serde_json::to_string(&reopened).expect("serialize substituted reopened contract");
    receipt["reopened_contract_json"] = serde_json::json!(reopened_json);
    receipt["reopened_contract_sha256"] =
        serde_json::json!(format!("{:x}", Sha256::digest(reopened_json.as_bytes())));
    reseal_receipt(event, &receipt);
    assert_rehashed_tamper_rejected(
        "rehash-consistent nested Surface picture substitution must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let mut old_shutdown: serde_json::Value = serde_json::from_str(
        receipt["shutdown_receipt_json"].as_str().expect("old shutdown receipt"),
    )
    .expect("parse old shutdown receipt");
    old_shutdown["surface_generation"] = serde_json::json!(99);
    let old_shutdown_json =
        serde_json::to_string(&old_shutdown).expect("serialize old shutdown receipt");
    receipt["shutdown_receipt_json"] = serde_json::json!(old_shutdown_json.clone());
    receipt["shutdown_receipt_sha256"] = serde_json::json!(format!(
        "{:x}",
        Sha256::digest(old_shutdown_json.as_bytes())
    ));
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected(
        "rehash-consistent old Surface generation substitution must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let mut old_shutdown: serde_json::Value = serde_json::from_str(
        receipt["shutdown_receipt_json"].as_str().expect("old shutdown receipt"),
    )
    .expect("parse old shutdown receipt");
    old_shutdown["surface_generation"] = serde_json::json!(receipt["surface_generation_before"]
        .as_u64()
        .expect("old Surface generation")
        .to_string());
    old_shutdown["worker_terminated"] = serde_json::json!("false");
    let old_shutdown_json =
        serde_json::to_string(&old_shutdown).expect("serialize typed old shutdown attack");
    receipt["shutdown_receipt_json"] = serde_json::json!(old_shutdown_json.clone());
    receipt["shutdown_receipt_sha256"] = serde_json::json!(format!(
        "{:x}",
        Sha256::digest(old_shutdown_json.as_bytes())
    ));
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected(
        "rehash-consistent string boolean/integer substitution must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    receipt["surface_generation_before"] = serde_json::json!(receipt["surface_generation_before"]
        .as_u64()
        .expect("old Surface generation")
        .to_string());
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected(
        "rehash-consistent recovery generation string substitution must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let reopened: serde_json::Value = serde_json::from_str(
        receipt["reopened_contract_json"].as_str().expect("reopened contract"),
    )
    .expect("parse reopened contract");
    let mut picture: serde_json::Value =
        serde_json::from_str(reopened["reopened_picture_json"].as_str().expect("reopened picture"))
            .expect("parse reopened picture");
    picture["frame_residency"]
        .as_object_mut()
        .expect("frame residency")
        .remove("decode_residency");
    let picture_json = serde_json::to_string(&picture).expect("serialize incomplete picture");
    reseal_reopened_picture(&mut receipt, picture_json);
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected(
        "rehash-consistent incomplete frame-residency shape must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let reopened: serde_json::Value = serde_json::from_str(
        receipt["reopened_contract_json"].as_str().expect("reopened contract"),
    )
    .expect("parse reopened contract");
    let picture_json =
        reopened["reopened_picture_json"].as_str().expect("reopened picture").replace(
            &format!("\"display_contract_sha256\":\"{SHA}\""),
            &format!("\"display_contract_sha256\":{}", "1".repeat(64)),
        );
    reseal_reopened_picture(&mut receipt, picture_json);
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected(
        "rehash-consistent numeric display-contract digest must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let reopened: serde_json::Value = serde_json::from_str(
        receipt["reopened_contract_json"].as_str().expect("reopened contract"),
    )
    .expect("parse reopened contract");
    let mut picture: serde_json::Value =
        serde_json::from_str(reopened["reopened_picture_json"].as_str().expect("reopened picture"))
            .expect("parse reopened picture");
    picture["width"] = serde_json::json!(u64::from(u32::MAX) + 1);
    let picture_json = serde_json::to_string(&picture).expect("serialize oversized picture");
    reseal_reopened_picture(&mut receipt, picture_json);
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected("rehash-consistent picture u32 overflow must fail", &raw);

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse Surface receipt");
    let old_shutdown_json = format!(
        " {}",
        receipt["shutdown_receipt_json"].as_str().expect("old shutdown receipt")
    );
    receipt["shutdown_receipt_json"] = serde_json::json!(old_shutdown_json.clone());
    receipt["shutdown_receipt_sha256"] = serde_json::json!(format!(
        "{:x}",
        Sha256::digest(old_shutdown_json.as_bytes())
    ));
    reseal_receipt(event, &receipt);
    reseal_window_recovery_binding(event);
    assert_rehashed_tamper_rejected(
        "rehash-consistent noncanonical old shutdown JSON must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let event = raw["events"]
        .as_array_mut()
        .expect("producer events array")
        .iter_mut()
        .find(|event| event["step"] == "surface_device_reopen")
        .expect("Surface recovery event");
    let mut window_run: serde_json::Value = serde_json::from_str(
        event["window_run_receipt_json"].as_str().expect("Window-run receipt"),
    )
    .expect("parse Window-run receipt");
    let mut final_gpu: serde_json::Value =
        serde_json::from_str(window_run["gpu_shutdown_json"].as_str().expect("final GPU receipt"))
            .expect("parse final GPU receipt");
    final_gpu["device_generation"] = serde_json::json!(99);
    let final_gpu_json = serde_json::to_string(&final_gpu).expect("serialize final GPU receipt");
    window_run["gpu_shutdown_json"] = serde_json::json!(final_gpu_json.clone());
    window_run["gpu_shutdown_sha256"] =
        serde_json::json!(format!("{:x}", Sha256::digest(final_gpu_json.as_bytes())));
    let window_run_json = serde_json::to_string(&window_run).expect("serialize Window-run receipt");
    event["window_run_receipt_sha256"] =
        serde_json::json!(format!("{:x}", Sha256::digest(window_run_json.as_bytes())));
    event["window_run_receipt_json"] = serde_json::json!(window_run_json);
    assert_rehashed_tamper_rejected(
        "rehash-consistent final Device generation substitution must fail",
        &raw,
    );

    let mut raw = baseline_raw.clone();
    let events = raw["events"].as_array_mut().expect("producer events array");
    let first_operation_id = events
        .iter()
        .find(|event| event["kind"] == "recovery_step_completed")
        .and_then(|event| event["operation_receipt_json"].as_str())
        .map(|json| serde_json::from_str::<serde_json::Value>(json).expect("parse receipt"))
        .and_then(|receipt| receipt["operation_id"].as_str().map(str::to_owned))
        .expect("first operation id");
    let event = events
        .iter_mut()
        .filter(|event| event["kind"] == "recovery_step_completed")
        .nth(1)
        .expect("second recovery event");
    let mut receipt: serde_json::Value =
        serde_json::from_str(event["operation_receipt_json"].as_str().expect("receipt"))
            .expect("parse receipt");
    receipt["operation_id"] = serde_json::json!(first_operation_id);
    reseal_receipt(event, &receipt);
    assert_rehashed_tamper_rejected("a replayed recovery operation identity must fail", &raw);

    let mut raw = baseline_raw.clone();
    let events = raw["events"].as_array_mut().expect("producer events array");
    let seek_indices = events
        .iter()
        .enumerate()
        .filter_map(|(index, event)| (event["step"] == "seek").then_some(index))
        .collect::<Vec<_>>();
    assert_eq!(
        seek_indices.len(),
        2,
        "fixture must contain two seek cycles"
    );
    let first_json = events[seek_indices[0]]["operation_receipt_json"].clone();
    let first_hash = events[seek_indices[0]]["operation_receipt_sha256"].clone();
    events[seek_indices[0]]["operation_receipt_json"] =
        events[seek_indices[1]]["operation_receipt_json"].clone();
    events[seek_indices[0]]["operation_receipt_sha256"] =
        events[seek_indices[1]]["operation_receipt_sha256"].clone();
    events[seek_indices[1]]["operation_receipt_json"] = first_json;
    events[seek_indices[1]]["operation_receipt_sha256"] = first_hash;
    assert_rehashed_tamper_rejected("cross-cycle receipt substitution must fail", &raw);

    let mut raw = baseline_raw.clone();
    let events = raw["events"].as_array_mut().expect("producer events array");
    let partial_index = events
        .iter()
        .rposition(|event| event["kind"] == "recovery_step_completed")
        .expect("last recovery event");
    events.remove(partial_index);
    assert_rehashed_tamper_rejected("a partial recovery cycle must fail", &raw);
}
