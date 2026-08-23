use anyhow::{Context, Result};
use mondrian_core::{ColorSpace, WaveformMode};
use mondrian_renderer::profile::{gpu_timestamp_query_device_features, GpuTimestampFrameTimer};
use mondrian_renderer::{
    request_adapter_with_native_video_preference, GpuProgramScopesRequest, GpuProgramScopesRuntime,
};
use std::time::Instant;

const WIDTH: u32 = 3_840;
const HEIGHT: u32 = 2_160;
const WARMUP_COUNT: usize = 2;
const SAMPLE_COUNT: usize = 8;
const DEFAULT_GPU_P95_BUDGET_US: u64 = 5_000;
const DEFAULT_CPU_RECORD_P95_BUDGET_US: u64 = 500;

#[tokio::test]
#[ignore = "short manual 4K hardware timestamp gate; requires a timestamp-capable real GPU"]
async fn program_scopes_4k_warm_path_stays_pooled_and_within_budget() -> Result<()> {
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
        Err(_) => {
            eprintln!("skipping 4K GPU scopes gate: no hardware adapter available");
            return Ok(());
        }
    };
    let timestamp_features = gpu_timestamp_query_device_features(adapter.features());
    if timestamp_features.is_empty() {
        eprintln!("skipping 4K GPU scopes gate: adapter has no timestamp queries");
        return Ok(());
    }
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("mondrian-program-scopes-4k-gate-device"),
            required_features: timestamp_features,
            ..wgpu::DeviceDescriptor::default()
        })
        .await
        .context("request timestamp-capable scopes device")?;
    let input = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("mondrian-program-scopes-4k-gate-input"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());
    let mut initialize = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mondrian-program-scopes-4k-gate-initialize"),
    });
    {
        let _pass = initialize.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian-program-scopes-4k-gate-clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &input_view,
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
    let initialization = queue.submit(std::iter::once(initialize.finish()));
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(initialization),
            timeout: None,
        })
        .context("initialize 4K scope input")?;

    let request = GpuProgramScopesRequest::new(ColorSpace::Rec709, WaveformMode::Luma, 256, 512)
        .context("construct 4K scope request")?;
    let timer = GpuTimestampFrameTimer::new(&device, &queue)
        .context("construct scopes hardware timestamp timer")?;
    let mut runtime = GpuProgramScopesRuntime::default();
    let mut gpu_samples_us = Vec::with_capacity(SAMPLE_COUNT);
    let mut cpu_record_samples_us = Vec::with_capacity(SAMPLE_COUNT);

    for sample_index in 0..WARMUP_COUNT + SAMPLE_COUNT {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-program-scopes-4k-gate-sample"),
        });
        timer.begin(&mut encoder);
        let record_started = Instant::now();
        let record = runtime
            .record(
                &device,
                &queue,
                &mut encoder,
                &input_view,
                WIDTH,
                HEIGHT,
                request,
            )
            .context("record 4K Program Output scopes")?;
        let cpu_record_us = saturating_u64(record_started.elapsed().as_micros());
        timer.finish(&mut encoder);
        let submission = queue.submit(std::iter::once(encoder.finish()));
        let gpu_us = timer
            .read_elapsed_us_after_submission(&device, submission)
            .context("read 4K scopes hardware timestamp")?;
        drop(record);
        if sample_index >= WARMUP_COUNT {
            gpu_samples_us.push(gpu_us);
            cpu_record_samples_us.push(cpu_record_us);
        }
    }

    let diagnostics = runtime.diagnostics();
    assert_eq!(diagnostics.pipeline_creations, 1);
    assert_eq!(diagnostics.buffer_allocations, 1);
    assert_eq!(diagnostics.texture_allocations, 3);
    assert_eq!(
        diagnostics.frames_recorded,
        (WARMUP_COUNT + SAMPLE_COUNT) as u64
    );
    let gpu_p95_us = nearest_rank(&gpu_samples_us, 95);
    let cpu_record_p95_us = nearest_rank(&cpu_record_samples_us, 95);
    let gpu_budget_us = env_u64(
        "MONDRIAN_SCOPES_GPU_P95_BUDGET_US",
        DEFAULT_GPU_P95_BUDGET_US,
    );
    let cpu_budget_us = env_u64(
        "MONDRIAN_SCOPES_CPU_RECORD_P95_BUDGET_US",
        DEFAULT_CPU_RECORD_P95_BUDGET_US,
    );
    let info = adapter.get_info();
    eprintln!(
        "4K GPU scopes: adapter={} backend={:?} samples={} gpu_p95={}us cpu_record_p95={}us",
        info.name, info.backend, SAMPLE_COUNT, gpu_p95_us, cpu_record_p95_us
    );
    assert!(
        gpu_p95_us <= gpu_budget_us,
        "4K GPU scopes p95 {gpu_p95_us} us exceeds {gpu_budget_us} us budget"
    );
    assert!(
        cpu_record_p95_us <= cpu_budget_us,
        "4K GPU scopes CPU record p95 {cpu_record_p95_us} us exceeds {cpu_budget_us} us budget"
    );
    Ok(())
}

fn nearest_rank(samples: &[u64], percentile: usize) -> u64 {
    let mut samples = samples.to_vec();
    samples.sort_unstable();
    let rank =
        (percentile.saturating_mul(samples.len()).saturating_add(99) / 100).clamp(1, samples.len());
    samples[rank - 1]
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|value| value.parse().ok()).unwrap_or(default)
}

fn saturating_u64(value: u128) -> u64 {
    value.min(u64::MAX as u128) as u64
}
