#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::sync::Arc;

use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, BlendMode, Color, ColorEngine, ColorSpace,
    WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{identity_compiled_effect_graph, lower_effect_graph_to_gpu_plan};
use mondrian_media::{
    probe_media_info, ResidentEncodeBitDepth, ResidentEncodeCodec, ResidentEncodeColorimetry,
    ResidentVideoEncoderConfig,
};
use mondrian_renderer::{
    color::{
        GpuColorBackendContext, GpuColorExecutionSession, GpuProgramInput, ProgramOutputBoundary,
        ProgramOutputModule,
    },
    ColorFrameResidency, CpuColorFrame, GpuColorFrameTextureFormat, GpuContext,
    GpuVisualFrameElement, GpuVisualFrameExecutor, GpuVisualFrameRequest, GpuVisualFrameSource,
    GpuVisualSourceLayer, RenderColorTransformGpuOptions, RenderCpuColorExecutionSession,
    RenderGpuOutputExecutionResourceGrant, ResidentEncodeAdapterContract,
};

#[cfg(target_os = "linux")]
use mondrian_media::CudaResidentHevcEncoderSession as NativeEncoder;
#[cfg(target_os = "macos")]
use mondrian_media::VideoToolboxResidentVideoEncoderSession as NativeEncoder;
#[cfg(target_os = "macos")]
use mondrian_renderer::MetalResidentEncodeAdapter as NativeAdapter;
#[cfg(target_os = "linux")]
use mondrian_renderer::VulkanCudaResidentEncodeAdapter as NativeAdapter;

#[tokio::test]
#[ignore = "physical resident encode qualification; requires exact native GPU/codec device"]
async fn production_color_output_stays_gpu_resident_through_hevc() {
    for (signal, bit_depth, colorimetry) in [
        (
            ColorSpace::Rec709,
            ResidentEncodeBitDepth::Eight,
            ResidentEncodeColorimetry::Rec709,
        ),
        (
            ColorSpace::Rec709,
            ResidentEncodeBitDepth::Ten,
            ResidentEncodeColorimetry::Rec709,
        ),
        (
            ColorSpace::Rec2100Pq,
            ResidentEncodeBitDepth::Ten,
            ResidentEncodeColorimetry::Rec2100Pq,
        ),
        (
            ColorSpace::Rec2100Hlg,
            ResidentEncodeBitDepth::Ten,
            ResidentEncodeColorimetry::Rec2100Hlg,
        ),
    ] {
        for spatial_pattern in [false, true] {
            compare_production_output(
                ResidentEncodeCodec::Hevc,
                signal,
                bit_depth,
                colorimetry,
                spatial_pattern,
                18,
            )
            .await;
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "physical H.264 resident encode color qualification; real Metal and VideoToolbox"]
async fn production_color_output_stays_gpu_resident_through_h264() {
    for quantizer in [18, 23] {
        for spatial_pattern in [false, true] {
            compare_production_output(
                ResidentEncodeCodec::H264High,
                ColorSpace::Rec709,
                ResidentEncodeBitDepth::Eight,
                ResidentEncodeColorimetry::Rec709,
                spatial_pattern,
                quantizer,
            )
            .await;
        }
    }
}

async fn compare_production_output(
    codec: ResidentEncodeCodec,
    signal: ColorSpace,
    bit_depth: ResidentEncodeBitDepth,
    colorimetry: ResidentEncodeColorimetry,
    spatial_pattern: bool,
    quantizer: u8,
) {
    // The spatial case deliberately has a partial packed word and padded rows.
    let width: u32 = if spatial_pattern { 322 } else { 320 };
    const HEIGHT: u32 = 180;

    ensure_mondrian_default_ocio_loaded().expect("Mondrian OCIO configuration");
    let context = GpuContext::new().await.expect("physical GPU");
    let adapter_info = context.adapter.get_info();
    #[cfg(target_os = "linux")]
    {
        assert_eq!(adapter_info.backend, wgpu::Backend::Vulkan);
        assert_eq!(adapter_info.vendor, 0x10de);
    }
    #[cfg(target_os = "macos")]
    assert_eq!(adapter_info.backend, wgpu::Backend::Metal);

    let contract = ResidentEncodeAdapterContract {
        width,
        height: HEIGHT,
        frame_rate_num: 30_000,
        frame_rate_den: 1_001,
        bit_depth,
        colorimetry,
        full_range: false,
        chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
        max_frames_in_flight: 3,
    };
    let mut adapter = NativeAdapter::new(&context.device, &context.queue, contract)
        .expect("same-device native adapter");
    let directory = tempfile::tempdir().expect("temporary artifact directory");
    let artifact = directory.path().join(if cfg!(target_os = "macos") {
        "resident-video.ts"
    } else {
        "resident-video.mkv"
    });
    let config = ResidentVideoEncoderConfig {
        codec,
        output_path: artifact.clone(),
        width,
        height: HEIGHT,
        frame_rate_num: 30_000,
        frame_rate_den: 1_001,
        sample_aspect_ratio_num: 1,
        sample_aspect_ratio_den: 1,
        bit_depth,
        colorimetry,
        full_range: false,
        chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
        keyframe_interval_frames: 30,
        max_b_frames: 0,
        quantizer,
        surface_pool_size: 3,
    };
    let mut encoder = NativeEncoder::open(&adapter.encoder_device_root(), config.clone())
        .expect("same-device native codec session");

    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        sampling: mondrian_core::ImageSampling::UNIT_SQUARE,
        alpha: mondrian_core::ColorFrameAlpha::StraightCoverage,
        width,
        height: HEIGHT,
        color_space: WorkingColorSpace::LinearRec709,
        data: (0..width * HEIGHT)
            .map(|index| {
                if !spatial_pattern {
                    return [0.18, 0.04, 0.72, 1.0];
                }
                let x = (index % width) as f32 / (width - 1) as f32;
                let y = (index / width) as f32 / (HEIGHT - 1) as f32;
                [0.02 + 0.70 * x, 0.04 + 0.60 * y, 0.03 + 0.25 * (x + y), 1.0]
            })
            .collect(),
    });
    let boundary = ProgramOutputBoundary::export(signal, false, ColorEngine::mondrian_standard());
    let expected = ProgramOutputModule::execute_cpu_float(
        &frame,
        &boundary,
        &mut RenderCpuColorExecutionSession::default(),
    )
    .expect("CPU reference for the same Program Output contract");
    let identity = Arc::new(
        lower_effect_graph_to_gpu_plan(
            &identity_compiled_effect_graph().expect("identity Effect graph"),
        )
        .expect("identity GPU plan"),
    );
    let elements = [
        GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
            source: GpuVisualFrameSource::Solid(Color::BLACK),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Arc::clone(&identity),
            frame_seed: 0,
        })),
        GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
            source: GpuVisualFrameSource::Working(Arc::new(frame.clone())),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: identity,
            frame_seed: 0,
        })),
    ];
    let mut session = GpuColorExecutionSession::new().expect("production color session");
    let visual = GpuVisualFrameExecutor::new(&context.device).expect("production GPU compositor");
    let mut commands = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("resident-encode-production-color"),
    });
    let composite = visual
        .record(
            &mut session,
            &context.device,
            &context.queue,
            &mut commands,
            None,
            GpuVisualFrameRequest {
                width,
                height: HEIGHT,
                working_color_space: WorkingColorSpace::LinearRec709,
                color_engine: ColorEngine::mondrian_standard(),
                elements: &elements,
            },
        )
        .expect("production opaque GPU composite");
    assert_eq!(
        composite.output.descriptor().alpha,
        mondrian_renderer::ColorFrameAlpha::Opaque
    );
    let record = session
        .record_program_output(
            &boundary,
            GpuProgramInput::Gpu(&composite.output),
            if bit_depth == ResidentEncodeBitDepth::Eight {
                GpuColorFrameTextureFormat::Rgba8Unorm
            } else {
                GpuColorFrameTextureFormat::Rgba32Float
            },
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Gpu,
                ..RenderColorTransformGpuOptions::default()
            },
            RenderGpuOutputExecutionResourceGrant::default(),
            GpuColorBackendContext {
                device: &context.device,
                queue: &context.queue,
                encoder: &mut commands,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            },
        )
        .expect("production Program Output recording");
    assert_eq!(record.stage_diagnostics().readback_stages, 0);
    let output = record.output().clone();
    context.queue.submit([commands.finish()]);
    let source = session
        .take_resident_encoder_input(&output)
        .expect("move-only resident Program Output");
    let destination = encoder.acquire_input_frame().expect("codec-owned surface");
    let ready = adapter.process(source, destination).expect("native resident conversion/copy");
    encoder.submit_input_frame(ready, 0).expect("native frame submission");
    encoder.finish().expect("native codec/mux finalization");

    let renderer = adapter.diagnostics();
    assert_eq!(renderer.color_conversion_submissions, 1);
    assert_eq!(
        renderer.device_to_device_copies,
        if cfg!(target_os = "macos") { 2 } else { 1 }
    );
    #[cfg(target_os = "macos")]
    assert!(encoder.diagnostics().hardware_observed && encoder.diagnostics().consuming_shutdown);
    assert_eq!(renderer.cpu_pixel_readbacks, 0);
    assert_eq!(renderer.rawvideo_pipe_bytes, 0);
    assert_eq!(renderer.cpu_pixel_uploads, 0);
    let media = encoder.diagnostics();
    assert_eq!(media.surfaces_acquired, 1);
    assert_eq!(media.frames_submitted, 1);
    assert!(media.packets_written >= 1);
    assert_eq!(media.cpu_pixel_readbacks, 0);
    assert_eq!(media.rawvideo_pipe_bytes, 0);
    assert_eq!(media.cpu_pixel_uploads, 0);
    println!(
        "resident_color_case={codec:?}/{signal:?}/{bit_depth:?}; hardware observed and owner shutdown proven"
    );
    let probe = probe_media_info(&artifact).expect("independent FFmpeg artifact probe");
    assert!(probe.has_video);
    assert_eq!(probe.primary_video().expect("video stream").width, width);
    assert_eq!(probe.primary_video().expect("video stream").height, HEIGHT);
    let mut chroma_probe = mondrian_media::ffprobe_command().expect("qualified ffprobe command");
    let chroma_probe = chroma_probe
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=chroma_location",
            "-of",
            "json",
        ])
        .arg(&artifact)
        .output()
        .expect("probe authored chroma location");
    assert!(chroma_probe.status.success());
    let chroma: serde_json::Value =
        serde_json::from_slice(&chroma_probe.stdout).expect("structured chroma evidence");
    let streams = chroma["streams"].as_array().expect("stream inventory");
    assert_eq!(streams.len(), 1, "one coded video stream");
    assert_eq!(
        streams[0]["chroma_location"], "left",
        "encoded signal matches shared sampling"
    );
    let mut decode = mondrian_media::ffmpeg_command().expect("qualified FFmpeg command");
    let decoded = decode
        .args(["-v", "error", "-i"])
        .arg(&artifact)
        .args([
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ])
        .output()
        .expect("decode resident artifact");
    assert!(
        decoded.status.success(),
        "artifact decode failed: {}",
        String::from_utf8_lossy(&decoded.stderr)
    );
    assert_eq!(decoded.stdout.len(), (width * HEIGHT * 3) as usize);
    let expected_pixels = expected
        .frame
        .rgba_f32()
        .data
        .iter()
        .map(|pixel| {
            [pixel[0], pixel[1], pixel[2]]
                .map(|value| (value.clamp(0.0, 1.0) * 255.0).round() as u8)
        })
        .collect::<Vec<_>>();
    let mut actual_sums = [0_u64; 3];
    let mut expected_sums = [0_u64; 3];
    let mut maximum_pixel_delta = 0_u8;
    for (index, (actual, expected)) in
        decoded.stdout.chunks_exact(3).zip(&expected_pixels).enumerate()
    {
        for channel in 0..3 {
            actual_sums[channel] += u64::from(actual[channel]);
            expected_sums[channel] += u64::from(expected[channel]);
            let delta = actual[channel].abs_diff(expected[channel]);
            maximum_pixel_delta = maximum_pixel_delta.max(delta);
            if spatial_pattern {
                assert!(delta <= 12, "{signal:?}/{bit_depth:?}: pixel ({},{}) channel {channel}: decoded {} versus independent CPU {}, delta {delta}", index % width as usize, index / width as usize, actual[channel], expected[channel]);
            }
        }
    }
    for channel in 0..3 {
        let actual = actual_sums[channel] / u64::from(width * HEIGHT);
        let expected = expected_sums[channel] / u64::from(width * HEIGHT);
        assert!(actual.abs_diff(expected) <= 12, "{signal:?}/{bit_depth:?}: channel {channel} average {actual} versus independent CPU {expected}");
    }
    println!("resident_pixel_evidence={codec:?}/QP{quantizer}/{signal:?}/{bit_depth:?}; spatial_pattern={spatial_pattern}; width={width}; height={HEIGHT}; max_rgb8_delta={maximum_pixel_delta}");
    #[cfg(target_os = "macos")]
    {
        let mut exhausted = NativeEncoder::open(
            &adapter.encoder_device_root(),
            ResidentVideoEncoderConfig {
                codec,
                output_path: directory.path().join("rejected-pool.ts"),
                ..config
            },
        )
        .expect("fresh native pool owner");
        let inputs = (0..3)
            .map(|_| exhausted.acquire_input_frame().expect("bounded pool slot"))
            .collect::<Vec<_>>();
        assert!(
            exhausted.acquire_input_frame().is_err(),
            "pool must reject allocation past its grant"
        );
        exhausted.shutdown().expect("error path consumes actual native worker");
        assert!(exhausted.diagnostics().consuming_shutdown);
        assert!(
            !exhausted.diagnostics().hardware_observed,
            "opening a session is not observed hardware output"
        );
        // Escaped +1 pixel buffers remain valid through codec invalidation;
        // they carry no right to submit to a replacement owner.
        assert_eq!(inputs[0].surface().width, width);
        drop(inputs);
        println!("resident_pool_error_case={codec:?}/{signal:?}/{bit_depth:?}; allocation rejected and native owner joined");
    }
}
