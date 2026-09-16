#![cfg(target_os = "linux")]

use std::sync::Arc;

use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, BlendMode, Color, ColorEngine, ColorSpace,
    WorkingColorSpace, WorkingRgbaF32Frame,
};
use mondrian_effects::{identity_compiled_effect_graph, lower_effect_graph_to_gpu_plan};
use mondrian_media::{
    probe_media_info, CudaResidentHevcEncoderSession, ResidentEncodeBitDepth,
    ResidentEncodeColorimetry, ResidentHevcEncoderConfig,
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
    VulkanCudaResidentEncodeAdapter,
};

#[tokio::test]
#[ignore = "manual NVIDIA qualification; requires Vulkan external memory, CUDA, and NVENC"]
async fn production_color_output_stays_gpu_resident_through_nvenc() {
    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 180;

    ensure_mondrian_default_ocio_loaded().expect("Mondrian OCIO configuration");
    let context = GpuContext::new().await.expect("physical Vulkan GPU");
    let adapter_info = context.adapter.get_info();
    assert_eq!(adapter_info.backend, wgpu::Backend::Vulkan);
    assert_eq!(adapter_info.vendor, 0x10de, "NVIDIA qualification device");

    let contract = ResidentEncodeAdapterContract {
        width: WIDTH,
        height: HEIGHT,
        frame_rate_num: 30_000,
        frame_rate_den: 1_001,
        bit_depth: ResidentEncodeBitDepth::Eight,
        colorimetry: ResidentEncodeColorimetry::Rec709,
        full_range: false,
        chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
        max_frames_in_flight: 3,
    };
    let mut adapter =
        VulkanCudaResidentEncodeAdapter::new(&context.device, &context.queue, contract)
            .expect("same-device Vulkan/CUDA Adapter");
    let directory = tempfile::tempdir().expect("temporary artifact directory");
    let artifact = directory.path().join("resident-hevc.mkv");
    let mut encoder = CudaResidentHevcEncoderSession::open(
        &adapter.encoder_device_root(),
        ResidentHevcEncoderConfig {
            output_path: artifact.clone(),
            width: WIDTH,
            height: HEIGHT,
            frame_rate_num: 30_000,
            frame_rate_den: 1_001,
            sample_aspect_ratio_num: 1,
            sample_aspect_ratio_den: 1,
            bit_depth: ResidentEncodeBitDepth::Eight,
            colorimetry: ResidentEncodeColorimetry::Rec709,
            full_range: false,
            chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
            keyframe_interval_frames: 30,
            max_b_frames: 0,
            quantizer: 18,
            surface_pool_size: 3,
        },
    )
    .expect("same-device CUDA/NVENC Session");

    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        width: WIDTH,
        height: HEIGHT,
        color_space: WorkingColorSpace::LinearRec709,
        data: vec![[0.18, 0.04, 0.72, 1.0]; (WIDTH * HEIGHT) as usize],
    });
    let boundary =
        ProgramOutputBoundary::export(ColorSpace::Rec709, false, ColorEngine::mondrian_standard());
    let expected = ProgramOutputModule::execute_cpu_rgba8(
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
        label: Some("linux-resident-encode-production-color"),
    });
    let composite = visual
        .record(
            &mut session,
            &context.device,
            &context.queue,
            &mut commands,
            GpuVisualFrameRequest {
                width: WIDTH,
                height: HEIGHT,
                working_color_space: WorkingColorSpace::LinearRec709,
                color_engine: ColorEngine::mondrian_standard(),
                elements: &elements,
            },
        )
        .expect("production opaque-black GPU composite");
    assert_eq!(
        composite.output.descriptor().alpha,
        mondrian_renderer::ColorFrameAlpha::Opaque
    );
    let record = session
        .record_program_output(
            &boundary,
            GpuProgramInput::Gpu(&composite.output),
            GpuColorFrameTextureFormat::Rgba8Unorm,
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
    let destination = encoder.acquire_input_frame().expect("FFmpeg CUDA surface");
    let ready = adapter.process(source, destination).expect("Vulkan-to-CUDA resident copy");
    encoder.submit_input_frame(ready, 0).expect("NVENC frame submission");
    encoder.finish().expect("NVENC trailer");

    let renderer = adapter.diagnostics();
    assert_eq!(renderer.color_conversion_submissions, 1);
    assert_eq!(renderer.device_to_device_copies, 1);
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
    let probe = probe_media_info(&artifact).expect("independent FFmpeg artifact probe");
    assert!(probe.has_video);
    assert_eq!(probe.primary_video().expect("video stream").width, WIDTH);
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
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&artifact)
        .output()
        .expect("probe authored chroma location");
    assert!(chroma_probe.status.success());
    assert_eq!(
        String::from_utf8_lossy(&chroma_probe.stdout).trim(),
        "left",
        "encoded metadata must identify the conversion kernel's left-sited chroma samples"
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
    assert_eq!(decoded.stdout.len(), (WIDTH * HEIGHT * 3) as usize);
    let expected_rgb = &expected.rgba[..3];
    let pixel_count = (WIDTH * HEIGHT) as usize;
    let actual_average = (0..3)
        .map(|channel| {
            decoded
                .stdout
                .chunks_exact(3)
                .map(|pixel| u64::from(pixel[channel]))
                .sum::<u64>()
                / pixel_count as u64
        })
        .collect::<Vec<_>>();
    for (channel, (actual, expected)) in actual_average.iter().zip(expected_rgb.iter()).enumerate()
    {
        assert!(
            actual.abs_diff(u64::from(*expected)) <= 12,
            "decoded channel {channel} average {actual} differs from same-contract CPU output {expected}"
        );
    }
}
