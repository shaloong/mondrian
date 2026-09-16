use mondrian_core::types::{ColorEngine, ColorSpace};
use mondrian_core::{ColorMatrixCoefficients, ColorTransferCharacteristic, WorkingColorSpace};
use mondrian_media::DecodedGpuFrameHandleKind;
use mondrian_renderer::{
    product_gpu_working_texture_format, ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain,
    ColorFrameEncoding, ColorFrameResidency, GpuColorFrameHandle, GpuColorFrameIdAllocator,
    GpuColorFrameReadback, GpuColorFrameReadbackPlan, GpuContext,
    GpuNativeDecodedFrameImportContract, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameImportSupport, GpuNativeDecodedFrameTextureFormat,
    GpuNativeDecodedFrameVideoSampling, GpuNativeRgbDecodePlan, GpuNativeRgbDecoder,
    GpuNativeRgbPrepareError, GpuNativeVideoExtent, GpuNativeYuvDecodePlan, GpuNativeYuvDecoder,
    GpuNativeYuvPlaneViews, GpuVideoChromaLocation, GpuVideoRange, RenderInputTransform,
};

#[tokio::test]
async fn rgba32_native_materialization_preserves_extended_range_and_alpha() {
    let Ok(context) = GpuContext::new().await else {
        eprintln!("skipping native RGB execution test: no GPU adapter available");
        return;
    };
    let texture = create_test_texture(
        &context.device,
        "mondrian-test-native-rgba32",
        2,
        1,
        wgpu::TextureFormat::Rgba32Float,
    );
    let samples = [-0.25_f32, 0.5, 1.5, 0.25, 2.0, -1.0, 0.125, 0.75];
    write_texture(
        &context.queue,
        &texture,
        2,
        1,
        32,
        bytemuck::cast_slice(&samples),
    );

    let support = GpuNativeDecodedFrameImportSupport::ready_zero_copy(
        vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
        vec![GpuNativeDecodedFrameTextureFormat::Rgba32Float],
    );
    let mut ids = GpuColorFrameIdAllocator::new(1_000).expect("frame ids");
    let import = GpuNativeDecodedFrameImportPlan::from_contract(
        &mut ids,
        GpuNativeDecodedFrameImportContract {
            width: 2,
            height: 1,
            output_width: 2,
            output_height: 1,
            source_color_space: ColorSpace::Srgb,
            input_transform: RenderInputTransform::to_working_gpu(
                WorkingColorSpace::LinearRec709,
                false,
                ColorEngine::mondrian_standard(),
            ),
            handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
            source_texture_format: GpuNativeDecodedFrameTextureFormat::Rgba32Float,
            video_sampling: GpuNativeDecodedFrameVideoSampling::from_source_color_space(
                ColorSpace::Srgb,
                GpuVideoRange::Full,
                32,
                GpuVideoChromaLocation::Unspecified,
            ),
            label: "native-rgba32".to_owned(),
        },
        &support,
    )
    .expect("native RGB import plan");
    let plan = GpuNativeRgbDecodePlan::from_import_plan(&import).expect("RGB decode plan");
    let decoder = GpuNativeRgbDecoder::new(&context.device);

    let wrong_texture = create_test_texture(
        &context.device,
        "mondrian-test-native-rgba16-wrong",
        2,
        1,
        wgpu::TextureFormat::Rgba16Float,
    );
    assert!(matches!(
        decoder.prepare_pass(&context.device, &plan, &wrong_texture),
        Err(GpuNativeRgbPrepareError::SourceFormatMismatch {
            expected: wgpu::TextureFormat::Rgba32Float,
            actual: wgpu::TextureFormat::Rgba16Float,
        })
    ));

    let output = GpuNativeRgbDecoder::allocate_output(&context.device, &plan);
    let prepared = decoder
        .prepare_pass(&context.device, &plan, &texture)
        .expect("prepare native RGB texture");
    let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mondrian-test-native-rgba32-materialize"),
    });
    decoder
        .record(&mut encoder, &plan, &prepared, &output)
        .expect("record RGB materialize");
    let actual = readback_rgba32(&context, encoder, &output);
    assert_eq!(actual, samples);
}

#[tokio::test]
async fn high_bit_420_422_444_planes_execute_without_half_float_staging() {
    let Ok(context) = GpuContext::new().await else {
        eprintln!("skipping high-bit native YUV matrix test: no GPU adapter available");
        return;
    };
    let mut ids = GpuColorFrameIdAllocator::new(900).expect("frame ids");
    for (format, chroma_width, chroma_height, bit_depth) in [
        (GpuNativeDecodedFrameTextureFormat::P012, 1, 1, 12),
        (GpuNativeDecodedFrameTextureFormat::P212, 1, 2, 12),
        (GpuNativeDecodedFrameTextureFormat::P416, 2, 2, 16),
    ] {
        let luma = create_test_texture(
            &context.device,
            "mondrian-test-high-bit-yuv-luma",
            2,
            2,
            wgpu::TextureFormat::R16Unorm,
        );
        let chroma = create_test_texture(
            &context.device,
            "mondrian-test-high-bit-yuv-chroma",
            chroma_width,
            chroma_height,
            wgpu::TextureFormat::Rg16Unorm,
        );
        let white_word = 60_160_u16.to_le_bytes();
        let neutral_word = 32_768_u16.to_le_bytes();
        let luma_bytes: Vec<u8> = (0..4).flat_map(|_| white_word).collect();
        let chroma_bytes: Vec<u8> = (0..chroma_width * chroma_height)
            .flat_map(|_| neutral_word.into_iter().chain(neutral_word))
            .collect();
        write_texture(&context.queue, &luma, 2, 2, 4, &luma_bytes);
        write_texture(
            &context.queue,
            &chroma,
            chroma_width,
            chroma_height,
            chroma_width * 4,
            &chroma_bytes,
        );
        let output_handle = GpuColorFrameHandle::new(
            ids.allocate().expect("high-bit frame id"),
            ColorFrameDescriptor {
                width: 2,
                height: 2,
                color_space: ColorSpace::Rec2100Pq.into(),
                domain: ColorFrameDomain::Source,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::Opaque,
            },
            product_gpu_working_texture_format(),
            "high-bit-native-yuv-output",
        )
        .expect("encoded high-bit output");
        let plan = GpuNativeYuvDecodePlan::new(
            format,
            GpuNativeVideoExtent { width: 2, height: 2 },
            GpuNativeVideoExtent { width: 2, height: 2 },
            GpuNativeVideoExtent { width: 2, height: 2 },
            GpuNativeDecodedFrameVideoSampling {
                range: GpuVideoRange::Limited,
                matrix: ColorMatrixCoefficients::Bt2020NonConstant,
                transfer: ColorTransferCharacteristic::Pq,
                bit_depth,
                chroma_location: if format == GpuNativeDecodedFrameTextureFormat::P416 {
                    GpuVideoChromaLocation::Unspecified
                } else {
                    GpuVideoChromaLocation::Left
                },
            },
            output_handle,
        )
        .expect("qualified high-bit YUV plan");
        let decoder = GpuNativeYuvDecoder::new(&context.device);
        let output = GpuNativeYuvDecoder::allocate_output(&context.device, &plan);
        let luma_view = luma.create_view(&wgpu::TextureViewDescriptor::default());
        let chroma_view = chroma.create_view(&wgpu::TextureViewDescriptor::default());
        let prepared = decoder.prepare_pass(
            &context.device,
            &plan,
            GpuNativeYuvPlaneViews {
                luma: &luma_view,
                chroma: &chroma_view,
                chroma_v: &chroma_view,
            },
        );
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-high-bit-yuv"),
        });
        decoder
            .record(&mut encoder, &plan, &prepared, &output)
            .expect("record high-bit YUV");
        let actual = readback_rgba32(&context, encoder, &output);
        for pixel in actual.chunks_exact(4) {
            assert!(
                pixel[..3].iter().all(|channel| (*channel - 1.0).abs() < 2.0e-4),
                "{format:?} expected neutral white, got {pixel:?}"
            );
            assert!((pixel[3] - 1.0).abs() < 1.0e-6);
        }
    }
}

fn create_test_texture(
    device: &wgpu::Device,
    label: &'static str,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn write_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    bytes_per_row: u32,
    bytes: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bytes_per_row),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}

fn readback_rgba32(
    context: &GpuContext,
    mut encoder: wgpu::CommandEncoder,
    output: &mondrian_renderer::GpuColorFrameResource<mondrian_renderer::GpuColorFrameWgpuResource>,
) -> Vec<f32> {
    let plan =
        GpuColorFrameReadbackPlan::encoded_rgba32float(output.handle().clone()).expect("readback");
    let readback = GpuColorFrameReadback::record_copy(&context.device, &mut encoder, &plan, output)
        .expect("record readback");
    context.queue.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    let slice = readback.slice(..);
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let _ = context
        .device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
    rx.recv().expect("readback callback").expect("readback map");
    let mapped = slice.get_mapped_range().expect("mapped readback").to_vec();
    let actual = plan.unpack_mapped_rgba32float(&mapped).expect("unpack RGBA32F");
    readback.unmap();
    actual
}
