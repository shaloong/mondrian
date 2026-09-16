use std::sync::Arc;

use mondrian_core::types::{BlendMode, ColorEngine};
use mondrian_core::{WorkingColorSpace, WorkingRgbaF32Frame};
use mondrian_effects::{identity_compiled_effect_graph, lower_effect_graph_to_gpu_plan};
use mondrian_renderer::{
    color::{GpuColorExecutionSession, RenderColorStageDiagnostics},
    estimate_gpu_visual_frame_active_working_set, product_gpu_working_bytes_per_pixel,
    ColorFrameDomain, ColorFrameSpace, CpuColorFrame, GpuColorFrameIdAllocator,
    GpuColorFrameReadbackPlan, GpuColorFrameTextureFormat, GpuColorFrameUploadPlan, GpuContext,
    GpuVisualFrameActiveTextureDemand, GpuVisualFrameActiveWorkingSetAdmissionError,
    GpuVisualFrameElement, GpuVisualFrameExecutionError, GpuVisualFrameExecutionResourceGrant,
    GpuVisualFrameExecutor, GpuVisualFrameRequest, GpuVisualFrameSource, GpuVisualSourceLayer,
};

const IDENTITY_AFFINE: [f32; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

#[test]
fn data_texture_upload_plan_has_non_color_identity() {
    let frame = data_frame();
    let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame ids");
    let plan = GpuColorFrameUploadPlan::from_cpu_data_texture(
        ids.allocate().expect("data texture id"),
        &frame,
        "typed-data-texture",
    )
    .expect("typed data-texture upload plan");

    assert_eq!(
        plan.handle.descriptor().color_space,
        ColorFrameSpace::NonColorData
    );
    assert_eq!(
        plan.handle.descriptor().domain,
        ColorFrameDomain::DataTexture
    );
    assert_eq!(
        plan.handle.texture_format(),
        GpuColorFrameTextureFormat::Rgba32Float
    );
}

#[test]
fn visual_working_set_estimate_includes_retained_closure_and_numeric_upload() {
    let identity = Arc::new(
        lower_effect_graph_to_gpu_plan(
            &identity_compiled_effect_graph().expect("identity Effect graph"),
        )
        .expect("identity GPU plan"),
    );
    let elements = [GpuVisualFrameElement::Source(Box::new(
        GpuVisualSourceLayer {
            source: GpuVisualFrameSource::DataTexture(Arc::new(data_frame())),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: IDENTITY_AFFINE,
            effect_plan: identity,
            frame_seed: 0,
        },
    ))];
    let request = GpuVisualFrameRequest {
        width: 2,
        height: 1,
        working_color_space: WorkingColorSpace::LinearRec709,
        color_engine: ColorEngine::mondrian_standard(),
        elements: &elements,
    };

    let estimate = estimate_gpu_visual_frame_active_working_set(
        &request,
        GpuVisualFrameActiveTextureDemand { bytes: 10, textures: 1 },
    )
    .expect("checked visual working set");

    assert_eq!(estimate.retained_closure.bytes, 10);
    let working_bpp = u64::from(product_gpu_working_bytes_per_pixel());
    assert_eq!(estimate.source_uploads.bytes, 2 * working_bpp);
    assert_eq!(estimate.working_composite.bytes, 2 * 2 * working_bpp);
    assert_eq!(
        estimate.total().bytes,
        10 + 2 * working_bpp + 2 * 2 * working_bpp
    );
    assert_eq!(estimate.total().textures, 4);
}

#[tokio::test]
async fn data_texture_gpu_visual_path_preserves_numeric_channels_without_ocio() {
    let Ok(context) = GpuContext::new().await else {
        eprintln!("skipping GPU DataTexture visual test: no adapter available");
        return;
    };
    let mut runtime = GpuColorExecutionSession::new().expect("frame runtime");
    let identity = Arc::new(
        lower_effect_graph_to_gpu_plan(
            &identity_compiled_effect_graph().expect("identity Effect graph"),
        )
        .expect("identity GPU plan"),
    );
    let source = data_frame();
    let elements = [GpuVisualFrameElement::Source(Box::new(
        GpuVisualSourceLayer {
            source: GpuVisualFrameSource::DataTexture(Arc::new(source.clone())),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: IDENTITY_AFFINE,
            effect_plan: identity,
            frame_seed: 0,
        },
    ))];
    let mut rejected_encoder =
        context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu-data-texture-admission-test"),
        });
    let rejected_executor = GpuVisualFrameExecutor::with_resource_grant(
        &context.device,
        GpuVisualFrameExecutionResourceGrant::new(1, 1),
    )
    .expect("visual admission executor");
    let rejected = rejected_executor
        .record(
            &mut runtime,
            &context.device,
            &context.queue,
            &mut rejected_encoder,
            GpuVisualFrameRequest {
                width: 2,
                height: 1,
                working_color_space: WorkingColorSpace::LinearRec709,
                color_engine: ColorEngine::mondrian_standard(),
                elements: &elements,
            },
        )
        .expect_err("insufficient grant must reject before recording");
    assert!(matches!(
        rejected,
        GpuVisualFrameExecutionError::ActiveWorkingSet(
            GpuVisualFrameActiveWorkingSetAdmissionError::GrantExceeded { .. }
        )
    ));
    assert_eq!(runtime.retained_frame_count(), 0);

    let executor = GpuVisualFrameExecutor::new(&context.device).expect("visual executor");
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu-data-texture-visual-test"),
    });
    let record = executor
        .record(
            &mut runtime,
            &context.device,
            &context.queue,
            &mut encoder,
            GpuVisualFrameRequest {
                width: 2,
                height: 1,
                working_color_space: WorkingColorSpace::LinearRec709,
                color_engine: ColorEngine::mondrian_standard(),
                elements: &elements,
            },
        )
        .expect("record DataTexture visual frame");
    assert_eq!(record.compositing_diagnostics.data_texture_uploads, 1);
    assert_eq!(
        record.color_stage_diagnostics,
        RenderColorStageDiagnostics::default()
    );
    assert_eq!(record.output.descriptor().domain, ColorFrameDomain::Working);
    assert_eq!(record.active_working_set.total().textures, 3);

    let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba32float(record.output.clone())
        .expect("working Float32 readback plan");
    let readback = runtime
        .record_readback(&context.device, &mut encoder, &readback_plan)
        .expect("record working readback");
    context.queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    let _ = context
        .device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    receiver.recv().expect("map callback").expect("map working output");
    let mapped = slice.get_mapped_range().expect("mapped working bytes");
    let actual = readback_plan
        .unpack_mapped_rgba32float(&mapped)
        .expect("unpack working Float32 output");
    drop(mapped);
    readback.unmap();

    let expected = source.rgba_f32().data.as_flattened();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "{actual} != {expected}"
        );
    }
}

#[tokio::test]
async fn export_visual_path_uses_shared_spatial_composite_execution_plan() {
    let Ok(context) = GpuContext::new().await else {
        eprintln!("skipping GPU composite execution test: no adapter available");
        return;
    };
    let identity = Arc::new(
        lower_effect_graph_to_gpu_plan(
            &identity_compiled_effect_graph().expect("identity Effect graph"),
        )
        .expect("identity GPU plan"),
    );
    let base = Arc::new(CpuColorFrame::working(WorkingRgbaF32Frame {
        width: 4,
        height: 4,
        color_space: WorkingColorSpace::LinearRec709,
        data: vec![[0.0, 0.0, 1.0, 1.0]; 16],
    }));
    let overlay = Arc::new(CpuColorFrame::working(WorkingRgbaF32Frame {
        width: 2,
        height: 2,
        color_space: WorkingColorSpace::LinearRec709,
        data: vec![[1.0, 0.0, 0.0, 1.0]; 4],
    }));
    let elements = [
        GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
            source: GpuVisualFrameSource::Working(base),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: IDENTITY_AFFINE,
            effect_plan: Arc::clone(&identity),
            frame_seed: 0,
        })),
        GpuVisualFrameElement::Source(Box::new(GpuVisualSourceLayer {
            source: GpuVisualFrameSource::Working(overlay),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 1.0, 0.0, 1.0, 1.0],
            effect_plan: identity,
            frame_seed: 0,
        })),
    ];
    let mut runtime = GpuColorExecutionSession::new().expect("frame runtime");
    let executor = GpuVisualFrameExecutor::new(&context.device).expect("visual executor");
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("export-spatial-composite-plan"),
    });

    let record = executor
        .record(
            &mut runtime,
            &context.device,
            &context.queue,
            &mut encoder,
            GpuVisualFrameRequest {
                width: 4,
                height: 4,
                working_color_space: WorkingColorSpace::LinearRec709,
                color_engine: ColorEngine::mondrian_standard(),
                elements: &elements,
            },
        )
        .expect("record export visual frame");
    context.queue.submit(std::iter::once(encoder.finish()));

    assert_eq!(record.compositing_diagnostics.execution.render_passes, 2);
    assert_eq!(record.compositing_diagnostics.execution.shaded_pixels, 20);
    assert_eq!(
        record.compositing_diagnostics.execution.avoided_shader_pixels,
        12
    );
    assert_eq!(
        record.compositing_diagnostics.execution.preserved_copy_pixels,
        12
    );
}

fn data_frame() -> CpuColorFrame {
    CpuColorFrame::working(WorkingRgbaF32Frame {
        width: 2,
        height: 1,
        data: vec![[0.125, 0.5, 1.25, 1.0], [-0.25, 0.75, 2.0, 0.5]],
        color_space: WorkingColorSpace::LinearRec709,
    })
}
