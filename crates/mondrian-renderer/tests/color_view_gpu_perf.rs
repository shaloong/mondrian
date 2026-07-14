use anyhow::{anyhow, Context, Result};
use mondrian_core::types::{AcesConfigPreset, Color, ColorEngine, ColorSpace};
use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, GpuLanguage, OcioColorSpaceIdentity, WorkingColorSpace,
};
use mondrian_effects::{
    get_or_compile_scheduled_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
    EffectRenderOp,
};
use mondrian_renderer::profile::{gpu_timestamp_query_device_features, GpuTimestampFrameTimer};
use mondrian_renderer::{
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    request_adapter_with_native_video_preference, ColorFrameResidency, GpuColorFrameHandle,
    GpuColorFrameTextureFormat, GpuCompositeLayer, GpuCompositeLayerSource, GpuCompositeRequest,
    GpuFrameCompositor, OcioGpuShaderPlan, OcioGpuShaderRequest, RenderColorTransformGpuOptions,
    RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
    RenderOutputColorBoundary,
};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const WIDTH: u32 = 3_840;
const HEIGHT: u32 = 2_160;
const DEFAULT_SAMPLE_COUNT: usize = 60;
const DEFAULT_STANDARD_P95_BUDGET_US: u64 = 5_000;
const DEFAULT_STANDARD_TO_ACES_P95_RATIO: f64 = 0.80;
const WARMUP_COUNT: usize = 4;

struct TimestampGpuContext {
    _instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

#[derive(Clone)]
struct ViewCase {
    mode: &'static str,
    engine: ColorEngine,
    output_color_space: ColorSpace,
    display: &'static str,
    view: &'static str,
}

impl ViewCase {
    fn boundary(&self) -> RenderOutputColorBoundary {
        RenderOutputColorBoundary::display_view(
            self.output_color_space,
            self.display,
            self.view,
            false,
            self.engine.clone(),
        )
    }

    fn shader_request(&self) -> OcioGpuShaderRequest {
        OcioGpuShaderRequest::DisplayView {
            engine: self.engine.clone(),
            src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
            display: self.display.to_owned(),
            view: self.view.to_owned(),
            language: GpuLanguage::Glsl4_0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct QuantilesUs {
    p50: u64,
    p95: u64,
    p99: u64,
    min: u64,
    max: u64,
}

#[derive(Debug, Serialize)]
struct AdapterReport {
    name: String,
    backend: String,
    device_type: String,
    driver: String,
    driver_info: String,
}

#[derive(Debug, Serialize)]
struct ShaderReport {
    bytes: usize,
    texture_2d_count: u32,
    texture_3d_count: u32,
    textures_2d: Vec<Lut2dReport>,
    textures_3d: Vec<Lut3dReport>,
    uniform_count: u32,
    processor_cache_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct Lut2dReport {
    width: u32,
    height: u32,
    channels: String,
    interpolation: String,
}

#[derive(Debug, Serialize)]
struct Lut3dReport {
    edge_length: u32,
    interpolation: String,
}

#[derive(Debug, Serialize)]
struct ViewReport {
    mode: &'static str,
    display: &'static str,
    view: &'static str,
    cold_record_cpu_us: u64,
    warm_record_cpu_us: QuantilesUs,
    warm_gpu_us: QuantilesUs,
    shader: ShaderReport,
    fullscreen_passes: u32,
    output_texture_writes: u32,
    input_uploads_in_measured_region: u32,
    gpu_readbacks_in_measured_region: u32,
}

#[derive(Debug, Serialize)]
struct ComparisonReport {
    standard_pq_to_aces_p95_ratio: f64,
    standard_pq_to_aces_p99_ratio: f64,
    standard_p95_budget_us: u64,
    standard_pq_p95_within_budget: bool,
    standard_hlg_p95_within_budget: bool,
    all_standard_views_within_budget: bool,
    maximum_standard_to_aces_p95_ratio: f64,
    standard_pq_is_materially_faster: bool,
}

#[derive(Debug, Serialize)]
struct ColorViewGpuPerfReport {
    schema_version: u32,
    scenario: &'static str,
    width: u32,
    height: u32,
    samples_per_view: usize,
    warmups_per_view: usize,
    input_contract: &'static str,
    measured_region: &'static str,
    adapter: AdapterReport,
    runtime_cache: RuntimeCacheReport,
    standard_pq: ViewReport,
    standard_hlg: ViewReport,
    aces_reference_pq: ViewReport,
    comparison: ComparisonReport,
}

#[derive(Debug, Serialize)]
struct RuntimeCacheReport {
    shader_entries: usize,
    shader_hits: u64,
    shader_misses: u64,
    static_pipeline_entries: usize,
    static_pipeline_hits: u64,
    static_pipeline_misses: u64,
    backend_object_entries: usize,
    backend_object_hits: u64,
    backend_object_misses: u64,
    texture_pool_hits: u64,
    texture_pool_misses: u64,
    texture_pool_releases: u64,
}

#[derive(Default)]
struct ViewSamples {
    gpu_us: Vec<u64>,
    record_cpu_us: Vec<u64>,
}

struct RecordedSample {
    gpu_us: Option<u64>,
    record_cpu_us: u64,
}

#[tokio::test]
#[ignore = "manual 4K hardware timestamp gate; requires a timestamp-capable real GPU"]
async fn standard_hdr_4k_gpu_timestamp_is_materially_faster_than_aces2() -> Result<()> {
    ensure_mondrian_default_ocio_loaded()
        .map_err(|error| anyhow!("load Mondrian Standard OCIO package: {error}"))?;
    let Some(context) = create_timestamp_gpu_context().await? else {
        eprintln!(
            "MONDRIAN_COLOR_VIEW_GPU_PERF_JSON={}",
            serde_json::json!({
                "schema_version": 2,
                "scenario": "renderer_color_view_4k_gpu_timestamp",
                "skipped": "no real adapter with complete encoder timestamp-query support"
            })
        );
        return Ok(());
    };

    let sample_count = env_usize_clamped(
        "MONDRIAN_COLOR_VIEW_GPU_PERF_SAMPLES",
        DEFAULT_SAMPLE_COUNT,
        20,
        500,
    );
    let standard_p95_budget_us = env_u64(
        "MONDRIAN_STANDARD_HDR_4K_P95_US",
        DEFAULT_STANDARD_P95_BUDGET_US,
    );
    let maximum_ratio = env_f64(
        "MONDRIAN_STANDARD_HDR_TO_ACES_P95_RATIO",
        DEFAULT_STANDARD_TO_ACES_P95_RATIO,
    );
    let standard_pq_case = ViewCase {
        mode: "mondrian_standard_pq",
        engine: ColorEngine::mondrian_standard(),
        output_color_space: ColorSpace::Rec2100Pq,
        display: "Rec.2100-PQ - Display",
        view: "Mondrian Standard HDR 1000 nits v1",
    };
    let standard_hlg_case = ViewCase {
        mode: "mondrian_standard_hlg",
        engine: ColorEngine::mondrian_standard(),
        output_color_space: ColorSpace::Rec2100Hlg,
        display: "Rec.2100-HLG - Display",
        view: "Mondrian Standard HDR 1000 nits v1",
    };
    let aces_pq_case = ViewCase {
        mode: "aces_pq",
        engine: ColorEngine::Aces { preset: AcesConfigPreset::StudioV4Aces2Ocio25 },
        output_color_space: ColorSpace::Rec2100Pq,
        display: "Rec.2100-PQ - Display",
        view: "ACES 2.0 - HDR 1000 nits (Rec.2020)",
    };

    let compositor = GpuFrameCompositor::new(&context.device);
    let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(50_000);
    let input = create_spatially_varying_4k_working_frame(&context, &compositor, &mut runtime)?;
    let timer = GpuTimestampFrameTimer::new(&context.device, &context.queue)
        .context("timestamp features were enabled but timer creation failed")?;

    let standard_pq_cold =
        record_view_sample(&context, &mut runtime, &input, &standard_pq_case, None)?;
    let standard_hlg_cold =
        record_view_sample(&context, &mut runtime, &input, &standard_hlg_case, None)?;
    let aces_pq_cold = record_view_sample(&context, &mut runtime, &input, &aces_pq_case, None)?;
    for _ in 0..WARMUP_COUNT {
        record_view_sample(&context, &mut runtime, &input, &standard_pq_case, None)?;
        record_view_sample(&context, &mut runtime, &input, &standard_hlg_case, None)?;
        record_view_sample(&context, &mut runtime, &input, &aces_pq_case, None)?;
    }

    let standard_pq_shader = runtime
        .shader_cache_mut()
        .get_or_extract(standard_pq_case.shader_request())
        .context("extract cached Standard PQ GPU shader")?;
    let standard_hlg_shader = runtime
        .shader_cache_mut()
        .get_or_extract(standard_hlg_case.shader_request())
        .context("extract cached Standard HLG GPU shader")?;
    let aces_pq_shader = runtime
        .shader_cache_mut()
        .get_or_extract(aces_pq_case.shader_request())
        .context("extract cached ACES 2 PQ GPU shader")?;

    let mut standard_pq_samples = ViewSamples::default();
    let mut standard_hlg_samples = ViewSamples::default();
    let mut aces_pq_samples = ViewSamples::default();
    for iteration in 0..sample_count {
        let ordered = match iteration % 3 {
            0 => [
                (&standard_pq_case, &mut standard_pq_samples),
                (&standard_hlg_case, &mut standard_hlg_samples),
                (&aces_pq_case, &mut aces_pq_samples),
            ],
            1 => [
                (&standard_hlg_case, &mut standard_hlg_samples),
                (&aces_pq_case, &mut aces_pq_samples),
                (&standard_pq_case, &mut standard_pq_samples),
            ],
            _ => [
                (&aces_pq_case, &mut aces_pq_samples),
                (&standard_pq_case, &mut standard_pq_samples),
                (&standard_hlg_case, &mut standard_hlg_samples),
            ],
        };
        for (case, samples) in ordered {
            let sample = record_view_sample(&context, &mut runtime, &input, case, Some(&timer))?;
            samples
                .gpu_us
                .push(sample.gpu_us.context("measured sample missing GPU timestamp")?);
            samples.record_cpu_us.push(sample.record_cpu_us);
        }
    }

    let standard_pq = build_view_report(
        &standard_pq_case,
        standard_pq_cold.record_cpu_us,
        standard_pq_samples,
        &standard_pq_shader,
    );
    let standard_hlg = build_view_report(
        &standard_hlg_case,
        standard_hlg_cold.record_cpu_us,
        standard_hlg_samples,
        &standard_hlg_shader,
    );
    let aces_reference_pq = build_view_report(
        &aces_pq_case,
        aces_pq_cold.record_cpu_us,
        aces_pq_samples,
        &aces_pq_shader,
    );
    let p95_ratio = ratio(
        standard_pq.warm_gpu_us.p95,
        aces_reference_pq.warm_gpu_us.p95,
    );
    let p99_ratio = ratio(
        standard_pq.warm_gpu_us.p99,
        aces_reference_pq.warm_gpu_us.p99,
    );
    let standard_pq_within_budget = standard_pq.warm_gpu_us.p95 <= standard_p95_budget_us;
    let standard_hlg_within_budget = standard_hlg.warm_gpu_us.p95 <= standard_p95_budget_us;
    let comparison = ComparisonReport {
        standard_pq_to_aces_p95_ratio: p95_ratio,
        standard_pq_to_aces_p99_ratio: p99_ratio,
        standard_p95_budget_us,
        standard_pq_p95_within_budget: standard_pq_within_budget,
        standard_hlg_p95_within_budget: standard_hlg_within_budget,
        all_standard_views_within_budget: standard_pq_within_budget && standard_hlg_within_budget,
        maximum_standard_to_aces_p95_ratio: maximum_ratio,
        standard_pq_is_materially_faster: p95_ratio <= maximum_ratio,
    };
    let runtime_diagnostics = runtime.diagnostics();
    let info = context.adapter.get_info();
    let report = ColorViewGpuPerfReport {
        schema_version: 2,
        scenario: "renderer_color_view_4k_gpu_timestamp",
        width: WIDTH,
        height: HEIGHT,
        samples_per_view: sample_count,
        warmups_per_view: WARMUP_COUNT,
        input_contract:
            "GPU-resident RGBA32F Linear Rec.2020 spatially varying compositor output",
        measured_region:
            "one OCIO output fullscreen pass only; input upload, initialization, and timestamp readback excluded",
        adapter: AdapterReport {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
        },
        runtime_cache: RuntimeCacheReport {
            shader_entries: runtime_diagnostics.shader_cache.entries,
            shader_hits: runtime_diagnostics.shader_cache.hits,
            shader_misses: runtime_diagnostics.shader_cache.misses,
            static_pipeline_entries: runtime_diagnostics.backend_prep.static_pipelines.entries,
            static_pipeline_hits: runtime_diagnostics.backend_prep.static_pipelines.hits,
            static_pipeline_misses: runtime_diagnostics.backend_prep.static_pipelines.misses,
            backend_object_entries: runtime_diagnostics.backend_objects.entries,
            backend_object_hits: runtime_diagnostics.backend_objects.hits,
            backend_object_misses: runtime_diagnostics.backend_objects.misses,
            texture_pool_hits: runtime_diagnostics.resource_pool.hits,
            texture_pool_misses: runtime_diagnostics.resource_pool.misses,
            texture_pool_releases: runtime_diagnostics.resource_pool.releases,
        },
        standard_pq,
        standard_hlg,
        aces_reference_pq,
        comparison,
    };
    let json = serde_json::to_string(&report).context("serialize color-view GPU report")?;
    eprintln!("MONDRIAN_COLOR_VIEW_GPU_PERF_JSON={json}");
    if let Some(path) = std::env::var_os("MONDRIAN_COLOR_VIEW_GPU_PERF_OUTPUT").map(PathBuf::from) {
        append_jsonl(&path, &json)?;
    }

    assert!(
        report.comparison.all_standard_views_within_budget,
        "Mondrian Standard 4K HDR p95 exceeds {} us budget: PQ={} us, HLG={} us\n{json}",
        report.comparison.standard_p95_budget_us,
        report.standard_pq.warm_gpu_us.p95,
        report.standard_hlg.warm_gpu_us.p95,
    );
    assert!(
        report.comparison.standard_pq_is_materially_faster,
        "Mondrian Standard HDR p95 ratio {:.3} exceeds {:.3} ACES-reference ceiling\n{json}",
        report.comparison.standard_pq_to_aces_p95_ratio,
        report.comparison.maximum_standard_to_aces_p95_ratio
    );
    Ok(())
}

async fn create_timestamp_gpu_context() -> Result<Option<TimestampGpuContext>> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = match request_adapter_with_native_video_preference(
        &instance,
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        },
    )
    .await
    {
        Ok(adapter) => adapter,
        Err(_) => return Ok(None),
    };
    let timestamp_features = gpu_timestamp_query_device_features(adapter.features());
    if timestamp_features.is_empty() {
        return Ok(None);
    }
    let required_features = timestamp_features
        | native_video_texture_device_features(adapter.features())
        | ocio_lut_filtering_device_features(adapter.features());
    let descriptor = wgpu::DeviceDescriptor {
        label: Some("mondrian-color-view-4k-timestamp-device"),
        required_features,
        ..wgpu::DeviceDescriptor::default()
    };
    let (device, queue) = adapter
        .request_device(&descriptor)
        .await
        .context("request timestamp-capable color benchmark device")?;
    Ok(Some(TimestampGpuContext {
        _instance: instance,
        adapter,
        device,
        queue,
    }))
}

fn create_spatially_varying_4k_working_frame(
    context: &TimestampGpuContext,
    compositor: &GpuFrameCompositor,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
) -> Result<GpuColorFrameHandle> {
    let mut graph = EffectGraphBuilderState::new();
    graph.append_unary(EffectRenderOp::ColorAdjust {
        exposure: 2.0,
        contrast: 1.1,
        saturation: 1.15,
    });
    graph.append_unary(EffectRenderOp::Grain { amount: 0.35 });
    let graph = get_or_compile_scheduled_render_graph(graph.finish())
        .context("compile spatially varying benchmark effect graph")?;
    let effect_plan = lower_effect_graph_to_gpu_plan(&graph)
        .context("lower spatially varying benchmark effect graph")?;
    let layer = GpuCompositeLayer {
        source: GpuCompositeLayerSource::SolidColor(Color { r: 0.18, g: 0.42, b: 0.73, a: 1.0 }),
        opacity: 1.0,
        blend_mode: mondrian_core::types::BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_plan: Some(&effect_plan),
        frame_seed: 0x4d53,
    };
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mondrian-color-view-4k-input"),
    });
    let record = runtime
        .record_wgpu_working_composite(
            compositor,
            &context.device,
            &context.queue,
            &mut encoder,
            GpuCompositeRequest {
                width: WIDTH,
                height: HEIGHT,
                working_color_space: WorkingColorSpace::LinearRec2020,
                layers: std::slice::from_ref(&layer),
            },
        )
        .context("record spatially varying 4K working frame")?;
    let submission = context.queue.submit(std::iter::once(encoder.finish()));
    context
        .device
        .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
        .context("complete spatially varying 4K working frame")?;

    let input = record.output;
    let pool = runtime.resource_pool();
    let mut input_resource = None;
    for resource in runtime.frame_table_mut().drain() {
        if resource.handle() == &input {
            input_resource = Some(resource);
        } else {
            pool.release(resource);
        }
    }
    runtime
        .frame_table_mut()
        .insert(input_resource.context("compositor output missing from resource table")?)
        .map_err(|error| anyhow!("restore retained 4K working input: {error:?}"))?;
    Ok(input)
}

fn record_view_sample(
    context: &TimestampGpuContext,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    input: &GpuColorFrameHandle,
    case: &ViewCase,
    timer: Option<&GpuTimestampFrameTimer>,
) -> Result<RecordedSample> {
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mondrian-color-view-4k-sample"),
    });
    if let Some(timer) = timer {
        timer.begin(&mut encoder);
    }
    let started = Instant::now();
    let record = runtime
        .record_wgpu_output_boundary_gpu_frame_owned_backend(
            &case.boundary(),
            input,
            GpuColorFrameTextureFormat::Rgba16Float,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Gpu,
                ..RenderColorTransformGpuOptions::default()
            },
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device: &context.device,
                queue: &context.queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            },
        )
        .map_err(|error| anyhow!("record {} 4K HDR view: {error:?}", case.mode))?;
    let record_cpu_us = saturating_u64(started.elapsed().as_micros());
    if let Some(timer) = timer {
        timer.finish(&mut encoder);
    }
    assert_eq!(record.stage_diagnostics.gpu_color_stages, 1);
    assert_eq!(record.stage_diagnostics.upload_stages, 0);
    assert_eq!(record.stage_diagnostics.readback_stages, 0);
    let output_id = record.materialized.output.id();
    let submission = context.queue.submit(std::iter::once(encoder.finish()));
    let gpu_us = timer
        .map(|timer| {
            timer
                .read_elapsed_us_after_submission(&context.device, submission)
                .context("read color-view hardware timestamp")
        })
        .transpose()?;
    let output = runtime
        .frame_table_mut()
        .remove(output_id)
        .context("recorded output missing from resource table")?;
    runtime.resource_pool().release(output);
    assert_eq!(runtime.frame_table().len(), 1);
    Ok(RecordedSample { gpu_us, record_cpu_us })
}

fn build_view_report(
    case: &ViewCase,
    cold_record_cpu_us: u64,
    samples: ViewSamples,
    shader: &OcioGpuShaderPlan,
) -> ViewReport {
    ViewReport {
        mode: case.mode,
        display: case.display,
        view: case.view,
        cold_record_cpu_us,
        warm_record_cpu_us: quantiles(samples.record_cpu_us),
        warm_gpu_us: quantiles(samples.gpu_us),
        shader: ShaderReport {
            bytes: shader.shader_len,
            texture_2d_count: shader.texture_2d_count,
            texture_3d_count: shader.texture_3d_count,
            textures_2d: shader
                .bundle()
                .textures_2d
                .iter()
                .map(|texture| Lut2dReport {
                    width: texture.width,
                    height: texture.height,
                    channels: format!("{:?}", texture.channel),
                    interpolation: format!("{:?}", texture.interpolation),
                })
                .collect(),
            textures_3d: shader
                .bundle()
                .textures_3d
                .iter()
                .map(|texture| Lut3dReport {
                    edge_length: texture.edge_len,
                    interpolation: format!("{:?}", texture.interpolation),
                })
                .collect(),
            uniform_count: shader.uniform_count,
            processor_cache_id: shader.processor_cache_id.clone(),
        },
        fullscreen_passes: 1,
        output_texture_writes: 1,
        input_uploads_in_measured_region: 0,
        gpu_readbacks_in_measured_region: 0,
    }
}

fn quantiles(mut samples: Vec<u64>) -> QuantilesUs {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    QuantilesUs {
        p50: nearest_rank(&samples, 50),
        p95: nearest_rank(&samples, 95),
        p99: nearest_rank(&samples, 99),
        min: samples[0],
        max: samples[samples.len() - 1],
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        f64::INFINITY
    } else {
        numerator as f64 / denominator as f64
    }
}

fn saturating_u64(value: u128) -> u64 {
    value.min(u128::from(u64::MAX)) as u64
}

fn env_usize_clamped(name: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(default)
}

fn append_jsonl(path: &Path, json: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut output = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(output, "{json}").with_context(|| format!("append {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_quantiles_do_not_report_only_the_best_sample() {
        let samples = (1_u64..=100).rev().collect();
        assert_eq!(
            quantiles(samples),
            QuantilesUs { p50: 50, p95: 95, p99: 99, min: 1, max: 100 }
        );
    }
}
