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
