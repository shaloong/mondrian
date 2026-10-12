use super::*;
use crate::{
    ColorFrameAlpha, GpuColorFrameReadback, GpuColorFrameReadbackPlan, GpuContext,
    TimelineSolidColorLayer,
};
use mondrian_core::display_calibration::{
    IccProfileFingerprint, DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE,
};
use mondrian_core::{
    ColorEngine, ColorSpace, ProgramScopesTap, SignalComplianceContract, SignalMonitoringSettings,
    WaveformMode,
};
use mondrian_effects::{
    compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectGraphBuilderState,
};

fn calibration(space: ColorSpace, nonlinear: bool) -> Arc<DisplayCalibrationLut3d> {
    let edge = DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE;
    let mut samples = Vec::new();
    for b in 0..edge {
        for g in 0..edge {
            for r in 0..edge {
                for v in [r, g, b] {
                    let v = f32::from(v) / f32::from(edge - 1);
                    samples.push(if nonlinear { v * v } else { v });
                }
                samples.push(1.0);
            }
        }
    }
    Arc::new(
        DisplayCalibrationLut3d::from_rgba32f_samples(
            space,
            IccProfileFingerprint::from_bytes(b"ICC-presentation-matte-test"),
            edge,
            samples,
        )
        .unwrap(),
    )
}

fn readback(
    context: &GpuContext,
    encoder: &mut wgpu::CommandEncoder,
    resource: &crate::GpuColorFrameResource<crate::GpuColorFrameWgpuResource>,
) -> (GpuColorFrameReadbackPlan, wgpu::Buffer) {
    let plan = GpuColorFrameReadbackPlan::encoded_rgba16float(resource.handle().clone()).unwrap();
    let buffer =
        GpuColorFrameReadback::record_copy(&context.device, encoder, &plan, resource).unwrap();
    (plan, buffer)
}

fn map(context: &GpuContext, copy: (GpuColorFrameReadbackPlan, wgpu::Buffer)) -> Vec<f32> {
    let (plan, buffer) = copy;
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).unwrap();
    });
    context
        .device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .unwrap();
    rx.recv().unwrap().unwrap();
    let bytes = buffer.slice(..).get_mapped_range().unwrap();
    let pixels = plan.unpack_mapped_rgba16float(&bytes).unwrap();
    drop(bytes);
    buffer.unmap();
    pixels
}

fn record(
    context: &GpuContext,
    runtime: &mut ViewerGpuExecutionRuntime,
    alpha: f32,
    lut: Arc<DisplayCalibrationLut3d>,
    backdrop: crate::GpuDisplayCalibrationBackdrop,
    monitoring: bool,
) -> (Vec<f32>, Vec<f32>) {
    let graph = compile_reference_render_graph(EffectGraphBuilderState::new().finish()).unwrap();
    let plan = Arc::new(lower_effect_graph_to_gpu_plan(&graph).unwrap());
    let layers = [ViewerGpuExecutionLayer::Source(Box::new(
        crate::ViewerGpuSourceLayer::SolidColor {
            layer: TimelineSolidColorLayer {
                color: Color { r: 1.0, g: 1.0, b: 1.0, a: alpha },
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_graph: graph,
                frame_seed: 0,
            },
            effect_plan: plan,
        },
    ))];
    let space = lut.source_color_space();
    let boundary =
        RenderOutputColorBoundary::display(space, false, ColorEngine::mondrian_standard());
    let monitor =
        RenderMonitorAdaptation::new(space, space, ColorEngine::mondrian_standard()).unwrap();
    let mut encoder = context.device.create_command_encoder(&Default::default());
    let mut recorded = runtime
        .record(
            &context.device,
            &context.queue,
            &mut encoder,
            ViewerGpuExecutionRequest {
                sequence_id: SequenceId::new(),
                timeline_frame: 0,
                width: 4,
                height: 2,
                working_color_space: WorkingColorSpace::LinearRec709,
                layers: &layers,
                heterogeneous_inputs: Vec::new(),
                program_output_boundary: &boundary,
                monitor_adaptation: &monitor,
                source_rect: ViewerSourceRect::FULL,
                output_width: 4,
                output_height: 2,
                output_precision: ViewerGpuOutputPrecision::EncodedFloat16,
                display_calibration: Some(ViewerGpuDisplayCalibration { lut, backdrop }),
                program_scopes: Some(
                    crate::GpuProgramScopesRequest::new(space, WaveformMode::Luma, 256, 64)
                        .unwrap(),
                ),
                signal_monitoring: monitoring.then(|| {
                    crate::GpuSignalMonitorRequest::new(
                        SignalComplianceContract::normalized_rgb(space).unwrap(),
                        SignalMonitoringSettings { false_color: true, ..Default::default() },
                        ProgramScopesTap::ProgramOutput,
                    )
                    .unwrap()
                }),
            },
        )
        .unwrap();
    assert_eq!(recorded.output.descriptor().alpha, ColorFrameAlpha::Opaque);
    assert_eq!(recorded.stage_diagnostics.readback_stages, 0);
    assert_eq!(recorded.stage_diagnostics.upload_stages, 0);
    let program = readback(
        context,
        &mut encoder,
        runtime
            .color_output
            .runtime()
            .expect("local recording cache")
            .frame_table()
            .get(&recorded.program_output)
            .unwrap(),
    );
    let device = readback(
        context,
        &mut encoder,
        runtime.display_calibration.output(&recorded.output).unwrap(),
    );
    let submission = context.queue.submit([encoder.finish()]);
    let _submitted = recorded.assert_adapter_submission(submission);
    let program = map(context, program);
    let device = map(context, device);
    for p in program.chunks_exact(4) {
        assert!((p[3] - alpha).abs() < 0.0001);
    }
    (program, device)
}

fn finish(context: &GpuContext, runtime: ViewerGpuExecutionRuntime) {
    context
        .device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .unwrap();
    let mut retirement = runtime.into_retirement();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(receipt) = retirement.poll().unwrap() {
            assert!(receipt.is_healthy());
            eprintln!("ICC Viewer consuming shutdown: {receipt:?}");
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
}

async fn context() -> Arc<GpuContext> {
    let context = GpuContext::new().await.expect("physical GPU required");
    let info = context.adapter.get_info();
    eprintln!("ICC Viewer adapter: {info:?}");
    assert_ne!(info.device_type, wgpu::DeviceType::Cpu);
    if cfg!(target_os = "macos") {
        assert_eq!(info.backend, wgpu::Backend::Metal);
    }
    context
}

#[tokio::test]
#[ignore = "requires a physical GPU"]
async fn icc_mattes_in_linear_light_before_nonlinear_device_calibration() {
    let context = context().await;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue).unwrap();
    let lut = calibration(ColorSpace::Srgb, true);
    for alpha in [0.0, 0.5, 1.0] {
        let (_, pixels) = record(
            &context,
            &mut runtime,
            alpha,
            lut.clone(),
            crate::GpuDisplayCalibrationBackdrop::solid(Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 }),
            false,
        );
        let code = if alpha == 0.0 {
            0.0
        } else if alpha == 1.0 {
            1.0
        } else {
            0.735_356_9
        };
        let expected = lut.sample_trilinear([code, code, 1.0]);
        for pixel in pixels.chunks_exact(4) {
            for channel in 0..3 {
                assert!(
                    (pixel[channel] - expected[channel]).abs() < 0.002,
                    "alpha={alpha} pixel={pixel:?} expected={expected:?}"
                );
            }
            assert_eq!(pixel[3], 1.0);
        }
        eprintln!("nonlinear ICC: alpha={alpha}, pixel={:?}", &pixels[..4]);
    }
    finish(&context, runtime);
}

fn linear(space: ColorSpace, code: f32) -> f64 {
    let v = f64::from(code);
    match space {
        ColorSpace::Srgb | ColorSpace::DisplayP3 => {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        }
        ColorSpace::Rec709 | ColorSpace::Rec2020 => {
            if v < 0.081 {
                v / 4.5
            } else {
                ((v + 0.099) / 1.099).powf(1.0 / 0.45)
            }
        }
        ColorSpace::Rec2100Pq => mondrian_core::bt2100_pq_to_display_linear_rgb([v; 3])
            .unwrap()
            .components_nits()[0],
        ColorSpace::Rec2100Hlg => mondrian_core::bt2100_hlg_1000_nit_to_display_linear_rgb([v; 3])
            .unwrap()
            .components_nits()[0],
        _ => panic!("unsupported fixture"),
    }
}

#[tokio::test]
#[ignore = "requires a physical GPU"]
async fn icc_matte_preserves_sdr_and_hdr_luminance_and_program_alpha() {
    let context = context().await;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue).unwrap();
    for space in [
        ColorSpace::Srgb,
        ColorSpace::DisplayP3,
        ColorSpace::Rec709,
        ColorSpace::Rec2020,
        ColorSpace::Rec2100Pq,
        ColorSpace::Rec2100Hlg,
    ] {
        for alpha in [0.0, 0.5, 1.0] {
            let (program, pixels) = record(
                &context,
                &mut runtime,
                alpha,
                calibration(space, false),
                crate::GpuDisplayCalibrationBackdrop::solid(Color::BLACK),
                false,
            );
            for (p, d) in program.chunks_exact(4).zip(pixels.chunks_exact(4)) {
                for c in 0..3 {
                    let expected = linear(space, p[c]) * f64::from(alpha);
                    let actual = linear(space, d[c]);
                    assert!((actual - expected).abs() <= expected.abs() * 0.007 + 0.0002,
                        "{space:?} alpha={alpha}, source={p:?}, matted={d:?}, linear={actual}, expected={expected}");
                }
                assert_eq!(d[3], 1.0);
            }
            eprintln!(
                "{space:?} alpha={alpha}: program={:?}, ICC={:?}",
                &program[..4],
                &pixels[..4]
            );
        }
    }
    // Monitoring has an independent ID allocator. Its exact allocation must
    // transfer without colliding with the color-runtime resources.
    let (_, pixels) = record(
        &context,
        &mut runtime,
        0.5,
        calibration(ColorSpace::Srgb, false),
        crate::GpuDisplayCalibrationBackdrop::solid(Color::BLACK),
        true,
    );
    assert!(pixels.iter().all(|v| v.is_finite()));
    finish(&context, runtime);
}

#[tokio::test]
#[ignore = "requires a physical GPU"]
async fn icc_checker_tracks_logical_crop_geometry_and_background_changes() {
    let context = context().await;
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue).unwrap();
    let mut backdrop = crate::GpuDisplayCalibrationBackdrop {
        canvas: Color::BLACK,
        dark: Color::TRANSPARENT,
        light: Color::WHITE,
        tile_size: 16.0,
        offset: [8.0, 0.0],
        pixel_step: [8.0, 16.0],
    };
    for (offset, first_row) in [(8.0, [1.0, 0.0, 0.0, 1.0]), (16.0, [0.0, 0.0, 1.0, 1.0])] {
        backdrop.offset[0] = offset;
        let (_, pixels) = record(
            &context,
            &mut runtime,
            0.0,
            calibration(ColorSpace::Srgb, false),
            backdrop,
            false,
        );
        for y in 0..2 {
            for (x, v) in first_row.iter().enumerate() {
                let expected = if y == 0 { *v } else { 1.0 - *v };
                let pixel = &pixels[(y * 4 + x) * 4..(y * 4 + x + 1) * 4];
                assert!(
                    (pixel[0] - expected).abs() < 0.001,
                    "offset={offset}, x={x}, y={y}, pixel={pixel:?}"
                );
            }
        }
    }
    let (_, pixels) = record(
        &context,
        &mut runtime,
        0.0,
        calibration(ColorSpace::Srgb, false),
        crate::GpuDisplayCalibrationBackdrop::solid(Color::BLACK),
        false,
    );
    assert!(pixels.chunks_exact(4).all(|p| p == [0.0, 0.0, 0.0, 1.0]));
    finish(&context, runtime);
}
