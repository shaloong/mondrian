use super::*;
use mondrian_effects::AdjustmentLayerParams;
use mondrian_renderer::{
    composite_timeline_elements_into, TimelineCompositeElement, TimelineCompositeOptions,
    TimelineCompositeScratch, TimelineMediaLayer,
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

fn generate_layer(width: u32, height: u32, seed: u8) -> Arc<DecodedVideoLayer> {
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
    Arc::new(DecodedVideoLayer { width, height, data })
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

fn compose_frame_layers_into_canvas(
    canvas: &mut Vec<u8>,
    width: u32,
    height: u32,
    layers: &[(Arc<DecodedVideoLayer>, f32)],
    frame_idx: u64,
) {
    let mut scratch = TimelineCompositeScratch::default();
    let elements = layers
        .iter()
        .map(|(layer, opacity)| {
            TimelineCompositeElement::Media(TimelineMediaLayer {
                rgba: &layer.data,
                width: layer.width,
                height: layer.height,
                opacity: *opacity,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_params: AdjustmentLayerParams::default(),
                frame_seed: frame_idx as i64,
            })
        })
        .collect::<Vec<_>>();
    composite_timeline_elements_into(
        canvas,
        width,
        height,
        &elements,
        TimelineCompositeOptions::default(),
        &mut scratch,
    );
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
    let frame_budget_ms = 1000.0 / target_fps;
    let layer_count = layer_count.max(1);
    let mut layers = Vec::with_capacity(layer_count);
    for i in 0..layer_count {
        layers.push(generate_layer(
            width,
            height,
            (17 + i as u8).wrapping_mul(9),
        ));
    }

    let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut passthrough_frames = 0usize;
    let pattern_name = match pattern {
        OpacityPattern::Blend => "blend",
        OpacityPattern::Passthrough => "passthrough",
    };

    let first_frame_layers = build_frame_layers(&layers, 0, pattern);
    let first_frame_passthrough = first_frame_layers.len() == 1
        && first_frame_layers[0].1 >= 0.999
        && first_frame_layers[0].0.width == width
        && first_frame_layers[0].0.height == height;
    let first_started = Instant::now();
    compose_frame_layers_into_canvas(&mut canvas, width, height, &first_frame_layers, 0);
    let first_frame_ms = first_started.elapsed().as_millis();
    if first_frame_passthrough {
        passthrough_frames += 1;
    }

    let mut frame_samples_ms = Vec::with_capacity(sim_frames);
    let mut effective_timeline_ms = 0.0f64;
    let mut missed_budget_frames = 0usize;

    for frame in 0..sim_frames as u64 {
        let frame_layers = build_frame_layers(&layers, frame + 1, pattern);
        let frame_passthrough = frame_layers.len() == 1
            && frame_layers[0].1 >= 0.999
            && frame_layers[0].0.width == width
            && frame_layers[0].0.height == height;

        let started = Instant::now();
        compose_frame_layers_into_canvas(&mut canvas, width, height, &frame_layers, frame + 1);
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

        if frame_passthrough {
            passthrough_frames += 1;
        }
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

    let simulated_total = sim_frames.saturating_add(1);
    let passthrough_ratio_pct = if simulated_total > 0 {
        passthrough_frames as f64 * 100.0 / simulated_total as f64
    } else {
        0.0
    };

    const FPS_EPSILON: f64 = 0.001;
    let passed = first_frame_ms <= first_frame_threshold_ms
        && achieved_fps + FPS_EPSILON >= fps_min_threshold
        && achieved_fps <= fps_max_threshold + FPS_EPSILON;

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

    let width = env_u128("MONDRIAN_EXPORT_SIM_4K_WIDTH", 3840).clamp(640, 7680) as u32;
    let height = env_u128("MONDRIAN_EXPORT_SIM_4K_HEIGHT", 2160).clamp(360, 4320) as u32;
    let target_fps = env_f64("MONDRIAN_EXPORT_SIM_4K_TARGET_FPS", 60.0).clamp(1.0, 240.0);
    let sim_frames = env_usize("MONDRIAN_EXPORT_SIM_4K_FRAMES", 120).clamp(16, 1200);
    let layer_count = env_usize("MONDRIAN_EXPORT_SIM_4K_LAYERS", 2).clamp(1, 8);

    let first_frame_threshold_ms = env_u128("MONDRIAN_EXPORT_SIM_4K_TTFF_MS", 3_000);
    let fps_min_threshold = env_f64("MONDRIAN_EXPORT_SIM_4K_FPS_MIN", 12.0);
    let fps_max_threshold =
        env_f64("MONDRIAN_EXPORT_SIM_4K_FPS_MAX", target_fps).max(fps_min_threshold);

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
