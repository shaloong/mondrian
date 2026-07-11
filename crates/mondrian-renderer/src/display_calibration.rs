//! GPU monitor calibration pass driven by a core ICC 3D LUT contract.

use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency,
    ColorFrameSpace, GpuColorFrameAllocationPlan, GpuColorFrameHandle, GpuColorFrameHandleError,
    GpuColorFrameIdAllocator, GpuColorFrameResource, GpuColorFrameTextureFormat,
    GpuColorFrameUploader, GpuColorFrameWgpuResource,
};
use mondrian_core::display_calibration::{DisplayCalibrationLut3d, IccProfileFingerprint};
use std::sync::Arc;
use thiserror::Error;

/// Validated GPU device-calibration resource plan.
#[derive(Debug, Clone)]
pub struct GpuDisplayCalibrationPlan {
    /// Encoded display signal entering calibration.
    pub input: GpuColorFrameHandle,
    /// Device RGB signal produced by calibration.
    pub output: GpuColorFrameHandle,
    /// Immutable calibration samples and profile identity.
    pub calibration: Arc<DisplayCalibrationLut3d>,
}

impl GpuDisplayCalibrationPlan {
    /// Plan one display-boundary calibration pass.
    pub fn new(
        ids: &mut GpuColorFrameIdAllocator,
        input: GpuColorFrameHandle,
        calibration: Arc<DisplayCalibrationLut3d>,
        output_texture_format: GpuColorFrameTextureFormat,
    ) -> Result<Self, GpuDisplayCalibrationPlanError> {
        let descriptor = input.descriptor();
        if descriptor.domain != ColorFrameDomain::Display
            || descriptor.encoding != ColorFrameEncoding::EncodedFloat
            || descriptor.residency != ColorFrameResidency::Gpu
            || descriptor.color_space.encoded() != Some(calibration.source_color_space)
        {
            return Err(GpuDisplayCalibrationPlanError::InvalidInputContract {
                actual: descriptor,
            });
        }
        if !matches!(
            input.texture_format(),
            GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float
        ) {
            return Err(
                GpuDisplayCalibrationPlanError::UnsupportedInputTextureFormat(
                    input.texture_format(),
                ),
            );
        }
        if !matches!(
            output_texture_format,
            GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float
        ) {
            return Err(
                GpuDisplayCalibrationPlanError::UnsupportedOutputTextureFormat(
                    output_texture_format,
                ),
            );
        }
        let output_descriptor = ColorFrameDescriptor {
            color_space: ColorFrameSpace::Device(calibration.profile_fingerprint.calibration_key()),
            encoding: ColorFrameEncoding::DeviceFloat,
            ..descriptor
        };
        let output = GpuColorFrameHandle::new(
            ids.allocate(),
            output_descriptor,
            output_texture_format,
            "display-calibration-device-output",
        )
        .map_err(GpuDisplayCalibrationPlanError::OutputHandle)?;
        Ok(Self { input, output, calibration })
    }
}

/// Uploaded immutable ICC calibration LUT.
pub struct GpuDisplayCalibrationLut {
    profile_fingerprint: IccProfileFingerprint,
    edge_size: u16,
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// Input/LUT bindings prepared for one calibration plan.
pub struct GpuDisplayCalibrationPreparedPass {
    input_contract: crate::GpuColorFrameContract,
    profile_fingerprint: IccProfileFingerprint,
    edge_size: u16,
    bind_group: wgpu::BindGroup,
}

/// Device-owned monitor calibration pipeline.
pub struct GpuDisplayCalibrationPipeline {
    output_texture_format: GpuColorFrameTextureFormat,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl GpuDisplayCalibrationPipeline {
    /// Build a calibration pipeline for one float render-target format.
    pub fn new(
        device: &wgpu::Device,
        output_texture_format: GpuColorFrameTextureFormat,
    ) -> Result<Self, GpuDisplayCalibrationPipelineError> {
        if !matches!(
            output_texture_format,
            GpuColorFrameTextureFormat::Rgba16Float | GpuColorFrameTextureFormat::Rgba32Float
        ) {
            return Err(
                GpuDisplayCalibrationPipelineError::UnsupportedOutputTextureFormat(
                    output_texture_format,
                ),
            );
        }
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.display-calibration.shader"),
            source: wgpu::ShaderSource::Wgsl(DISPLAY_CALIBRATION_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.display-calibration.bindings"),
            entries: &[
                nonfilterable_texture_binding(0, wgpu::TextureViewDimension::D2),
                nonfilterable_texture_binding(1, wgpu::TextureViewDimension::D3),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.display-calibration.layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.display-calibration.pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: output_texture_format.to_wgpu(),
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Ok(Self { output_texture_format, pipeline, bind_group_layout })
    }

    /// Upload one immutable RGBA32F 3D calibration texture.
    pub fn upload_lut(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        calibration: &DisplayCalibrationLut3d,
    ) -> GpuDisplayCalibrationLut {
        let edge = u32::from(calibration.edge_size);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian.display-calibration.lut"),
            size: wgpu::Extent3d {
                width: edge,
                height: edge,
                depth_or_array_layers: edge,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let (bytes, bytes_per_row) = pack_lut_upload(calibration);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(edge),
            },
            wgpu::Extent3d {
                width: edge,
                height: edge,
                depth_or_array_layers: edge,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("mondrian.display-calibration.lut-view"),
            dimension: Some(wgpu::TextureViewDimension::D3),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..wgpu::TextureViewDescriptor::default()
        });
        GpuDisplayCalibrationLut {
            profile_fingerprint: calibration.profile_fingerprint,
            edge_size: calibration.edge_size,
            _texture: texture,
            view,
        }
    }

    /// Allocate the device-RGB output declared by a calibration plan.
    pub fn allocate_output(
        device: &wgpu::Device,
        plan: &GpuDisplayCalibrationPlan,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        GpuColorFrameUploader::allocate(
            device,
            &GpuColorFrameAllocationPlan::for_handle(plan.output.clone()),
        )
    }

    /// Bind an encoded display input and its exact calibration LUT.
    pub fn prepare_pass(
        &self,
        device: &wgpu::Device,
        plan: &GpuDisplayCalibrationPlan,
        input_view: &wgpu::TextureView,
        lut: &GpuDisplayCalibrationLut,
    ) -> Result<GpuDisplayCalibrationPreparedPass, GpuDisplayCalibrationPrepareError> {
        if lut.profile_fingerprint != plan.calibration.profile_fingerprint
            || lut.edge_size != plan.calibration.edge_size
        {
            return Err(GpuDisplayCalibrationPrepareError::LutContractMismatch);
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.display-calibration.bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&lut.view),
                },
            ],
        });
        Ok(GpuDisplayCalibrationPreparedPass {
            input_contract: plan.input.contract(),
            profile_fingerprint: lut.profile_fingerprint,
            edge_size: lut.edge_size,
            bind_group,
        })
    }

    /// Record one monitor calibration draw.
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        plan: &GpuDisplayCalibrationPlan,
        prepared: &GpuDisplayCalibrationPreparedPass,
        output: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
    ) -> Result<(), GpuDisplayCalibrationRecordError> {
        if self.output_texture_format != plan.output.texture_format() {
            return Err(GpuDisplayCalibrationRecordError::PipelineOutputFormatMismatch);
        }
        if prepared.input_contract != plan.input.contract()
            || prepared.profile_fingerprint != plan.calibration.profile_fingerprint
            || prepared.edge_size != plan.calibration.edge_size
        {
            return Err(GpuDisplayCalibrationRecordError::PreparedPassMismatch);
        }
        if output.handle().contract() != plan.output.contract() {
            return Err(GpuDisplayCalibrationRecordError::OutputContractMismatch);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.display-calibration.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output.resource().texture_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &prepared.bind_group, &[]);
        pass.draw(0..4, 0..1);
        Ok(())
    }
}

/// Calibration planning failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GpuDisplayCalibrationPlanError {
    /// Input is not an encoded GPU display frame matching the LUT source.
    #[error("invalid display calibration input contract {actual:?}")]
    InvalidInputContract { actual: ColorFrameDescriptor },
    /// Input storage cannot carry float display values.
    #[error("unsupported display calibration input texture {0:?}")]
    UnsupportedInputTextureFormat(GpuColorFrameTextureFormat),
    /// Output storage cannot carry float device values.
    #[error("unsupported display calibration output texture {0:?}")]
    UnsupportedOutputTextureFormat(GpuColorFrameTextureFormat),
    /// Output handle validation failed.
    #[error("invalid display calibration output handle: {0}")]
    OutputHandle(GpuColorFrameHandleError),
}

/// Pipeline construction failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GpuDisplayCalibrationPipelineError {
    /// Output storage cannot carry float device values.
    #[error("unsupported display calibration pipeline output texture {0:?}")]
    UnsupportedOutputTextureFormat(GpuColorFrameTextureFormat),
}

/// Binding preparation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GpuDisplayCalibrationPrepareError {
    /// Uploaded LUT does not match the plan.
    #[error("uploaded display calibration LUT does not match the plan")]
    LutContractMismatch,
}

/// Calibration recording failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GpuDisplayCalibrationRecordError {
    /// Pipeline format differs from the planned device output.
    #[error("display calibration pipeline output format does not match the plan")]
    PipelineOutputFormatMismatch,
    /// Prepared bindings differ from the plan.
    #[error("prepared display calibration pass does not match the plan")]
    PreparedPassMismatch,
    /// Materialized output differs from the plan.
    #[error("display calibration output resource does not match the plan")]
    OutputContractMismatch,
}

fn nonfilterable_texture_binding(
    binding: u32,
    view_dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn pack_lut_upload(calibration: &DisplayCalibrationLut3d) -> (Vec<u8>, u32) {
    let edge = usize::from(calibration.edge_size);
    let source_row_bytes = edge * 4 * size_of::<f32>();
    let bytes_per_row = source_row_bytes.div_ceil(256) * 256;
    let mut bytes = vec![0_u8; bytes_per_row * edge * edge];
    for layer in 0..edge {
        for row in 0..edge {
            let source_start = (layer * edge + row) * edge * 4;
            let destination_start = (layer * edge + row) * bytes_per_row;
            for component in 0..edge * 4 {
                let destination = destination_start + component * size_of::<f32>();
                bytes[destination..destination + 4].copy_from_slice(
                    &calibration.samples()[source_start + component].to_le_bytes(),
                );
            }
        }
    }
    (bytes, bytes_per_row as u32)
}

const DISPLAY_CALIBRATION_SHADER: &str = r#"
@group(0) @binding(0) var input_texture: texture_2d<f32>;
@group(0) @binding(1) var calibration_lut: texture_3d<f32>;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return output;
}

fn calibration_sample(rgb: vec3<f32>) -> vec3<f32> {
    let dimensions = vec3<i32>(textureDimensions(calibration_lut));
    let coordinate = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0))
        * vec3<f32>(dimensions - vec3<i32>(1));
    let lower = vec3<i32>(floor(coordinate));
    let upper = min(lower + vec3<i32>(1), dimensions - vec3<i32>(1));
    let fraction = fract(coordinate);

    let c000 = textureLoad(calibration_lut, vec3<i32>(lower.x, lower.y, lower.z), 0).rgb;
    let c100 = textureLoad(calibration_lut, vec3<i32>(upper.x, lower.y, lower.z), 0).rgb;
    let c010 = textureLoad(calibration_lut, vec3<i32>(lower.x, upper.y, lower.z), 0).rgb;
    let c110 = textureLoad(calibration_lut, vec3<i32>(upper.x, upper.y, lower.z), 0).rgb;
    let c001 = textureLoad(calibration_lut, vec3<i32>(lower.x, lower.y, upper.z), 0).rgb;
    let c101 = textureLoad(calibration_lut, vec3<i32>(upper.x, lower.y, upper.z), 0).rgb;
    let c011 = textureLoad(calibration_lut, vec3<i32>(lower.x, upper.y, upper.z), 0).rgb;
    let c111 = textureLoad(calibration_lut, vec3<i32>(upper.x, upper.y, upper.z), 0).rgb;
    let z0 = mix(mix(c000, c100, fraction.x), mix(c010, c110, fraction.x), fraction.y);
    let z1 = mix(mix(c001, c101, fraction.x), mix(c011, c111, fraction.x), fraction.y);
    return mix(z0, z1, fraction.z);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(input.position.xy));
    let encoded = textureLoad(input_texture, pixel, 0);
    return vec4<f32>(calibration_sample(encoded.rgb), encoded.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::display_calibration::DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE;
    use mondrian_core::types::ColorSpace;

    #[test]
    fn plan_produces_typed_device_rgb_output() {
        let calibration = Arc::new(identity_calibration());
        let input = input_handle(1, calibration.source_color_space);
        let mut ids = GpuColorFrameIdAllocator::new(2);

        let plan = GpuDisplayCalibrationPlan::new(
            &mut ids,
            input,
            calibration.clone(),
            GpuColorFrameTextureFormat::Rgba32Float,
        )
        .expect("calibration plan");

        assert_eq!(
            plan.output.descriptor().color_space,
            ColorFrameSpace::Device(calibration.profile_fingerprint.calibration_key())
        );
        assert_eq!(
            plan.output.descriptor().encoding,
            ColorFrameEncoding::DeviceFloat
        );
    }

    #[tokio::test]
    async fn gpu_calibration_matches_cpu_trilinear_reference() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping display calibration GPU test: no adapter available");
            return;
        };
        let calibration = Arc::new(identity_calibration());
        let input_handle = input_handle(10, calibration.source_color_space);
        let input = GpuColorFrameUploader::allocate(
            &context.device,
            &GpuColorFrameAllocationPlan::for_handle(input_handle.clone()),
        );
        let source = [0.1_f32, 0.5, 0.9, 0.25, 1.0, 0.0, 0.33, 0.75];
        let source_bytes = source.iter().flat_map(|value| value.to_le_bytes()).collect::<Vec<_>>();
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &source_bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(2 * 4 * size_of::<f32>() as u32),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 2, height: 1, depth_or_array_layers: 1 },
        );

        let mut ids = GpuColorFrameIdAllocator::new(11);
        let plan = GpuDisplayCalibrationPlan::new(
            &mut ids,
            input_handle,
            calibration.clone(),
            GpuColorFrameTextureFormat::Rgba32Float,
        )
        .expect("calibration plan");
        let pipeline = GpuDisplayCalibrationPipeline::new(
            &context.device,
            GpuColorFrameTextureFormat::Rgba32Float,
        )
        .expect("calibration pipeline");
        let lut = GpuDisplayCalibrationPipeline::upload_lut(
            &context.device,
            &context.queue,
            &calibration,
        );
        let prepared = pipeline
            .prepare_pass(&context.device, &plan, &input.resource().texture_view, &lut)
            .expect("prepared calibration pass");
        let output = GpuDisplayCalibrationPipeline::allocate_output(&context.device, &plan);
        let readback_plan =
            crate::GpuColorFrameReadbackPlan::encoded_rgba32float(plan.output.clone())
                .expect("device output readback");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-display-calibration"),
        });
        pipeline
            .record(&mut encoder, &plan, &prepared, &output)
            .expect("record calibration");
        let readback = crate::GpuColorFrameReadback::record_copy(
            &context.device,
            &mut encoder,
            &readback_plan,
            output.resource(),
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_readback_buffer(&context.device, &readback);
        let actual =
            readback_plan.unpack_mapped_rgba32float(&mapped).expect("unpack device output");
        readback.unmap();

        for pixel in 0..2 {
            let base = pixel * 4;
            let expected =
                calibration.sample_trilinear([source[base], source[base + 1], source[base + 2]]);
            for channel in 0..3 {
                assert!((actual[base + channel] - expected[channel]).abs() < 1.0e-6);
            }
            assert!((actual[base + 3] - source[base + 3]).abs() < 1.0e-6);
        }
    }

    fn identity_calibration() -> DisplayCalibrationLut3d {
        let edge = DEFAULT_DISPLAY_CALIBRATION_LUT_EDGE;
        let mut samples = Vec::new();
        let denominator = f32::from(edge - 1);
        for blue in 0..edge {
            for green in 0..edge {
                for red in 0..edge {
                    samples.extend_from_slice(&[
                        f32::from(red) / denominator,
                        f32::from(green) / denominator,
                        f32::from(blue) / denominator,
                        1.0,
                    ]);
                }
            }
        }
        DisplayCalibrationLut3d::from_rgba32f_samples(
            ColorSpace::Srgb,
            IccProfileFingerprint::from_bytes(b"identity-calibration"),
            edge,
            samples,
        )
        .expect("identity calibration")
    }

    fn input_handle(id: u64, color_space: ColorSpace) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(id),
            ColorFrameDescriptor {
                width: 2,
                height: 1,
                color_space: ColorFrameSpace::Encoded(color_space),
                domain: ColorFrameDomain::Display,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "display-calibration-input",
        )
        .expect("input handle")
    }

    fn map_readback_buffer(device: &wgpu::Device, readback: &wgpu::Buffer) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        rx.recv().expect("readback map callback").expect("readback map success");
        slice.get_mapped_range().expect("mapped readback range").to_vec()
    }
}
