//! Actual ordinary App import, Timeline and Export capture for COL-045.

use super::harness::{wait_for_export_job_until, wait_for_media_imports_until};
use super::headless_preview::{GoldenHeadlessPreview, GoldenViewerClosedFailure};
use super::workflow::{GoldenProductWorkflowDriver, GoldenWorkflowStartupClosedFailure};
use crate::app::ui_actions::*;
use anyhow::{ensure, Context};
use mondrian_core::timeline_data::{
    AlphaInterpretation, AssetMediaInterpretation, ClipContent, MediaColorInterpretation,
    MediaRangeInterpretation, MediaSignalRange,
};
use mondrian_core::{
    ColorSpace, DisplayToneMapPolicy, FramePosition, ProjectColorEnvironment, ProjectSettings,
    Rational, Resolution, TimelineTime, WorkingColorSpace,
};
use mondrian_export::preset::{
    ExportAlphaMode, ExportColorTarget, ExportOutputPolicy, ExportPreset, TimelineExportRange,
};
use mondrian_export::queue::JobStatus;
use mondrian_timeline::sequence::ColorWorkflow;
use mondrian_timeline::SequenceSettings;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RUN_ID: &str = "mondrian-color-local-20260906";
const STIMULUS_ID: &str = "mondrian-cross-application-color-stimulus-blender-premiere-v1";
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(900);

/// A separate validation process owns the complete ordinary product lifetime.
pub(super) fn run(input: PathBuf, output: PathBuf) -> anyhow::Result<PathBuf> {
    run_until(input, output, Instant::now() + CAPTURE_TIMEOUT)
}

fn run_until(input: PathBuf, output: PathBuf, deadline: Instant) -> anyhow::Result<PathBuf> {
    ensure!(
        !output.exists(),
        "capture output must be new: {}",
        output.display()
    );
    std::fs::create_dir_all(&output)?;
    let output = mondrian_assets::canonical_native_path(&output)?;
    let report_path = output.join("mondrian-capture.json");
    let mut report = json!({"schema_version":1,"run_id":RUN_ID,"producer":"mondrian",
        "version":env!("CARGO_PKG_VERSION"),"capture_completed":false,
        "cross_application_qualified":false,"cases":[],"inputs":[],"failure":null});
    report["hard_deadline_seconds"] = json!(CAPTURE_TIMEOUT.as_secs());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        capture(&input, &output, &mut report, deadline)
    }))
    .unwrap_or_else(|payload| {
        Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
    });
    if let Err(error) = &result {
        report["failure"] = json!(format!("{error:#}"));
        if let Some(failure) = error.downcast_ref::<GoldenWorkflowStartupClosedFailure>() {
            report["app_shutdown"] = serde_json::to_value(&failure.app)?;
        }
    }
    write_new_json(&report_path, &report)?;
    result.with_context(|| {
        format!(
            "capture failed; complete receipt: {}",
            report_path.display()
        )
    })?;
    Ok(report_path)
}

fn capture(
    input: &Path,
    output: &Path,
    report: &mut Value,
    deadline: Instant,
) -> anyhow::Result<()> {
    remaining(deadline, CAPTURE_TIMEOUT)?;
    let sha256_file = |path: &Path| sha256_file_until(path, deadline);
    let manifest_path = super::repository_root()
        .join("tests/validation/cross-application-color-stimulus-blender-premiere-v1.json");
    let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    ensure!(
        manifest["id"] == STIMULUS_ID,
        "unexpected stimulus identity"
    );
    report["stimulus_manifest"] = manifest;
    report["stimulus_manifest_sha256"] = json!(sha256_file(&manifest_path)?);
    let executable = std::env::current_exe()?;
    report["executable"] = json!({"path":executable,"sha256":sha256_file(&executable)?});
    let mut paths = Vec::with_capacity(120);
    // Retain read handles throughout the actual import/decode/export operation.
    let mut leases = Vec::with_capacity(120);
    for frame in 0..120 {
        let path = mondrian_assets::canonical_asset_file_path(
            &input.join(format!("input-{frame:04}.exr")),
        )?;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let lease = options.open(&path)?;
        ensure!(
            lease.metadata()?.len() <= 4 * 1024 * 1024,
            "oversized stimulus frame"
        );
        let evidence = json!({"frame":frame,"path":path,"sha256":sha256_file(&path)?,"byte_len":lease.metadata()?.len()});
        report["inputs"].as_array_mut().context("inputs array")?.push(evidence);
        paths.push(path);
        leases.push(lease);
    }
    let mut settings = SequenceSettings {
        resolution: Resolution { width: 64, height: 64 },
        frame_rate: Rational::FPS_23976,
        ..SequenceSettings::default()
    };
    settings.color.working_color_space = WorkingColorSpace::LinearRec2020;
    settings.color.input.auto_tone_map_media = false;
    settings.color.program_output.workflow = ColorWorkflow::DisplayReferred;
    settings.color.program_output.tone_map_policy = DisplayToneMapPolicy::Never;
    // Viewer Program is display-referred; each export explicitly selects its own target.
    settings.color.program_output.color_space = ColorSpace::Srgb;
    settings.delivery.video_range = mondrian_timeline::sequence::VideoRange::Full;
    report["sequence_settings"] = serde_json::to_value(&settings)?;
    let mut workflow = GoldenProductWorkflowDriver::create_until(
        output.join("capture.mdp"),
        "COL-045 native capture",
        settings,
        ProjectColorEnvironment::default(),
        ProjectSettings::default(),
        deadline,
    )?;
    let operation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        report["project_color_environment"] = serde_json::to_value(workflow.app().project_color_environment())?;
        author_stimulus(workflow.app_mut(), &paths,deadline)?;
        remaining(deadline,CAPTURE_TIMEOUT)?;
        let persistence = workflow.app_mut().request_project_save()?;
        workflow.app_mut().wait_for_persistence_request_until(persistence,deadline)?;
        ensure!(!workflow.app().has_unsaved_project_changes(), "capture project save does not cover current author state");
        report["authored_project"] = json!({"path":output.join("capture.mdp"),"sha256":sha256_file(&output.join("capture.mdp"))?,"persistence_request_id":persistence.get()});
        report["author_checkpoint"] = serde_json::to_value(super::harness::author_checkpoint(workflow.app())?)?;
        let viewer = GoldenHeadlessPreview::run_with_until(deadline,|viewer| {
            for (frame, id, mut preset, color, extension) in [
                (0, "scene-linear-rec2020-f32", ExportPreset::open_exr_float_sequence(), ColorSpace::LinearRec2020, "exr"),
                (17, "sdr-srgb-alpha-rgba8", ExportPreset::png_sequence(), ColorSpace::Srgb, "png"),
                (119, "bt2100-pq-rgba-f32", ExportPreset::tiff16_sequence(), ColorSpace::Rec2100Pq, "tiff"),
            ] {
                preset.color_target = ExportColorTarget::Colorimetric(color);
                preset.alpha_mode = if frame == 119 {ExportAlphaMode::FlattenBlack} else {ExportAlphaMode::Preserve};
                let state = workflow.app_mut();
                let rate = state.active_sequence().context("capture Sequence")?.time_base();
                state.dispatch_action(timeline_seek_action(FramePosition::new(frame, rate)))?;
                let presentation = viewer.present_current(state, remaining(deadline,Duration::from_secs(120))?)?;
                remaining(deadline,CAPTURE_TIMEOUT)?;
                let mut case = json!({"id":id,"frame":frame,"preset":preset,"viewer_presentation":presentation,"capture_completed":false});
                let case_result = export_case(state, frame, &preset, &output.join(id), extension, &mut case,deadline);
                if let Err(error) = &case_result { case["failure"] = json!(format!("{error:#}")); }
                report["cases"].as_array_mut().context("cases array")?.push(case);
                case_result?;
            }
            report["viewer"] = serde_json::to_value(viewer.evidence()?)?;
            Ok(())
        });
        match viewer {
            Ok(((), shutdown)) => report["viewer_shutdown"] = serde_json::to_value(shutdown)?,
            Err(error) => {
                if let Some(failure) = error.downcast_ref::<GoldenViewerClosedFailure>() {
                    report["viewer_shutdown"] = serde_json::to_value(&failure.shutdown)?;
                }
                if let Some(failure) = error.downcast_ref::<crate::app::headless_execution_startup::HeadlessStartupClosedFailure>() {
                    report["viewer_startup_shutdown"] = serde_json::to_value(&failure.evidence)?;
                }
                return Err(error);
            }
        }
        Ok(())
    })).unwrap_or_else(|payload| Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload)));
    // Inventory before consuming shutdown remains available even when a job failed or timed out.
    let job_inventory = serde_json::to_value(workflow.app().export_jobs_snapshot());
    let shutdown = workflow.shutdown_until(deadline.min(Instant::now() + Duration::from_secs(30)));
    let clean = shutdown.all_resources_released();
    report["app_shutdown"] = serde_json::to_value(shutdown)?;
    report["export_jobs_before_shutdown"] = job_inventory?;
    operation?;
    ensure!(clean, "capture App/Export owner closure failed");
    for (path, evidence) in paths.iter().zip(report["inputs"].as_array().context("inputs")?) {
        ensure!(
            evidence["sha256"] == sha256_file(path)?,
            "input changed during capture"
        );
    }
    drop(leases);
    remaining(deadline, CAPTURE_TIMEOUT)?;
    report["capture_completed"] = json!(true);
    report["qualification_limit"] = json!("Native captures require independent same-run comparison. PQ uses the current product colorimetric engine; no authored 203-nit reference-white parameter exists, so the manifest's PQ reference-white qualification is not established.");
    Ok(())
}

fn author_stimulus(
    state: &mut crate::app::AppState,
    paths: &[PathBuf],
    deadline: Instant,
) -> anyhow::Result<()> {
    remaining(deadline, CAPTURE_TIMEOUT)?;
    state.dispatch_action(mondrian_editor_state::Action::ImportMedia(paths.to_vec()))?;
    wait_for_media_imports_until(
        state,
        deadline.min(Instant::now() + Duration::from_secs(120)),
    )?;
    let assets = state.asset_library().context("asset library")?.list_assets()?;
    ensure!(
        assets.len() == 120,
        "all 120 EXR assets must be admitted through ordinary import"
    );
    let before: BTreeSet<_> = state
        .active_sequence()
        .context("Sequence")?
        .video_tracks
        .iter()
        .map(|t| t.id)
        .collect();
    state.dispatch_action(track_add_action(TrackAddPayload {
        kind: TrackAddKind::Video,
    }))?;
    let sequence = state.active_sequence().context("Sequence")?;
    let track = sequence
        .video_tracks
        .iter()
        .find(|t| !before.contains(&t.id))
        .context("new track")?
        .id;
    let rate = sequence.time_base();
    for (frame, path) in paths.iter().enumerate() {
        remaining(deadline, CAPTURE_TIMEOUT)?;
        let asset = assets
            .iter()
            .find(|a| a.file_path() == Some(path.as_path()))
            .context("ordinary import omitted EXR")?;
        state.dispatch_action(asset_set_interpretation_action(
            AssetSetInterpretationPayload {
                asset_id: asset.id,
                interpretation: AssetMediaInterpretation {
                    color: MediaColorInterpretation::Override {
                        color_space: ColorSpace::LinearRec2020,
                    },
                    range: MediaRangeInterpretation::Override { range: MediaSignalRange::Full },
                    ..Default::default()
                },
            },
        ))?;
        let frame = i64::try_from(frame)?;
        state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
            asset_id: asset.id,
            target_track_id: track,
            position: FramePosition::new(frame, rate),
        }))?;
        let expected_position = TimelineTime::from_frame_position(FramePosition::new(frame, rate))?;
        let clip = state
            .active_sequence()
            .context("Sequence")?
            .video_tracks
            .iter()
            .find(|t| t.id == track)
            .context("track")?
            .clips
            .iter()
            .find(|c| c.position == expected_position)
            .context("placed clip")?
            .id;
        state.dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
            clip_ids: vec![clip],
            edge: TimelineTrimPayloadEdge::Out,
            position: FramePosition::new(frame + 1, rate),
        }))?;
        let sequence = state.active_sequence().context("Sequence")?;
        let clip = sequence
            .video_tracks
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.id == clip)
            .context("trimmed clip")?;
        ensure!(
            matches!(&clip.content, ClipContent::Media { interpretation, .. } if interpretation.alpha == AlphaInterpretation::Straight),
            "stimulus clip must retain straight alpha interpretation"
        );
        ensure!(
            clip.position == expected_position
                && clip.end_position()?
                    == TimelineTime::from_frame_position(FramePosition::new(frame + 1, rate))?,
            "nonexact stimulus frame placement"
        );
    }
    Ok(())
}

fn export_case(
    state: &mut crate::app::AppState,
    frame: i64,
    preset: &ExportPreset,
    output: &Path,
    extension: &str,
    case: &mut Value,
    deadline: Instant,
) -> anyhow::Result<()> {
    remaining(deadline, CAPTURE_TIMEOUT)?;
    let sha256_file = |path: &Path| sha256_file_until(path, deadline);
    let before: BTreeSet<_> = state.export_jobs_snapshot().iter().map(|j| j.id).collect();
    state.dispatch_action(export_enqueue_action(TimelineExportRequest {
        preset: preset.clone(),
        sequence_id: Some(state.active_sequence().context("Sequence")?.id),
        range: TimelineExportRange::WorkArea { start_frame: frame, end_frame_exclusive: frame + 1 },
        output_path: output.to_path_buf(),
        output_policy: ExportOutputPolicy::CreateNew,
        broadcast_qc: None,
        regulatory_pse: None,
        frozen_ancillary: None,
    }))?;
    let jobs: Vec<_> = state
        .export_jobs_snapshot()
        .into_iter()
        .filter(|j| !before.contains(&j.id))
        .collect();
    ensure!(
        jobs.len() == 1,
        "capture must admit exactly one ordinary Export job"
    );
    case["job_admission"] = serde_json::to_value(&jobs[0])?;
    let job = wait_for_export_job_until(
        state,
        jobs[0].id,
        deadline.min(Instant::now() + Duration::from_secs(600)),
    )?;
    case["job_terminal"] = serde_json::to_value(&job)?;
    ensure!(
        matches!(job.status, JobStatus::Completed) && job.executed,
        "capture export failed: {:?}",
        job.status
    );
    ensure!(
        job.terminal_evidence.as_ref().is_some_and(|t| t.generation == job.generation
            && t.disposition == mondrian_core::ExecutionTerminalDisposition::Completed),
        "capture terminal authority mismatch"
    );
    let manifest_path = output.join("manifest.json");
    let manifest: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    case["native_manifest"] = manifest.clone();
    case["native_manifest_sha256"] = json!(sha256_file(&manifest_path)?);
    ensure!(
        manifest["frame_count"] == 1 && manifest["width"] == 64 && manifest["height"] == 64,
        "native output raster/frame count mismatch"
    );
    let frames = manifest["frames"].as_array().context("native frame manifest")?;
    ensure!(frames.len() == 1, "native frame inventory mismatch");
    let name = frames[0]["file_name"].as_str().context("native frame name")?;
    ensure!(
        Path::new(name).components().count() == 1
            && Path::new(name).extension().is_some_and(|v| v == extension),
        "unexpected native payload name"
    );
    let artifact = output.join(name);
    let digest = sha256_file(&artifact)?;
    ensure!(
        frames[0]["sha256"] == digest,
        "native publication digest mismatch"
    );
    case["native_artifact"] =
        json!({"path":artifact,"sha256":digest,"byte_len":std::fs::metadata(&artifact)?.len()});
    if extension == "tiff" {
        let mut decoder = tiff::decoder::Decoder::new(BufReader::new(File::open(&artifact)?))?;
        ensure!(
            decoder.dimensions()? == (64, 64) && decoder.colortype()? == tiff::ColorType::RGB(16),
            "PQ native TIFF must be opaque RGB16 unsigned integer"
        );
        let tiff::decoder::DecodingResult::U16(samples) = decoder.read_image()? else {
            anyhow::bail!("PQ TIFF contains non-UInt16 samples")
        };
        ensure!(
            samples.len() == 64 * 64 * 3,
            "PQ native sample inventory invalid"
        );
        let rgba: Vec<[f32; 4]> = samples
            .chunks_exact(3)
            .map(|v| {
                [
                    f32::from(v[0]) / 65535.0,
                    f32::from(v[1]) / 65535.0,
                    f32::from(v[2]) / 65535.0,
                    1.0,
                ]
            })
            .collect();
        let payload = output.join("pq-rgba-f32.json");
        write_new_json(
            &payload,
            &json!({"schema_version":1,"width":64,"height":64,"encoding":"bt2100_pq_rgba_f32","pixels":rgba}),
        )?;
        case["independent_decoded_payload"] = json!({"path":payload,"sha256":sha256_file(&payload)?,"decoder":"tiff 0.11.3","source_native_sha256":digest});
        case["native_precision"] = json!({"kind":"unsigned_integer","bits_per_channel":16,"normalization_divisor":65535,"float_payload_is_native_float_capture":false});
        case["reference_white_203_nits_qualified"] = json!(false);
    }
    remaining(deadline, CAPTURE_TIMEOUT)?;
    case["capture_completed"] = json!(true);
    Ok(())
}

fn remaining(deadline: Instant, maximum: Duration) -> anyhow::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    ensure!(
        !remaining.is_zero(),
        "cross-application capture original deadline elapsed"
    );
    Ok(remaining.min(maximum))
}

fn sha256_file_until(path: &Path, deadline: Instant) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut digest = Sha256::new();
    loop {
        remaining(deadline, CAPTURE_TIMEOUT)?;
        let count = file.read(&mut buffer)?;
        remaining(deadline, CAPTURE_TIMEOUT)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn write_new_json(path: &Path, value: &Value) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_capture_deadline_preserves_failure_without_admitting_owners() {
        let root = tempfile::tempdir().expect("root");
        let output = root.path().join("expired");
        assert!(run_until(root.path().join("absent"), output.clone(), Instant::now()).is_err());
        let report: Value = serde_json::from_slice(
            &std::fs::read(output.join("mondrian-capture.json")).expect("durable failure"),
        )
        .expect("JSON");
        assert_eq!(report["capture_completed"], false);
        assert!(report["failure"].as_str().expect("failure").contains("original deadline"));
        assert!(report.get("app_shutdown").is_none());
        assert!(report.get("viewer_shutdown").is_none());
        assert!(report["inputs"].as_array().expect("inputs").is_empty());
    }

    #[test]
    fn capture_hash_rejects_expired_deadline_before_reading_payload() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("payload");
        std::fs::write(&path, b"bounded source").expect("payload");
        assert!(sha256_file_until(&path, Instant::now()).is_err());
        assert_eq!(
            sha256_file_until(&path, Instant::now() + Duration::from_secs(5)).expect("hash"),
            format!("{:x}", Sha256::digest(b"bounded source"))
        );
    }

    #[test]
    fn missing_capture_fixture_preserves_failure_report_without_starting_owners() {
        let root = tempfile::tempdir().expect("test root");
        let output = root.path().join("missing-source-run");
        assert!(run(root.path().join("absent-stimulus"), output.clone()).is_err());
        let report: Value = serde_json::from_slice(
            &std::fs::read(output.join("mondrian-capture.json")).expect("durable failure"),
        )
        .expect("report");
        assert_eq!(report["capture_completed"], false);
        assert!(report["failure"].is_string());
        assert!(report.get("app_shutdown").is_none());
        assert!(report.get("viewer_shutdown").is_none());
        assert!(report["cases"].as_array().expect("cases").is_empty());
    }

    #[test]
    fn capture_cannot_replace_existing_run_or_its_evidence() {
        let root = tempfile::tempdir().expect("test root");
        let evidence = root.path().join("mondrian-capture.json");
        std::fs::write(&evidence, b"prior independent evidence").expect("write original");
        assert!(run(root.path().join("absent"), root.path().to_path_buf()).is_err());
        assert_eq!(
            std::fs::read(evidence).expect("original survives"),
            b"prior independent evidence"
        );
    }
}
