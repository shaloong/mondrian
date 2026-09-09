//! Native diagnostic of the float-to-UNORM boundary; never qualification evidence.

use anyhow::{anyhow, ensure, Context, Result};
use mondrian_core::{
    ensure_mondrian_default_ocio_loaded, ColorEngine, ColorSpace, WorkingColorSpace,
    WorkingRgbaF32Frame,
};
use mondrian_renderer::{
    color::{
        qualification::{
            RenderGpuOutputBoundaryRuntime, RenderGpuOutputBoundaryRuntimeOwnedBackendContext,
        },
        ProgramOutputBoundary, ProgramOutputModule,
    },
    ocio_lut_filtering_device_features, request_adapter_with_native_video_preference,
    ColorFrameResidency, CpuColorFrame, GpuColorFrameReadbackPlan, GpuColorFrameTextureFormat,
    RenderColorTransformGpuOptions, RenderCpuColorExecutionSession,
};
use serde_json::json;
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;

enum Readback {
    Float(Vec<f32>),
    Unorm(Vec<u8>),
}

fn gpu_boundary(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    frame: &CpuColorFrame,
    boundary: &ProgramOutputBoundary,
    format: GpuColorFrameTextureFormat,
) -> Result<Readback> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut runtime = RenderGpuOutputBoundaryRuntime::new()?;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("alpha-quantization-diagnostic"),
    });
    let mut record = runtime
        .record_wgpu_output_boundary_owned_backend(
            boundary,
            frame,
            format,
            RenderColorTransformGpuOptions {
                output_residency: ColorFrameResidency::Cpu,
                ..Default::default()
            },
            RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                device,
                queue,
                encoder: &mut encoder,
                load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
            },
        )
        .map_err(|error| anyhow!("record {format:?} boundary: {error:?}"))?;
    ensure!(
        record.stage_diagnostics.gpu_color_stages == 1,
        "one real GPU color pass required"
    );
    ensure!(
        record.stage_diagnostics.readback_stages == 1,
        "real GPU readback required"
    );
    let plan = match format {
        GpuColorFrameTextureFormat::Rgba32Float => {
            GpuColorFrameReadbackPlan::encoded_rgba32float(record.materialized.output.clone())
        }
        GpuColorFrameTextureFormat::Rgba8Unorm => {
            GpuColorFrameReadbackPlan::encoded_rgba8(record.materialized.output.clone())
        }
        _ => unreachable!("only exact Float32 and UNORM8 diagnostic targets"),
    }
    .map_err(|error| anyhow!("plan {format:?} readback: {error:?}"))?;
    let buffer = record.readback_buffer.take().context("GPU readback buffer missing")?;
    let submission = queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .context("GPU boundary exhausted its original deadline before mapping")?;
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(remaining),
        })
        .context("wait for native GPU boundary")?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .context("GPU boundary exhausted its original deadline after submission")?;
    receiver.recv_timeout(remaining).context("GPU map callback deadline")??;
    let mapped = buffer
        .slice(..)
        .get_mapped_range()
        .map_err(|error| anyhow!("borrow mapped {format:?} readback: {error:?}"))?;
    let result = match format {
        GpuColorFrameTextureFormat::Rgba32Float => {
            plan.unpack_mapped_rgba32float(&mapped).map(Readback::Float)
        }
        GpuColorFrameTextureFormat::Rgba8Unorm => plan
            .unpack_mapped_rgba8(&mapped)
            .map(|frame| Readback::Unorm(frame.rgba().to_vec())),
        _ => unreachable!("diagnostic format validated above"),
    }
    .map_err(|error| anyhow!("unpack {format:?} readback: {error:?}"));
    drop(mapped);
    buffer.unmap();
    runtime.clear_frame_resources();
    result
}

#[test]
#[ignore = "manual native GPU diagnostic; requires physical GPU and MONDRIAN_GPU_ALPHA_DIAGNOSTIC_OUTPUT"]
fn native_srgb_float_and_unorm_alpha_boundary_diagnostic() -> Result<()> {
    pollster::block_on(run_diagnostic())
}

async fn run_diagnostic() -> Result<()> {
    let output = PathBuf::from(
        std::env::var_os("MONDRIAN_GPU_ALPHA_DIAGNOSTIC_OUTPUT")
            .context("set a new MONDRIAN_GPU_ALPHA_DIAGNOSTIC_OUTPUT JSON path")?,
    );
    ensure_mondrian_default_ocio_loaded().map_err(|error| anyhow!(error))?;
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = request_adapter_with_native_video_preference(
        &instance,
        &wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        },
    )
    .await
    .context("physical GPU unavailable; diagnostic did not run")?;
    let info = adapter.get_info();
    ensure!(
        matches!(
            info.device_type,
            wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
        ),
        "physical GPU required, received {:?}",
        info.device_type
    );
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("native-alpha-quantization-diagnostic"),
            required_features: ocio_lut_filtering_device_features(adapter.features()),
            ..Default::default()
        })
        .await
        .context("create physical GPU diagnostic device")?;
    // Four vertical coverage bands, each with non-flat RGB and 1,024 samples.
    // Keep exact binary coverage values independent of all color arithmetic.
    let pixels: Vec<[f32; 4]> = (0..WIDTH * HEIGHT)
        .map(|index| {
            let x = index % WIDTH;
            let y = index / WIDTH;
            [
                (x as f32 + 0.25) / WIDTH as f32,
                (y as f32 + 0.5) / HEIGHT as f32,
                ((x * 7 + y * 11) % 127) as f32 / 126.0,
                [0.0, 0.25, 0.5, 1.0][(x / 16) as usize],
            ]
        })
        .collect();
    let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
        width: WIDTH,
        height: HEIGHT,
        data: pixels.clone(),
        color_space: WorkingColorSpace::LinearRec709,
    });
    let boundary =
        ProgramOutputBoundary::export(ColorSpace::Srgb, false, ColorEngine::mondrian_standard());
    let mut cpu_session = RenderCpuColorExecutionSession::default();
    let cpu_float = ProgramOutputModule::execute_cpu_float(&frame, &boundary, &mut cpu_session)?;
    let cpu_unorm = ProgramOutputModule::execute_cpu_rgba8(&frame, &boundary, &mut cpu_session)?;
    let Readback::Float(gpu_float) = gpu_boundary(
        &device,
        &queue,
        &frame,
        &boundary,
        GpuColorFrameTextureFormat::Rgba32Float,
    )?
    else {
        unreachable!("Float32 target")
    };
    let Readback::Unorm(gpu_unorm) = gpu_boundary(
        &device,
        &queue,
        &frame,
        &boundary,
        GpuColorFrameTextureFormat::Rgba8Unorm,
    )?
    else {
        unreachable!("UNORM8 target")
    };
    ensure!(
        gpu_float.len() == pixels.len() * 4 && gpu_unorm.len() == pixels.len() * 4,
        "GPU output raster incomplete"
    );
    let cpu_float = &cpu_float.frame.rgba_f32().data;
    let samples: Vec<_> = pixels.iter().enumerate().map(|(index, source)| {
        let gpu_pixel = &gpu_float[index * 4..index * 4 + 4];
        let gpu_bytes = &gpu_unorm[index * 4..index * 4 + 4];
        let cpu_bytes = &cpu_unorm.rgba[index * 4..index * 4 + 4];
        let expected_alpha = (source[3] * 255.0).round() as u8;
        json!({
            "index": index,
            "source_rgba_f32_bits": source.map(f32::to_bits),
            "cpu_rgba_f32_bits": cpu_float[index].map(f32::to_bits),
            "gpu_rgba_f32_bits": gpu_pixel.iter().map(|value| value.to_bits()).collect::<Vec<_>>(),
            "cpu_rgba8": cpu_bytes,
            "gpu_rgba8": gpu_bytes,
            "expected_source_alpha_nearest_u8": expected_alpha,
            "gpu_float_alpha_bit_exact": gpu_pixel[3].to_bits() == source[3].to_bits(),
            "gpu_unorm_alpha_matches_nearest_source": gpu_bytes[3] == expected_alpha,
            "gpu_float_alpha_nearest_u8": (gpu_pixel[3].clamp(0.0, 1.0) * 255.0).round() as u8,
        })
    }).collect();
    let report = json!({
        "schema_version": 1,
        "kind": "native_gpu_alpha_quantization_diagnostic",
        "analysis_completed": true,
        "qualified": false,
        "limits": "Separate executions of the same production sRGB boundary; diagnoses precision and quantization without changing qualification tolerances. Input upload replaces upstream composition deliberately.",
        "width": WIDTH, "height": HEIGHT,
        "working_color_space": "LinearRec709", "output_color_space": "Srgb",
        "tone_map": false,
        "summary": {
            "gpu_float_alpha_bit_mismatches": pixels.iter().enumerate()
                .filter(|(index, source)| gpu_float[index * 4 + 3].to_bits() != source[3].to_bits()).count(),
            "gpu_unorm_alpha_nearest_source_mismatches": pixels.iter().enumerate()
                .filter(|(index, source)| gpu_unorm[index * 4 + 3] != (source[3] * 255.0).round() as u8).count(),
            "gpu_unorm_alpha_nearest_gpu_float_mismatches": gpu_float.chunks_exact(4)
                .zip(gpu_unorm.chunks_exact(4))
                .filter(|(float, bytes)| bytes[3] != (float[3].clamp(0.0, 1.0) * 255.0).round() as u8).count(),
        },
        "adapter": {
            "name": info.name, "driver": info.driver, "driver_info": info.driver_info,
            "backend": format!("{:?}", info.backend),
            "device_type": format!("{:?}", info.device_type),
            "vendor": info.vendor, "device": info.device,
        },
        "samples": samples,
    });
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .with_context(|| format!("create diagnostic {}", output.display()))?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    eprintln!("native GPU Alpha diagnostic saved: {}", output.display());
    // Preserve raw observations before checking coverage invariants. UNORM
    // byte differences remain diagnostic findings, never qualification passes.
    for (index, source) in pixels.iter().enumerate() {
        ensure!(
            cpu_float[index][3].to_bits() == source[3].to_bits(),
            "CPU alpha changed at {index}"
        );
        ensure!(
            cpu_unorm.rgba[index * 4 + 3] == (source[3] * 255.0).round() as u8,
            "CPU alpha quantizer differs at {index}"
        );
        ensure!(
            gpu_float[index * 4 + 3].to_bits() == source[3].to_bits(),
            "GPU Float32 output changed exact coverage at {index}"
        );
    }
    Ok(())
}
