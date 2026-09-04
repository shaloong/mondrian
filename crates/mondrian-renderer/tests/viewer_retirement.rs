//! Real GPU + public decoder/runtime seam, independent of the large unit target.

use mondrian_core::{
    types::BlendMode, ColorEngine, ColorSpace, SourceSampleTarget, TimelineTime, WorkingColorSpace,
};
use mondrian_effects::{
    compile_reference_render_graph, lower_effect_graph_to_gpu_plan, EffectRenderGraph,
};
use mondrian_media::{
    CpuYuvFrame, DecodedVideoMatrix, DecodedVideoRange, PreviewDecodeAccessMode,
    PreviewDecodeOutcome, PreviewDecodeRepresentation, PreviewDecodeRequest,
    PreviewDecodeSessionContext, PreviewSourceColorContract,
};
use mondrian_renderer::{
    GpuContext, RenderInputTransform, ViewerCpuYuvUploadWorkerExit, ViewerGpuCpuYuvSource,
    ViewerGpuExecutionLayer, ViewerGpuExecutionRetirement, ViewerGpuExecutionRuntime,
    ViewerGpuRetirementReceipt, ViewerGpuSourceLayer,
};
use std::{
    path::Path,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

fn decode_fixture(path: &Path) -> CpuYuvFrame {
    let mut bytes = b"YUV4MPEG2 W4 H2 F25:1 Ip A1:1 C422p10\nFRAME\n".to_vec();
    for value in [512_u16; 16] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write owned compact fixture");
    let color =
        PreviewSourceColorContract::automatic(ColorSpace::Rec709, DecodedVideoRange::Limited)
            .with_yuv_matrix_fallback(DecodedVideoMatrix::Bt709);
    let mut request = PreviewDecodeRequest::new(
        path,
        SourceSampleTarget::covering(TimelineTime::ZERO),
        PreviewDecodeAccessMode::PlaybackCursor,
        color,
    );
    request.representation = PreviewDecodeRepresentation::CompactCpuYuv;
    let mut decoder = PreviewDecodeSessionContext::new();
    let result = decoder.decode_cancellable(request, || false).expect("decode compact frame");
    decoder.clear();
    match result {
        PreviewDecodeOutcome::CpuYuvFrame(frame) => frame,
        other => panic!("expected compact YUV, got {other:?}"),
    }
}

fn layer(frame: CpuYuvFrame) -> ViewerGpuExecutionLayer {
    let graph =
        compile_reference_render_graph(EffectRenderGraph::identity()).expect("identity graph");
    let effect_plan = Arc::new(lower_effect_graph_to_gpu_plan(&graph).expect("identity plan"));
    let (width, height) = (frame.width, frame.height);
    ViewerGpuExecutionLayer::Source(Box::new(ViewerGpuSourceLayer::Media {
        frame: None,
        is_data_texture: false,
        gpu_source: None,
        native_source: None,
        cpu_yuv_source: Some(ViewerGpuCpuYuvSource {
            frame: Arc::new(frame),
            input_transform: RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec709,
                false,
                ColorEngine::mondrian_standard(),
            ),
            materialization_width: width,
            materialization_height: height,
        }),
        heterogeneous_input: None,
        opacity: 1.0,
        blend_mode: BlendMode::Normal,
        transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        effect_plan,
        frame_seed: 0,
    }))
}

fn finish(
    context: &GpuContext,
    owner: &mut ViewerGpuExecutionRetirement,
) -> ViewerGpuRetirementReceipt {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(receipt) = owner.poll().expect("native retirement") {
            context
                .device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(Duration::from_secs(5)),
                })
                .expect("independent whole-queue barrier");
            assert_eq!(owner.poll().expect("repeat retirement"), Some(receipt));
            return receipt;
        }
        assert!(Instant::now() < deadline, "upload worker closure timed out");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn real_gpu_upload_retirement_observes_idle_pending_and_panicked_workers() {
    let context = pollster::block_on(GpuContext::new()).expect("real local GPU required");
    let runtime = ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
        .expect("idle runtime");
    let mut idle = runtime.into_retirement();
    assert!(finish(&context, &mut idle).is_healthy());
    drop(idle);

    let temp = tempfile::tempdir().expect("owned fixture directory");
    let decoded = decode_fixture(&temp.path().join("tiny-422p10.y4m"));
    let layers = [layer(decoded.clone())];
    let (entered, entry) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let gate = Mutex::new(gate);
    let runtime = ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
        .expect("upload runtime");
    runtime.install_cpu_yuv_upload_waker(move || {
        entered.send(()).expect("observe real mapped upload");
        gate.lock()
            .expect("gate mutex")
            .recv_timeout(Duration::from_secs(5))
            .expect("release wake");
    });
    assert!(!runtime.prepare_cpu_yuv_uploads(&layers).expect("schedule upload"));
    entry.recv_timeout(Duration::from_secs(5)).expect("worker reached wake");
    let mut pending = runtime.into_retirement();
    assert_eq!(pending.poll().expect("pending retirement"), None);
    release.send(()).expect("release real worker");
    assert!(finish(&context, &mut pending).is_healthy());
    drop(pending);

    // Hold a recorded encoder until after worker exit, so the real map-on-submit
    // callback necessarily runs later while the retiring owner retains resources.
    mondrian_core::ensure_mondrian_default_ocio_loaded().expect("OCIO config");
    let mut runtime =
        ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
            .expect("submitted runtime");
    let upload_deadline = Instant::now() + Duration::from_secs(5);
    while !runtime.prepare_cpu_yuv_uploads(&layers).expect("prepare real upload") {
        assert!(
            Instant::now() < upload_deadline,
            "upload preparation timeout"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    let boundary = mondrian_renderer::color::ProgramOutputBoundary::display(
        ColorSpace::Srgb,
        false,
        ColorEngine::mondrian_standard(),
    );
    let monitor = mondrian_renderer::RenderMonitorAdaptation::new(
        ColorSpace::Srgb,
        ColorSpace::Srgb,
        ColorEngine::mondrian_standard(),
    )
    .expect("monitor contract");
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("retirement-late-remap"),
    });
    let mut record = runtime
        .record(
            &context.device,
            &context.queue,
            &mut encoder,
            mondrian_renderer::ViewerGpuExecutionRequest {
                sequence_id: mondrian_core::SequenceId::new(),
                timeline_frame: 0,
                width: 4,
                height: 2,
                working_color_space: WorkingColorSpace::LinearRec709,
                layers: &layers,
                heterogeneous_inputs: vec![],
                program_output_boundary: &boundary,
                monitor_adaptation: &monitor,
                source_rect: mondrian_renderer::ViewerSourceRect::FULL,
                output_width: 4,
                output_height: 2,
                output_precision: mondrian_renderer::ViewerGpuOutputPrecision::Encoded8,
                display_calibration: None,
                program_scopes: None,
                signal_monitoring: None,
            },
        )
        .expect("record submitted compact YUV frame");
    let escaped = runtime.take_presentation_output(&mut record).expect("detached output");
    let mut submitted = runtime.into_retirement();
    let worker_deadline = Instant::now() + Duration::from_secs(5);
    while submitted.poll().expect("worker exit before submit").is_none() {
        assert!(Instant::now() < worker_deadline, "worker exit timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
    let _submission = context.queue.submit(Some(encoder.finish()));
    assert!(finish(&context, &mut submitted).is_healthy());
    drop(submitted);
    drop(escaped);

    let runtime = ViewerGpuExecutionRuntime::new(&context.adapter, &context.device, &context.queue)
        .expect("panic runtime");
    let (entered, entry) = mpsc::channel();
    runtime.install_cpu_yuv_upload_waker(move || {
        entered.send(()).expect("observe panic path");
        panic!("injected real upload wake panic");
    });
    assert!(!runtime
        .prepare_cpu_yuv_uploads(&[layer(decoded)])
        .expect("schedule panic upload"));
    entry.recv_timeout(Duration::from_secs(5)).expect("real upload before panic");
    let mut panicked = runtime.into_retirement();
    let receipt = finish(&context, &mut panicked);
    assert_eq!(
        receipt.cpu_yuv_upload,
        ViewerCpuYuvUploadWorkerExit::Panicked
    );
    assert!(!receipt.is_healthy());
}
