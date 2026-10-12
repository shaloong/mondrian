use super::*;
use crate::{CpuEncodedColorFrame, CpuSourceColorFrame, GpuContext, RenderInputTransform};
use mondrian_core::{ensure_mondrian_default_ocio_loaded, ColorEngine, ColorSpace, WaveformMode};
use mondrian_effects::{
    compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
};

async fn context() -> Option<Arc<GpuContext>> {
    ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
    let policy =
        crate::GpuColorQualificationExecutionPolicy::from_environment().expect("GPU policy");
    let context = policy
        .admit_adapter("viewer-canonical-scope", GpuContext::new().await)
        .expect("required real GPU");
    if let Some(context) = &context {
        eprintln!("canonical Scope adapter: {:?}", context.adapter.get_info());
    } else {
        eprintln!("skipping canonical Scope: no GPU adapter");
    }
    context
}

fn gradient_layer() -> ViewerGpuExecutionLayer {
    let pixels: Vec<u8> = (0..32)
        .flat_map(|y| (0..64).flat_map(move |x| [x * 4, y * 8, (x + y) * 2, 255]))
        .collect();
    let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish())
        .expect("identity effects");
    ViewerGpuExecutionLayer::Source(Box::new(crate::ViewerGpuSourceLayer::Media {
        frame: None,
        is_data_texture: false,
        gpu_source: Some(ViewerGpuMediaSource {
            source: Arc::new(CpuSourceColorFrame::from(
                CpuEncodedColorFrame::source_rgba8(64, 32, ColorSpace::Rec709, pixels),
            )),
            input_transform: RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec709,
                false,
                ColorEngine::mondrian_standard(),
            ),
            decoder_residency: DecodedFrameResidency::CpuRgba,
            decoder_handle_kind: None,
            decoded_surface_format: mondrian_media::DecodedVideoSurfaceFormat::Rgba8,
            decoded_video_sampling: mondrian_media::DecodedVideoSampling::default(),
        }),
        native_source: None,
        cpu_yuv_source: None,
        heterogeneous_input: None,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_plan: Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("GPU identity")),
        frame_seed: 0,
    }))
}

#[tokio::test]
async fn scope_backpressure_discards_failed_candidate_textures_and_preserves_leased_counts() {
    let Some(context) = context().await else {
        return;
    };
    let boundary = RenderOutputColorBoundary::display(
        ColorSpace::Rec709,
        false,
        ColorEngine::mondrian_standard(),
    );
    let adaptation = RenderMonitorAdaptation::new(
        ColorSpace::Rec709,
        ColorSpace::Rec709,
        ColorEngine::mondrian_standard(),
    )
    .expect("matching monitor");
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
            .expect("production Viewer");
    let layers = [gradient_layer()];
    let sequence_id = SequenceId::new();
    let request = |timeline_frame| ViewerGpuExecutionRequest {
        sequence_id,
        timeline_frame,
        width: 64,
        height: 32,
        working_color_space: WorkingColorSpace::LinearRec709,
        layers: &layers,
        heterogeneous_inputs: Vec::new(),
        program_output_boundary: &boundary,
        monitor_adaptation: &adaptation,
        source_rect: ViewerSourceRect::FULL,
        output_width: 16,
        output_height: 8,
        output_precision: ViewerGpuOutputPrecision::Encoded8,
        display_calibration: None,
        program_scopes: Some(
            GpuProgramScopesRequest::new(ColorSpace::Rec709, WaveformMode::Luma, 256, 512)
                .expect("exact scope contract"),
        ),
        signal_monitoring: None,
    };
    let mut retained = Vec::new();
    for frame in 0..6 {
        let mut encoder = context.device.create_command_encoder(&Default::default());
        let mut record = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                request(frame),
            )
            .expect("retain all original scope resource sets");
        let submission = context.queue.submit([encoder.finish()]);
        let _submitted = record.assert_adapter_submission(submission.clone());
        context
            .device
            .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
            .expect("actual original GPU completion");
        retained.push(record.program_scopes.take().expect("leased scopes"));
    }
    let original_counts = read_counts(&context, &retained[0]);
    let original_uniforms = runtime.compositor_uniform_arena_diagnostics();
    let mut partial_frame_entries = Vec::new();
    let mut abandoned_pool_releases = None;
    for frame in 6..10 {
        let mut encoder = context.device.create_command_encoder(&Default::default());
        let error = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                request(frame),
            )
            .err()
            .expect("retained original capacity must reject this candidate");
        assert!(
            matches!(error, ViewerGpuExecutionError::ProgramScopes(error)
            if matches!(*error, GpuProgramScopesError::RetainedFrameCapacity))
        );
        // A failed caller must abandon its partial commands, not submit them.
        drop(encoder);
        partial_frame_entries.push(runtime.color_output_diagnostics().frame_table_entries);
        let releases = runtime.resource_pool_diagnostics().releases;
        if let Some(previous) = abandoned_pool_releases {
            assert_eq!(
                releases, previous,
                "failed textures must not enter the reusable pool"
            );
        }
        abandoned_pool_releases = Some(releases);
        assert_eq!(
            runtime.compositor_uniform_arena_diagnostics().high_watermark_slots,
            original_uniforms.high_watermark_slots,
            "failed frames must not accumulate uniform slots"
        );
    }
    eprintln!("failed candidate texture entries: {partial_frame_entries:?}");
    assert_eq!(partial_frame_entries, [0; 4]);
    assert_eq!(runtime.program_scopes_diagnostics().frames_recorded, 6);
    assert_eq!(read_counts(&context, &retained[0]), original_counts);
    drop(retained);
    let mut encoder = context.device.create_command_encoder(&Default::default());
    let mut record = runtime
        .record(&context.device, &context.queue, &mut encoder, request(10))
        .expect("the original grant recovers after its actual leases end");
    let submission = context.queue.submit([encoder.finish()]);
    let _submitted = record.assert_adapter_submission(submission.clone());
    context
        .device
        .poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: None })
        .expect("actual retry GPU completion");
    assert_eq!(
        read_counts(&context, record.program_scopes.as_ref().unwrap()),
        original_counts
    );
    assert_eq!(runtime.program_scopes_diagnostics().buffer_allocations, 6);
    assert_eq!(runtime.program_scopes_diagnostics().pipeline_creations, 1);
}

fn read_counts(context: &GpuContext, scopes: &GpuProgramScopesRecord) -> Vec<u32> {
    let size = u64::from(scopes.buffer_layout.counter_count) * 4;
    let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("leased-scope-counts"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = context.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(scopes.counts(), 0, &buffer, 0, size);
    context.queue.submit([encoder.finish()]);
    let (send, recv) = std::sync::mpsc::channel();
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        send.send(result).unwrap()
    });
    context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("count readback completion");
    recv.recv().unwrap().expect("mapped original counts");
    bytemuck::cast_slice::<u8, u32>(&slice.get_mapped_range().unwrap()).to_vec()
}

fn counts(
    context: &GpuContext,
    canvas: (u32, u32),
    viewport: (u32, u32),
    crop: ViewerSourceRect,
    monitor: ColorSpace,
    tap: ProgramScopesTap,
    precision: ViewerGpuOutputPrecision,
) -> Vec<u32> {
    let boundary = RenderOutputColorBoundary::display(
        ColorSpace::Rec709,
        false,
        ColorEngine::mondrian_standard(),
    );
    let adaptation = RenderMonitorAdaptation::new(
        ColorSpace::Rec709,
        monitor,
        ColorEngine::mondrian_standard(),
    )
    .expect("monitor adaptation");
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
            .expect("production Viewer");
    let mut encoder = context.device.create_command_encoder(&Default::default());
    let mut record = runtime
        .record(
            &context.device,
            &context.queue,
            &mut encoder,
            ViewerGpuExecutionRequest {
                sequence_id: SequenceId::new(),
                timeline_frame: 0,
                width: canvas.0,
                height: canvas.1,
                working_color_space: WorkingColorSpace::LinearRec709,
                layers: &[gradient_layer()],
                heterogeneous_inputs: Vec::new(),
                program_output_boundary: &boundary,
                monitor_adaptation: &adaptation,
                source_rect: crop,
                output_width: viewport.0,
                output_height: viewport.1,
                output_precision: precision,
                display_calibration: None,
                program_scopes: Some(
                    GpuProgramScopesRequest::with_controls(
                        if tap == ProgramScopesTap::ProgramOutput {
                            ColorSpace::Rec709
                        } else {
                            monitor
                        },
                        WaveformMode::RgbParade,
                        mondrian_core::ProgramScopeScale::Ire,
                        tap,
                        256,
                        64,
                    )
                    .expect("typed scopes"),
                ),
                signal_monitoring: None,
            },
        )
        .expect("record Viewer");
    let estimate = runtime
        .active_working_set_diagnostics()
        .last_admitted
        .expect("admitted resources");
    let distinct = crop != ViewerSourceRect::FULL || viewport != canvas;
    let canvas_bytes = u64::from(canvas.0) * u64::from(canvas.1) * 8;
    assert_eq!(
        estimate.program_output.textures,
        if distinct { 2 } else { 1 }
    );
    assert_eq!(
        estimate.program_output.bytes,
        u64::from(viewport.0) * u64::from(viewport.1) * 8 + if distinct { canvas_bytes } else { 0 }
    );
    assert_eq!(
        estimate.monitor_adaptation.bytes,
        if monitor == ColorSpace::Rec709 {
            0
        } else {
            u64::from(viewport.0) * u64::from(viewport.1) * 8
                + if distinct && tap == ProgramScopesTap::MonitorOutput {
                    canvas_bytes
                } else {
                    0
                }
        }
    );
    let scopes = record.program_scopes.as_ref().expect("Scope products");
    let size = u64::from(scopes.buffer_layout.counter_count) * 4;
    let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("canonical-scope-test-counts"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_buffer_to_buffer(scopes.counts(), 0, &buffer, 0, size);
    let layout = scopes.buffer_layout;
    let submission = context.queue.submit([encoder.finish()]);
    let _submitted = record.assert_adapter_submission(submission);
    let slice = buffer.slice(..);
    let (send, recv) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        send.send(result).expect("map receipt");
    });
    context
        .device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .expect("GPU completion");
    recv.recv().expect("map callback").expect("map count buffer");
    let bytes = slice.get_mapped_range().expect("mapped counts");
    let result = bytemuck::cast_slice::<u8, u32>(&bytes).to_vec();
    let histogram = layout.histogram_offset as usize;
    for channel in 0..4 {
        let sum: u32 =
            result[histogram + channel * 256..histogram + (channel + 1) * 256].iter().sum();
        eprintln!("viewport={viewport:?} crop={crop:?} monitor={monitor:?} tap={tap:?} precision={precision:?} histogram plane {channel}: {sum}");
        assert_eq!(
            sum,
            canvas.0 * canvas.1,
            "Scope must include the full canonical picture"
        );
    }
    assert_eq!(result[layout.invalid_pixel_count_offset as usize], 0);
    assert_eq!(
        (
            record.output.descriptor().width,
            record.output.descriptor().height
        ),
        viewport
    );
    result
}

#[tokio::test]
async fn scopes_preserve_complete_raster_across_viewer_resize_and_crop() {
    let Some(context) = context().await else {
        return;
    };
    for tap in [
        ProgramScopesTap::ProgramOutput,
        ProgramScopesTap::MonitorOutput,
    ] {
        let baseline = counts(
            &context,
            (64, 32),
            (64, 32),
            ViewerSourceRect::FULL,
            ColorSpace::DisplayP3,
            tap,
            ViewerGpuOutputPrecision::EncodedFloat16,
        );
        for (viewport, crop) in [
            ((8, 4), ViewerSourceRect::FULL),
            (
                (8, 4),
                ViewerSourceRect { x: 0.25, y: 0.25, width: 0.5, height: 0.5 },
            ),
        ] {
            assert!(
                counts(
                    &context,
                    (64, 32),
                    viewport,
                    crop,
                    ColorSpace::DisplayP3,
                    tap,
                    ViewerGpuOutputPrecision::EncodedFloat16
                ) == baseline,
                "Viewer geometry must not change any Scope counter"
            );
        }
    }
}

#[tokio::test]
async fn program_scope_counts_do_not_depend_on_monitor_carrier_or_target() {
    let Some(context) = context().await else {
        return;
    };
    let baseline = counts(
        &context,
        (64, 32),
        (64, 32),
        ViewerSourceRect::FULL,
        ColorSpace::Rec709,
        ProgramScopesTap::ProgramOutput,
        ViewerGpuOutputPrecision::EncodedFloat16,
    );
    for monitor in [ColorSpace::Rec709, ColorSpace::DisplayP3] {
        assert!(
            counts(
                &context,
                (64, 32),
                (64, 32),
                ViewerSourceRect::FULL,
                monitor,
                ProgramScopesTap::ProgramOutput,
                ViewerGpuOutputPrecision::Encoded8
            ) == baseline,
            "Scope must measure float Program Output independent of the display carrier"
        );
    }
}

#[tokio::test]
#[ignore = "explicit full 4K canonical Scope raster validation on a real GPU"]
async fn scopes_analyze_every_4k_pixel_with_a_small_viewer() {
    let Some(context) = context().await else {
        return;
    };
    counts(
        &context,
        (3840, 2160),
        (826, 465),
        ViewerSourceRect::FULL,
        ColorSpace::Rec709,
        ProgramScopesTap::ProgramOutput,
        ViewerGpuOutputPrecision::Encoded8,
    );
}
