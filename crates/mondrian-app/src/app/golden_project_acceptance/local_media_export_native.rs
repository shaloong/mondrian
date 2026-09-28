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

#[cfg(test)]
#[test]
#[ignore = "requires an external 4K ProRes 4444 source and a create-only /tmp evidence directory"]
fn native_high_depth_alpha_export_uses_product_app_and_queue() -> anyhow::Result<()> {
    run_high_depth_alpha_export(Some(mondrian_core::ColorSpace::Rec709))
}

#[cfg(test)]
#[test]
#[ignore = "requires an untagged 4K ProRes 4444 source and a create-only /tmp evidence directory"]
fn native_untagged_high_depth_export_uses_sequence_missing_metadata_policy() -> anyhow::Result<()> {
    run_high_depth_alpha_export(None)
}

#[cfg(test)]
fn run_high_depth_alpha_export(
    color_space_override: Option<mondrian_core::ColorSpace>,
) -> anyhow::Result<()> {
    let (frame_count, source, output) = high_depth_export_input()?;
    let mut state = AppState::new();
    let mut export_wall_ms = None;
    let result = (|| -> anyhow::Result<super::super::harness::CompletedExportEvidence> {
        author_high_depth_alpha_export(
            &mut state,
            &source,
            &output,
            frame_count,
            color_space_override,
        )?;
        let export_started = Instant::now();
        let export_result = super::super::harness::execute_export_job(
            &mut state,
            ExportPreset::prores_4444_alpha(),
            None,
            TimelineExportRange::WorkArea { start_frame: 0, end_frame_exclusive: frame_count },
            output.join("high-depth-export.mov"),
            Duration::from_secs(300),
        );
        export_wall_ms = Some(export_started.elapsed().as_millis());
        if let Ok(completed) = &export_result {
            ensure!(
                completed.diagnostics.visual.gpu_visual_compact_yuv_sources == frame_count as u64,
                "4K YUVA12 export did not execute every source through shared compact GPU input"
            );
        }
        if color_space_override.is_none()
            && let Ok(completed) = &export_result
        {
            let counts = completed.diagnostics.color.input_resolution_source_counts;
            ensure!(
                counts.missing_assume_rec709 == frame_count as u64
                    && counts.override_count == 0
                    && counts.detected_metadata == 0,
                "export did not execute every frame through the Sequence missing-metadata policy: {counts:?}"
            );
        }
        export_result
    })();
    let shutdown = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(90));
    let shutdown_released = shutdown.all_resources_released();
    let artifact_sha256 = result
        .as_ref()
        .ok()
        .map(|completed| sha256_file(&completed.output_path))
        .transpose()?;
    let report = json!({
        "schema_version": 1,
        "profile": "single-source-high-depth-alpha-export-v1",
        "requested_frames": frame_count,
        "color_space_override": color_space_override,
        "export_wall_ms": export_wall_ms,
        "source": source,
        "source_sha256": sha256_file(&source)?,
        "completed_export": result.as_ref().ok(),
        "artifact_sha256": artifact_sha256,
        "failure": result.as_ref().err().map(ToString::to_string),
        "shutdown_all_resources_released": shutdown_released,
        "shutdown": shutdown,
    });
    write_new(&output.join("high-depth-export-report.json"), &report)?;
    ensure!(
        shutdown_released,
        "App owner shutdown left resources behind"
    );
    result.map(drop)
}

#[cfg(test)]
fn high_depth_export_input() -> anyhow::Result<(i64, PathBuf, PathBuf)> {
    let frame_count = std::env::var("MONDRIAN_HIGH_DEPTH_EXPORT_FRAMES")
        .ok()
        .map(|value| value.parse::<i64>())
        .transpose()?
        .unwrap_or(2);
    ensure!(
        (2..=60).contains(&frame_count),
        "expected 2–60 full-quality export frames"
    );
    let source = PathBuf::from(
        std::env::var_os("MONDRIAN_HIGH_DEPTH_EXPORT_SOURCE").context("4K source path")?,
    );
    let output = PathBuf::from(
        std::env::var_os("MONDRIAN_HIGH_DEPTH_EXPORT_OUTPUT").context("evidence path")?,
    );
    ensure!(!output.exists(), "export evidence directory already exists");
    std::fs::create_dir(&output)?;
    let source = mondrian_assets::canonical_asset_file_path(&source)?;
    Ok((frame_count, source, output))
}

#[cfg(test)]
fn author_high_depth_alpha_export(
    state: &mut AppState,
    source: &Path,
    output: &Path,
    frame_count: i64,
    color_space_override: Option<mondrian_core::ColorSpace>,
) -> anyhow::Result<()> {
    state.create_new_project_with_settings_at(
        output.join("high-depth-export.mdp"),
        "High-depth alpha Export",
        SequenceSettings {
            resolution: Resolution { width: 3840, height: 2160 },
            frame_rate: Rational::FPS_30,
            ..SequenceSettings::default()
        },
        ProjectColorEnvironment::default(),
        ProjectSettings::default(),
    )?;
    state.dispatch_action(mondrian_editor_state::Action::ImportMedia(vec![
        source.to_path_buf()
    ]))?;
    let import_deadline = Instant::now() + Duration::from_secs(120);
    while state.pending_media_import_batches() > 0 {
        state.poll_media_imports();
        ensure!(
            Instant::now() < import_deadline,
            "4K source import timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let assets = state.asset_library().context("asset library")?.list_assets()?;
    let asset = assets
        .iter()
        .find(|asset| asset.file_path() == Some(source))
        .context("imported 4K source")?;
    let stream = asset
        .media_probe()
        .and_then(|probe| probe.video_streams.first())
        .context("probed 4K video stream")?;
    ensure!(
        stream.width == 3840
            && stream.height == 2160
            && stream.pixel_format == mondrian_core::PixelFormat::Yuva444p12le
            && stream.bit_depth == 12,
        "source is not the required full-resolution 12-bit 4:4:4 alpha media"
    );
    if color_space_override.is_none() {
        ensure!(
            stream.executable_color_space().is_none(),
            "missing-metadata policy test requires no executable source color identity"
        );
    }
    let track = add_track(state, TrackAddKind::Video)?;
    place(state, asset.id, track, 0, frame_count, true)?;
    let clip_id = state
        .active_sequence()
        .context("sequence")?
        .video_tracks
        .iter()
        .find(|candidate| candidate.id == track)
        .and_then(|track| track.clips.first())
        .context("placed source clip")?
        .id;
    let interpretation = mondrian_timeline::clip::MediaInterpretation {
        color_space_override,
        field_order_override: Some(FieldOrder::Progressive),
        ..Default::default()
    };
    state.set_clip_media_interpretation(
        crate::app::selection::SelectedClipRef { track_id: track, is_video_track: true, clip_id },
        interpretation.clone(),
    )?;
    let authored = state
        .active_sequence()
        .context("sequence")?
        .video_tracks
        .iter()
        .find(|candidate| candidate.id == track)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id))
        .and_then(|clip| clip.media_interpretation())
        .context("authored source interpretation")?;
    ensure!(
        authored == &interpretation,
        "requested source interpretation does not match the Timeline owner"
    );
    Ok(())
}

#[cfg(test)]
#[test]
#[ignore = "requires an untagged 4K YUVA12 source, real GPU/FFmpeg, and create-only evidence"]
fn native_compact_yuva12_cancel_retry_retires_gpu_and_verifies_distinct_artifacts(
) -> anyhow::Result<()> {
    use mondrian_export::{ExportProgressDetail, JobStatus};
    let (frame_count, source, output) = high_depth_export_input()?;
    ensure!(
        frame_count == 60,
        "GPU recovery requires 60 full-quality 4K frames"
    );
    let started = Instant::now();
    let deadline = started + Duration::from_secs(900);
    let source_lease = File::open(&source)?;
    let source_sha256 = sha256_file(&source)?;
    let mut app = Some(AppState::new());
    let mut cleanup = DirectoryCleanup::default();
    let mut exports = None;
    let mut history = BTreeMap::new();
    let mut cancellation_start = None;
    let mut report = json!({
        "schema_version":1,"profile":"compact-yuva12-repeated-export-cancel-retry-v1",
        "commercial_qualification":false,"concurrent_playback_qualified":false,
        "requested_frames":frame_count,"source":source,"source_sha256":source_sha256,
        "events":[],"memory_stop_bytes":MEMORY_LIMIT,
    });
    let operation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<()> {
        let state = app.as_mut().context("App")?;
        author_high_depth_alpha_export(state, &source, &output, frame_count, None)?;
        cleanup.track(state.project_runtime_dir().map(Path::to_path_buf));
        state.stop()?;
        let save = state.request_project_save()?;
        state.wait_for_persistence_request_until(save, deadline)?;
        report["project_sha256"] = json!(sha256_file(&output.join("high-depth-export.mdp"))?);
        exports = Some(FrozenRepeatedExportPhase::start_until(
            state,
            FrozenRepeatedExportRequest {
                phase_id:"native-compact-yuva12-recovery".to_owned(),
                frozen_ancillary:None,approved_bmx:None,
                preset:ExportPreset::prores_4444_alpha(),sequence_id:None,
                range:TimelineExportRange::WorkArea {start_frame:0,end_frame_exclusive:frame_count},
                output_directory:output.clone(),artifact_prefix:"gpu-recovery".to_owned(),
                broadcast_qc:None,regulatory_pse:None,
                verification_policy:mondrian_export::IndependentExportArtifactPolicy::new(
                    256 * 1024 * 1024, Duration::from_secs(90),
                )?,
            }, deadline,
        )?);
        loop {
            ensure!(Instant::now() < deadline, "original GPU recovery deadline expired");
            poll_exports(state, &mut exports, &mut history, &mut report, started)?;
            let phase = exports.as_mut().context("Export")?;
            if cancellation_start.is_none()
                && let Some(job) = state.export_jobs_snapshot().into_iter().find(|job| {
                    matches!(job.status, JobStatus::Running { .. })
                        && matches!(job.progress.detail, ExportProgressDetail::Frames {completed,total}
                            if completed >= 4 && completed < total)
                        && job.diagnostics.visual.gpu_visual_compact_yuv_sources >= 4
                })
            {
                cancellation_start = Some(job);
                phase.begin_cancel_retry_recovery(1)?;
            }
            ensure!(
                cancellation_start.is_some()
                    || !history.values().any(|job| job.status == JobStatus::Completed),
                "Export completed without proving a cancellable compact GPU attempt"
            );
            if cancellation_start.is_some()
                && export_minimum_complete(phase.verified_artifacts(), phase.cancel_retry_recovery_in_progress())
            {
                phase.begin_close();
                break;
            }
            let memory = SystemPlatformService.product_process_tree_memory();
            ensure!(memory.inventory_complete && memory.private_memory_bytes.is_some_and(|v| v <= MEMORY_LIMIT),
                "native memory evidence unavailable or exceeds 6 GiB");
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })).unwrap_or_else(|payload| Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload)));
    let close_deadline = deadline.min(Instant::now() + Duration::from_secs(90));
    let mut close_errors = Vec::new();
    if let Some(phase) = exports.as_mut() {
        phase.begin_close();
    }
    while exports.as_ref().is_some_and(|phase| !phase.is_quiescent())
        && Instant::now() < close_deadline
    {
        if let Err(error) = poll_exports(
            app.as_mut().context("App before close")?,
            &mut exports,
            &mut history,
            &mut report,
            started,
        ) {
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
        let receipt = phase.shutdown_until(close_deadline);
        if !receipt.all_resources_released() {
            close_errors.push("Export verifier closure incomplete".to_owned());
        }
        report["export_verifier_shutdown"] = json!(receipt);
    }
    let state = app.take().context("App consuming close")?;
    for job in state.export_jobs_snapshot() {
        history.insert(job.id, job);
    }
    let receipt = state.shutdown_for_endurance(close_deadline);
    if !receipt.all_resources_released() {
        cleanup.retain();
        close_errors.push("App closure incomplete".to_owned());
    }
    report["app_shutdown"] = json!(receipt);
    report["cancellation_start_snapshot"] = json!(cancellation_start);
    report["export_job_history"] = json!(history.values().collect::<Vec<_>>());
    report["close_errors"] = json!(close_errors);
    report["elapsed_millis"] = json!(started.elapsed().as_millis());
    let validation = operation.and_then(|()| -> anyhow::Result<()> {
        ensure!(
            close_errors.is_empty() && Instant::now() < deadline,
            "GPU Export consuming closure failed"
        );
        ensure!(
            cancellation_start.is_some(),
            "no running GPU attempt was cancelled"
        );
        ensure!(
            report["verified_artifacts"].as_u64().is_some_and(|count| count >= 2),
            "two independently verified artifacts required"
        );
        let cancelled = history
            .values()
            .filter(|job| job.status == JobStatus::Cancelled)
            .collect::<Vec<_>>();
        let completed = history
            .values()
            .filter(|job| job.status == JobStatus::Completed)
            .collect::<Vec<_>>();
        ensure!(
            cancelled.len() == 1 && completed.len() >= 2,
            "GPU cancel/retry terminal inventory incomplete"
        );
        ensure!(
            !cancelled[0].output_path.exists(),
            "cancel partial artifact survived"
        );
        for job in completed {
            ensure!(
                job.id != cancelled[0].id
                    && job.generation > cancelled[0].generation
                    && job.output_path != cancelled[0].output_path,
                "retry reused cancelled identity or artifact"
            );
            ensure!(
                job.diagnostics.visual.gpu_visual_compact_yuv_sources == frame_count as u64
                    && job.diagnostics.visual.gpu_visual_nodes_completed == frame_count as u64
                    && job.diagnostics.color.gpu_output_cpu_fallbacks == 0
                    && job.diagnostics.color.stage_diagnostics.cpu_input_stages == 0
                    && job.diagnostics.color.stage_diagnostics.cpu_output_stages == 0,
                "complete Export did not retain compact GPU path"
            );
            let counts = job.diagnostics.color.input_resolution_source_counts;
            ensure!(
                counts.missing_assume_rec709 == frame_count as u64
                    && counts.override_count == 0
                    && counts.detected_metadata == 0,
                "GPU retry bypassed Sequence missing-metadata policy"
            );
        }
        ensure!(
            sha256_file(&source)? == source_sha256,
            "source identity changed during recovery"
        );
        Ok(())
    });
    report["status"] = json!(if validation.is_ok() {
        "Completed"
    } else {
        "Failed"
    });
    report["failure"] = json!(validation.as_ref().err().map(|error| format!("{error:#}")));
    write_new(&output.join("gpu-recovery-report.json"), &report)?;
    drop(source_lease);
    validation
}
