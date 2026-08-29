use super::*;
use mondrian_core::types::{BlendMode, ColorEngine, ColorSpace};
use mondrian_core::WorkingColorSpace;
use mondrian_effects::{
    blend_rgba_f32_pixel_seeded, compile_reference_effect_graph, EffectRenderPlan,
};
use mondrian_renderer::{
    composite_timeline_elements_color_frame_with_diagnostics, execute_cpu_input_stage,
    CpuColorFrame, CpuEncodedColorFrame, RenderColorStageDiagnostics, RenderInputTransform,
    TimelineCompositeDiagnostics, TimelineCompositeElement, TimelineCompositeExecutionDiagnostics,
    TimelineCompositeOptions, TimelineCompositeScratch, TimelineEffectColorRuntime,
    TimelineMediaLayer,
};
use serde::Serialize;
use std::cmp;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
enum OpacityPattern {
    Blend,
    Passthrough,
}

#[derive(Debug, Serialize)]
struct ExportPerfSimReport {
    scenario: &'static str,
    resolution: (u32, u32),
    target_fps: f64,
    simulated_frames: usize,
    layers: usize,
    opacity_pattern: &'static str,
    first_frame_ms: u128,
    first_frame_threshold_ms: u128,
    frame_ms_avg: f64,
    frame_ms_p50: u128,
    frame_ms_p95: u128,
    frame_ms_max: u128,
    frame_budget_ms: f64,
    missed_budget_frames: usize,
    achieved_fps: f64,
    fps_min_threshold: f64,
    fps_max_threshold: f64,
    passthrough_frames: usize,
    passthrough_ratio_pct: f64,
    passthrough_execution_proven: bool,
    fused_first_two_frames: usize,
    fused_first_two_ratio_pct: f64,
    fused_first_two_execution_proven: bool,
    pixel_oracle_proven: bool,
    color_stage_plans: u64,
    color_stage_total_stages: u64,
    color_stage_cpu_input_stages: u64,
    color_stage_cpu_output_stages: u64,
    color_stage_gpu_color_stages: u64,
    color_stage_upload_stages: u64,
    color_stage_readback_stages: u64,
    color_stage_gpu_blockers: u64,
    color_stage_pixels: u64,
    color_report: ExportColorHealthReport,
    passed: bool,
}

fn perf_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_u128(key: &str, default: u128) -> u128 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u128>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

fn report_output_path() -> Option<PathBuf> {
    std::env::var_os("MONDRIAN_EXPORT_SIM_OUTPUT").map(PathBuf::from)
}

fn write_report_if_needed(report_json: &str) {
    if let Some(path) = report_output_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{report_json}");
        }
    }
}

fn percentile_ms(values: &[u128], percentile: f64) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let p = percentile.clamp(0.0, 1.0);
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

fn generate_layer(
    width: u32,
    height: u32,
    seed: u8,
) -> (Arc<DecodedVideoLayer>, RenderColorStageDiagnostics) {
    let pixel_count = (width as usize) * (height as usize);
    let mut data = vec![0u8; pixel_count * 4];
    for i in 0..pixel_count {
        let base = i * 4;
        let x = (i % width as usize) as u8;
        let y = (i / width as usize) as u8;
        data[base] = x.wrapping_mul(5).wrapping_add(seed);
        data[base + 1] = y.wrapping_mul(7).wrapping_add(seed.wrapping_mul(2));
        data[base + 2] = x ^ y ^ seed.wrapping_mul(3);
        data[base + 3] = 240;
    }
    let source = CpuEncodedColorFrame::source_rgba8(width, height, ColorSpace::Rec709, data);
    let frame = execute_cpu_input_stage(
        &source,
        &RenderInputTransform::to_working(
            WorkingColorSpace::LinearRec709,
            false,
            ColorEngine::mondrian_standard(),
        ),
    )
    .expect("perf input transform");
    let diagnostics = frame.stage_diagnostics;
    (
        Arc::new(DecodedVideoLayer {
            frame: frame.result.frame,
            is_data_texture: false,
            source_resolution: Resolution { width, height },
            picture_geometry: ResolvedPictureGeometry::square(Resolution { width, height })
                .expect("non-empty perf picture geometry"),
            source_fingerprint: MediaFileFingerprint::default(),
            video_stream_index: 0,
            decode_diagnostics: None,
            stage_diagnostics: diagnostics,
        }),
        diagnostics,
    )
}

fn build_frame_layers(
    layers: &[Arc<DecodedVideoLayer>],
    frame_idx: u64,
    pattern: OpacityPattern,
) -> Vec<(Arc<DecodedVideoLayer>, f32)> {
    let mut out = Vec::with_capacity(layers.len());
    match pattern {
        OpacityPattern::Blend => {
            for (layer_idx, layer) in layers.iter().enumerate() {
                let opacity = (0.45 + ((frame_idx + layer_idx as u64) % 7) as f32 * 0.07).min(1.0);
                out.push((Arc::clone(layer), opacity));
            }
        }
        OpacityPattern::Passthrough => {
            if let Some(layer) = layers.first() {
                out.push((Arc::clone(layer), 1.0));
            }
        }
    }
    out
}

fn compose_frame_layers_with_diagnostics(
    width: u32,
    height: u32,
    layers: &[(Arc<DecodedVideoLayer>, f32)],
    frame_idx: u64,
    identity_effect_graph: &Arc<mondrian_effects::CompiledEffectGraph>,
    scratch: &mut TimelineCompositeScratch,
) -> (
    TimelineCompositeDiagnostics,
    TimelineCompositeExecutionDiagnostics,
    [[f32; 4]; 3],
) {
    let elements = layers
        .iter()
        .map(|(layer, opacity)| {
            TimelineCompositeElement::Media(TimelineMediaLayer {
                frame: &layer.frame,
                opacity: *opacity,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: Arc::clone(identity_effect_graph),
                frame_seed: frame_idx as i64,
            })
        })
        .collect::<Vec<_>>();
    let output = composite_timeline_elements_color_frame_with_diagnostics(
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        TimelineEffectColorRuntime::new(
            &ColorEngine::mondrian_standard(),
            WorkingColorSpace::LinearRec709,
        ),
        scratch,
    )
    .expect("composite export simulation frame");
    // This gate measures real pixel execution, not just diagnostic control
    // flow. Keep the complete typed output observable across release LTO, then
    // retain three deterministic pixels for the canonical oracle outside the
    // compositor.
    std::hint::black_box(&output.frame);
    let pixel_probe = sampled_working_pixels(&output.frame);
    (output.diagnostics, output.execution, pixel_probe)
}

fn sampled_working_pixels(frame: &CpuColorFrame) -> [[f32; 4]; 3] {
    let pixels = &frame.rgba_f32().data;
    assert!(
        !pixels.is_empty(),
        "export performance output must contain pixels"
    );
    [
        pixels[0],
        pixels[pixels.len() / 2],
        pixels[pixels.len() - 1],
    ]
}

fn canonical_frame_layer_probe(
    layers: &[(Arc<DecodedVideoLayer>, f32)],
    pattern: OpacityPattern,
) -> [[f32; 4]; 3] {
    let first = layers.first().expect("export performance frame has a layer");
    let pixels = &first.0.frame.rgba_f32().data;
    assert!(
        !pixels.is_empty(),
        "export performance source must contain pixels"
    );
    let indices = [0, pixels.len() / 2, pixels.len() - 1];
    indices.map(|index| match pattern {
        OpacityPattern::Passthrough => first.0.frame.rgba_f32().data[index],
        OpacityPattern::Blend => {
            layers.iter().fold([0.0, 0.0, 0.0, 0.0], |base, (layer, opacity)| {
                let source = layer.frame.rgba_f32().data[index];
                blend_rgba_f32_pixel_seeded(base, source, *opacity, BlendMode::Normal, index as u32)
            })
        }
    })
}

fn pixel_probe_matches(actual: [[f32; 4]; 3], expected: [[f32; 4]; 3]) -> bool {
    actual.iter().zip(expected).all(|(actual, expected)| {
        actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| (*actual - expected).abs() <= 1.0e-6)
    })
}

fn run_export_render_simulation(
    scenario: &'static str,
    width: u32,
    height: u32,
    target_fps: f64,
    sim_frames: usize,
    layer_count: usize,
    first_frame_threshold_ms: u128,
    fps_min_threshold: f64,
    fps_max_threshold: f64,
    pattern: OpacityPattern,
) -> Result<ExportPerfSimReport, Box<dyn std::error::Error>> {
    run_export_render_simulation_with_report(
        scenario,
        width,
        height,
        target_fps,
        sim_frames,
        layer_count,
        first_frame_threshold_ms,
        fps_min_threshold,
        fps_max_threshold,
        pattern,
    )
}

fn run_export_render_simulation_with_report(
    scenario: &'static str,
    width: u32,
    height: u32,
    target_fps: f64,
    sim_frames: usize,
    layer_count: usize,
    first_frame_threshold_ms: u128,
    fps_min_threshold: f64,
    fps_max_threshold: f64,
    pattern: OpacityPattern,
) -> Result<ExportPerfSimReport, Box<dyn std::error::Error>> {
    let frame_budget_ms = 1000.0 / target_fps;
    let layer_count = layer_count.max(1);
    let mut layers = Vec::with_capacity(layer_count);
    let mut color_stage_diagnostics = RenderColorStageDiagnostics::default();
    for i in 0..layer_count {
        let (layer, diagnostics) = generate_layer(width, height, (17 + i as u8).wrapping_mul(9));
        color_stage_diagnostics.accumulate(diagnostics);
        layers.push(layer);
    }

    let mut passthrough_frames = 0usize;
    let mut fused_first_two_frames = 0usize;
    let mut pixel_oracle_proven = true;
    let mut color_diagnostics = ExportJobColorDiagnostics::default();
    // Production Export compiles Effect programs once per immutable snapshot
    // and retains one bounded working set for the job. The performance gate
    // must measure frame execution, not per-frame graph compilation or fresh
    // scratch allocation that production never performs.
    let identity_effect_graph = compile_reference_effect_graph(&EffectRenderPlan::default())
        .expect("compile identity graph");
    let mut composite_scratch = TimelineCompositeScratch::default();
    let pattern_name = match pattern {
        OpacityPattern::Blend => "blend",
        OpacityPattern::Passthrough => "passthrough",
    };

    let first_frame_layers = build_frame_layers(&layers, 0, pattern);
    let first_started = Instant::now();
    let (first_composite_diagnostics, first_execution_diagnostics, first_pixel_probe) =
        compose_frame_layers_with_diagnostics(
            width,
            height,
            &first_frame_layers,
            0,
            &identity_effect_graph,
            &mut composite_scratch,
        );
    let first_frame_ms = first_started.elapsed().as_millis();
    pixel_oracle_proven &= pixel_probe_matches(
        first_pixel_probe,
        canonical_frame_layer_probe(&first_frame_layers, pattern),
    );
    color_diagnostics.record_frame_diagnostics(
        InputColorResolutionSourceCounts::default(),
        color_stage_diagnostics,
        first_composite_diagnostics,
    );
    passthrough_frames = passthrough_frames.saturating_add(
        usize::try_from(first_execution_diagnostics.zero_copy_identity_passthroughs)
            .unwrap_or(usize::MAX),
    );
    fused_first_two_frames = fused_first_two_frames.saturating_add(
        usize::try_from(first_execution_diagnostics.fused_first_two_full_frame_normal_blends)
            .unwrap_or(usize::MAX),
    );

    let mut frame_samples_ms = Vec::with_capacity(sim_frames);
    let mut effective_timeline_ms = 0.0f64;
    let mut missed_budget_frames = 0usize;

    for frame in 0..sim_frames as u64 {
        let frame_layers = build_frame_layers(&layers, frame + 1, pattern);
        let started = Instant::now();
        let (composite_diagnostics, execution_diagnostics, pixel_probe) =
            compose_frame_layers_with_diagnostics(
                width,
                height,
                &frame_layers,
                frame + 1,
                &identity_effect_graph,
                &mut composite_scratch,
            );
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        pixel_oracle_proven &= pixel_probe_matches(
            pixel_probe,
            canonical_frame_layer_probe(&frame_layers, pattern),
        );
        color_diagnostics.record_frame_diagnostics(
            InputColorResolutionSourceCounts::default(),
            RenderColorStageDiagnostics::default(),
            composite_diagnostics,
        );

        passthrough_frames = passthrough_frames.saturating_add(
            usize::try_from(execution_diagnostics.zero_copy_identity_passthroughs)
                .unwrap_or(usize::MAX),
        );
        fused_first_two_frames = fused_first_two_frames.saturating_add(
            usize::try_from(execution_diagnostics.fused_first_two_full_frame_normal_blends)
                .unwrap_or(usize::MAX),
        );
        frame_samples_ms.push(elapsed_ms.round().max(0.0) as u128);
        if elapsed_ms > frame_budget_ms {
            missed_budget_frames += 1;
        }
        effective_timeline_ms += elapsed_ms.max(frame_budget_ms);
    }

    let total_ms = frame_samples_ms.iter().copied().sum::<u128>();
    let frame_ms_avg = total_ms as f64 / cmp::max(frame_samples_ms.len(), 1) as f64;
    let frame_ms_p50 = percentile_ms(&frame_samples_ms, 0.50);
    let frame_ms_p95 = percentile_ms(&frame_samples_ms, 0.95);
    let frame_ms_max = frame_samples_ms.iter().copied().max().unwrap_or(0);
    let achieved_fps = if effective_timeline_ms > 0.0 {
        sim_frames as f64 * 1000.0 / effective_timeline_ms
    } else {
        0.0
    };
    let color_report = color_diagnostics
        .health_report(scenario)
        .expect("export perf color diagnostics");

    let simulated_total = sim_frames.saturating_add(1);
    let passthrough_ratio_pct = if simulated_total > 0 {
        passthrough_frames as f64 * 100.0 / simulated_total as f64
    } else {
        0.0
    };
    let passthrough_execution_proven =
        !matches!(pattern, OpacityPattern::Passthrough) || passthrough_frames == simulated_total;
    let fused_first_two_ratio_pct = if simulated_total > 0 {
        fused_first_two_frames as f64 * 100.0 / simulated_total as f64
    } else {
        0.0
    };
    let fused_first_two_execution_proven = !matches!(pattern, OpacityPattern::Blend)
        || layer_count < 2
        || fused_first_two_frames == simulated_total;

    const FPS_EPSILON: f64 = 0.001;
    let passed = first_frame_ms <= first_frame_threshold_ms
        && achieved_fps + FPS_EPSILON >= fps_min_threshold
        && achieved_fps <= fps_max_threshold + FPS_EPSILON
        && passthrough_execution_proven
        && fused_first_two_execution_proven
        && pixel_oracle_proven
        && color_report.verdict != ExportColorHealthVerdict::Fail;

    Ok(ExportPerfSimReport {
        scenario,
        resolution: (width, height),
        target_fps,
        simulated_frames: sim_frames,
        layers: layer_count,
        opacity_pattern: pattern_name,
        first_frame_ms,
        first_frame_threshold_ms,
        frame_ms_avg,
        frame_ms_p50,
        frame_ms_p95,
        frame_ms_max,
        frame_budget_ms,
        missed_budget_frames,
        achieved_fps,
        fps_min_threshold,
        fps_max_threshold,
        passthrough_frames,
        passthrough_ratio_pct,
        passthrough_execution_proven,
        fused_first_two_frames,
        fused_first_two_ratio_pct,
        fused_first_two_execution_proven,
        pixel_oracle_proven,
        color_stage_plans: layer_count as u64,
        color_stage_total_stages: color_stage_diagnostics.total_stages,
        color_stage_cpu_input_stages: color_stage_diagnostics.cpu_input_stages,
        color_stage_cpu_output_stages: color_stage_diagnostics.cpu_output_stages,
        color_stage_gpu_color_stages: color_stage_diagnostics.gpu_color_stages,
        color_stage_upload_stages: color_stage_diagnostics.upload_stages,
        color_stage_readback_stages: color_stage_diagnostics.readback_stages,
        color_stage_gpu_blockers: color_stage_diagnostics.gpu_blockers,
        color_stage_pixels: color_stage_diagnostics.stage_pixels,
        color_report,
        passed,
    })
}

fn run_and_report_scenario(
    scenario: &'static str,
    width: u32,
    height: u32,
    target_fps: f64,
    sim_frames: usize,
    layer_count: usize,
    first_frame_threshold_ms: u128,
    fps_min_threshold: f64,
    fps_max_threshold: f64,
    pattern: OpacityPattern,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = run_export_render_simulation(
        scenario,
        width,
        height,
        target_fps,
        sim_frames,
        layer_count,
        first_frame_threshold_ms,
        fps_min_threshold,
        fps_max_threshold,
        pattern,
    )?;
    let report_json = serde_json::to_string(&report)?;
    eprintln!("MONDRIAN_EXPORT_SIM_JSON={report_json}");
    write_report_if_needed(&report_json);

    if !report.passed {
        return Err(
            format!("export render simulation perf test failed; report: {report_json}").into(),
        );
    }
    Ok(())
}

#[test]
fn export_perf_sim_report_includes_color_report() {
    let report = run_export_render_simulation_with_report(
        "export-color-health-test",
        8,
        4,
        30.0,
        2,
        1,
        1_000,
        1.0,
        60.0,
        OpacityPattern::Passthrough,
    )
    .expect("export perf report");

    assert_eq!(
        report.color_report.schema_version,
        super::EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION
    );
    assert_eq!(report.color_report.profile, "export-color-health-test");
    assert_eq!(report.color_report.verdict, ExportColorHealthVerdict::Pass);
    assert_eq!(report.color_report.summary.diagnosed_frames, 3);
    assert_eq!(
        report.color_report.summary.cpu_input_stages,
        report.color_stage_cpu_input_stages
    );
    assert_eq!(
        report.color_report.summary.gpu_blockers,
        report.color_stage_gpu_blockers
    );
    assert_eq!(report.color_report.summary.legacy_reason_total, 0);
    assert!(report.color_report.summary.fully_float_linear);
    assert!(report.color_report.summary.gpu_path_ready);
    assert!(report.color_report.root_causes.is_empty());
    assert!(report.color_report.actions.is_empty());
    assert_eq!(report.fused_first_two_frames, 0);
    assert!(report.fused_first_two_execution_proven);
    assert!(report.pixel_oracle_proven);
    assert!(report.passed);

    let report_json = serde_json::to_value(&report).expect("serialize report");
    assert!(report_json.get("color_health").is_none());
    assert!(report_json.get("color_health_passed").is_none());
    assert!(report_json.get("color_health_budget").is_none());
    assert!(report_json.get("color_health_failures").is_none());
    assert_eq!(
        report_json["color_report"]["schema_version"],
        serde_json::Value::from(super::EXPORT_COLOR_HEALTH_REPORT_SCHEMA_VERSION)
    );
    assert_eq!(report_json["color_report"]["verdict"], "Pass");
    assert_eq!(
        report_json["color_report"]["summary"]["gpu_blocker_breakdown"]
            ["render_pipeline_not_prepared"],
        0
    );
    assert_eq!(
        report_json["color_report"]["summary"]["legacy_breakdown"]["media_transform"],
        0
    );
}

#[test]
fn export_perf_sim_report_proves_fusion_and_canonical_pixels_independently() {
    let report = run_export_render_simulation_with_report(
        "export-fusion-evidence-test",
        8,
        4,
        30.0,
        2,
        2,
        1_000,
        1.0,
        60.0,
        OpacityPattern::Blend,
    )
    .expect("export fusion evidence report");

    assert_eq!(report.fused_first_two_frames, 3);
    assert!(report.fused_first_two_execution_proven);
    assert!(report.pixel_oracle_proven);
    assert!(report.passed);
}

#[test]
fn export_color_health_report_reports_root_causes() {
    let report = ExportJobColorDiagnosticsSummary {
        diagnosed_frames: 1,
        policy_rejections: 1,
        gpu_blockers: 2,
        gpu_output_cpu_fallbacks: 1,
        gpu_output_attempts: 1,
        transfer_stages: 1,
        legacy_reason_total: 3,
        fully_float_linear: false,
        gpu_path_ready: false,
        ..ExportJobColorDiagnosticsSummary::default()
    }
    .health_report("export-ci");

    assert_eq!(report.verdict, ExportColorHealthVerdict::Fail);
    assert!(report.checks.iter().any(|check| {
        check.code == "gpu_blockers" && check.severity == ExportColorHealthSeverity::Fail
    }));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "input_color_policy_rejected_source"));
    assert!(report
        .root_causes
        .iter()
        .any(|root| root.code == "export_gpu_color_stage_blocked"));
    assert!(report.root_causes.iter().any(|root| root.code == "export_gpu_output_fallback"));
    assert!(report.root_causes.iter().any(|root| root.code == "legacy_rgba8_composite_path"));
}

#[test]
#[ignore = "development export render perf simulation test; run manually"]
fn export_1080p2997_simulated_perf() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = perf_lock().lock().expect("export perf lock poisoned");

    let width = 1920u32;
    let height = 1080u32;
    let target_fps = env_f64("MONDRIAN_EXPORT_SIM_TARGET_FPS", 29.97).clamp(1.0, 240.0);
    let sim_frames = env_usize("MONDRIAN_EXPORT_SIM_FRAMES", 96).clamp(24, 900);
    let layer_count = env_usize("MONDRIAN_EXPORT_SIM_LAYERS", 2).clamp(1, 8);

    let first_frame_threshold_ms = env_u128("MONDRIAN_EXPORT_SIM_TTFF_MS", 2_000);
    let fps_min_threshold = env_f64("MONDRIAN_EXPORT_SIM_FPS_MIN", 18.0);
    let fps_max_threshold = env_f64("MONDRIAN_EXPORT_SIM_FPS_MAX", 30.0).max(fps_min_threshold);

    run_and_report_scenario(
        "export-1080p2997-simulated",
        width,
        height,
        target_fps,
        sim_frames,
        layer_count,
        first_frame_threshold_ms,
        fps_min_threshold,
        fps_max_threshold,
        OpacityPattern::Blend,
    )
}

#[test]
#[ignore = "development export render perf simulation test; run manually"]
fn export_4k60_simulated_perf() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = perf_lock().lock().expect("export perf lock poisoned");

    // This is a named qualification gate, so workload identity is fixed.
    // Frame count may be raised for a longer observation, while thresholds may
    // only be tightened. A lighter resolution, single layer, lower target
    // rate, or looser budget must use a differently named diagnostic scenario.
    let width = 3840u32;
    let height = 2160u32;
    let target_fps = 60.0;
    let sim_frames = env_usize("MONDRIAN_EXPORT_SIM_4K_FRAMES", 120).clamp(16, 1200);
    let layer_count = 2;

    let first_frame_threshold_ms = env_u128("MONDRIAN_EXPORT_SIM_4K_TTFF_MS", 3_000).min(3_000);
    let fps_min_threshold = env_f64("MONDRIAN_EXPORT_SIM_4K_FPS_MIN", 12.0).max(12.0);
    let fps_max_threshold = env_f64("MONDRIAN_EXPORT_SIM_4K_FPS_MAX", target_fps)
        .min(target_fps)
        .max(fps_min_threshold);

    run_and_report_scenario(
        "export-4k60-simulated",
        width,
        height,
        target_fps,
        sim_frames,
        layer_count,
        first_frame_threshold_ms,
        fps_min_threshold,
        fps_max_threshold,
        OpacityPattern::Blend,
    )
}

#[test]
#[ignore = "development export render perf simulation test; run manually"]
fn export_4k60_single_layer_passthrough_simulated_perf() -> Result<(), Box<dyn std::error::Error>> {
    let _guard = perf_lock().lock().expect("export perf lock poisoned");

    let width = env_u128("MONDRIAN_EXPORT_SIM_PASS_WIDTH", 3840).clamp(640, 7680) as u32;
    let height = env_u128("MONDRIAN_EXPORT_SIM_PASS_HEIGHT", 2160).clamp(360, 4320) as u32;
    let target_fps = env_f64("MONDRIAN_EXPORT_SIM_PASS_TARGET_FPS", 60.0).clamp(1.0, 240.0);
    let sim_frames = env_usize("MONDRIAN_EXPORT_SIM_PASS_FRAMES", 120).clamp(16, 1200);

    let first_frame_threshold_ms = env_u128("MONDRIAN_EXPORT_SIM_PASS_TTFF_MS", 2_500);
    let fps_min_threshold = env_f64("MONDRIAN_EXPORT_SIM_PASS_FPS_MIN", 20.0);
    let fps_max_threshold =
        env_f64("MONDRIAN_EXPORT_SIM_PASS_FPS_MAX", target_fps).max(fps_min_threshold);

    run_and_report_scenario(
        "export-4k60-single-layer-passthrough-simulated",
        width,
        height,
        target_fps,
        sim_frames,
        1,
        first_frame_threshold_ms,
        fps_min_threshold,
        fps_max_threshold,
        OpacityPattern::Passthrough,
    )
}
