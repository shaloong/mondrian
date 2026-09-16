//! Independent native Export coverage when realtime admission cannot complete.
use super::*;

pub(super) fn run(root: PathBuf, output: PathBuf) -> anyhow::Result<PathBuf> {
    ensure!(!output.exists(), "evidence directory already exists");
    std::fs::create_dir_all(&output)?;
    let output = mondrian_assets::canonical_native_path(&output)?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(900);
    let mut leases = Vec::new();
    let mut paths = Vec::new();
    let mut report = json!({"schema_version":1,"profile":"local-independent-repeated-export-v1","commercial_qualification":false,"concurrent_playback_qualified":false,"duration_72h_qualified":false,"status":"Started","fixtures":[],"events":[]});
    for relative in FIXTURES {
        let path = mondrian_assets::canonical_asset_file_path(&root.join(relative))?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let mut file = options.open(&path)?;
        let digest = hash_until(&mut file, deadline)?;
        report["fixtures"]
            .as_array_mut()
            .context("fixtures")?
            .push(json!({"path":path,"sha256":digest,"byte_len":file.metadata()?.len()}));
        paths.push(path);
        leases.push(file);
    }
    let executable = std::env::current_exe()?;
    report["producer"] = json!({"path":executable,"sha256":sha256_file(&executable)?});
    let mut app = Some(AppState::new());
    let mut cleanup = DirectoryCleanup::default();
    let mut exports = None;
    let mut history = BTreeMap::new();
    let operation =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<()> {
            let state = app.as_mut().context("App")?;
            state.create_new_project_with_settings_at(
                output.join("export.mdp"),
                "Native repeated export",
                SequenceSettings {
                    resolution: Resolution { width: 640, height: 360 },
                    frame_rate: Rational::FPS_60,
                    ..SequenceSettings::default()
                },
                ProjectColorEnvironment::default(),
                ProjectSettings::default(),
            )?;
            cleanup.track(state.project_runtime_dir().map(Path::to_path_buf));
            author(state, &paths, 0, deadline, &mut report, false, 60)?;
            state.stop()?;
            let save = state.request_project_save()?;
            state.wait_for_persistence_request_until(save, deadline)?;
            report["project_sha256"] = json!(sha256_file(&output.join("export.mdp"))?);
            let mut preset = ExportPreset::h264_aac_sdr_1080p();
            preset.resolution =
                Some(mondrian_export::preset::Resolution { width: 640, height: 360 });
            exports = Some(FrozenRepeatedExportPhase::start_until(
                state,
                FrozenRepeatedExportRequest {
                    phase_id: "native-independent-export".to_owned(),
                    frozen_ancillary: None,
                    approved_bmx: None,
                    preset,
                    sequence_id: None,
                    range: TimelineExportRange::WorkArea {
                        start_frame: 0,
                        end_frame_exclusive: 120,
                    },
                    output_directory: output.clone(),
                    artifact_prefix: "native-export".to_owned(),
                    broadcast_qc: None,
                    regulatory_pse: None,
                    verification_policy: mondrian_export::IndependentExportArtifactPolicy::new(
                        64 * 1024 * 1024,
                        Duration::from_secs(30),
                    )?,
                },
                deadline,
            )?);
            exports.as_mut().context("Export")?.begin_cancel_retry_recovery(1)?;
            loop {
                ensure!(
                    Instant::now() < deadline,
                    "original native Export deadline expired"
                );
                poll_exports(state, &mut exports, &mut history, &mut report, started)?;
                let phase = exports.as_ref().context("Export")?;
                if export_minimum_complete(
                    phase.verified_artifacts(),
                    phase.cancel_retry_recovery_in_progress(),
                ) {
                    break;
                }
                let memory = SystemPlatformService.product_process_tree_memory();
                ensure!(
                    memory.inventory_complete
                        && memory.private_memory_bytes.is_some_and(|v| v <= MEMORY_LIMIT),
                    "native memory evidence unavailable or exceeds 6 GiB"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        }))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
    let close_deadline = deadline.min(Instant::now() + Duration::from_secs(90));
    let mut close_errors = Vec::new();
    if let Some(phase) = exports.as_mut() {
        phase.begin_close();
    }
    while exports.as_ref().is_some_and(|phase| !phase.is_quiescent())
        && Instant::now() < close_deadline
    {
        let state = app.as_mut().context("App before close")?;
        if let Err(error) = poll_exports(state, &mut exports, &mut history, &mut report, started) {
            close_errors.push(format!("{error:#}"));
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(phase) = exports.take() {
        report["verified_artifacts"] = json!(phase.verified_artifacts());
        if !phase.is_quiescent() || phase.cancel_retry_recovery_in_progress() {
            close_errors.push("Export recovery/quiescence incomplete".to_owned());
        }
        let raw = phase.shutdown_until(close_deadline);
        if !raw.all_resources_released() {
            close_errors.push("Export verifier closure incomplete".to_owned());
        }
        report["export_verifier_shutdown"] = serde_json::to_value(raw)?;
    }
    let state = app.take().context("App consuming close")?;
    for job in state.export_jobs_snapshot() {
        history.insert(job.id, job);
    }
    let raw = state.shutdown_for_endurance(close_deadline);
    let clean = raw.all_resources_released();
    if !clean {
        cleanup.retain();
        close_errors.push("App closure incomplete".to_owned());
    }
    report["app_shutdown"] = serde_json::to_value(raw)?;
    report["export_job_history"] = serde_json::to_value(history.into_values().collect::<Vec<_>>())?;
    report["close_errors"] = json!(close_errors);
    report["elapsed_millis"] = json!(started.elapsed().as_millis());
    if let Err(error) = &operation {
        report["failure"] = json!(format!("{error:#}"));
    }
    let passed = operation.is_ok() && close_errors.is_empty() && Instant::now() < deadline;
    report["status"] = json!(if passed { "Completed" } else { "Failed" });
    let report_path = output.join("native-repeated-export.json");
    write_new(&report_path, &report)?;
    drop(leases);
    operation?;
    ensure!(
        passed,
        "native Export did not complete with consuming closure"
    );
    Ok(report_path)
}

#[cfg(test)]
#[test]
#[ignore = "requires real media and native FFmpeg; explicit create-only evidence directory"]
fn native_repeated_export_cancel_retry_retains_independent_artifacts_and_closure(
) -> anyhow::Result<()> {
    let root =
        PathBuf::from(std::env::var_os("MONDRIAN_LOCAL_MEDIA_ROOT").context("fixture root")?);
    let output = PathBuf::from(
        std::env::var_os("MONDRIAN_LOCAL_EXPORT_EVIDENCE").context("evidence directory")?,
    );
    run(root, output).map(drop)
}
