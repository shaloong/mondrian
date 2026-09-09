//! Bounded three-phase local software smoke over ordinary product owners.

#[path = "local_media_export_native.rs"]
mod export_native;

pub(super) fn run_export_native(root: PathBuf, output: PathBuf) -> anyhow::Result<PathBuf> {
    export_native::run(root, output)
}

use super::fixture::sha256_file;
use super::harness::DirectoryCleanup;
use crate::app::endurance_campaign::{EnduranceExecutionOwners, EnduranceExecutionStartFailure};
use crate::app::endurance_export::{FrozenRepeatedExportPhase, FrozenRepeatedExportRequest};
use crate::app::endurance_playback::PersistentTimelinePlaybackPhase;
use crate::app::perf_process_memory::{ProfessionalProcessMemorySampler, TimedProcessMemorySample};
use crate::app::ui_actions::*;
use crate::app::AppState;
use anyhow::{ensure, Context};
use mondrian_core::{
    timeline_data::FieldOrder, FramePosition, PictureInterpretationOverrides,
    ProjectColorEnvironment, ProjectSettings, Rational, Resolution, TimelineTime,
};
use mondrian_export::preset::{ExportPreset, TimelineExportRange};
use mondrian_platform::{ProcessMemoryProbe, SystemPlatformService};
use mondrian_timeline::SequenceSettings;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const FIXTURES: [&str; 6] = [
    "HEVC Samples/hevc_4k24P_main10_hdr10_3.mp4",
    "HEVC Samples/hevc_4k25P_main10_1.mp4",
    "HEVC Samples/hevc_4k60P_main10_pq10.mp4",
    "HEVC Samples/hevc_with_alpha_layer.mp4",
    "Graphics/Adobe Mayan Art.png",
    "Audio/APM_Adobe_Going Home_v3.wav",
];
const MEMORY_LIMIT: u64 = 6 * 1024 * 1024 * 1024;
const MAXIMUM_PHASE_SECONDS: u64 = 60;
const MAXIMUM_ARTIFACTS: u64 = 8;
const MINIMUM_EXPORT_COMPLETION_GRACE_SECONDS: u64 = 90;
const PROXY_CONTROL_EXPORT_COMPLETION_GRACE_SECONDS: u64 = 360;
const PROXY_CONTROL_HARD_DEADLINE_SECONDS: u64 = 1_200;
const TIMELINE_SEGMENT_SECONDS: u64 = 2;
const RECOVERY_SEEK_TARGET_FRAME: u64 = 1_800;
const TIMELINE_TERMINAL_GUARD_SECONDS: u64 = 10;

/// Execute software-only stages with no commercial-campaign admission or qualification.
pub(super) fn run(root: PathBuf, output: PathBuf, phase_seconds: u64) -> anyhow::Result<PathBuf> {
    run_profile(root, output, phase_seconds, false)
}

pub(super) fn run_proxy_control(
    root: PathBuf,
    output: PathBuf,
    phase_seconds: u64,
) -> anyhow::Result<PathBuf> {
    run_profile(root, output, phase_seconds, true)
}

fn run_profile(
    root: PathBuf,
    output: PathBuf,
    phase_seconds: u64,
    proxy_control: bool,
) -> anyhow::Result<PathBuf> {
    ensure!(
        (15..=MAXIMUM_PHASE_SECONDS).contains(&phase_seconds),
        "phase seconds must be in 15..=60; default 30"
    );
    ensure!(!output.exists(), "smoke output must be a new directory");
    std::fs::create_dir_all(&output)?;
    let output = mondrian_assets::canonical_native_path(&output)?;
    let report_path = output.join("local-media-smoke.json");
    let started = Instant::now();
    let hard_deadline_seconds = if proxy_control {
        PROXY_CONTROL_HARD_DEADLINE_SECONDS
    } else {
        900
    };
    let deadline = started + Duration::from_secs(hard_deadline_seconds);
    let profile = if proxy_control {
        "local-media-proxy-control-three-phase-v2"
    } else {
        "local-media-three-phase-smoke-v1"
    };
    let mut report = json!({"schema_version":1,"profile":profile,"status":"NotRun","commercial_qualification":false,"duration_72h_qualified":false,"physical_surface_qualified":false,"physical_reference_output_qualified":false,"hdr_surface_qualified":false,"original_native_media_qualified":false,"proxy_control":proxy_control,"phase_seconds":phase_seconds,"hard_deadline_seconds":hard_deadline_seconds,"memory_stop_bytes":MEMORY_LIMIT,"maximum_artifacts_per_phase":MAXIMUM_ARTIFACTS,"fixtures":[],"phases":[]});
    report["program_frame_rate"] = if proxy_control {
        json!({"numerator": 24, "denominator": 1})
    } else {
        json!({"numerator": 60, "denominator": 1})
    };
    let mut leases = Vec::new();
    let mut paths = Vec::new();
    let admission = (|| -> anyhow::Result<()> {
        for relative in FIXTURES {
            let path = mondrian_assets::canonical_asset_file_path(&root.join(relative))?;
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(1);
            }
            let mut lease = options.open(&path)?;
            ensure!(
                lease.metadata()?.is_file() && lease.metadata()?.len() <= 512 * 1024 * 1024,
                "fixture must be a regular file within 512 MiB"
            );
            let digest = hash_until(&mut lease, deadline)?;
            report["fixtures"]
                .as_array_mut()
                .context("fixture inventory")?
                .push(json!({"path":path,"sha256":digest,"byte_len":lease.metadata()?.len()}));
            paths.push(path);
            leases.push(lease);
        }
        let mut tools = Vec::new();
        for command in [
            mondrian_media::ffmpeg_command()?,
            mondrian_media::ffprobe_command()?,
        ] {
            let requested = PathBuf::from(command.get_program());
            let candidate = if requested.is_file() {
                Some(requested.clone())
            } else {
                let name = format!(
                    "{}{}",
                    requested.to_string_lossy(),
                    std::env::consts::EXE_SUFFIX
                );
                std::env::var_os("PATH").and_then(|path| {
                    std::env::split_paths(&path)
                        .map(|directory| directory.join(&name))
                        .find(|path| path.is_file())
                })
            }
            .context("FFmpeg/ffprobe provider is missing before App admission")?;
            tools.push(json!({"requested_program":requested,"available_provider_file":mondrian_assets::canonical_native_path(&candidate)?}));
        }
        report["command_provider_admission"] = json!(tools);
        let memory = SystemPlatformService.product_process_tree_memory();
        ensure!(
            memory.inventory_complete && memory.private_memory_bytes.is_some(),
            "complete native process-tree memory admission unavailable"
        );
        report["memory_admission"] = serde_json::to_value(memory)?;
        Ok(())
    })();
    if let Err(error) = admission {
        report["admission_notrun_reason"] = json!(format!("{error:#}"));
        write_new(&report_path, &report)?;
        return Ok(report_path);
    }
    report["status"] = json!("Started");
    for phase in 0..3 {
        let phase_path = output.join(format!("phase-{}", phase + 1));
        std::fs::create_dir(&phase_path)?;
        let workload = [
            "playback-seek-cache",
            "continuous-export-with-playback",
            "concurrent-cancel-retry-seek-cache",
        ][phase];
        let mut evidence = json!({"phase":phase+1,"status":"Started","workload":workload,"events":[],"recovery_receipts":[],"samples":[]});
        let result = run_phase(
            phase,
            &paths,
            &phase_path,
            phase_seconds,
            deadline,
            &mut evidence,
            proxy_control,
        );
        if let Err(error) = &result {
            evidence["status"] = json!("Failed");
            evidence["failure"] = json!(format!("{error:#}"));
        }
        write_new(&phase_path.join("phase-owner-report.json"), &evidence)?;
        report["phases"].as_array_mut().context("phase history")?.push(evidence);
        if let Err(error) = result {
            report["status"] = json!("Failed");
            report["failure"] = json!(format!("{error:#}"));
            report["elapsed_millis"] = json!(started.elapsed().as_millis());
            write_new(&report_path, &report)?;
            anyhow::bail!(
                "local smoke failed; raw report {}: {error:#}",
                report_path.display()
            );
        }
    }
    drop(leases);
    report["status"] = json!("Completed");
    report["elapsed_millis"] = json!(started.elapsed().as_millis());
    report["device_session_recreation"] = json!("Each phase starts a fresh real Headless GPU/Preview session after the prior consuming closure. This is not a physical Window surface-reopen receipt.");
    write_new(&report_path, &report)?;
    Ok(report_path)
}

fn run_phase(
    index: usize,
    paths: &[PathBuf],
    output: &Path,
    seconds: u64,
    deadline: Instant,
    report: &mut Value,
    proxy_control: bool,
) -> anyhow::Result<()> {
    let mut app = Some(AppState::new());
    let mut runtime_cleanup = DirectoryCleanup::default();
    let mut owners = None;
    let mut startup_failure: Option<EnduranceExecutionStartFailure> = None;
    let mut playback: Option<PersistentTimelinePlaybackPhase> = None;
    let mut exports: Option<FrozenRepeatedExportPhase> = None;
    let mut history = BTreeMap::new();
    let started = Instant::now();
    let result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> anyhow::Result<()> {
            let state = app.as_mut().context("App owner")?;
            ensure!(
                Instant::now() < deadline,
                "local smoke deadline elapsed before phase startup"
            );
            let settings = SequenceSettings {
                resolution: Resolution { width: 640, height: 360 },
                frame_rate: if proxy_control {
                    Rational::FPS_24
                } else {
                    Rational::FPS_60
                },
                ..SequenceSettings::default()
            };
            let mut project_settings = ProjectSettings::default();
            if proxy_control {
                project_settings.cache_dir = Some(
                    output.parent().context("proxy control root")?.join("proxy-control-cache"),
                );
            }
            state.create_new_project_with_settings_at(
                output.join("smoke.mdp"),
                "Local media smoke",
                settings,
                ProjectColorEnvironment::default(),
                project_settings,
            )?;
            runtime_cleanup.track(state.project_runtime_dir().map(Path::to_path_buf));
            let (playback_rate, frames_per_second) = if proxy_control {
                (Rational::FPS_24, 24_u64)
            } else {
                (Rational::FPS_60, 60_u64)
            };
            let export_grace_seconds = if proxy_control {
                PROXY_CONTROL_EXPORT_COMPLETION_GRACE_SECONDS
            } else {
                MINIMUM_EXPORT_COMPLETION_GRACE_SECONDS
            };
            let timeline_extent = local_smoke_timeline_extent(
                seconds,
                frames_per_second,
                export_grace_seconds,
                index != 1,
            )?;
            report["timeline_extent"] = json!({
                "segment_count": timeline_extent.segment_count,
                "frames_per_segment": timeline_extent.frames_per_segment,
                "minimum_presented_frames": timeline_extent.minimum_presented_frames,
                "terminal_guard_seconds": TIMELINE_TERMINAL_GUARD_SECONDS,
            });
            author(
                state,
                paths,
                index,
                deadline,
                report,
                proxy_control,
                timeline_extent.segment_count,
            )?;
            if proxy_control {
                wait_for_proxy_control(state, deadline, report)?;
                prove_proxy_control_selection(state, report)?;
            }
            // Ordinary drop/trim authoring moves the playhead; establish the phase admission state.
            state.stop()?;
            state.dispatch_action(timeline_seek_action(FramePosition::new(
                0,
                state.active_sequence().context("Sequence")?.time_base(),
            )))?;
            let persistence = state.request_project_save()?;
            state.wait_for_persistence_request_until(persistence, deadline)?;
            report["project_sha256"] = json!(sha256_file(&output.join("smoke.mdp"))?);
            match EnduranceExecutionOwners::start(state) {
                Ok(value) => owners = Some(value),
                Err(failure) => {
                    let diagnostic = failure.diagnostic().to_string();
                    startup_failure = Some(failure);
                    anyhow::bail!("execution startup: {diagnostic}");
                }
            }
            let owner = owners.as_mut().context("execution owners")?;
            playback = Some(PersistentTimelinePlaybackPhase::start_local_smoke(
                state,
                owner,
                timeline_extent.minimum_presented_frames,
                playback_rate,
                deadline,
                Duration::from_secs(5),
            )?);
            if index > 0 {
                let mut preset = ExportPreset::h264_aac_sdr_1080p();
                preset.resolution =
                    Some(mondrian_export::preset::Resolution { width: 640, height: 360 });
                exports = Some(FrozenRepeatedExportPhase::start_until(
                    state,
                    FrozenRepeatedExportRequest {
                        phase_id: format!("local-smoke-{index}"),
                        frozen_ancillary: None,
                        approved_bmx: None,
                        preset,
                        sequence_id: None,
                        range: TimelineExportRange::WorkArea {
                            start_frame: 0,
                            end_frame_exclusive: 120,
                        },
                        output_directory: output.to_path_buf(),
                        artifact_prefix: "smoke-export".to_owned(),
                        broadcast_qc: None,
                        regulatory_pse: None,
                        verification_policy: mondrian_export::IndependentExportArtifactPolicy::new(
                            64 * 1024 * 1024,
                            Duration::from_secs(30),
                        )?,
                    },
                    deadline,
                )?);
                if index == 2 {
                    exports.as_mut().context("export owner")?.begin_cancel_retry_recovery(1)?;
                }
            }
            // Export preparation may perform synchronous snapshot/admission work after
            // AudioDevice has entered clock ownership. Close that startup interval
            // through the same phase boundary used by the production campaign: prove
            // the current A/V picture, then freeze transport until the first measured
            // coordinator interval resumes it. Otherwise harness setup time is charged
            // as an unobserved multi-frame advance on the first sample.
            playback
                .as_mut()
                .context("playback owner")?
                .refresh_pre_measurement_picture(state, owner, deadline)?;
            let run_started = Instant::now();
            let finish_at = run_started + Duration::from_secs(seconds);
            let observation_deadline =
                deadline.min(finish_at + Duration::from_secs(export_grace_seconds));
            report["minimum_export_completion_grace_seconds"] = json!(export_grace_seconds);
            let memory_sampler = ProfessionalProcessMemorySampler::start(run_started)?;
            let mut memory_sample_count = 0_usize;
            let mut recovery_done = false;
            // Match `EnduranceProductRuntime::pump_until`: all fallible
            // measurement setup above runs with transport frozen, then the
            // same paired session re-enters realtime residency immediately
            // before its first coordinator interval.
            playback.as_mut().context("playback owner")?.resume_audio_device_window(
                state,
                owner,
                Some(deadline),
            )?;
            loop {
                let now = Instant::now();
                let minimum_complete = exports.as_ref().is_none_or(|phase| {
                    export_minimum_complete(
                        phase.verified_artifacts(),
                        phase.cancel_retry_recovery_in_progress(),
                    )
                });
                if observation_complete(now, finish_at, observation_deadline, minimum_complete)? {
                    break;
                }
                playback.as_mut().context("playback owner")?.pump_interval(state, owner)?;
                poll_exports(state, &mut exports, &mut history, report, started)?;
                if !recovery_done && run_started.elapsed() >= Duration::from_secs(5) && index != 1 {
                    let phase = playback.as_mut().context("playback owner")?;
                    let recovery_seek_target = i64::try_from(RECOVERY_SEEK_TARGET_FRAME)?;
                    let target = if state.current_frame() < recovery_seek_target {
                        recovery_seek_target
                    } else {
                        120
                    };
                    let seek = phase.recover_seek(state, owner, 1, target, Some(deadline))?;
                    report["recovery_receipts"]
                        .as_array_mut()
                        .context("recovery history")?
                        .push(json!({"json":seek.canonical_json(),"sha256":seek.sha256()}));
                    let pressure = phase.recover_cache_pressure(state, owner, 1, Some(deadline))?;
                    report["recovery_receipts"]
                        .as_array_mut()
                        .context("recovery history")?
                        .push(json!({"json":pressure.canonical_json(),"sha256":pressure.sha256()}));
                    if index == 2 {
                        let reopen = phase.recover_local_headless_reopen(state, owner, deadline);
                        let failure = reopen.failure.clone();
                        report["headless_device_reopen"] = serde_json::to_value(reopen)?;
                        if let Some(error) = failure {
                            anyhow::bail!("local Headless device reopen: {error}");
                        }
                    }
                    recovery_done = true;
                }
                for sample in memory_sampler.drain_ready() {
                    record_local_memory_sample(
                        sample,
                        state,
                        owner,
                        report,
                        started,
                        observation_deadline,
                    )?;
                    memory_sample_count = memory_sample_count.saturating_add(1);
                }
            }
            for sample in memory_sampler.finish()? {
                record_local_memory_sample(
                    sample,
                    state,
                    owner,
                    report,
                    started,
                    observation_deadline,
                )?;
                memory_sample_count = memory_sample_count.saturating_add(1);
            }
            ensure!(
                memory_sample_count > 0,
                "native process-tree memory sampler produced no evidence"
            );
            ensure!(
                index == 1 || recovery_done,
                "seek/cache recovery did not execute"
            );
            report["accepted_intervals"] =
                json!(playback.as_ref().context("playback owner")?.accepted_intervals());
            Ok(())
        }))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
    // Every actual owner stays outside the unwind scope and shares one close deadline.
    let close_deadline = deadline.min(Instant::now() + Duration::from_secs(90));
    if let Some(state) = app.as_ref() {
        report["playback_before_close"] = json!({
            "engine": format!("{:?}", state.playback_engine.snapshot()),
            "audio": format!("{:?}", state.audio_playback_snapshot()),
            "evidence": format!("{:?}", state.playback_evidence_report()),
        });
    }
    let mut close_errors = Vec::new();
    if let (Some(phase), Some(state), Some(owner)) =
        (playback.as_mut(), app.as_mut(), owners.as_mut())
    {
        let close = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            phase.begin_close(state, owner).map_err(anyhow::Error::from)
        }))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
        if let Err(error) = close {
            close_errors.push(error.to_string());
        }
    }
    if let Some(phase) = exports.as_mut() {
        phase.begin_close();
    }
    while exports.as_ref().is_some_and(|phase| !phase.is_quiescent())
        && Instant::now() < close_deadline
    {
        let Some(state) = app.as_mut() else { break };
        let poll = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            poll_exports(state, &mut exports, &mut history, report, started)
        }))
        .unwrap_or_else(|payload| {
            Err(crate::app::headless_execution_startup::startup_panic_diagnostic(payload))
        });
        if let Err(error) = poll {
            close_errors.push(error.to_string());
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    report["verified_artifacts"] =
        json!(exports.as_ref().map_or(0, |phase| phase.verified_artifacts()));
    if exports
        .as_ref()
        .is_some_and(|phase| !phase.is_quiescent() || phase.cancel_retry_recovery_in_progress())
    {
        close_errors
            .push("Export phase did not become quiescent with completed recovery".to_owned());
    }
    if index > 0 && exports.as_ref().is_some_and(|phase| phase.verified_artifacts() < 2) {
        close_errors
            .push("short phase requires at least two independently verified exports".to_owned());
    }
    if let Some(state) = app.as_ref() {
        for job in state.export_jobs_snapshot() {
            history.insert(job.id, job);
        }
    }
    // Frozen phase holds the queue Arc; release it before App consumes its queue owner.
    if let Some(phase) = exports.take() {
        let raw = phase.shutdown_until(close_deadline);
        if !raw.all_resources_released() {
            close_errors.push("phase-owned independent Export verifier did not close".to_owned());
        }
        report["export_verifier_shutdown"] = serde_json::to_value(raw)?;
    }
    report["stopped_picture_preparation"] = serde_json::to_value(
        owners.as_ref().and_then(EnduranceExecutionOwners::stopped_preparation),
    )?;
    drop(playback.take());
    report["av_picture_completions"] =
        serde_json::to_value(owners.as_ref().map(EnduranceExecutionOwners::av_completions))?;
    let state = app.take().context("App owner missing at consuming close")?;
    let closed = if let Some(failure) = startup_failure.take() {
        let (_, raw) = failure.shutdown_until(state, close_deadline);
        let clean = raw.all_created_resources_released();
        report["startup_shutdown"] = serde_json::to_value(raw)?;
        clean
    } else if let Some(owner) = owners.take() {
        let raw = owner.shutdown_local_smoke_until(state, close_deadline)?;
        let clean = raw.all_remaining_resources_released();
        report["owner_shutdown"] = serde_json::to_value(raw)?;
        clean
    } else {
        let raw = state.shutdown_for_endurance(close_deadline);
        let clean = raw.all_resources_released();
        report["app_shutdown"] = serde_json::to_value(raw)?;
        clean
    };
    report["export_job_history"] = serde_json::to_value(history.into_values().collect::<Vec<_>>())?;
    report["close_errors"] = json!(close_errors);
    if !closed {
        runtime_cleanup.retain();
    }
    result?;
    ensure!(
        closed && close_errors.is_empty(),
        "local smoke owner closure or minimum export count failed"
    );
    ensure!(
        Instant::now() < deadline,
        "local smoke closure completed after original deadline"
    );
    report["status"] = json!("Completed");
    Ok(())
}

fn record_local_memory_sample(
    timed: TimedProcessMemorySample,
    state: &AppState,
    owner: &EnduranceExecutionOwners,
    report: &mut Value,
    started: Instant,
    observation_deadline: Instant,
) -> anyhow::Result<()> {
    let within_limit = timed.sample.inventory_complete
        && timed.sample.private_memory_bytes.is_some_and(|value| value <= MEMORY_LIMIT);
    let facts = owner.capture_facts(state)?;
    report["samples"].as_array_mut().context("sample history")?.push(json!({
        "elapsed_millis": started.elapsed().as_millis(),
        "memory_observed_at_us": timed.observed_at_us,
        "memory": timed.sample,
        "facts": facts,
    }));
    ensure!(
        within_limit,
        "native process-tree memory unavailable or over 6 GiB stop limit"
    );
    ensure!(
        Instant::now() < observation_deadline,
        "local smoke sampling exceeded original observation deadline"
    );
    Ok(())
}

fn export_minimum_complete(verified: u64, recovery_in_progress: bool) -> bool {
    verified >= 2 && !recovery_in_progress
}

fn observation_complete(
    now: Instant,
    finish_at: Instant,
    deadline: Instant,
    minimum_complete: bool,
) -> anyhow::Result<bool> {
    ensure!(
        now < deadline,
        "local smoke observation/minimum export completion deadline reached"
    );
    Ok(now >= finish_at && minimum_complete)
}

fn poll_exports(
    state: &mut AppState,
    exports: &mut Option<FrozenRepeatedExportPhase>,
    history: &mut BTreeMap<mondrian_core::JobId, mondrian_export::queue::ExportJobSnapshot>,
    report: &mut Value,
    started: Instant,
) -> anyhow::Result<()> {
    state.poll_export_queue();
    for job in state.export_jobs_snapshot() {
        history.insert(job.id, job);
    }
    ensure!(history.len() <= 16, "bounded smoke job inventory exceeded");
    if let Some(phase) = exports.as_mut() {
        let events = phase.poll(u64::try_from(started.elapsed().as_micros())?)?;
        let target = report["events"].as_array_mut().context("event inventory")?;
        ensure!(
            target.len() + events.len() <= 32,
            "bounded event inventory exceeded"
        );
        for event in events {
            target.push(serde_json::to_value(event)?);
        }
        if phase.verified_artifacts() >= MAXIMUM_ARTIFACTS {
            phase.begin_close();
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalSmokeTimelineExtent {
    segment_count: u64,
    frames_per_segment: u64,
    minimum_presented_frames: u64,
}

fn local_smoke_timeline_extent(
    phase_seconds: u64,
    frames_per_second: u64,
    export_grace_seconds: u64,
    recovery_enabled: bool,
) -> anyhow::Result<LocalSmokeTimelineExtent> {
    let horizon_seconds = phase_seconds
        .checked_add(export_grace_seconds)
        .and_then(|seconds| seconds.checked_add(TIMELINE_TERMINAL_GUARD_SECONDS))
        .context("local smoke Timeline horizon overflow")?;
    let horizon_frames = horizon_seconds
        .checked_mul(frames_per_second)
        .context("local smoke Timeline frame horizon overflow")?;
    let minimum_presented_frames = horizon_frames
        .checked_add(if recovery_enabled {
            RECOVERY_SEEK_TARGET_FRAME
        } else {
            0
        })
        .context("local smoke recovery extent overflow")?;
    let frames_per_segment = frames_per_second
        .checked_mul(TIMELINE_SEGMENT_SECONDS)
        .context("local smoke Timeline segment extent overflow")?;
    ensure!(
        frames_per_segment > 0,
        "local smoke Timeline rate must be nonzero"
    );
    let required_content_frames = minimum_presented_frames
        .checked_add(1)
        .context("local smoke terminal guard frame overflow")?;
    let segment_count = required_content_frames.div_ceil(frames_per_segment);
    ensure!(
        segment_count > 0 && segment_count <= 512,
        "local smoke Timeline segment count exceeds its fixed bound"
    );
    Ok(LocalSmokeTimelineExtent {
        segment_count,
        frames_per_segment,
        minimum_presented_frames,
    })
}

fn author(
    state: &mut AppState,
    paths: &[PathBuf],
    index: usize,
    deadline: Instant,
    report: &mut Value,
    proxy_control: bool,
    segment_count: u64,
) -> anyhow::Result<()> {
    let import_paths = if proxy_control {
        vec![
            paths[0].clone(),
            paths[1].clone(),
            paths[2].clone(),
            paths[4].clone(),
            paths[5].clone(),
        ]
    } else {
        paths.to_vec()
    };
    state.dispatch_action(mondrian_editor_state::Action::ImportMedia(
        import_paths.clone(),
    ))?;
    let import_deadline = deadline.min(Instant::now() + Duration::from_secs(120));
    while state.pending_media_import_batches() > 0 {
        state.poll_media_imports();
        ensure!(
            Instant::now() < import_deadline,
            "bounded real media import timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let assets = state.asset_library().context("asset library")?.list_assets()?;
    let import_diagnostics = state.media_import_diagnostics();
    report["media_import"] = json!({
        "expected_paths": import_paths,
        "published_assets": assets
            .iter()
            .map(|asset| json!({"asset_id": asset.id, "path": asset.file_path()}))
            .collect::<Vec<_>>(),
        "started_workers": import_diagnostics.started_workers,
        "imported_files": import_diagnostics.imported_files,
        "failed_files": import_diagnostics.failed_files,
        "preparation_failures": import_diagnostics.preparation_failures,
        "publication_failures": import_diagnostics.publication_failures,
        "terminal_records": import_diagnostics
            .terminal_records
            .iter()
            .map(|terminal| json!({
                "batch_id": terminal.batch_id,
                "path": terminal.path,
                "disposition": format!("{:?}", terminal.evidence.disposition),
                "failure": terminal.failure.map(|failure| format!("{failure:?}")),
                "elapsed_micros": terminal.elapsed.as_micros(),
            }))
            .collect::<Vec<_>>(),
    });
    ensure!(
        assets.len() == import_paths.len(),
        "ordinary media import did not admit all real fixtures: expected={}, imported={}, failed={}, preparation_failures={}, publication_failures={}",
        import_paths.len(),
        assets.len(),
        import_diagnostics.failed_files,
        import_diagnostics.preparation_failures,
        import_diagnostics.publication_failures,
    );
    if proxy_control {
        let mut proxy_mode_assets = Vec::new();
        for asset in assets.iter().filter(|asset| asset.kind == mondrian_assets::AssetKind::Video) {
            if !state.is_asset_proxy_mode(asset.id) {
                state.dispatch_action(asset_set_proxy_mode_action(AssetsSetProxyModePayload {
                    asset_id: asset.id,
                    enabled: true,
                }))?;
            }
            ensure!(
                state.is_asset_proxy_mode(asset.id),
                "product action did not enable the requested Asset proxy mode"
            );
            proxy_mode_assets.push(asset.id);
        }
        ensure!(
            proxy_mode_assets.len() == 3,
            "proxy control requires exactly three opaque video Asset preferences"
        );
        report["proxy_mode_asset_ids"] = json!(proxy_mode_assets);
    }
    report["media_probes"]=json!(assets.iter().map(|asset|json!({"asset_id":asset.id,"path":asset.file_path(),"kind":asset.kind,"probe":asset.media_probe()})).collect::<Vec<_>>());
    let video = add_track(state, TrackAddKind::Video)?;
    let overlay = add_track(state, TrackAddKind::Video)?;
    let audio = add_track(state, TrackAddKind::Audio)?;
    let mut rates = BTreeSet::new();
    for path in &paths[..3] {
        let info = assets
            .iter()
            .find(|asset| asset.file_path() == Some(path.as_path()))
            .and_then(|asset| asset.media_probe())
            .context("HEVC fixture probe")?;
        let video = info.video_streams.first().context("HEVC fixture video stream")?;
        ensure!(
            video.codec == mondrian_core::VideoCodec::H265
                && video.bit_depth == 10
                && video.frame_rate_proven,
            "fixture probe does not prove real HEVC 10-bit with exact frame rate"
        );
        rates.insert((video.frame_rate.num, video.frame_rate.den));
    }
    ensure!(
        rates.len() == 3,
        "real fixture rates must be three distinct cadences"
    );
    // Repeat genuine two-second source contributions far enough to cover the
    // full observation plus independent-Export completion grace and recovery
    // seek displacement. The final segment is the terminal guard authority.
    let frames_per_segment = if proxy_control { 48 } else { 120 };
    for ordinal in 0..segment_count {
        ensure!(
            Instant::now() < deadline,
            "Timeline authoring reached the run deadline"
        );
        let ordinal_usize = usize::try_from(ordinal).context("Timeline segment index overflow")?;
        let video_index = (ordinal_usize + index) % 3;
        let segment_start = i64::try_from(ordinal)?
            .checked_mul(frames_per_segment)
            .context("Timeline segment position overflow")?;
        for (source_path, track, force_progressive) in [
            (&paths[video_index], video, proxy_control),
            (&paths[5], audio, false),
        ] {
            let asset = assets
                .iter()
                .find(|asset| asset.file_path() == Some(source_path.as_path()))
                .context("imported fixture")?;
            place(
                state,
                asset.id,
                track,
                segment_start,
                frames_per_segment,
                force_progressive,
            )?;
        }
        if ordinal % 4 == 0 {
            let source = if index == 2 && !proxy_control { 3 } else { 4 };
            let asset = assets
                .iter()
                .find(|asset| asset.file_path() == Some(paths[source].as_path()))
                .context("alpha fixture")?;
            place(
                state,
                asset.id,
                overlay,
                segment_start,
                frames_per_segment,
                false,
            )?;
        }
    }
    if proxy_control {
        report["proxy_picture_interpretation"] = json!({
            "field_order_override": "progressive",
            "clip_occurrences": segment_count,
            "scope": "validation_project_clip_occurrence",
            "reason": "The source fixtures omit field-order metadata; the control authors the independently inspected progressive scan identity instead of weakening fail-closed source resolution."
        });
    }
    Ok(())
}

fn wait_for_proxy_control(
    state: &mut AppState,
    deadline: Instant,
    report: &mut Value,
) -> anyhow::Result<()> {
    loop {
        ensure!(
            Instant::now() < deadline,
            "product proxy generation exceeded original run deadline"
        );
        state.poll_proxy_generation();
        let diagnostics = state.proxy_generation_diagnostics();
        if diagnostics.queued == 0 && diagnostics.running == 0 && diagnostics.yielding == 0 {
            ensure!(
                diagnostics.failures == 0
                    && diagnostics.rejections == 0
                    && diagnostics.completions.saturating_add(diagnostics.fresh_hits) >= 3,
                "proxy control did not prepare all three opaque video sources: {diagnostics:?}"
            );
            report["proxy_generation"] = json!({
                "generation": diagnostics.generation,
                "admissions": diagnostics.admissions,
                "completions": diagnostics.completions,
                "fresh_hits": diagnostics.fresh_hits,
                "failures": diagnostics.failures,
                "rejections": diagnostics.rejections,
                "terminal_records": diagnostics.terminal_records.iter().map(|record| format!("{record:?}")).collect::<Vec<_>>(),
            });
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn prove_proxy_control_selection(state: &AppState, report: &mut Value) -> anyhow::Result<()> {
    let assets = state.asset_library().context("asset library")?.list_assets()?;
    let mut selections = Vec::new();
    for asset in assets.iter().filter(|asset| asset.kind == mondrian_assets::AssetKind::Video) {
        ensure!(
            state.project_settings().proxy_enabled && state.is_asset_proxy_mode(asset.id),
            "proxy control lost its project or Asset preference"
        );
        let resolved =
            super::proxy_relink::resolve_media_path_for_preference_with_picture_overrides(
                state,
                asset,
                true,
                PictureInterpretationOverrides {
                    field_order: Some(FieldOrder::Progressive),
                    ..Default::default()
                },
            )?;
        ensure!(
            resolved.resolution == "proxy" && Some(resolved.path.as_path()) != asset.file_path(),
            "product Preview did not resolve the fresh proxy for Asset {}",
            asset.id
        );
        selections.push(resolved);
    }
    ensure!(
        selections.len() == 3,
        "proxy control did not prove exactly three product Preview proxy selections"
    );
    report["proxy_preview_selections"] = serde_json::to_value(selections)?;
    Ok(())
}

fn add_track(state: &mut AppState, kind: TrackAddKind) -> anyhow::Result<mondrian_core::TrackId> {
    let sequence = state.active_sequence().context("Sequence")?;
    let before: BTreeSet<_> = sequence
        .video_tracks
        .iter()
        .chain(sequence.audio_tracks.iter())
        .map(|track| track.id)
        .collect();
    state.dispatch_action(track_add_action(TrackAddPayload { kind }))?;
    let sequence = state.active_sequence().context("Sequence")?;
    sequence
        .video_tracks
        .iter()
        .chain(sequence.audio_tracks.iter())
        .find(|track| !before.contains(&track.id))
        .map(|track| track.id)
        .context("new ordinary track")
}

fn place(
    state: &mut AppState,
    asset_id: mondrian_core::AssetId,
    track: mondrian_core::TrackId,
    start: i64,
    duration_frames: i64,
    force_progressive: bool,
) -> anyhow::Result<()> {
    let rate = state.active_sequence().context("Sequence")?.time_base();
    let position = TimelineTime::from_frame_position(FramePosition::new(start, rate))?;
    state.dispatch_action(timeline_drop_asset_action(TimelineDropAssetPayload {
        asset_id,
        target_track_id: track,
        position: FramePosition::new(start, rate),
    }))?;
    let sequence = state.active_sequence().context("Sequence")?;
    let id = sequence
        .video_tracks
        .iter()
        .chain(sequence.audio_tracks.iter())
        .find(|t| t.id == track)
        .context("track")?
        .clips
        .iter()
        .find(|clip| clip.position == position)
        .context("placed media clip")?
        .id;
    state.dispatch_action(timeline_trim_clips_action(TimelineTrimClipsPayload {
        clip_ids: vec![id],
        edge: TimelineTrimPayloadEdge::Out,
        position: FramePosition::new(start + duration_frames, rate),
    }))?;
    if force_progressive {
        let changed = state.set_clip_media_interpretation(
            crate::app::selection::SelectedClipRef {
                track_id: track,
                is_video_track: true,
                clip_id: id,
            },
            mondrian_timeline::clip::MediaInterpretation {
                field_order_override: Some(FieldOrder::Progressive),
                ..Default::default()
            },
        )?;
        ensure!(
            changed,
            "proxy control did not author the progressive Clip interpretation"
        );
    }
    Ok(())
}

fn hash_until(file: &mut File, deadline: Instant) -> anyhow::Result<String> {
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        ensure!(
            Instant::now() < deadline,
            "fixture hash admission timed out"
        );
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hash.update(&buffer[..size]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn write_new(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimum_export_completion_keeps_admission_open_but_never_accepts_late_success() {
        let start = Instant::now();
        let finish = start + Duration::from_secs(30);
        let deadline = finish + Duration::from_secs(90);
        for (count, recovery) in [(0, false), (1, false), (2, true)] {
            assert!(!observation_complete(
                finish,
                finish,
                deadline,
                export_minimum_complete(count, recovery)
            )
            .expect("within grace"));
        }
        assert!(
            observation_complete(finish, finish, deadline, export_minimum_complete(2, false))
                .expect("minimum complete")
        );
        assert!(!observation_complete(start, finish, deadline, true).expect("minimum observation"));
        assert!(observation_complete(deadline, finish, deadline, true).is_err());
    }

    #[test]
    fn missing_real_media_is_notrun_before_any_phase_owner() {
        let root = tempfile::tempdir().expect("root");
        let output = root.path().join("run");
        let path = run(root.path().join("missing"), output, 30).expect("durable NotRun");
        let report: Value =
            serde_json::from_slice(&std::fs::read(path).expect("report")).expect("JSON");
        assert_eq!(report["status"], "NotRun");
        assert!(report["phases"].as_array().expect("phases").is_empty());
        assert_eq!(report["commercial_qualification"], false);
    }

    #[test]
    fn smoke_rejects_unbounded_duration_and_existing_output() {
        let root = tempfile::tempdir().expect("root");
        for seconds in [0, 14, 61, u64::MAX] {
            let output = root.path().join(format!("invalid-{seconds}"));
            assert!(run(root.path().to_path_buf(), output.clone(), seconds).is_err());
            assert!(!output.exists());
        }
        assert!(run(root.path().to_path_buf(), root.path().to_path_buf(), 30).is_err());
    }

    #[test]
    fn timeline_extent_covers_export_grace_seek_displacement_and_terminal_guard() {
        assert_eq!(
            local_smoke_timeline_extent(30, 24, 360, true).expect("proxy recovery extent"),
            LocalSmokeTimelineExtent {
                segment_count: 238,
                frames_per_segment: 48,
                minimum_presented_frames: 11_400,
            }
        );
        assert_eq!(
            local_smoke_timeline_extent(30, 24, 360, false).expect("proxy Export extent"),
            LocalSmokeTimelineExtent {
                segment_count: 201,
                frames_per_segment: 48,
                minimum_presented_frames: 9_600,
            }
        );
        assert_eq!(
            local_smoke_timeline_extent(30, 60, 90, true).expect("native recovery extent"),
            LocalSmokeTimelineExtent {
                segment_count: 81,
                frames_per_segment: 120,
                minimum_presented_frames: 9_600,
            }
        );
        assert!(local_smoke_timeline_extent(u64::MAX, 24, 360, true).is_err());
        assert!(local_smoke_timeline_extent(60, u64::MAX, 360, true).is_err());
    }
}
