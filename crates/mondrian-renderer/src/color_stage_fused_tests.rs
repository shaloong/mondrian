//! Physical-pass parity; both sides consume the production YUV and OCIO stages.
use super::*;
use crate::*;

#[test]
#[ignore = "requires an explicitly available Vulkan adapter"]
fn fused_yuv_input_matches_two_pass_sdr_pq_hlg() {
    mondrian_core::ensure_mondrian_default_ocio_loaded().expect("bundled OCIO");
    let context = pollster::block_on(GpuContext::new()).expect("required GPU adapter");
    let decoder = GpuNativeYuvDecoder::new(&context.device);
    let mut runtime = RenderGpuOutputBoundaryRuntime::new().expect("color owner");
    for space in [
        ColorSpace::Rec709,
        ColorSpace::Rec2100Pq,
        ColorSpace::Rec2100Hlg,
    ] {
        for range in [GpuVideoRange::Limited, GpuVideoRange::Full] {
            let input = handle(
                &mut runtime,
                space.into(),
                ColorFrameDomain::Source,
                ColorFrameEncoding::EncodedFloat,
            );
            let working = handle(
                &mut runtime,
                WorkingColorSpace::LinearRec2020.into(),
                ColorFrameDomain::Working,
                ColorFrameEncoding::LinearFloat,
            );
            let fused = handle(
                &mut runtime,
                WorkingColorSpace::LinearRec2020.into(),
                ColorFrameDomain::Working,
                ColorFrameEncoding::LinearFloat,
            );
            let plan = GpuNativeYuvDecodePlan::new(
                GpuNativeDecodedFrameTextureFormat::P010,
                GpuNativeVideoExtent { width: 2, height: 2 },
                GpuNativeVideoExtent { width: 3, height: 3 },
                GpuNativeVideoExtent { width: 2, height: 2 },
                GpuNativeDecodedFrameVideoSampling::from_source_color_space(
                    space,
                    range,
                    10,
                    GpuVideoChromaLocation::Left,
                ),
                input.clone(),
            )
            .expect("exact YUV sampling");
            let luma = texture(
                &context,
                wgpu::TextureFormat::R16Unorm,
                2,
                2,
                4,
                bytemuck::cast_slice(&[0_u16, 64 * 64, 940 * 64, 1023 * 64]),
            );
            let chroma = texture(
                &context,
                wgpu::TextureFormat::Rg16Unorm,
                1,
                1,
                4,
                bytemuck::cast_slice(&[400_u16 * 64, 700 * 64]),
            );
            let y = luma.create_view(&Default::default());
            let uv = chroma.create_view(&Default::default());
            let prepared = decoder.prepare_pass(
                &context.device,
                &plan,
                GpuNativeYuvPlaneViews { luma: &y, chroma: &uv, chroma_v: &uv },
            );
            let mut encoder = context.device.create_command_encoder(&Default::default());
            let encoded = GpuNativeYuvDecoder::allocate_output(&context.device, &plan);
            decoder
                .record(&mut encoder, &plan, &prepared, &encoded)
                .expect("ordinary YUV pass");
            runtime.frame_table_mut().insert(encoded).expect("source table");
            let transform = RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec2020,
                false,
                ColorEngine::mondrian_standard(),
            );
            runtime
                .record_wgpu_input_stage_gpu_frame_owned_backend(
                    &transform,
                    &input,
                    &working,
                    RenderColorTransformGpuOptions::default(),
                    RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                        device: &context.device,
                        queue: &context.queue,
                        encoder: &mut encoder,
                        load_op: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    },
                )
                .expect("ordinary OCIO pass");
            let backend = runtime
                .prepare_fused_yuv_input(
                    &transform,
                    &input,
                    &fused,
                    &decoder,
                    &context.device,
                    &context.queue,
                )
                .expect("fused production backend");
            let output = runtime.resource_pool().acquire(
                &context.device,
                &GpuColorFrameAllocationPlan::for_handle(fused),
            );
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &output.resource().texture_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(&backend.pipeline);
                pass.set_bind_group(0, &backend.objects.ocio_bind_group.bind_group, &[]);
                pass.set_bind_group(1, &prepared.bind_group, &[]);
                pass.draw(0..4, 0..1);
            }
            let reference = runtime.frame_table().get(&working).expect("working result");
            let expected = readback(&context, &mut encoder, reference);
            let actual = readback(&context, &mut encoder, &output);
            context.queue.submit([encoder.finish()]);
            let expected = map(&context, expected);
            let actual = map(&context, actual);
            for (index, (expected, actual)) in expected.iter().zip(&actual).enumerate() {
                let tolerance = 2.0e-5 * expected.abs().max(1.0);
                assert!(
                    expected.is_finite()
                        && actual.is_finite()
                        && (expected - actual).abs() <= tolerance,
                    "{space:?}/{range:?} channel {index}: {expected} versus {actual}"
                );
            }
            assert!(actual.chunks_exact(4).all(|pixel| pixel[3] == 1.0));
            runtime.resource_pool().release(output);
            runtime.clear_frame_resources();
        }
    }
}

fn handle(
    runtime: &mut RenderGpuOutputBoundaryRuntime,
    color_space: ColorFrameSpace,
    domain: ColorFrameDomain,
    encoding: ColorFrameEncoding,
) -> GpuColorFrameHandle {
    GpuColorFrameHandle::new(
        runtime.frame_ids_mut().allocate().expect("id"),
        ColorFrameDescriptor {
            width: 3,
            height: 3,
            color_space,
            domain,
            encoding,
            residency: ColorFrameResidency::Gpu,
            alpha: ColorFrameAlpha::Opaque,
        },
        product_gpu_working_texture_format(),
        "fused-input-parity",
    )
    .expect("handle")
}

fn texture(
    context: &GpuContext,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
    row_bytes: u32,
    bytes: &[u8],
) -> wgpu::Texture {
    let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    context.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(row_bytes),
            rows_per_image: Some(height),
        },
        size,
    );
    texture
}

fn readback(
    context: &GpuContext,
    encoder: &mut wgpu::CommandEncoder,
    output: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
) -> (GpuColorFrameReadbackPlan, wgpu::Buffer) {
    let plan = GpuColorFrameReadbackPlan::encoded_rgba32float(output.handle().clone())
        .expect("readback plan");
    let buffer = GpuColorFrameReadback::record_copy(&context.device, encoder, &plan, output)
        .expect("readback copy");
    (plan, buffer)
}

fn map(
    context: &GpuContext,
    (plan, buffer): (GpuColorFrameReadbackPlan, wgpu::Buffer),
) -> Vec<f32> {
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    context
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(10)),
        })
        .expect("bounded GPU completion");
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("callback deadline")
        .expect("map");
    let pixels = plan
        .unpack_mapped_rgba32float(&buffer.slice(..).get_mapped_range().expect("mapped bytes"))
        .expect("pixels");
    buffer.unmap();
    pixels
}
