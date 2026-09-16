//! Renderer-owned native RGB texture materialization into encoded RGBA32F.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt;

use crate::{
    product_gpu_working_texture_format, GpuColorFrameAllocationPlan, GpuColorFrameHandle,
    GpuColorFrameResource, GpuColorFrameUploader, GpuColorFrameWgpuResource,
    GpuNativeDecodedFrameTextureFormat, GpuNativeRgbDecodePlan,
};

const RGB_MATERIALIZE_SHADER: &str = r#"
struct ExtentUniforms {
    source_output: vec4<u32>,
};

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> uniforms: ExtentUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex_index & 1u) << 1u) - 1.0;
    let y = f32(vertex_index & 2u) - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn load_source(coordinate: vec2<i32>, dimensions: vec2<i32>) -> vec4<f32> {
    return textureLoad(
        source_texture,
        clamp(coordinate, vec2<i32>(0), dimensions - vec2<i32>(1)),
        0,
    );
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let source_extent = vec2<f32>(uniforms.source_output.xy);
    let output_extent = vec2<f32>(uniforms.source_output.zw);
    let sample_coordinate = position.xy * source_extent / output_extent - vec2<f32>(0.5);
    let base = vec2<i32>(floor(sample_coordinate));
    let weight = fract(sample_coordinate);
    let dimensions = vec2<i32>(textureDimensions(source_texture));
    let p00 = load_source(base, dimensions);
    let p10 = load_source(base + vec2<i32>(1, 0), dimensions);
    let p01 = load_source(base + vec2<i32>(0, 1), dimensions);
    let p11 = load_source(base + vec2<i32>(1, 1), dimensions);
    return mix(mix(p00, p10, weight.x), mix(p01, p11, weight.x), weight.y);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RgbMaterializeUniforms {
    source_output: [u32; 4],
}

/// Prepared immutable binding for one imported RGB texture.
pub struct GpuNativeRgbPreparedPass {
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
    source_width: u32,
    source_height: u32,
    output: GpuColorFrameHandle,
    bind_group: wgpu::BindGroup,
    _uniform_buffer: wgpu::Buffer,
}

/// Shared RGB native-surface materializer used before the OCIO input stage.
pub struct GpuNativeRgbDecoder {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl GpuNativeRgbDecoder {
    /// Create the device-owned RGB materialization pipeline.
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.shader"),
            source: wgpu::ShaderSource::Wgsl(RGB_MATERIALIZE_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.pipeline"),
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
                    format: product_gpu_working_texture_format().to_wgpu(),
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
        Self { pipeline, bind_group_layout }
    }

    /// Allocate the encoded RGBA32F output declared by the plan.
    pub fn allocate_output(
        device: &wgpu::Device,
        plan: &GpuNativeRgbDecodePlan,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        GpuColorFrameUploader::allocate(
            device,
            &GpuColorFrameAllocationPlan::for_handle(plan.encoded_source_frame.clone()),
        )
    }

    /// Validate and bind one platform RGB texture to a reusable plan.
    pub fn prepare_pass(
        &self,
        device: &wgpu::Device,
        plan: &GpuNativeRgbDecodePlan,
        source: &wgpu::Texture,
    ) -> Result<GpuNativeRgbPreparedPass, GpuNativeRgbPrepareError> {
        let expected_format = match plan.source_texture_format {
            GpuNativeDecodedFrameTextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            GpuNativeDecodedFrameTextureFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
            GpuNativeDecodedFrameTextureFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
            GpuNativeDecodedFrameTextureFormat::Rgba32Float => wgpu::TextureFormat::Rgba32Float,
            actual => return Err(GpuNativeRgbPrepareError::UnsupportedSourceFormat { actual }),
        };
        if source.width() != plan.source_width || source.height() != plan.source_height {
            return Err(GpuNativeRgbPrepareError::SourceExtentMismatch {
                expected_width: plan.source_width,
                expected_height: plan.source_height,
                actual_width: source.width(),
                actual_height: source.height(),
            });
        }
        if source.format() != expected_format {
            return Err(GpuNativeRgbPrepareError::SourceFormatMismatch {
                expected: expected_format,
                actual: source.format(),
            });
        }
        if !source.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING) {
            return Err(GpuNativeRgbPrepareError::MissingTextureBindingUsage);
        }
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output = plan.encoded_source_frame.descriptor();
        let uniforms = RgbMaterializeUniforms {
            source_output: [
                plan.source_width,
                plan.source_height,
                output.width,
                output.height,
            ],
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.uniforms"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        Ok(GpuNativeRgbPreparedPass {
            source_texture_format: plan.source_texture_format,
            source_width: plan.source_width,
            source_height: plan.source_height,
            output: plan.encoded_source_frame.clone(),
            bind_group,
            _uniform_buffer: uniform_buffer,
        })
    }

    /// Record native RGB scaling/materialization without changing source values or alpha.
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        plan: &GpuNativeRgbDecodePlan,
        prepared: &GpuNativeRgbPreparedPass,
        output: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
    ) -> Result<(), GpuNativeRgbDecodeRecordError> {
        if prepared.source_texture_format != plan.source_texture_format
            || prepared.source_width != plan.source_width
            || prepared.source_height != plan.source_height
            || prepared.output != plan.encoded_source_frame
        {
            return Err(GpuNativeRgbDecodeRecordError::PreparedPlanMismatch);
        }
        if output.handle() != &plan.encoded_source_frame {
            return Err(GpuNativeRgbDecodeRecordError::OutputHandleMismatch);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.native-video.rgb-materialize.pass"),
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

/// Error returned before an imported RGB texture enters renderer execution.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeRgbPrepareError {
    /// The RGB decoder was given a YCbCr source plan.
    #[error("native RGB preparation does not support source format {actual:?}")]
    UnsupportedSourceFormat {
        /// Planned logical decoder-surface format.
        actual: GpuNativeDecodedFrameTextureFormat,
    },
    /// The platform texture extent differs from the import contract.
    #[error(
        "native RGB texture extent {actual_width}x{actual_height} does not match {expected_width}x{expected_height}"
    )]
    SourceExtentMismatch {
        /// Planned source width.
        expected_width: u32,
        /// Planned source height.
        expected_height: u32,
        /// Imported source width.
        actual_width: u32,
        /// Imported source height.
        actual_height: u32,
    },
    /// The platform texture format differs from the exact logical format.
    #[error("native RGB texture format {actual:?} does not match {expected:?}")]
    SourceFormatMismatch {
        /// Required wgpu texture format.
        expected: wgpu::TextureFormat,
        /// Actual imported wgpu texture format.
        actual: wgpu::TextureFormat,
    },
    /// Shader sampling requires texture-binding usage.
    #[error("native RGB texture is missing TEXTURE_BINDING usage")]
    MissingTextureBindingUsage,
}

/// Error returned while recording direct native RGB materialization.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeRgbDecodeRecordError {
    /// Prepared bindings were built for another plan.
    #[error("prepared native RGB bindings do not match the requested plan")]
    PreparedPlanMismatch,
    /// Output resource identity or contract differs from the plan.
    #[error("native RGB output resource does not match the requested plan")]
    OutputHandleMismatch,
}
