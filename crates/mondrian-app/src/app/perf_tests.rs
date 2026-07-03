use super::*;
use crate::app_ui::panels::{ViewerPreviewSource, ViewerPreviewState};
use crate::app_ui::preview::{AppUiPreviewDiagnostics, AppUiPreviewService};
use crate::app_ui::shell::AppUiAppRoot;
use serde::Serialize;
use std::cmp;
use std::fs::OpenOptions;
use std::io::Write;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mondrian_core::types::Rational;
use mondrian_effects::{EffectNode, EffectNodeExt};
use mondrian_timeline::track::Track;
use mondrian_ui_core::tree::TreeWalker;
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::DrawEncoder;
use mondrian_ui_theme::ThemePreset;

#[derive(Debug, Serialize)]
struct PerfCaseReport {
    case: &'static str,
    iterations: usize,
    samples_ms: Vec<u128>,
    avg_ms: u128,
    max_ms: u128,
    threshold_ms: u128,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct AppUiScaleReport {
    scenario: &'static str,
    assets: usize,
    clips: usize,
    effects: usize,
    resize_iterations: usize,
    playback_frames: usize,
    initial_paint_commands: usize,
    playback_paint_commands_max: usize,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_path: PreviewColorPathReport,
    preview_playback_diagnostics: AppUiPreviewDiagnostics,
    preview_playback_color_path: PreviewColorPathReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize)]
struct PreviewMediaPerfReport {
    scenario: &'static str,
    frames: usize,
    cache_iterations: usize,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_path: PreviewColorPathReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Serialize, Default)]
struct PreviewReadinessCounts {
    ready: usize,
    loading: usize,
    stale: usize,
    unavailable: usize,
}

#[derive(Debug, Serialize)]
struct PreviewMediaPlaybackPerfReport {
    scenario: &'static str,
    frames: usize,
    frame_interval_ms: u64,
    readiness: PreviewReadinessCounts,
    preview_diagnostics: AppUiPreviewDiagnostics,
    preview_color_path: PreviewColorPathReport,
    cases: Vec<PerfCaseReport>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
struct PreviewColorPathReport {
    composite_plans: u64,
    composite_elements: u64,
    float_linear_composites: u64,
    legacy_rgba8_composites: u64,
    legacy_reasons: u64,
    gpu_color_stages: u64,
    gpu_blockers: u64,
    rgba8_boundary_calls: u64,
    fully_float_linear: bool,
    gpu_path_ready: bool,
}

impl PreviewColorPathReport {
    fn from_diagnostics(diagnostics: AppUiPreviewDiagnostics) -> Self {
        let legacy_reasons = diagnostics
            .color_composite_legacy_media_blend_mode
            .saturating_add(diagnostics.color_composite_legacy_media_transform)
            .saturating_add(diagnostics.color_composite_legacy_media_effect)
            .saturating_add(diagnostics.color_composite_legacy_solid_blend_mode)
            .saturating_add(diagnostics.color_composite_legacy_solid_transform)
            .saturating_add(diagnostics.color_composite_legacy_solid_effect)
            .saturating_add(diagnostics.color_composite_legacy_adjustment_blend_mode)
            .saturating_add(diagnostics.color_composite_legacy_adjustment_effect);
        let fully_float_linear = diagnostics.color_composite_plans > 0
            && diagnostics.color_composite_legacy_rgba8 == 0
            && diagnostics.color_composite_float_linear == diagnostics.color_composite_plans;
        let gpu_path_ready = diagnostics.color_stage_gpu_blockers == 0
            && diagnostics.color_stage_readback_stages == 0
            && diagnostics.color_stage_upload_stages == 0;

        Self {
            composite_plans: diagnostics.color_composite_plans,
            composite_elements: diagnostics.color_composite_elements,
            float_linear_composites: diagnostics.color_composite_float_linear,
            legacy_rgba8_composites: diagnostics.color_composite_legacy_rgba8,
            legacy_reasons,
            gpu_color_stages: diagnostics.color_stage_gpu_color_stages,
            gpu_blockers: diagnostics.color_stage_gpu_blockers,
            rgba8_boundary_calls: diagnostics.color_rgba8_boundary_calls,
            fully_float_linear,
            gpu_path_ready,
        }
    }
}

fn perf_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn env_usize_clamped(key: &str, default: usize, min: usize, max: usize) -> usize {
    env_usize(key, default).clamp(min, max)
}

fn env_u128(key: &str, default: u128) -> u128 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u128>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn perf_output_path() -> Option<PathBuf> {
    std::env::var_os("MONDRIAN_PERF_OUTPUT").map(PathBuf::from)
}

fn write_report_if_needed(report_json: &str) {
    if let Some(path) = perf_output_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{report_json}");
        }
    }
}

fn run_case<F>(
    case: &'static str,
    iterations: usize,
    threshold_ms: u128,
    mut f: F,
) -> anyhow::Result<PerfCaseReport>
where
    F: FnMut() -> anyhow::Result<()>,
{
    let mut samples_ms = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started_at = Instant::now();
        f()?;
        samples_ms.push(started_at.elapsed().as_millis());
    }

    let total_ms = samples_ms.iter().copied().sum::<u128>();
    let avg_ms = total_ms / cmp::max(iterations as u128, 1);
    let max_ms = samples_ms.iter().copied().max().unwrap_or(0);
    let passed = max_ms <= threshold_ms;

    Ok(PerfCaseReport {
        case,
        iterations,
        samples_ms,
        avg_ms,
        max_ms,
        threshold_ms,
        passed,
    })
}

#[test]
#[ignore = "development performance smoke test; run manually"]
fn perf_project_lifecycle_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let create_threshold_ms = env_u128("MONDRIAN_PERF_CREATE_MS", 8_000);
    let open_threshold_ms = env_u128("MONDRIAN_PERF_OPEN_MS", 6_000);
    let save_threshold_ms = env_u128("MONDRIAN_PERF_SAVE_MS", 6_000);

    let open_iters = env_usize("MONDRIAN_PERF_OPEN_ITERS", 3);
    let save_iters = env_usize("MONDRIAN_PERF_SAVE_ITERS", 5);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root = std::env::temp_dir().join(format!("mondrian_perf_smoke_{uniq}"));
    fs::create_dir_all(&root)?;

    let result = (|| -> anyhow::Result<Vec<PerfCaseReport>> {
        let project_path = root.join("perf-smoke.mdp");
        let mut state = AppState::new();

        let create_case = run_case("project.create_new_project", 1, create_threshold_ms, || {
            state.create_new_project_at(
                project_path.clone(),
                "perf-smoke",
                1920,
                1080,
                Rational::FPS_25,
            )
        })?;

        let open_case = run_case(
            "project.open_existing",
            open_iters,
            open_threshold_ms,
            || state.open_project_file(project_path.clone()),
        )?;

        let save_case = run_case(
            "project.save_existing",
            save_iters,
            save_threshold_ms,
            || state.save_project_file(),
        )?;

        Ok(vec![create_case, open_case, save_case])
    })();

    let _ = fs::remove_dir_all(&root);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> = report.iter().filter(|c| !c.passed).map(|c| c.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "performance smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }

    Ok(())
}

#[test]
#[ignore = "development app UI scale smoke test; run manually"]
fn app_ui_scale_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let asset_count = env_usize_clamped("MONDRIAN_UI_PERF_ASSETS", 360, 32, 5_000);
    let clip_count = env_usize_clamped("MONDRIAN_UI_PERF_CLIPS", 720, 24, 10_000);
    let effect_count = env_usize_clamped("MONDRIAN_UI_PERF_EFFECTS", 480, 0, 20_000);
    let resize_iterations = env_usize_clamped("MONDRIAN_UI_PERF_RESIZE_ITERS", 40, 4, 500);
    let playback_frames = env_usize_clamped("MONDRIAN_UI_PERF_PLAYBACK_FRAMES", 120, 8, 2_000);
    let refresh_iterations = env_usize_clamped("MONDRIAN_UI_PERF_REFRESH_ITERS", 8, 1, 100);

    let build_threshold_ms = env_u128("MONDRIAN_UI_PERF_BUILD_MS", 10_000);
    let refresh_threshold_ms = env_u128("MONDRIAN_UI_PERF_REFRESH_MS", 2_500);
    let resize_threshold_ms = env_u128("MONDRIAN_UI_PERF_RESIZE_MS", 1_500);
    let paint_threshold_ms = env_u128("MONDRIAN_UI_PERF_PAINT_MS", 1_500);
    let playback_threshold_ms = env_u128("MONDRIAN_UI_PERF_PLAYBACK_MS", 8_000);
    let preview_probe_threshold_ms = env_u128("MONDRIAN_UI_PERF_PREVIEW_PROBE_MS", 1_500);
    let preview_playback_threshold_ms = env_u128("MONDRIAN_UI_PERF_PREVIEW_PLAYBACK_MS", 4_000);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_ui_perf_smoke_{uniq}"));
    fs::create_dir_all(&root_dir)?;

    let result = (|| -> anyhow::Result<AppUiScaleReport> {
        let mut state = build_app_ui_perf_state(&root_dir, asset_count, clip_count, effect_count)?;
        let bounds = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let theme = ThemePreset::Dark.build();

        let build_case = run_case(
            "app_ui.root_build_large_project",
            1,
            build_threshold_ms,
            || {
                let mut root = AppUiAppRoot::from_app_state(&state);
                TreeWalker::layout(&mut root, bounds);
                Ok(())
            },
        )?;

        let mut root = AppUiAppRoot::from_app_state(&state);
        TreeWalker::layout(&mut root, bounds);

        let refresh_case = run_case(
            "app_ui.refresh_large_project",
            refresh_iterations,
            refresh_threshold_ms,
            || {
                root.refresh_from_app_state(&state);
                TreeWalker::layout(&mut root, bounds);
                Ok(())
            },
        )?;

        let resize_case = run_case("app_ui.resize_loop", 1, resize_threshold_ms, || {
            for index in 0..resize_iterations {
                let width = 1280.0 + (index % 9) as f32 * 83.0;
                let height = 720.0 + (index % 7) as f32 * 47.0;
                TreeWalker::layout(&mut root, Rect::new(0.0, 0.0, width, height));
            }
            TreeWalker::layout(&mut root, bounds);
            Ok(())
        })?;

        let mut initial_paint_commands = 0usize;
        let paint_case = run_case("app_ui.paint_large_project", 1, paint_threshold_ms, || {
            initial_paint_commands = paint_command_count(&root, &theme, bounds);
            anyhow::ensure!(
                initial_paint_commands > 0,
                "app UI root emitted no paint commands"
            );
            Ok(())
        })?;

        state.play();
        let mut playback_paint_commands_max = 0usize;
        let playback_case = run_case(
            "app_ui.sustained_playback_refresh",
            1,
            playback_threshold_ms,
            || {
                for frame in 0..playback_frames {
                    state.set_playback_frame_running(frame as i64);
                    root.refresh_playback_frame_from_app_state(&state, None);
                    TreeWalker::layout(&mut root, bounds);
                    if frame % 12 == 0 {
                        playback_paint_commands_max = playback_paint_commands_max
                            .max(paint_command_count(&root, &theme, bounds));
                    }
                }
                Ok(())
            },
        )?;
        state.pause();

        let preview_service = AppUiPreviewService::new();
        let mut preview_diagnostics = preview_service.diagnostics();
        let preview_probe_case = run_case(
            "app_ui.preview_diagnostics_probe",
            1,
            preview_probe_threshold_ms,
            || {
                let _ = preview_service.viewer_preview_for_state(&state);
                preview_diagnostics = preview_service.diagnostics();
                Ok(())
            },
        )?;

        let preview_playback_service = AppUiPreviewService::new();
        let mut preview_playback_diagnostics = preview_playback_service.diagnostics();
        let preview_playback_case = run_case(
            "app_ui.preview_playback_refresh",
            1,
            preview_playback_threshold_ms,
            || {
                for frame in 0..playback_frames {
                    state.set_playback_frame_running(frame as i64);
                    root.refresh_playback_frame_from_app_state(
                        &state,
                        Some(&preview_playback_service),
                    );
                    TreeWalker::layout(&mut root, bounds);
                }
                preview_playback_diagnostics = preview_playback_service.diagnostics();
                Ok(())
            },
        )?;
        state.pause();

        Ok(AppUiScaleReport {
            scenario: "app_ui_scale",
            assets: asset_count,
            clips: clip_count,
            effects: effect_count,
            resize_iterations,
            playback_frames,
            initial_paint_commands,
            playback_paint_commands_max,
            preview_diagnostics,
            preview_color_path: PreviewColorPathReport::from_diagnostics(preview_diagnostics),
            preview_playback_diagnostics,
            preview_playback_color_path: PreviewColorPathReport::from_diagnostics(
                preview_playback_diagnostics,
            ),
            cases: vec![
                build_case,
                refresh_case,
                resize_case,
                paint_case,
                playback_case,
                preview_probe_case,
                preview_playback_case,
            ],
        })
    })();

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "app UI scale smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }

    Ok(())
}

#[test]
#[ignore = "development preview media decode/cache smoke test; run manually"]
fn preview_media_decode_cache_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_MEDIA_FRAMES", 12, 2, 120);
    let cache_iterations = env_usize_clamped("MONDRIAN_PREVIEW_MEDIA_CACHE_ITERS", 30, 1, 500);
    let first_frame_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_FIRST_READY_MS", 10_000);
    let cache_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_CACHE_REFRESH_MS", 1_000);
    let sequential_threshold_ms = env_u128("MONDRIAN_PREVIEW_MEDIA_SEQUENCE_READY_MS", 8_000);
    let ready_timeout =
        Duration::from_millis(env_u128("MONDRIAN_PREVIEW_MEDIA_READY_TIMEOUT_MS", 10_000) as u64);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_media_perf_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let video_path = root_dir.join("preview-media-smoke.mp4");

    if !generate_preview_media_fixture(&video_path)? {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_decode_cache\",\"skipped\":\"ffmpeg CLI unavailable or fixture generation failed\"}}"
        );
        let _ = fs::remove_dir_all(&root_dir);
        return Ok(());
    }

    let result = (|| -> anyhow::Result<PreviewMediaPerfReport> {
        let mut state = build_preview_media_perf_state(&root_dir, &video_path, frame_count)?;
        let preview_service = AppUiPreviewService::new();

        let first_frame_case = run_case(
            "preview_media.first_frame_ready",
            1,
            first_frame_threshold_ms,
            || {
                state.seek(0);
                wait_for_preview_ready(&preview_service, &state, ready_timeout)
            },
        )?;

        let cached_frame_case = run_case(
            "preview_media.cached_frame_refresh",
            1,
            cache_threshold_ms,
            || {
                for _ in 0..cache_iterations {
                    assert_preview_ready(&preview_service, &state)?;
                }
                Ok(())
            },
        )?;

        let sequential_case = run_case(
            "preview_media.sequential_frame_ready_window",
            1,
            sequential_threshold_ms,
            || {
                for frame in 0..frame_count {
                    state.seek(frame as i64);
                    wait_for_preview_ready(&preview_service, &state, ready_timeout)?;
                }
                Ok(())
            },
        )?;

        let preview_diagnostics = preview_service.diagnostics();
        Ok(PreviewMediaPerfReport {
            scenario: "preview_media_decode_cache",
            frames: frame_count,
            cache_iterations,
            preview_diagnostics,
            preview_color_path: PreviewColorPathReport::from_diagnostics(preview_diagnostics),
            cases: vec![first_frame_case, cached_frame_case, sequential_case],
        })
    })();

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "preview media performance smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }

    Ok(())
}

#[test]
#[ignore = "development preview media continuous playback smoke test; run manually"]
fn preview_media_continuous_playback_smoke() -> anyhow::Result<()> {
    let _guard = perf_lock().lock().expect("perf lock poisoned");

    let frame_count = env_usize_clamped("MONDRIAN_PREVIEW_PLAYBACK_FRAMES", 60, 8, 60);
    let frame_interval_ms =
        env_usize_clamped("MONDRIAN_PREVIEW_PLAYBACK_FRAME_MS", 33, 1, 250) as u64;
    let playback_threshold_ms = env_u128("MONDRIAN_PREVIEW_PLAYBACK_WINDOW_MS", 8_000);
    let ready_timeout = Duration::from_millis(env_u128(
        "MONDRIAN_PREVIEW_PLAYBACK_READY_TIMEOUT_MS",
        10_000,
    ) as u64);

    let uniq = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root_dir = std::env::temp_dir().join(format!("mondrian_preview_playback_perf_{uniq}"));
    fs::create_dir_all(&root_dir)?;
    let video_path = root_dir.join("preview-playback-smoke.mp4");

    if !generate_preview_media_fixture(&video_path)? {
        eprintln!(
            "MONDRIAN_PERF_JSON={{\"scenario\":\"preview_media_continuous_playback\",\"skipped\":\"ffmpeg CLI unavailable or fixture generation failed\"}}"
        );
        let _ = fs::remove_dir_all(&root_dir);
        return Ok(());
    }

    let result = (|| -> anyhow::Result<PreviewMediaPlaybackPerfReport> {
        let mut state = build_preview_media_perf_state(&root_dir, &video_path, frame_count)?;
        let preview_service = AppUiPreviewService::new();
        let mut readiness = PreviewReadinessCounts::default();

        state.seek(0);
        wait_for_preview_ready(&preview_service, &state, ready_timeout)?;
        state.play();
        let playback_case = run_case(
            "preview_media.continuous_playback_readiness",
            1,
            playback_threshold_ms,
            || {
                for frame in 0..frame_count {
                    state.set_playback_frame_running(frame as i64);
                    let _ = preview_service.poll_finished();
                    record_preview_readiness(
                        &mut readiness,
                        preview_service.viewer_preview_for_state(&state),
                    );
                    let _ = preview_service.poll_finished();
                    thread::sleep(Duration::from_millis(frame_interval_ms));
                }
                let _ = preview_service.poll_finished();
                Ok(())
            },
        )?;
        state.pause();

        anyhow::ensure!(
            readiness.unavailable == 0,
            "continuous playback returned unavailable frames: {:?}; diagnostics: {:?}",
            readiness,
            preview_service.diagnostics()
        );
        anyhow::ensure!(
            readiness.ready + readiness.stale >= frame_count.saturating_sub(2),
            "continuous playback did not keep enough frames visible: {:?}; diagnostics: {:?}",
            readiness,
            preview_service.diagnostics()
        );

        let preview_diagnostics = preview_service.diagnostics();
        Ok(PreviewMediaPlaybackPerfReport {
            scenario: "preview_media_continuous_playback",
            frames: frame_count,
            frame_interval_ms,
            readiness,
            preview_diagnostics,
            preview_color_path: PreviewColorPathReport::from_diagnostics(preview_diagnostics),
            cases: vec![playback_case],
        })
    })();

    let _ = fs::remove_dir_all(&root_dir);

    let report = result?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_PERF_JSON={report_json}");
    write_report_if_needed(&report_json);

    let failed_cases: Vec<_> =
        report.cases.iter().filter(|case| !case.passed).map(|case| case.case).collect();
    if !failed_cases.is_empty() {
        anyhow::bail!(
            "preview media continuous playback smoke test failed: {:?}; report: {}",
            failed_cases,
            report_json
        );
    }

    Ok(())
}

fn build_app_ui_perf_state(
    root_dir: &Path,
    asset_count: usize,
    clip_count: usize,
    effect_count: usize,
) -> anyhow::Result<AppState> {
    let library = AssetLibrary::open(root_dir.join("library"))?;
    let mut asset_ids = Vec::with_capacity(asset_count);
    for index in 0..asset_count {
        let id = if index % 5 == 0 {
            library.create_adjustment_layer_asset(Some(&format!("Adjustment {index:04}")))?
        } else {
            library.create_solid_color_asset(Some(&format!("Solid {index:04}")))?
        };
        asset_ids.push(id);
    }

    let mut sequence = Sequence::new("App UI perf");
    sequence.video_tracks.clear();
    sequence.audio_tracks.clear();

    let video_track_count = 8usize;
    let audio_track_count = 6usize;
    for index in 0..video_track_count {
        sequence.video_tracks.push(Track::new_video(format!("V{}", index + 1)));
    }
    for index in 0..audio_track_count {
        sequence.audio_tracks.push(Track::new_audio(format!("A{}", index + 1)));
    }

    let tb = sequence.time_base();
    let effect_types = [
        EffectType::GaussianBlur,
        EffectType::Sharpen,
        EffectType::BasicCorrection,
        EffectType::Vignette,
        EffectType::ChromaKey,
        EffectType::LumaKey,
    ];
    let mut remaining_effects = effect_count;
    let mut first_clip = None;
    let mut first_effect = None;

    for index in 0..clip_count {
        let track_index = index % video_track_count;
        let lane_index = index / video_track_count;
        let position = TimeCode::new((lane_index as i64) * 96, tb);
        let duration = TimeCode::new(72 + (index % 5) as i64 * 6, tb);
        let asset_id = asset_ids[index % asset_ids.len()];
        let mut clip = if index % 5 == 0 {
            Clip::new_adjustment_layer(asset_id, position, duration)
        } else {
            let color = Color::from_hex(0x244C7A + ((index as u32 * 997) & 0x003F3F));
            Clip::new_solid_color(asset_id, color, position, duration)
        };
        clip.label = Some(format!("Clip {index:04}"));

        let local_effects = remaining_effects.min(if index % 3 == 0 { 3 } else { 1 });
        for effect_offset in 0..local_effects {
            let effect_type = effect_types[(index + effect_offset) % effect_types.len()].clone();
            let effect_id = clip.add_effect_node(EffectNode::with_defaults(effect_type));
            first_effect.get_or_insert(effect_id);
            remaining_effects -= 1;
        }

        first_clip.get_or_insert((sequence.video_tracks[track_index].id, clip.id));
        sequence.video_tracks[track_index].add_clip(clip)?;
    }

    for index in 0..(clip_count / 3).max(audio_track_count) {
        let track_index = index % audio_track_count;
        let lane_index = index / audio_track_count;
        let position = TimeCode::new((lane_index as i64) * 120, tb);
        let duration = TimeCode::new(96, tb);
        let mut clip = Clip::new(asset_ids[index % asset_ids.len()], position, duration);
        clip.label = Some(format!("Audio {index:04}"));
        sequence.audio_tracks[track_index].add_clip(clip)?;
    }

    sequence.playhead = TimeCode::new(0, tb);
    sequence.mark_out(sequence.total_duration().frame.max(1));
    let sequence_id = sequence.id;

    let mut state = AppState::new();
    state.asset_library = Some(library);
    state.active_sequence_id = Some(sequence_id);
    state.default_sequence_id = Some(sequence_id);
    state.sequences = vec![sequence.clone()];
    state.sequence = Some(sequence);

    if let Some((track_id, clip_id)) = first_clip {
        state.replace_clip_selection(vec![SelectedClipRef {
            track_id,
            is_video_track: true,
            clip_id,
        }]);
        if let Some(effect_id) = first_effect {
            let _ = state.select_effect_by_id(clip_id, effect_id);
        }
    }

    Ok(state)
}

fn generate_preview_media_fixture(path: &Path) -> anyhow::Result<bool> {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        return Ok(false);
    }

    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc2=size=320x180:rate=30:duration=2")
        .arg("-an")
        .arg("-c:v")
        .arg("mpeg4")
        .arg("-q:v")
        .arg("5")
        .arg(path)
        .status()?;

    Ok(status.success() && path.exists())
}

fn build_preview_media_perf_state(
    root_dir: &Path,
    video_path: &Path,
    frame_count: usize,
) -> anyhow::Result<AppState> {
    let library = AssetLibrary::open(root_dir.join("library"))?;
    let asset_id = library.import_media_file(video_path)?;

    let mut sequence = Sequence::new("Preview media perf");
    sequence.settings.frame_rate = Rational::FPS_30;
    let tb = sequence.time_base();
    let duration = TimeCode::new(frame_count as i64, tb);
    sequence.video_tracks[0].add_clip(Clip::new(asset_id, TimeCode::new(0, tb), duration))?;
    sequence.playhead = TimeCode::new(0, tb);
    sequence.mark_out(frame_count as i64);
    let sequence_id = sequence.id;

    let mut state = AppState::new();
    state.asset_library = Some(library);
    state.active_sequence_id = Some(sequence_id);
    state.default_sequence_id = Some(sequence_id);
    state.sequences = vec![sequence.clone()];
    state.sequence = Some(sequence);
    Ok(state)
}

fn wait_for_preview_ready(
    preview_service: &AppUiPreviewService,
    state: &AppState,
    timeout: Duration,
) -> anyhow::Result<()> {
    let started_at = Instant::now();
    loop {
        match preview_service.viewer_preview_for_state(state) {
            ViewerPreviewState::Ready(_) => return Ok(()),
            ViewerPreviewState::Loading | ViewerPreviewState::Stale(_) => {
                let _ = preview_service.poll_finished();
            }
            ViewerPreviewState::Unavailable => {
                let _ = preview_service.poll_finished();
            }
        }
        if started_at.elapsed() > timeout {
            anyhow::bail!(
                "preview frame did not become ready within {} ms; diagnostics: {:?}",
                timeout.as_millis(),
                preview_service.diagnostics()
            );
        }
        thread::sleep(Duration::from_millis(8));
    }
}

fn assert_preview_ready(
    preview_service: &AppUiPreviewService,
    state: &AppState,
) -> anyhow::Result<()> {
    match preview_service.viewer_preview_for_state(state) {
        ViewerPreviewState::Ready(_) => Ok(()),
        other => anyhow::bail!(
            "expected ready preview frame, got {:?}; diagnostics: {:?}",
            other,
            preview_service.diagnostics()
        ),
    }
}

fn record_preview_readiness(counts: &mut PreviewReadinessCounts, state: ViewerPreviewState) {
    match state {
        ViewerPreviewState::Ready(_) => counts.ready += 1,
        ViewerPreviewState::Loading => counts.loading += 1,
        ViewerPreviewState::Stale(_) => counts.stale += 1,
        ViewerPreviewState::Unavailable => counts.unavailable += 1,
    }
}

#[test]
fn preview_color_path_report_summarizes_legacy_and_gpu_blockers() {
    let diagnostics = AppUiPreviewDiagnostics {
        color_composite_plans: 3,
        color_composite_elements: 7,
        color_composite_float_linear: 2,
        color_composite_legacy_rgba8: 1,
        color_composite_legacy_media_transform: 1,
        color_composite_legacy_solid_effect: 2,
        color_stage_gpu_color_stages: 4,
        color_stage_gpu_blockers: 1,
        color_rgba8_boundary_calls: 3,
        ..AppUiPreviewDiagnostics::default()
    };

    let report = PreviewColorPathReport::from_diagnostics(diagnostics);

    assert_eq!(report.composite_plans, 3);
    assert_eq!(report.composite_elements, 7);
    assert_eq!(report.float_linear_composites, 2);
    assert_eq!(report.legacy_rgba8_composites, 1);
    assert_eq!(report.legacy_reasons, 3);
    assert_eq!(report.gpu_color_stages, 4);
    assert_eq!(report.gpu_blockers, 1);
    assert_eq!(report.rgba8_boundary_calls, 3);
    assert!(!report.fully_float_linear);
    assert!(!report.gpu_path_ready);
}

#[test]
fn preview_color_path_report_marks_clean_float_linear_path() {
    let diagnostics = AppUiPreviewDiagnostics {
        color_composite_plans: 2,
        color_composite_elements: 2,
        color_composite_float_linear: 2,
        ..AppUiPreviewDiagnostics::default()
    };

    let report = PreviewColorPathReport::from_diagnostics(diagnostics);

    assert_eq!(report.legacy_reasons, 0);
    assert!(report.fully_float_linear);
    assert!(report.gpu_path_ready);
}

fn paint_command_count(
    root: &AppUiAppRoot,
    theme: &mondrian_ui_theme::Theme,
    bounds: Rect,
) -> usize {
    let mut encoder = DrawEncoder::new();
    TreeWalker::paint_clipped(root, &mut encoder, theme, bounds);
    encoder.finish().len()
}
