use anyhow::{anyhow, Context, Result};
use mondrian_core::types::{BlendMode, Color, ColorEngine};
use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, ProgramScopeScale, ProgramScopesTap, SequenceId,
    WorkingColorSpace,
};
use mondrian_effects::{
    compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
    EffectRenderOp,
};
use mondrian_renderer::profile::{
    gpu_timestamp_query_device_features, GpuTimestampQueryRing, GpuTimestampStageMarker,
    GpuTimestampToken,
};
use mondrian_renderer::{
    color::ProgramOutputBoundary, evaluate_realtime_visual_performance,
    native_video_texture_device_features, ocio_lut_filtering_device_features,
    request_adapter_with_native_video_preference, GpuColorFrameTextureFormat,
    GpuProgramScopesRequest, RealtimePerformanceExecutionPolicy, RealtimeVisualAdapterIdentity,
    RealtimeVisualFrameEvidence, RealtimeVisualPerformanceObservation, RealtimeVisualScenarioId,
    RealtimeVisualWarmPathEvidence, RenderMonitorAdaptation, TimelineSolidColorLayer,
    ViewerGpuExecutionGpuStage, ViewerGpuExecutionLayer, ViewerGpuExecutionRequest,
    ViewerGpuExecutionRuntime, ViewerGpuExecutionStageMarker, ViewerGpuOutputPrecision,
    ViewerGpuSourceLayer, ViewerSourceRect,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

const REPORT_ENV: &str = "MONDRIAN_REALTIME_VISUAL_PERF_OUTPUT";

struct GpuContext {
    _instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

struct TimestampStageBridge<'a> {
    ring: &'a mut GpuTimestampQueryRing,
    token: GpuTimestampToken,
}

impl ViewerGpuExecutionStageMarker for TimestampStageBridge<'_> {
    fn mark(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        stage: ViewerGpuExecutionGpuStage,
    ) -> Result<(), String> {
        let marker = match stage {
            ViewerGpuExecutionGpuStage::WorkingComposite => {
                GpuTimestampStageMarker::AfterWorkingComposite
            }
            ViewerGpuExecutionGpuStage::Spatial => GpuTimestampStageMarker::AfterSpatial,
            ViewerGpuExecutionGpuStage::ProgramOutputBoundary => {
                GpuTimestampStageMarker::AfterProgramOutputBoundary
            }
            ViewerGpuExecutionGpuStage::MonitorAdaptation => {
                GpuTimestampStageMarker::AfterMonitorAdaptation
            }
            ViewerGpuExecutionGpuStage::ProgramScopes => {
                GpuTimestampStageMarker::AfterProgramScopes
            }
            ViewerGpuExecutionGpuStage::SignalMonitoring => {
                GpuTimestampStageMarker::AfterSignalMonitoring
            }
        };
        self.ring
            .mark_stage(encoder, self.token, marker)
            .map_err(|error| error.to_string())
    }
}

#[tokio::test]
#[ignore = "requires a qualified high-memory timestamp-capable GPU"]
async fn realtime_visual_gpu_matrix_gate() -> Result<()> {
    ensure_mondrian_default_ocio_loaded().map_err(anyhow::Error::msg)?;
    let policy = RealtimePerformanceExecutionPolicy::from_environment()?;
    let Some(context) = create_gpu_context().await? else {
        if policy.hardware_required() {
            return Err(anyhow!(
                "sealed realtime matrix requires a timestamp-capable hardware GPU"
            ));
        }
        eprintln!("development-optional realtime matrix: no timestamp-capable GPU");
        return Ok(());
    };

    let output = std::env::var_os(REPORT_ENV).map(std::path::PathBuf::from);
    if policy.hardware_required() && output.is_none() {
        return Err(anyhow!("sealed realtime matrix requires {REPORT_ENV}"));
    }

    for scenario in RealtimeVisualScenarioId::ALL {
        let report = run_scenario(&context, scenario)?;
        let json = serde_json::to_string(&report).context("serialize realtime visual report")?;
        if let Some(path) = output.as_deref() {
            append_jsonl(path, &json)?;
        } else {
            eprintln!("{json}");
        }
        if !report.passed {
            return Err(anyhow!(
                "realtime visual scenario {:?} failed: {:?}",
                scenario,
                report.root_causes
            ));
        }
    }
    Ok(())
}

fn run_scenario(
    context: &GpuContext,
    scenario: RealtimeVisualScenarioId,
) -> Result<mondrian_renderer::RealtimeVisualPerformanceReport> {
    let workload = scenario.contract();
    let (graph, effect_plan) = build_effect_graph()?;
    let effect_plan = Arc::new(effect_plan);
    let layers = (0..workload.layer_count)
        .map(|index| {
            let opacity = if index == 0 { 1.0 } else { 0.72 };
            ViewerGpuExecutionLayer::Source(Box::new(ViewerGpuSourceLayer::SolidColor {
                layer: TimelineSolidColorLayer {
                    color: layer_color(index),
                    opacity,
                    blend_mode: BlendMode::Normal,
                    transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                    effect_graph: Arc::clone(&graph),
                    frame_seed: i64::from(index),
                },
                effect_plan: Arc::clone(&effect_plan),
            }))
        })
        .collect::<Vec<_>>();
    let boundary = ProgramOutputBoundary::display(
        workload.output_color_space,
        false,
        ColorEngine::mondrian_standard(),
    );
    let monitor = RenderMonitorAdaptation::new(
        workload.output_color_space,
        workload.output_color_space,
        ColorEngine::mondrian_standard(),
    )?;
    let scopes = GpuProgramScopesRequest::with_controls(
        workload.output_color_space,
        mondrian_core::WaveformMode::RgbParade,
        ProgramScopeScale::Nits1000,
        ProgramScopesTap::ProgramOutput,
        1_024,
        1_024,
    )?;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)?;
    runtime.reconfigure_resource_grant(workload.resource_grant);

    for frame in 0..workload.warmup_frames {
        runtime.clear_frame_resources();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-realtime-visual-warmup"),
        });
        let mut record = runtime.record(
            &context.device,
            &context.queue,
            &mut encoder,
            request(
                &workload,
                &layers,
                &boundary,
                &monitor,
                scopes,
                i64::from(frame),
            ),
        )?;
        let lease = runtime.take_presentation_output(&mut record)?;
        let submission = context.queue.submit(std::iter::once(encoder.finish()));
        drop(lease);
        context
            .device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .context("complete realtime visual warmup")?;
    }
    runtime.clear_frame_resources();

    let pool_before = runtime.resource_pool_diagnostics();
    let color_before = runtime.color_output_diagnostics();
    let scopes_before = runtime.program_scopes_diagnostics();
    let mut ring = GpuTimestampQueryRing::new(
        &context.device,
        &context.queue,
        workload.measured_frames as usize,
    )
    .ok_or_else(|| anyhow!("timestamp query ring unavailable after device admission"))?;
    let mut cpu_record_samples_us = Vec::with_capacity(workload.measured_frames as usize);
    let mut frames = RealtimeVisualFrameEvidence::default();

    for frame in 0..workload.measured_frames {
        runtime.clear_frame_resources();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-realtime-visual-measured"),
        });
        let token = ring
            .begin_frame(&context.device, &mut encoder)?
            .ok_or_else(|| anyhow!("timestamp query ring discarded a measured frame"))?;
        let started = Instant::now();
        let mut bridge = TimestampStageBridge { ring: &mut ring, token };
        let mut record = runtime.record_with_stage_marker(
            &context.device,
            &context.queue,
            &mut encoder,
            request(
                &workload,
                &layers,
                &boundary,
                &monitor,
                scopes,
                i64::from(workload.warmup_frames + frame),
            ),
            Some(&mut bridge),
        )?;
        cpu_record_samples_us.push(elapsed_us(started));
        ring.finish_frame(&mut encoder, token)?;
        let output_matches =
            record.output.texture_format() == GpuColorFrameTextureFormat::Rgba16Float;
        let lease = runtime.take_presentation_output(&mut record)?;
        let _submission = context.queue.submit(std::iter::once(encoder.finish()));
        ring.after_submit(token)?;

        frames.submitted_frames = frames.submitted_frames.saturating_add(1);
        frames.presentation_output_leases = frames.presentation_output_leases.saturating_add(1);
        frames.program_scopes_frames = frames
            .program_scopes_frames
            .saturating_add(u64::from(record.program_scopes.is_some()));
        frames.gpu_native_composites = frames
            .gpu_native_composites
            .saturating_add(record.compositing_diagnostics.gpu_native_composites);
        frames.fused_point_operations = frames
            .fused_point_operations
            .saturating_add(record.compositing_diagnostics.execution.fused_point_operations);
        frames.cpu_fallback_composites = frames
            .cpu_fallback_composites
            .saturating_add(record.compositing_diagnostics.cpu_fallback_composites);
        frames.gpu_color_stages = frames
            .gpu_color_stages
            .saturating_add(record.stage_diagnostics.gpu_color_stages);
        frames.upload_stages =
            frames.upload_stages.saturating_add(record.stage_diagnostics.upload_stages);
        frames.readback_stages =
            frames.readback_stages.saturating_add(record.stage_diagnostics.readback_stages);
        frames.gpu_blockers =
            frames.gpu_blockers.saturating_add(record.stage_diagnostics.gpu_blockers);
        frames.output_format_mismatches =
            frames.output_format_mismatches.saturating_add(u64::from(!output_matches));
        drop(lease);
    }

    let samples = ring.finish_all(&context.device)?;
    frames.discarded_timestamp_samples = ring.discarded_samples();
    let pool_after = runtime.resource_pool_diagnostics();
    let color_after = runtime.color_output_diagnostics();
    let scopes_after = runtime.program_scopes_diagnostics();
    let info = context.adapter.get_info();
    let observation = RealtimeVisualPerformanceObservation {
        workload,
        adapter: RealtimeVisualAdapterIdentity {
            name: info.name,
            vendor: info.vendor,
            device: info.device,
            backend: format!("{:?}", info.backend),
            driver: info.driver,
            driver_info: info.driver_info,
            timestamp_queries: true,
        },
        gpu_samples_us: samples.iter().map(|sample| sample.elapsed_us).collect(),
        cpu_record_samples_us,
        gpu_stage_samples: samples.iter().map(|sample| sample.stages).collect(),
        frames,
        warm_path: RealtimeVisualWarmPathEvidence {
            texture_pool_hits: pool_after.hits.saturating_sub(pool_before.hits),
            texture_pool_misses: pool_after.misses.saturating_sub(pool_before.misses),
            texture_pool_evictions: pool_after.evictions.saturating_sub(pool_before.evictions),
            shader_extractions: color_after
                .shader_cache
                .misses
                .saturating_sub(color_before.shader_cache.misses),
            static_pipeline_preparations: color_after
                .backend_prep
                .static_pipelines
                .misses
                .saturating_sub(color_before.backend_prep.static_pipelines.misses),
            backend_object_preparations: color_after
                .backend_objects
                .misses
                .saturating_sub(color_before.backend_objects.misses),
            scope_pipeline_creations: scopes_after
                .pipeline_creations
                .saturating_sub(scopes_before.pipeline_creations),
            scope_buffer_allocations: scopes_after
                .buffer_allocations
                .saturating_sub(scopes_before.buffer_allocations),
            scope_texture_allocations: scopes_after
                .texture_allocations
                .saturating_sub(scopes_before.texture_allocations),
        },
    };
    Ok(evaluate_realtime_visual_performance(observation))
}

fn request<'a>(
    workload: &mondrian_renderer::RealtimeVisualWorkload,
    layers: &'a [ViewerGpuExecutionLayer],
    boundary: &'a ProgramOutputBoundary,
    monitor: &'a RenderMonitorAdaptation,
    scopes: GpuProgramScopesRequest,
    timeline_frame: i64,
) -> ViewerGpuExecutionRequest<'a> {
    ViewerGpuExecutionRequest {
        sequence_id: SequenceId::new(),
        timeline_frame,
        width: workload.width,
        height: workload.height,
        working_color_space: WorkingColorSpace::LinearRec2020,
        layers,
        heterogeneous_inputs: Vec::new(),
        program_output_boundary: boundary,
        monitor_adaptation: monitor,
        source_rect: ViewerSourceRect::FULL,
        output_width: workload.width,
        output_height: workload.height,
        output_precision: ViewerGpuOutputPrecision::EncodedFloat16,
        display_calibration: None,
        program_scopes: Some(scopes),
        signal_monitoring: None,
    }
}

fn build_effect_graph() -> Result<(
    Arc<mondrian_effects::CompiledEffectGraph>,
    mondrian_effects::CompiledEffectGpuPlan,
)> {
    let mut graph = EffectGraphBuilderState::new();
    graph.append_unary(EffectRenderOp::ColorAdjust {
        exposure: 0.5,
        contrast: 1.08,
        saturation: 1.12,
        working_color_space: WorkingColorSpace::LinearRec2020,
    });
    graph.append_unary(EffectRenderOp::Grain { amount: 0.2 });
    let graph = compile_reference_render_graph(graph.finish())
        .ok_or_else(|| anyhow!("compile realtime visual effect graph"))?;
    let plan = lower_effect_graph_to_gpu_plan(&graph)?;
    Ok((graph, plan))
}

fn layer_color(index: u32) -> Color {
    match index % 4 {
        0 => Color { r: 0.18, g: 0.08, b: 0.03, a: 1.0 },
        1 => Color { r: 0.02, g: 0.24, b: 0.08, a: 1.0 },
        2 => Color { r: 0.04, g: 0.08, b: 0.32, a: 1.0 },
        _ => Color { r: 0.28, g: 0.04, b: 0.12, a: 1.0 },
    }
}

async fn create_gpu_context() -> Result<Option<GpuContext>> {
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
        label: Some("mondrian-realtime-visual-matrix-device"),
        required_features,
        ..wgpu::DeviceDescriptor::default()
    };
    let (device, queue) = adapter
        .request_device(&descriptor)
        .await
        .context("request realtime visual matrix device")?;
    Ok(Some(GpuContext {
        _instance: instance,
        adapter,
        device,
        queue,
    }))
}

fn elapsed_us(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
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
