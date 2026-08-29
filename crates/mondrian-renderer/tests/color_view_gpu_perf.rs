use anyhow::{anyhow, Context, Result};
use mondrian_core::types::{AcesConfigPreset, Color, ColorEngine, ColorSpace};
use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, GpuLanguage, OcioColorSpaceIdentity, WorkingColorSpace,
};
use mondrian_effects::{
    compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
    EffectRenderOp,
};
use mondrian_renderer::profile::{gpu_timestamp_query_device_features, GpuTimestampFrameTimer};
use mondrian_renderer::{
    color::{
        qualification::{
            RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeDiagnostics,
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
        },
        ProgramOutputBoundary,
    },
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    request_adapter_with_native_video_preference, ColorFrameDescriptor, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, GpuColorFrameAllocationPlan, GpuColorFrameHandle,
    GpuColorFrameTextureFormat, GpuColorQualificationExecutionPolicy, GpuCompositeLayer,
    GpuCompositeLayerSource, GpuCompositeRequest, GpuFrameCompositor, OcioGpuShaderPlan,
    OcioGpuShaderRequest, RenderColorTransformGpuOptions, RenderInputTransform,
    RenderIntermediateColorTransform,
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
const DEFAULT_TRANSFORM_P95_BUDGET_US: u64 = 5_000;
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
    fn boundary(&self) -> ProgramOutputBoundary {
        ProgramOutputBoundary::display_view(
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

#[derive(Debug, Clone, Copy)]
enum TransformExecution {
    Intermediate {
        output_domain: ColorFrameDomain,
        output_encoding: ColorFrameEncoding,
        output_texture_format: GpuColorFrameTextureFormat,
    },
    InputToWorking,
}

#[derive(Clone)]
struct TransformCase {
    mode: &'static str,
    operation_class: &'static str,
    src: OcioColorSpaceIdentity,
    dst: OcioColorSpaceIdentity,
    execution: TransformExecution,
}

impl TransformCase {
    fn shader_request(&self) -> OcioGpuShaderRequest {
        OcioGpuShaderRequest::ColorSpace {
            engine: ColorEngine::mondrian_standard(),
            src: self.src,
            dst: self.dst,
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
    standard_sdr_p95_within_budget: bool,
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
    standard_sdr: ViewReport,
    standard_pq: ViewReport,
    standard_hlg: ViewReport,
    aces_reference_pq: ViewReport,
    comparison: ComparisonReport,
}

#[derive(Debug, Serialize)]
struct TransformReport {
    mode: &'static str,
    operation_class: &'static str,
    source_identity: String,
    destination_identity: String,
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
struct ColorTransformGpuPerfReport {
    schema_version: u32,
    scenario: &'static str,
    width: u32,
    height: u32,
    samples_per_transform: usize,
    warmups_per_transform: usize,
    measured_region: &'static str,
    adapter: AdapterReport,
    runtime_cache: TransformRuntimeCacheReport,
    identity: TransformReport,
    matrix_oetf: TransformReport,
    rec709_to_working: TransformReport,
    camera_log_to_working: TransformReport,
    p95_budget_us: u64,
    all_transforms_within_budget: bool,
}

#[derive(Debug, Serialize)]
struct TransformRuntimeCacheReport {
    measured_delta: RuntimeCacheActivity,
    warm_path_gate: TransformWarmPathReuseGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct TransformWarmPathReuseGate {
    expected_samples: u64,
    shader_cache_creation_free: bool,
    static_pipeline_creation_free: bool,
    backend_object_creation_free: bool,
    wrapper_bind_group_creation_free: bool,
    output_texture_allocation_free: bool,
    wrapper_bind_group_hits_cover_samples: bool,
    output_texture_hits_cover_samples: bool,
    passed: bool,
}

impl TransformWarmPathReuseGate {
    fn evaluate(activity: RuntimeCacheActivity, expected_samples: u64) -> Self {
        let shader_cache_creation_free =
            activity.shader_misses == 0 && activity.shader_extraction_failures == 0;
        let static_pipeline_creation_free = activity.static_pipeline_misses == 0;
        let backend_object_creation_free =
            activity.backend_object_misses == 0 && activity.backend_object_failures == 0;
        let wrapper_bind_group_creation_free = activity.wrapper_bind_group_creations == 0;
        let output_texture_allocation_free =
            activity.texture_pool_misses == 0 && activity.texture_pool_evictions == 0;
        let wrapper_bind_group_hits_cover_samples =
            activity.wrapper_bind_group_cache_hits >= expected_samples;
        let output_texture_hits_cover_samples = activity.texture_pool_hits >= expected_samples;
        let passed = shader_cache_creation_free
            && static_pipeline_creation_free
            && backend_object_creation_free
            && wrapper_bind_group_creation_free
            && output_texture_allocation_free
            && wrapper_bind_group_hits_cover_samples
            && output_texture_hits_cover_samples;
        Self {
            expected_samples,
            shader_cache_creation_free,
            static_pipeline_creation_free,
            backend_object_creation_free,
            wrapper_bind_group_creation_free,
            output_texture_allocation_free,
            wrapper_bind_group_hits_cover_samples,
            output_texture_hits_cover_samples,
            passed,
        }
    }
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
    measured_delta: RuntimeCacheActivity,
    warm_path_gate: WarmPathReuseGate,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct RuntimeCacheActivity {
    shader_hits: u64,
    shader_misses: u64,
    shader_extraction_failures: u64,
    static_pipeline_hits: u64,
    static_pipeline_misses: u64,
    backend_object_hits: u64,
    backend_object_misses: u64,
    backend_object_failures: u64,
    wrapper_bind_group_creations: u64,
    wrapper_bind_group_cache_hits: u64,
    texture_pool_hits: u64,
    texture_pool_misses: u64,
    texture_pool_releases: u64,
    texture_pool_evictions: u64,
}

impl RuntimeCacheActivity {
    fn from_runtime(diagnostics: RenderGpuOutputBoundaryRuntimeDiagnostics) -> Self {
        Self {
            shader_hits: diagnostics.shader_cache.hits,
            shader_misses: diagnostics.shader_cache.misses,
            shader_extraction_failures: diagnostics.shader_cache.extraction_failures,
            static_pipeline_hits: diagnostics.backend_prep.static_pipelines.hits,
            static_pipeline_misses: diagnostics.backend_prep.static_pipelines.misses,
            backend_object_hits: diagnostics.backend_objects.hits,
            backend_object_misses: diagnostics.backend_objects.misses,
            backend_object_failures: diagnostics.backend_objects.failures,
            wrapper_bind_group_creations: diagnostics
                .backend_objects
                .wrapper_input_bindings
                .bind_group_creations,
            wrapper_bind_group_cache_hits: diagnostics
                .backend_objects
                .wrapper_input_bindings
                .cache_hits,
            texture_pool_hits: diagnostics.resource_pool.hits,
            texture_pool_misses: diagnostics.resource_pool.misses,
            texture_pool_releases: diagnostics.resource_pool.releases,
            texture_pool_evictions: diagnostics.resource_pool.evictions,
        }
    }

    fn delta_since(self, before: Self) -> Self {
        Self {
            shader_hits: monotonic_delta(self.shader_hits, before.shader_hits),
            shader_misses: monotonic_delta(self.shader_misses, before.shader_misses),
            shader_extraction_failures: monotonic_delta(
                self.shader_extraction_failures,
                before.shader_extraction_failures,
            ),
            static_pipeline_hits: monotonic_delta(
                self.static_pipeline_hits,
                before.static_pipeline_hits,
            ),
            static_pipeline_misses: monotonic_delta(
                self.static_pipeline_misses,
                before.static_pipeline_misses,
            ),
            backend_object_hits: monotonic_delta(
                self.backend_object_hits,
                before.backend_object_hits,
            ),
            backend_object_misses: monotonic_delta(
                self.backend_object_misses,
                before.backend_object_misses,
            ),
            backend_object_failures: monotonic_delta(
                self.backend_object_failures,
                before.backend_object_failures,
            ),
            wrapper_bind_group_creations: monotonic_delta(
                self.wrapper_bind_group_creations,
                before.wrapper_bind_group_creations,
            ),
            wrapper_bind_group_cache_hits: monotonic_delta(
                self.wrapper_bind_group_cache_hits,
                before.wrapper_bind_group_cache_hits,
            ),
            texture_pool_hits: monotonic_delta(self.texture_pool_hits, before.texture_pool_hits),
            texture_pool_misses: monotonic_delta(
                self.texture_pool_misses,
                before.texture_pool_misses,
            ),
            texture_pool_releases: monotonic_delta(
                self.texture_pool_releases,
                before.texture_pool_releases,
            ),
            texture_pool_evictions: monotonic_delta(
                self.texture_pool_evictions,
                before.texture_pool_evictions,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct WarmPathReuseGate {
    expected_view_samples: u64,
    shader_cache_creation_free: bool,
    static_pipeline_creation_free: bool,
    backend_object_creation_free: bool,
    wrapper_bind_group_creation_free: bool,
    output_texture_allocation_free: bool,
    wrapper_bind_group_hits_cover_samples: bool,
    output_texture_hits_cover_samples: bool,
    passed: bool,
}

impl WarmPathReuseGate {
    fn evaluate(activity: RuntimeCacheActivity, expected_view_samples: u64) -> Self {
        let shader_cache_creation_free =
            activity.shader_misses == 0 && activity.shader_extraction_failures == 0;
        let static_pipeline_creation_free = activity.static_pipeline_misses == 0;
        let backend_object_creation_free =
            activity.backend_object_misses == 0 && activity.backend_object_failures == 0;
        let wrapper_bind_group_creation_free = activity.wrapper_bind_group_creations == 0;
        let output_texture_allocation_free =
            activity.texture_pool_misses == 0 && activity.texture_pool_evictions == 0;
        let wrapper_bind_group_hits_cover_samples =
            activity.wrapper_bind_group_cache_hits >= expected_view_samples;
        let output_texture_hits_cover_samples = activity.texture_pool_hits >= expected_view_samples;
        let passed = shader_cache_creation_free
            && static_pipeline_creation_free
            && backend_object_creation_free
            && wrapper_bind_group_creation_free
            && output_texture_allocation_free
            && wrapper_bind_group_hits_cover_samples
            && output_texture_hits_cover_samples;
        Self {
            expected_view_samples,
            shader_cache_creation_free,
            static_pipeline_creation_free,
            backend_object_creation_free,
            wrapper_bind_group_creation_free,
            output_texture_allocation_free,
            wrapper_bind_group_hits_cover_samples,
            output_texture_hits_cover_samples,
            passed,
        }
    }
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
async fn standard_views_4k_gpu_timestamp_meet_budget_and_beat_aces2() -> Result<()> {
    ensure_mondrian_default_ocio_loaded()
        .map_err(|error| anyhow!("load Mondrian Standard OCIO package: {error}"))?;
    let qualification_policy = GpuColorQualificationExecutionPolicy::from_environment()?;
    let Some(context) = create_timestamp_gpu_context().await? else {
        eprintln!(
            "MONDRIAN_COLOR_VIEW_GPU_PERF_JSON={}",
            serde_json::json!({
                "schema_version": 4,
                "scenario": "renderer_color_view_4k_gpu_timestamp",
                "skipped": "no real adapter with complete encoder timestamp-query support"
            })
        );
        qualification_policy.admit_capability(
            "standard-view-4k-performance",
            "real-adapter-with-complete-timestamp-query",
            false,
        )?;
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
    let standard_sdr_case = ViewCase {
        mode: "mondrian_standard_sdr",
        engine: ColorEngine::mondrian_standard(),
        output_color_space: ColorSpace::Srgb,
        display: "sRGB - Display",
        view: "Mondrian Standard SDR v2",
    };
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

    let compositor = GpuFrameCompositor::new(&context.device)?;
    let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(50_000)?;
    let input = create_spatially_varying_4k_working_frame(&context, &compositor, &mut runtime)?;
    let timer = GpuTimestampFrameTimer::new(&context.device, &context.queue)
        .context("timestamp features were enabled but timer creation failed")?;

    let standard_sdr_cold =
        record_view_sample(&context, &mut runtime, &input, &standard_sdr_case, None)?;
    let standard_pq_cold =
        record_view_sample(&context, &mut runtime, &input, &standard_pq_case, None)?;
    let standard_hlg_cold =
        record_view_sample(&context, &mut runtime, &input, &standard_hlg_case, None)?;
    let aces_pq_cold = record_view_sample(&context, &mut runtime, &input, &aces_pq_case, None)?;
    for _ in 0..WARMUP_COUNT {
        record_view_sample(&context, &mut runtime, &input, &standard_sdr_case, None)?;
        record_view_sample(&context, &mut runtime, &input, &standard_pq_case, None)?;
        record_view_sample(&context, &mut runtime, &input, &standard_hlg_case, None)?;
        record_view_sample(&context, &mut runtime, &input, &aces_pq_case, None)?;
    }

    let standard_sdr_shader = runtime
        .shader_cache_mut()
        .get_or_extract(standard_sdr_case.shader_request())
        .context("extract cached Standard SDR GPU shader")?;
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
    let runtime_before_measurement = RuntimeCacheActivity::from_runtime(runtime.diagnostics());

    let mut standard_sdr_samples = ViewSamples::default();
    let mut standard_pq_samples = ViewSamples::default();
    let mut standard_hlg_samples = ViewSamples::default();
    let mut aces_pq_samples = ViewSamples::default();
    for iteration in 0..sample_count {
        for offset in 0..4 {
            let (case, samples) = match (iteration + offset) % 4 {
                0 => (&standard_sdr_case, &mut standard_sdr_samples),
                1 => (&standard_pq_case, &mut standard_pq_samples),
                2 => (&standard_hlg_case, &mut standard_hlg_samples),
                _ => (&aces_pq_case, &mut aces_pq_samples),
            };
            let sample = record_view_sample(&context, &mut runtime, &input, case, Some(&timer))?;
            samples
                .gpu_us
                .push(sample.gpu_us.context("measured sample missing GPU timestamp")?);
            samples.record_cpu_us.push(sample.record_cpu_us);
        }
    }
    let runtime_diagnostics = runtime.diagnostics();
    let measured_cache_activity = RuntimeCacheActivity::from_runtime(runtime_diagnostics)
        .delta_since(runtime_before_measurement);
    let expected_view_samples = u64::try_from(sample_count)
        .expect("bounded sample count fits u64")
        .saturating_mul(4);
    let warm_path_gate =
        WarmPathReuseGate::evaluate(measured_cache_activity, expected_view_samples);

    let standard_sdr = build_view_report(
        &standard_sdr_case,
        standard_sdr_cold.record_cpu_us,
        standard_sdr_samples,
        &standard_sdr_shader,
    );
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
    let standard_sdr_within_budget = standard_sdr.warm_gpu_us.p95 <= standard_p95_budget_us;
    let standard_pq_within_budget = standard_pq.warm_gpu_us.p95 <= standard_p95_budget_us;
    let standard_hlg_within_budget = standard_hlg.warm_gpu_us.p95 <= standard_p95_budget_us;
    let comparison = ComparisonReport {
        standard_pq_to_aces_p95_ratio: p95_ratio,
        standard_pq_to_aces_p99_ratio: p99_ratio,
        standard_p95_budget_us,
        standard_sdr_p95_within_budget: standard_sdr_within_budget,
        standard_pq_p95_within_budget: standard_pq_within_budget,
        standard_hlg_p95_within_budget: standard_hlg_within_budget,
        all_standard_views_within_budget: standard_sdr_within_budget
            && standard_pq_within_budget
            && standard_hlg_within_budget,
        maximum_standard_to_aces_p95_ratio: maximum_ratio,
        standard_pq_is_materially_faster: p95_ratio <= maximum_ratio,
    };
    let info = context.adapter.get_info();
    let report = ColorViewGpuPerfReport {
        schema_version: 4,
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
            measured_delta: measured_cache_activity,
            warm_path_gate,
        },
        standard_sdr,
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
        report.runtime_cache.warm_path_gate.passed,
        "warm 4K color-view sampling created or failed to reuse GPU runtime objects\n{json}"
    );
    assert!(
        report.comparison.all_standard_views_within_budget,
        "Mondrian Standard 4K View p95 exceeds {} us budget: SDR={} us, PQ={} us, HLG={} us\n{json}",
        report.comparison.standard_p95_budget_us,
        report.standard_sdr.warm_gpu_us.p95,
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

#[tokio::test]
#[ignore = "manual 4K hardware timestamp gate; requires a timestamp-capable real GPU"]
async fn standard_input_transforms_4k_gpu_timestamp_meet_budget() -> Result<()> {
    ensure_mondrian_default_ocio_loaded()
        .map_err(|error| anyhow!("load Mondrian Standard OCIO package: {error}"))?;
    let qualification_policy = GpuColorQualificationExecutionPolicy::from_environment()?;
    let Some(context) = create_timestamp_gpu_context().await? else {
        eprintln!(
            "MONDRIAN_COLOR_TRANSFORM_GPU_PERF_JSON={}",
            serde_json::json!({
                "schema_version": 1,
                "scenario": "renderer_color_transform_4k_gpu_timestamp",
                "skipped": "no real adapter with complete encoder timestamp-query support"
            })
        );
        qualification_policy.admit_capability(
            "standard-input-4k-performance",
            "real-adapter-with-complete-timestamp-query",
            false,
        )?;
        return Ok(());
    };

    let sample_count = env_usize_clamped(
        "MONDRIAN_COLOR_TRANSFORM_GPU_PERF_SAMPLES",
        DEFAULT_SAMPLE_COUNT,
        20,
        500,
    );
    let p95_budget_us = env_u64(
        "MONDRIAN_COLOR_TRANSFORM_4K_P95_US",
        DEFAULT_TRANSFORM_P95_BUDGET_US,
    );
    let identity = TransformCase {
        mode: "identity",
        operation_class: "OCIO optimized identity",
        src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        dst: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        execution: TransformExecution::Intermediate {
            output_domain: ColorFrameDomain::Working,
            output_encoding: ColorFrameEncoding::LinearFloat,
            output_texture_format: GpuColorFrameTextureFormat::Rgba32Float,
        },
    };
    let matrix_oetf = TransformCase {
        mode: "matrix_oetf",
        operation_class: "primaries matrix plus Rec.709 display OETF",
        src: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        dst: OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
        execution: TransformExecution::Intermediate {
            output_domain: ColorFrameDomain::Effect,
            output_encoding: ColorFrameEncoding::EncodedFloat,
            output_texture_format: GpuColorFrameTextureFormat::Rgba16Float,
        },
    };
    let rec709_to_working = TransformCase {
        mode: "rec709_to_working",
        operation_class: "Rec.709 input decode plus primaries conversion",
        src: OcioColorSpaceIdentity::Color(ColorSpace::Rec709),
        dst: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        execution: TransformExecution::InputToWorking,
    };
    let camera_log_to_working = TransformCase {
        mode: "sony_slog3_sgamut3cine_to_working",
        operation_class: "camera log decode plus gamut conversion",
        src: OcioColorSpaceIdentity::Color(ColorSpace::SonySLog3SGamut3Cine),
        dst: OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec2020),
        execution: TransformExecution::InputToWorking,
    };

    let compositor = GpuFrameCompositor::new(&context.device)?;
    let mut runtime = RenderGpuOutputBoundaryRuntime::with_first_frame_id(80_000)?;
    let working_input =
        create_spatially_varying_4k_working_frame(&context, &compositor, &mut runtime)?;
    let rec709_input = create_encoded_float_source(
        &context,
        &mut runtime,
        ColorSpace::Rec709,
        "mondrian-color-transform-rec709-input",
    )?;
    let camera_log_input = create_encoded_float_source(
        &context,
        &mut runtime,
        ColorSpace::SonySLog3SGamut3Cine,
        "mondrian-color-transform-slog3-input",
    )?;
    let timer = GpuTimestampFrameTimer::new(&context.device, &context.queue)
        .context("timestamp features were enabled but timer creation failed")?;

    let cases = [
        (&identity, &working_input),
        (&matrix_oetf, &working_input),
        (&rec709_to_working, &rec709_input),
        (&camera_log_to_working, &camera_log_input),
    ];
    let mut cold_record_cpu_us = [0_u64; 4];
    for (index, (case, input)) in cases.iter().enumerate() {
        cold_record_cpu_us[index] =
            record_transform_sample(&context, &mut runtime, input, case, None, 3)?.record_cpu_us;
    }
    for _ in 0..WARMUP_COUNT {
        for (case, input) in cases {
            record_transform_sample(&context, &mut runtime, input, case, None, 3)?;
        }
    }

    let shaders = [
        runtime
            .shader_cache_mut()
            .get_or_extract(identity.shader_request())
            .context("extract cached identity GPU shader")?,
        runtime
            .shader_cache_mut()
            .get_or_extract(matrix_oetf.shader_request())
            .context("extract cached matrix + OETF GPU shader")?,
        runtime
            .shader_cache_mut()
            .get_or_extract(rec709_to_working.shader_request())
            .context("extract cached Rec.709 input GPU shader")?,
        runtime
            .shader_cache_mut()
            .get_or_extract(camera_log_to_working.shader_request())
            .context("extract cached camera-log input GPU shader")?,
    ];
    let runtime_before_measurement = RuntimeCacheActivity::from_runtime(runtime.diagnostics());
    let mut samples = std::array::from_fn::<ViewSamples, 4, _>(|_| ViewSamples::default());
    for iteration in 0..sample_count {
        for offset in 0..cases.len() {
            let index = (iteration + offset) % cases.len();
            let (case, input) = cases[index];
            let sample =
                record_transform_sample(&context, &mut runtime, input, case, Some(&timer), 3)?;
            samples[index]
                .gpu_us
                .push(sample.gpu_us.context("measured sample missing GPU timestamp")?);
            samples[index].record_cpu_us.push(sample.record_cpu_us);
        }
    }

    let runtime_diagnostics = runtime.diagnostics();
    let measured_cache_activity = RuntimeCacheActivity::from_runtime(runtime_diagnostics)
        .delta_since(runtime_before_measurement);
    let expected_samples = u64::try_from(sample_count)
        .expect("bounded sample count fits u64")
        .saturating_mul(u64::try_from(cases.len()).expect("case count fits u64"));
    let warm_path_gate =
        TransformWarmPathReuseGate::evaluate(measured_cache_activity, expected_samples);
    let [identity_samples, matrix_samples, rec709_samples, camera_log_samples] = samples;
    let reports = [
        build_transform_report(
            &identity,
            cold_record_cpu_us[0],
            identity_samples,
            &shaders[0],
        ),
        build_transform_report(
            &matrix_oetf,
            cold_record_cpu_us[1],
            matrix_samples,
            &shaders[1],
        ),
        build_transform_report(
            &rec709_to_working,
            cold_record_cpu_us[2],
            rec709_samples,
            &shaders[2],
        ),
        build_transform_report(
            &camera_log_to_working,
            cold_record_cpu_us[3],
            camera_log_samples,
            &shaders[3],
        ),
    ];
    let all_transforms_within_budget =
        reports.iter().all(|report| report.warm_gpu_us.p95 <= p95_budget_us);
    let [identity, matrix_oetf, rec709_to_working, camera_log_to_working] = reports;
    let info = context.adapter.get_info();
    let report = ColorTransformGpuPerfReport {
        schema_version: 1,
        scenario: "renderer_color_transform_4k_gpu_timestamp",
        width: WIDTH,
        height: HEIGHT,
        samples_per_transform: sample_count,
        warmups_per_transform: WARMUP_COUNT,
        measured_region:
            "one production OCIO fullscreen pass only; source initialization, upload, and timestamp readback excluded",
        adapter: AdapterReport {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            driver_info: info.driver_info,
        },
        runtime_cache: TransformRuntimeCacheReport {
            measured_delta: measured_cache_activity,
            warm_path_gate,
        },
        identity,
        matrix_oetf,
        rec709_to_working,
        camera_log_to_working,
        p95_budget_us,
        all_transforms_within_budget,
    };
    let json = serde_json::to_string(&report).context("serialize color-transform GPU report")?;
    eprintln!("MONDRIAN_COLOR_TRANSFORM_GPU_PERF_JSON={json}");
    if let Some(path) =
        std::env::var_os("MONDRIAN_COLOR_TRANSFORM_GPU_PERF_OUTPUT").map(PathBuf::from)
    {
        append_jsonl(&path, &json)?;
    }

    assert!(
        report.runtime_cache.warm_path_gate.passed,
        "warm 4K color-transform sampling created or failed to reuse GPU runtime objects\n{json}"
    );
    assert!(
        report.all_transforms_within_budget,
        "Mondrian Standard 4K transform p95 exceeds {} us budget: identity={} us, matrix+OETF={} us, Rec.709={} us, camera-log={} us\n{json}",
        report.p95_budget_us,
        report.identity.warm_gpu_us.p95,
        report.matrix_oetf.warm_gpu_us.p95,
        report.rec709_to_working.warm_gpu_us.p95,
        report.camera_log_to_working.warm_gpu_us.p95,
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
        working_color_space: WorkingColorSpace::LinearRec2020,
    });
    graph.append_unary(EffectRenderOp::Grain { amount: 0.35 });
    let graph = compile_reference_render_graph(graph.finish())
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

fn create_encoded_float_source(
    context: &TimestampGpuContext,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    color_space: ColorSpace,
    label: &'static str,
) -> Result<GpuColorFrameHandle> {
    let handle = GpuColorFrameHandle::new(
        runtime
            .frame_ids_mut()
            .allocate()
            .map_err(|error| anyhow!("allocate {label} frame id: {error}"))?,
        ColorFrameDescriptor {
            width: WIDTH,
            height: HEIGHT,
            color_space: color_space.into(),
            domain: ColorFrameDomain::Source,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
            alpha: mondrian_renderer::ColorFrameAlpha::Opaque,
        },
        GpuColorFrameTextureFormat::Rgba16Float,
        label,
    )
    .map_err(|error| anyhow!("create {label} handle: {error}"))?;
    let allocation = GpuColorFrameAllocationPlan::for_handle(handle.clone());
    let pool = runtime.resource_pool();
    let resource = pool.acquire(&context.device, &allocation);
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mondrian-color-transform-source-initialization"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian-color-transform-source-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &resource.resource().texture_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.18, g: 0.42, b: 0.73, a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    let submission = context.queue.submit(std::iter::once(encoder.finish()));
    context
        .device
        .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
        .with_context(|| format!("complete {label} initialization"))?;
    runtime
        .frame_table_mut()
        .insert(resource)
        .map_err(|error| anyhow!("insert {label}: {error:?}"))?;
    Ok(handle)
}

fn record_transform_sample(
    context: &TimestampGpuContext,
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    input: &GpuColorFrameHandle,
    case: &TransformCase,
    timer: Option<&GpuTimestampFrameTimer>,
    retained_input_count: usize,
) -> Result<RecordedSample> {
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mondrian-color-transform-4k-sample"),
    });
    if let Some(timer) = timer {
        timer.begin(&mut encoder);
    }
    let started = Instant::now();
    let (output_id, stage_diagnostics) = match case.execution {
        TransformExecution::Intermediate {
            output_domain,
            output_encoding,
            output_texture_format,
        } => {
            let record = runtime
                .record_wgpu_intermediate_color_transform_owned_backend(
                    &RenderIntermediateColorTransform {
                        output_identity: case.dst,
                        output_domain,
                        output_encoding,
                        engine: ColorEngine::mondrian_standard(),
                    },
                    input,
                    output_texture_format,
                    format!("mondrian-color-transform-{}-output", case.mode),
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
                .map_err(|error| anyhow!("record {} transform: {error:?}", case.mode))?;
            (record.materialized.output.id(), record.stage_diagnostics)
        }
        TransformExecution::InputToWorking => {
            let output = GpuColorFrameHandle::new(
                runtime
                    .frame_ids_mut()
                    .allocate()
                    .map_err(|error| anyhow!("allocate {} output frame id: {error}", case.mode))?,
                ColorFrameDescriptor {
                    width: WIDTH,
                    height: HEIGHT,
                    color_space: WorkingColorSpace::LinearRec2020.into(),
                    domain: ColorFrameDomain::Working,
                    encoding: ColorFrameEncoding::LinearFloat,
                    residency: ColorFrameResidency::Gpu,
                    alpha: mondrian_renderer::ColorFrameAlpha::StraightCoverage,
                },
                GpuColorFrameTextureFormat::Rgba32Float,
                format!("mondrian-color-transform-{}-output", case.mode),
            )
            .map_err(|error| anyhow!("create {} output handle: {error}", case.mode))?;
            let record = runtime
                .record_wgpu_input_stage_gpu_frame_owned_backend(
                    &RenderInputTransform::to_working_gpu(
                        WorkingColorSpace::LinearRec2020,
                        false,
                        ColorEngine::mondrian_standard(),
                    ),
                    input,
                    &output,
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
                .map_err(|error| anyhow!("record {} input transform: {error:?}", case.mode))?;
            (record.materialized.output.id(), record.stage_diagnostics)
        }
    };
    let record_cpu_us = saturating_u64(started.elapsed().as_micros());
    if let Some(timer) = timer {
        timer.finish(&mut encoder);
    }
    assert_eq!(stage_diagnostics.gpu_color_stages, 1);
    assert_eq!(stage_diagnostics.upload_stages, 0);
    assert_eq!(stage_diagnostics.readback_stages, 0);
    let submission = context.queue.submit(std::iter::once(encoder.finish()));
    let gpu_us = timer
        .map(|timer| {
            timer
                .read_elapsed_us_after_submission(&context.device, submission)
                .context("read color-transform hardware timestamp")
        })
        .transpose()?;
    let output = runtime
        .frame_table_mut()
        .remove(output_id)
        .context("recorded transform output missing from resource table")?;
    runtime.resource_pool().release(output);
    assert_eq!(runtime.frame_table().len(), retained_input_count);
    Ok(RecordedSample { gpu_us, record_cpu_us })
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
        shader: shader_report(shader),
        fullscreen_passes: 1,
        output_texture_writes: 1,
        input_uploads_in_measured_region: 0,
        gpu_readbacks_in_measured_region: 0,
    }
}

fn build_transform_report(
    case: &TransformCase,
    cold_record_cpu_us: u64,
    samples: ViewSamples,
    shader: &OcioGpuShaderPlan,
) -> TransformReport {
    TransformReport {
        mode: case.mode,
        operation_class: case.operation_class,
        source_identity: format!("{:?}", case.src),
        destination_identity: format!("{:?}", case.dst),
        cold_record_cpu_us,
        warm_record_cpu_us: quantiles(samples.record_cpu_us),
        warm_gpu_us: quantiles(samples.gpu_us),
        shader: shader_report(shader),
        fullscreen_passes: 1,
        output_texture_writes: 1,
        input_uploads_in_measured_region: 0,
        gpu_readbacks_in_measured_region: 0,
    }
}

fn shader_report(shader: &OcioGpuShaderPlan) -> ShaderReport {
    ShaderReport {
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

fn monotonic_delta(after: u64, before: u64) -> u64 {
    after
        .checked_sub(before)
        .expect("runtime diagnostic counters must remain monotonic")
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

    #[test]
    fn runtime_cache_delta_preserves_each_warm_path_counter() {
        let before = RuntimeCacheActivity {
            shader_hits: 10,
            shader_misses: 3,
            static_pipeline_hits: 8,
            backend_object_hits: 7,
            wrapper_bind_group_creations: 3,
            wrapper_bind_group_cache_hits: 6,
            texture_pool_hits: 5,
            texture_pool_misses: 2,
            texture_pool_releases: 4,
            ..RuntimeCacheActivity::default()
        };
        let after = RuntimeCacheActivity {
            shader_hits: 19,
            shader_misses: 3,
            static_pipeline_hits: 17,
            backend_object_hits: 16,
            wrapper_bind_group_creations: 3,
            wrapper_bind_group_cache_hits: 15,
            texture_pool_hits: 14,
            texture_pool_misses: 2,
            texture_pool_releases: 13,
            ..RuntimeCacheActivity::default()
        };

        assert_eq!(
            after.delta_since(before),
            RuntimeCacheActivity {
                shader_hits: 9,
                static_pipeline_hits: 9,
                backend_object_hits: 9,
                wrapper_bind_group_cache_hits: 9,
                texture_pool_hits: 9,
                texture_pool_releases: 9,
                ..RuntimeCacheActivity::default()
            }
        );
    }

    #[test]
    fn warm_path_gate_requires_reuse_for_every_measured_view() {
        let activity = RuntimeCacheActivity {
            shader_hits: 9,
            static_pipeline_hits: 9,
            backend_object_hits: 9,
            wrapper_bind_group_cache_hits: 9,
            texture_pool_hits: 9,
            texture_pool_releases: 9,
            ..RuntimeCacheActivity::default()
        };

        assert!(WarmPathReuseGate::evaluate(activity, 9).passed);
        assert!(!WarmPathReuseGate::evaluate(activity, 10).passed);
        assert!(
            !WarmPathReuseGate::evaluate(
                RuntimeCacheActivity { wrapper_bind_group_creations: 1, ..activity },
                9,
            )
            .passed
        );
    }

    #[test]
    fn transform_warm_path_gate_rejects_per_sample_allocations() {
        let activity = RuntimeCacheActivity {
            shader_hits: 12,
            static_pipeline_hits: 12,
            backend_object_hits: 12,
            wrapper_bind_group_cache_hits: 12,
            texture_pool_hits: 12,
            texture_pool_releases: 12,
            ..RuntimeCacheActivity::default()
        };

        assert!(TransformWarmPathReuseGate::evaluate(activity, 12).passed);
        assert!(
            !TransformWarmPathReuseGate::evaluate(
                RuntimeCacheActivity { texture_pool_misses: 1, ..activity },
                12,
            )
            .passed
        );
        assert!(!TransformWarmPathReuseGate::evaluate(activity, 13).passed);
    }
}
