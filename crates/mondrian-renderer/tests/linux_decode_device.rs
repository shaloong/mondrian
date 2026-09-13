//! Device binding for the real Linux native-video admission boundary.
#![cfg(target_os = "linux")]

use mondrian_renderer::{
    GpuColorFrameWgpuResourcePool, GpuContext, VulkanNativeVideoImportBackend,
};
use std::sync::Arc;

#[test]
#[ignore = "requires a real Vulkan native-video adapter and matching decode provider"]
fn native_admission_binds_the_renderer_device() {
    let context = pollster::block_on(GpuContext::new()).expect("native Vulkan adapter required");
    let backend = VulkanNativeVideoImportBackend::new_with_resource_pool(
        &context.adapter,
        &context.device,
        &context.queue,
        Arc::new(GpuColorFrameWgpuResourcePool::default()),
    )
    .expect("real native-video import admission required");
    assert!(
        backend.support().hardware_decode_device_selector.is_some(),
        "native import must not let FFmpeg select an unrelated default GPU"
    );
}

#[test]
#[ignore = "requires a real NVIDIA Vulkan/CUDA external-memory adapter"]
fn cuda_admission_binds_the_renderer_device() {
    let context = pollster::block_on(GpuContext::new()).expect("native Vulkan adapter required");
    assert_eq!(
        context.adapter.get_info().vendor,
        0x10de,
        "NVIDIA fixture required"
    );
    let runtime = mondrian_renderer::ViewerNativeVideoImportRuntime::new(
        &context.adapter,
        &context.device,
        &context.queue,
    );
    let support = runtime.support();
    assert!(
        support
            .supported_handle_kinds
            .contains(&mondrian_media::DecodedGpuFrameHandleKind::CudaDeviceMemory),
        "the real NVIDIA renderer needs an executable CUDA resource route: {support:?}"
    );
    assert!(matches!(
        support.hardware_decode_device_selector,
        Some(mondrian_media::HwAccelDeviceSelector::CudaDeviceOrdinal(_))
    ));
}

#[test]
#[ignore = "requires NVIDIA CUDA/Vulkan and MONDRIAN_CUDA_NATIVE_FIXTURES (30 fps tagged SDR clips)"]
fn cuda_decoded_frames_enter_production_yuv_ocio_and_release_after_gpu_completion() {
    use mondrian_core::{
        ColorEngine, ColorSpace, SourceSampleTarget, TimelineTime, WorkingColorSpace,
    };
    use mondrian_media::preview::*;
    use mondrian_media::{DecodedVideoRange, MediaFileFingerprint};
    use mondrian_renderer::{
        GpuColorFrameIdAllocator, GpuColorFrameReadback, GpuColorFrameReadbackPlan,
        RenderInputTransform, ViewerNativeVideoImportRuntime,
    };
    let fixtures = std::env::var_os("MONDRIAN_CUDA_NATIVE_FIXTURES")
        .expect("explicit generated fixtures required");
    let context = pollster::block_on(GpuContext::new()).expect("real Vulkan context");
    let pool = Arc::new(GpuColorFrameWgpuResourcePool::default());
    let mut runtime = ViewerNativeVideoImportRuntime::new_with_resource_pool(
        &context.adapter,
        &context.device,
        &context.queue,
        Arc::clone(&pool),
    );
    let support = runtime.support();
    assert_eq!(
        support.import_mode,
        Some(mondrian_renderer::GpuNativeDecodedFrameImportMode::GpuBridgeCopy)
    );
    let selector = support.hardware_decode_device_selector.expect("exact CUDA device selector");
    assert!(matches!(
        selector,
        mondrian_media::HwAccelDeviceSelector::CudaDeviceOrdinal(_)
    ));
    let transform = RenderInputTransform::to_working_gpu(
        WorkingColorSpace::LinearRec2020,
        true,
        ColorEngine::mondrian_standard(),
    );
    let mut ids = GpuColorFrameIdAllocator::new(1).expect("ids");
    for path in std::env::split_paths(&fixtures) {
        let fingerprint = MediaFileFingerprint::capture(&path);
        let mut decoder = PreviewDecodeSessionContext::new();
        for index in [0, 17, 1, 29, 30, 5] {
            let mut request = PreviewDecodeRequest::new(
                &path,
                SourceSampleTarget::covering(TimelineTime::new(index, 30).expect("time")),
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewSourceColorContract::automatic(
                    ColorSpace::Rec709,
                    DecodedVideoRange::Limited,
                ),
            )
            .with_fingerprint(fingerprint)
            .with_field_processing(PreviewSourceFieldProcessing::Progressive)
            .with_hardware_decode_request(PreviewHardwareDecodeRequest::RequireGpuResident);
            request.representation = PreviewDecodeRepresentation::NativeSurface;
            request.hardware_decode_device_selector = Some(selector);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let frame = match decoder
                .decode_cancellable(request, move || std::time::Instant::now() >= deadline)
                .expect("native decode")
            {
                PreviewDecodeOutcome::NativeGpuFrame(frame) => frame,
                other => panic!("required native route returned {other:?}"),
            };
            assert!(frame.diagnostics.hardware_decode_active);
            assert!(
                !frame.diagnostics.zero_copy_active,
                "FFmpeg safe CUDA output performs a GPU copy"
            );
            assert!(matches!(
                frame.diagnostics.execution_path(),
                PreviewDecodeExecutionPath::HardwareNative { .. }
            ));
            assert!(!frame.diagnostics.hardware_decode_cpu_transfer_observed);
            assert!(!frame.diagnostics.temporal_approximation);
            runtime
                .prepare_import_backend_objects(
                    ColorSpace::Rec709,
                    &transform,
                    frame.width,
                    frame.height,
                    &frame,
                )
                .expect("production color preparation");
            let before_pool = pool.diagnostics();
            let output = runtime
                .import(
                    &mut ids,
                    ColorSpace::Rec709,
                    &transform,
                    frame.width,
                    frame.height,
                    &frame,
                )
                .expect("production CUDA YUV/OCIO import");
            assert!(
                runtime.frame_cpu_timings().bridge_acquire_us > 0,
                "the CUDA allocation/copy/import stage must not be reported as zero-cost source validation"
            );
            assert_eq!(
                pool.diagnostics().releases,
                before_pool.releases + 1,
                "the submitted native RGB intermediate must return to the production pool"
            );
            drop(frame);
            let plan = GpuColorFrameReadbackPlan::encoded_rgba32float(output.handle().clone())
                .expect("full precision oracle readback");
            let mut encoder = context
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            let buffer =
                GpuColorFrameReadback::record_copy(&context.device, &mut encoder, &plan, &output)
                    .expect("readback command");
            let submission = context.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(std::time::Duration::from_secs(5)),
                })
                .expect("GPU completion");
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .expect("map callback")
                .expect("map success");
            let mapped = buffer.slice(..).get_mapped_range().expect("mapped bytes");
            let pixels = plan.unpack_mapped_rgba32float(&mapped).expect("float pixels");
            assert!(pixels.iter().all(|v| v.is_finite()));
            assert!(pixels.chunks_exact(4).all(|p| (p[3] - 1.0).abs() < 1e-5));
            assert!(pixels.chunks_exact(4).any(|p| p[0] > 0.1));
            assert!(pixels.chunks_exact(4).any(|p| p[1] > 0.1));
            drop(mapped);
            buffer.unmap();
            drop(output);
            while runtime.retained_source_count() != 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "native destruction did not finish within the frame deadline"
                );
                std::thread::yield_now();
            }
            assert_eq!(
                runtime.retained_source_count(),
                0,
                "all CUDA, semaphore and FFmpeg owners must retire"
            );
        }
        decoder.clear();
        assert_eq!(decoder.resident_session_count(), 0);
        assert!(decoder.native_outputs_released());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !runtime.poll_native_release_retirement().expect("native worker retirement") {
        assert!(
            std::time::Instant::now() < deadline,
            "native worker was not joined"
        );
        std::thread::yield_now();
    }
}

#[test]
#[ignore = "requires NVIDIA CUDA/Vulkan and MONDRIAN_CUDA_NATIVE_FIXTURES (30 fps tagged SDR clips)"]
fn cuda_safe_output_leases_do_not_exhaust_decoder_surfaces() {
    use mondrian_core::{ColorSpace, SourceSampleTarget, TimelineTime};
    use mondrian_media::preview::*;
    use mondrian_media::{DecodedVideoRange, MediaFileFingerprint};
    let fixtures = std::env::var_os("MONDRIAN_CUDA_NATIVE_FIXTURES").expect("fixtures");
    let context = pollster::block_on(GpuContext::new()).expect("GPU context");
    let runtime = mondrian_renderer::ViewerNativeVideoImportRuntime::new(
        &context.adapter,
        &context.device,
        &context.queue,
    );
    let selector = runtime.support().hardware_decode_device_selector.expect("CUDA selector");
    assert!(matches!(
        selector,
        mondrian_media::HwAccelDeviceSelector::CudaDeviceOrdinal(_)
    ));
    for path in std::env::split_paths(&fixtures) {
        let mut decoder = PreviewDecodeSessionContext::new();
        let mut held = Vec::new();
        let fingerprint = MediaFileFingerprint::capture(&path);
        for index in 0..40 {
            let mut request = PreviewDecodeRequest::new(
                &path,
                SourceSampleTarget::covering(TimelineTime::new(index, 30).expect("time")),
                PreviewDecodeAccessMode::PlaybackCursor,
                PreviewSourceColorContract::automatic(
                    ColorSpace::Rec709,
                    DecodedVideoRange::Limited,
                ),
            )
            .with_fingerprint(fingerprint)
            .with_field_processing(PreviewSourceFieldProcessing::Progressive)
            .with_hardware_decode_request(PreviewHardwareDecodeRequest::RequireGpuResident)
            .with_hardware_decode_device_selector(Some(selector));
            request.representation = PreviewDecodeRepresentation::NativeSurface;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let outcome = decoder
                .decode_cancellable(request, move || std::time::Instant::now() >= deadline)
                .expect("decode beyond decoder pool size");
            let PreviewDecodeOutcome::NativeGpuFrame(frame) = outcome else {
                panic!("native output required");
            };
            assert!(!frame.diagnostics.hardware_decode_cpu_transfer_observed);
            held.push(frame);
        }
        decoder.clear();
        assert_eq!(decoder.resident_session_count(), 0);
        assert!(
            !decoder.native_outputs_released(),
            "external leases still own the FFmpeg contexts"
        );
        drop(held);
        assert!(decoder.native_outputs_released());
    }
}

#[test]
#[ignore = "manual NVIDIA NVDEC/Vulkan latency diagnostic; set MONDRIAN_PREVIEW_DECODE_FIXTURE and MONDRIAN_PREVIEW_DEMUX_WORKER_PATH"]
fn cuda_production_decode_import_perf_smoke() {
    use mondrian_core::{
        ColorEngine, ColorSpace, SourceSampleTarget, TimelineTime, WorkingColorSpace,
    };
    use mondrian_media::preview::*;
    use mondrian_media::{DecodedVideoRange, MediaFileFingerprint};
    use mondrian_renderer::{
        GpuColorFrameIdAllocator, RenderInputTransform, ViewerNativeVideoImportRuntime,
    };
    use std::time::{Duration, Instant};

    let path = std::env::var_os("MONDRIAN_PREVIEW_DECODE_FIXTURE")
        .map(std::path::PathBuf::from)
        .expect("explicit decode performance fixture required");
    let demux_worker = std::env::var_os("MONDRIAN_PREVIEW_DEMUX_WORKER_PATH")
        .map(std::path::PathBuf::from)
        .expect("explicit packaged demux worker required");
    let frame_count = std::env::var("MONDRIAN_PREVIEW_DECODE_SEQUENCE_FRAMES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(120)
        .clamp(2, 600);
    let frame_rate_spec =
        std::env::var("MONDRIAN_PREVIEW_DECODE_FRAME_RATE").unwrap_or_else(|_| "30/1".to_owned());
    let (frame_rate_num, frame_rate_den) = frame_rate_spec
        .split_once('/')
        .and_then(|(num, den)| Some((num.parse::<i64>().ok()?, den.parse::<i64>().ok()?)))
        .filter(|(num, den)| *num > 0 && *den > 0)
        .expect("positive exact performance frame rate");
    let p95_budget_us = std::env::var("MONDRIAN_PREVIEW_DECODE_P95_BUDGET_US")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());

    let context = pollster::block_on(GpuContext::new()).expect("real Vulkan context");
    let pool = Arc::new(GpuColorFrameWgpuResourcePool::default());
    let mut runtime = ViewerNativeVideoImportRuntime::new_with_resource_pool(
        &context.adapter,
        &context.device,
        &context.queue,
        Arc::clone(&pool),
    );
    let selector = runtime
        .support()
        .hardware_decode_device_selector
        .expect("exact CUDA device selector");
    assert!(matches!(
        selector,
        mondrian_media::HwAccelDeviceSelector::CudaDeviceOrdinal(_)
    ));
    let transform = RenderInputTransform::to_working_gpu(
        WorkingColorSpace::LinearRec2020,
        true,
        ColorEngine::mondrian_standard(),
    );
    let (bootstrap, observer) =
        PreviewDecodeSessionContext::observed_bootstrap_with_demux_worker(demux_worker);
    let mut decoder = bootstrap.build();
    let fingerprint = MediaFileFingerprint::capture(&path);
    let mut ids = GpuColorFrameIdAllocator::new(1).expect("ids");
    let mut samples_us = Vec::with_capacity(frame_count);
    let mut decode_samples_us = Vec::with_capacity(frame_count);
    let mut import_samples_us = Vec::with_capacity(frame_count);
    let mut gpu_wait_samples_us = Vec::with_capacity(frame_count);
    let mut release_samples_us = Vec::with_capacity(frame_count);
    let mut first_import_cpu_stages = None;
    let started = Instant::now();

    for index in 0..frame_count {
        let frame_started = Instant::now();
        let frame_index = i64::try_from(index).expect("bounded performance frame index");
        let source_numerator = frame_index
            .checked_mul(frame_rate_den)
            .expect("bounded performance sample numerator");
        let mut request = PreviewDecodeRequest::new(
            &path,
            SourceSampleTarget::covering(
                TimelineTime::new(source_numerator, frame_rate_num)
                    .expect("exact performance sample"),
            ),
            PreviewDecodeAccessMode::PlaybackCursor,
            PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited),
        )
        .with_fingerprint(fingerprint)
        .with_field_processing(PreviewSourceFieldProcessing::Progressive)
        .with_hardware_decode_request(PreviewHardwareDecodeRequest::RequireGpuResident)
        .with_hardware_decode_device_selector(Some(selector));
        request.representation = PreviewDecodeRepresentation::NativeSurface;
        let deadline = Instant::now() + Duration::from_secs(10);
        let decode_started = Instant::now();
        let frame = match decoder
            .decode_cancellable(request, move || Instant::now() >= deadline)
            .expect("native performance decode")
        {
            PreviewDecodeOutcome::NativeGpuFrame(frame) => frame,
            other => panic!("required native route returned {other:?}"),
        };
        decode_samples_us
            .push(u64::try_from(decode_started.elapsed().as_micros()).unwrap_or(u64::MAX));
        assert!(frame.diagnostics.hardware_decode_active);
        assert!(!frame.diagnostics.hardware_decode_cpu_transfer_observed);
        let import_started = Instant::now();
        runtime
            .prepare_import_backend_objects(
                ColorSpace::Rec709,
                &transform,
                frame.width,
                frame.height,
                &frame,
            )
            .expect("prepare production native import");
        let output = runtime
            .import(
                &mut ids,
                ColorSpace::Rec709,
                &transform,
                frame.width,
                frame.height,
                &frame,
            )
            .expect("production CUDA YUV/OCIO import");
        if index == 0 {
            first_import_cpu_stages = Some(runtime.frame_cpu_timings());
        }
        import_samples_us
            .push(u64::try_from(import_started.elapsed().as_micros()).unwrap_or(u64::MAX));
        drop(frame);
        drop(output);
        let gpu_wait_started = Instant::now();
        context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(5)),
            })
            .expect("GPU completion");
        gpu_wait_samples_us
            .push(u64::try_from(gpu_wait_started.elapsed().as_micros()).unwrap_or(u64::MAX));
        let release_started = Instant::now();
        let release_deadline = Instant::now() + Duration::from_secs(5);
        while runtime.retained_source_count() != 0 {
            assert!(
                Instant::now() < release_deadline,
                "native release missed the frame deadline"
            );
            std::thread::yield_now();
        }
        release_samples_us
            .push(u64::try_from(release_started.elapsed().as_micros()).unwrap_or(u64::MAX));
        samples_us.push(u64::try_from(frame_started.elapsed().as_micros()).unwrap_or(u64::MAX));
    }

    decoder.clear();
    assert_eq!(decoder.resident_session_count(), 0);
    assert!(decoder.native_outputs_released());
    let retirement_deadline = Instant::now() + Duration::from_secs(5);
    while !runtime.poll_native_release_retirement().expect("native release retirement") {
        assert!(
            Instant::now() < retirement_deadline,
            "native worker was not joined"
        );
        std::thread::yield_now();
    }
    let demux = observer.snapshot().isolated_demux;
    assert!(demux.completed_reads > 0 && demux.clean_closes > 0 && demux.active_sessions == 0);

    let wall_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    let (avg_us, p95_us, maximum_us) = summarize_warm_samples(&samples_us);
    let (decode_avg_us, decode_p95_us, _) = summarize_warm_samples(&decode_samples_us);
    let (import_avg_us, import_p95_us, _) = summarize_warm_samples(&import_samples_us);
    let (gpu_wait_avg_us, gpu_wait_p95_us, _) = summarize_warm_samples(&gpu_wait_samples_us);
    let (release_avg_us, release_p95_us, _) = summarize_warm_samples(&release_samples_us);
    eprintln!(
        "MONDRIAN_CUDA_DECODE_IMPORT_PERF_SUMMARY path=\"{}\" frames={} frame_rate={}/{} first_us={} first_decode_us={} first_import_us={} first_import_cpu_stages={:?} first_gpu_wait_us={} first_release_us={} warm_avg_us={} warm_p95_us={} warm_max_us={} warm_decode_avg_us={} warm_decode_p95_us={} warm_import_avg_us={} warm_import_p95_us={} warm_gpu_wait_avg_us={} warm_gpu_wait_p95_us={} warm_release_avg_us={} warm_release_p95_us={} wall_us={} p95_budget_us={:?} cpu_pixel_transfers=0 demux_clean_closes={} retained_sources={}",
        path.display(),
        frame_count,
        frame_rate_num,
        frame_rate_den,
        samples_us[0],
        decode_samples_us[0],
        import_samples_us[0],
        first_import_cpu_stages.expect("first import CPU stages"),
        gpu_wait_samples_us[0],
        release_samples_us[0],
        avg_us,
        p95_us,
        maximum_us,
        decode_avg_us,
        decode_p95_us,
        import_avg_us,
        import_p95_us,
        gpu_wait_avg_us,
        gpu_wait_p95_us,
        release_avg_us,
        release_p95_us,
        wall_us,
        p95_budget_us,
        demux.clean_closes,
        runtime.retained_source_count(),
    );
    if let Some(budget) = p95_budget_us {
        assert!(
            p95_us <= budget,
            "NVDEC/CUDA/Vulkan production import p95 {p95_us} us exceeds {budget} us"
        );
    }
}

fn summarize_warm_samples(samples: &[u64]) -> (u64, u64, u64) {
    let mut warm_samples = samples[1..].to_vec();
    warm_samples.sort_unstable();
    let rank = (warm_samples.len() * 95).div_ceil(100).saturating_sub(1);
    (
        warm_samples.iter().sum::<u64>() / warm_samples.len() as u64,
        warm_samples[rank],
        *warm_samples.last().expect("non-empty warm samples"),
    )
}
