//! Renderer-owned native NV12/P010 to encoded RGB conversion.

use crate::{
    ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, GpuColorFrameAllocationPlan,
    GpuColorFrameContract, GpuColorFrameHandle, GpuColorFrameResource, GpuColorFrameTextureFormat,
    GpuColorFrameUploader, GpuColorFrameWgpuResource, GpuNativeDecodedFrameImportPlan,
    GpuNativeDecodedFrameTextureFormat, GpuNativeDecodedFrameVideoSampling, GpuVideoChromaLocation,
    GpuVideoRange,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::ColorMatrixCoefficients;
use wgpu::util::DeviceExt;

const YUV_DECODE_SHADER: &str = r#"
struct DecodeUniforms {
    extent: vec4<u32>,
    code_range: vec4<f32>,
    chroma_matrix0: vec4<f32>,
    matrix1: vec4<f32>,
};

@group(0) @binding(0) var luma_texture: texture_2d<f32>;
@group(0) @binding(1) var chroma_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> uniforms: DecodeUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> @builtin(position) vec4<f32> {
    let x = f32((vertex_index & 1u) << 1u) - 1.0;
    let y = f32(vertex_index & 2u) - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn load_chroma(coordinate: vec2<i32>, dimensions: vec2<i32>) -> vec2<f32> {
    let clamped = clamp(coordinate, vec2<i32>(0), dimensions - vec2<i32>(1));
    return textureLoad(chroma_texture, clamped, 0).rg;
}

fn sample_chroma(pixel: vec2<i32>) -> vec2<f32> {
    let source_center = vec2<f32>(pixel) + vec2<f32>(0.5);
    let sample_coordinate =
        (source_center - uniforms.chroma_matrix0.yz) * vec2<f32>(0.5);
    let base = vec2<i32>(floor(sample_coordinate));
    let weight = fract(sample_coordinate);
    let dimensions = vec2<i32>(textureDimensions(chroma_texture));
    let c00 = load_chroma(base, dimensions);
    let c10 = load_chroma(base + vec2<i32>(1, 0), dimensions);
    let c01 = load_chroma(base + vec2<i32>(0, 1), dimensions);
    let c11 = load_chroma(base + vec2<i32>(1, 1), dimensions);
    return mix(mix(c00, c10, weight.x), mix(c01, c11, weight.x), weight.y);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let y_code = textureLoad(luma_texture, pixel, 0).r * uniforms.code_range.x;
    let chroma_code = sample_chroma(pixel) * uniforms.code_range.x;
    let y = (y_code - uniforms.code_range.y) * uniforms.code_range.z;
    let chroma = (chroma_code - vec2<f32>(uniforms.code_range.w))
        * uniforms.chroma_matrix0.x;
    let cb = chroma.x;
    let cr = chroma.y;
    let rgb = vec3<f32>(
        y + uniforms.chroma_matrix0.w * cr,
        y + uniforms.matrix1.x * cb + uniforms.matrix1.y * cr,
        y + uniforms.matrix1.z * cb,
    );
    return vec4<f32>(rgb, 1.0);
}
"#;

/// Two-dimensional native video extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpuNativeVideoExtent {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Shader-visible plane views for one prepared NV12/P010 surface.
#[derive(Debug, Clone, Copy)]
pub struct GpuNativeYuvPlaneViews<'a> {
    /// Full-resolution luma plane.
    pub luma: &'a wgpu::TextureView,
    /// Half-resolution interleaved CbCr plane.
    pub chroma: &'a wgpu::TextureView,
}

/// Validated native YUV decode pass plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuNativeYuvDecodePlan {
    /// Native decoder texture layout.
    pub source_texture_format: GpuNativeDecodedFrameTextureFormat,
    /// Visible frame extent written to the encoded RGB output.
    pub visible_extent: GpuNativeVideoExtent,
    /// Codec-aligned decoder surface extent sampled by the shader.
    pub storage_extent: GpuNativeVideoExtent,
    /// Range, matrix, bit-depth, transfer, and siting contract.
    pub video_sampling: GpuNativeDecodedFrameVideoSampling,
    /// Encoded floating-point RGB output consumed by OCIO.
    pub output: GpuColorFrameHandle,
}

impl GpuNativeYuvDecodePlan {
    /// Build the decode pass that precedes a validated native import plan.
    pub fn from_import_plan(
        import: &GpuNativeDecodedFrameImportPlan,
        storage_extent: GpuNativeVideoExtent,
    ) -> Result<Self, GpuNativeYuvDecodePlanError> {
        let descriptor = import.encoded_source_frame.descriptor();
        Self::new(
            import.source_texture_format,
            GpuNativeVideoExtent { width: descriptor.width, height: descriptor.height },
            storage_extent,
            import.video_sampling,
            import.encoded_source_frame.clone(),
        )
    }

    /// Build and validate a native YUV decode plan.
    pub fn new(
        source_texture_format: GpuNativeDecodedFrameTextureFormat,
        visible_extent: GpuNativeVideoExtent,
        storage_extent: GpuNativeVideoExtent,
        video_sampling: GpuNativeDecodedFrameVideoSampling,
        output: GpuColorFrameHandle,
    ) -> Result<Self, GpuNativeYuvDecodePlanError> {
        if visible_extent.width == 0 || visible_extent.height == 0 {
            return Err(GpuNativeYuvDecodePlanError::EmptyVisibleExtent);
        }
        if storage_extent.width < visible_extent.width
            || storage_extent.height < visible_extent.height
        {
            return Err(GpuNativeYuvDecodePlanError::StorageSmallerThanVisible {
                visible: visible_extent,
                storage: storage_extent,
            });
        }
        if !storage_extent.width.is_multiple_of(2) || !storage_extent.height.is_multiple_of(2) {
            return Err(GpuNativeYuvDecodePlanError::OddStorageExtent { storage: storage_extent });
        }
        let expected_bit_depth = match source_texture_format {
            GpuNativeDecodedFrameTextureFormat::Nv12 => 8,
            GpuNativeDecodedFrameTextureFormat::P010 => 10,
            other => {
                return Err(GpuNativeYuvDecodePlanError::UnsupportedSourceFormat { format: other })
            }
        };
        if video_sampling.bit_depth != expected_bit_depth {
            return Err(GpuNativeYuvDecodePlanError::BitDepthMismatch {
                format: source_texture_format,
                expected: expected_bit_depth,
                actual: video_sampling.bit_depth,
            });
        }
        if !matches!(
            video_sampling.matrix,
            ColorMatrixCoefficients::Bt709 | ColorMatrixCoefficients::Bt2020NonConstant
        ) {
            return Err(GpuNativeYuvDecodePlanError::UnsupportedMatrix {
                matrix: video_sampling.matrix,
            });
        }
        if video_sampling.chroma_location == GpuVideoChromaLocation::Unspecified {
            return Err(GpuNativeYuvDecodePlanError::UnspecifiedChromaLocation);
        }
        let descriptor = output.descriptor();
        if descriptor.width != visible_extent.width
            || descriptor.height != visible_extent.height
            || descriptor.domain != ColorFrameDomain::Source
            || descriptor.encoding != ColorFrameEncoding::EncodedFloat
            || descriptor.residency != ColorFrameResidency::Gpu
            || output.texture_format() != GpuColorFrameTextureFormat::Rgba16Float
        {
            return Err(GpuNativeYuvDecodePlanError::InvalidOutputContract {
                actual: output.contract(),
            });
        }
        let source_encoding = descriptor.color_space.encoding();
        if video_sampling.matrix != source_encoding.matrix
            || video_sampling.transfer != source_encoding.transfer
        {
            return Err(GpuNativeYuvDecodePlanError::SamplingColorSpaceMismatch {
                source_color_space: descriptor.color_space,
                sampling_matrix: video_sampling.matrix,
                sampling_transfer: video_sampling.transfer,
            });
        }
        Ok(Self {
            source_texture_format,
            visible_extent,
            storage_extent,
            video_sampling,
            output,
        })
    }

    fn contract(&self) -> GpuNativeYuvDecodeContract {
        GpuNativeYuvDecodeContract {
            source_texture_format: self.source_texture_format,
            visible_extent: self.visible_extent,
            storage_extent: self.storage_extent,
            video_sampling: self.video_sampling,
            output: self.output.contract(),
        }
    }
}

/// Error returned when native YUV decode cannot be planned.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeYuvDecodePlanError {
    /// Visible output extent is empty.
    #[error("native YUV decode requires a non-empty visible extent")]
    EmptyVisibleExtent,
    /// Decoder storage is smaller than the visible frame.
    #[error("native YUV storage {storage:?} is smaller than visible extent {visible:?}")]
    StorageSmallerThanVisible {
        /// Visible frame extent.
        visible: GpuNativeVideoExtent,
        /// Decoder storage extent.
        storage: GpuNativeVideoExtent,
    },
    /// 4:2:0 storage dimensions must be even.
    #[error("native YUV 4:2:0 storage extent must be even, got {storage:?}")]
    OddStorageExtent {
        /// Invalid storage extent.
        storage: GpuNativeVideoExtent,
    },
    /// Source format is not a supported two-plane YUV layout.
    #[error("native YUV decode does not support source format {format:?}")]
    UnsupportedSourceFormat {
        /// Unsupported source texture format.
        format: GpuNativeDecodedFrameTextureFormat,
    },
    /// Effective coded depth conflicts with the native texture format.
    #[error("native YUV format {format:?} requires {expected}-bit values, got {actual}")]
    BitDepthMismatch {
        /// Native source format.
        format: GpuNativeDecodedFrameTextureFormat,
        /// Required bit depth.
        expected: u8,
        /// Reported bit depth.
        actual: u8,
    },
    /// Matrix coefficients cannot be converted by this pass.
    #[error("native YUV decode does not support matrix {matrix:?}")]
    UnsupportedMatrix {
        /// Unsupported matrix.
        matrix: ColorMatrixCoefficients,
    },
    /// Chroma siting must be explicit for subsampled video.
    #[error("native YUV decode requires explicit chroma location")]
    UnspecifiedChromaLocation,
    /// Output is not an encoded-float source Rgba16Float frame.
    #[error("native YUV decode output is not an encoded-float source Rgba16Float contract")]
    InvalidOutputContract {
        /// Actual output contract.
        actual: GpuColorFrameContract,
    },
    /// Sampling matrix/transfer conflicts with the encoded source color space.
    #[error(
        "native YUV sampling matrix {sampling_matrix:?} / transfer {sampling_transfer:?} does not match source color space {source_color_space:?}"
    )]
    SamplingColorSpaceMismatch {
        /// Encoded RGB source color space.
        source_color_space: mondrian_core::types::ColorSpace,
        /// Matrix carried by the native sampling contract.
        sampling_matrix: ColorMatrixCoefficients,
        /// Transfer carried by the native sampling contract.
        sampling_transfer: mondrian_core::ColorTransferCharacteristic,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GpuNativeYuvDecodeContract {
    source_texture_format: GpuNativeDecodedFrameTextureFormat,
    visible_extent: GpuNativeVideoExtent,
    storage_extent: GpuNativeVideoExtent,
    video_sampling: GpuNativeDecodedFrameVideoSampling,
    output: GpuColorFrameContract,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct GpuNativeYuvDecodeUniforms {
    extent: [u32; 4],
    code_range: [f32; 4],
    chroma_matrix0: [f32; 4],
    matrix1: [f32; 4],
}

impl GpuNativeYuvDecodeUniforms {
    fn from_plan(plan: &GpuNativeYuvDecodePlan) -> Self {
        let bit_scale = (1u32 << (plan.video_sampling.bit_depth - 8)) as f32;
        let max_code = ((1u32 << plan.video_sampling.bit_depth) - 1) as f32;
        let code_scale = match plan.source_texture_format {
            GpuNativeDecodedFrameTextureFormat::Nv12 => 255.0,
            GpuNativeDecodedFrameTextureFormat::P010 => 65_535.0 / 64.0,
            _ => unreachable!("plan validation restricts native YUV source formats"),
        };
        let (y_offset, y_scale, chroma_offset, chroma_scale) = match plan.video_sampling.range {
            GpuVideoRange::Limited => (
                16.0 * bit_scale,
                1.0 / (219.0 * bit_scale),
                128.0 * bit_scale,
                1.0 / (224.0 * bit_scale),
            ),
            GpuVideoRange::Full => (0.0, 1.0 / max_code, 128.0 * bit_scale, 1.0 / max_code),
        };
        let chroma_origin = match plan.video_sampling.chroma_location {
            GpuVideoChromaLocation::Left => [0.5, 1.0],
            GpuVideoChromaLocation::Center => [1.0, 1.0],
            GpuVideoChromaLocation::TopLeft => [0.5, 0.5],
            GpuVideoChromaLocation::Unspecified => {
                unreachable!("plan validation requires explicit chroma location")
            }
        };
        let (kr, kb) = match plan.video_sampling.matrix {
            ColorMatrixCoefficients::Bt709 => (0.2126, 0.0722),
            ColorMatrixCoefficients::Bt2020NonConstant => (0.2627, 0.0593),
            _ => unreachable!("plan validation restricts native YUV matrices"),
        };
        let kg = 1.0 - kr - kb;
        let r_cr = 2.0 * (1.0 - kr);
        let b_cb = 2.0 * (1.0 - kb);
        let g_cb = -2.0 * kb * (1.0 - kb) / kg;
        let g_cr = -2.0 * kr * (1.0 - kr) / kg;
        Self {
            extent: [
                plan.visible_extent.width,
                plan.visible_extent.height,
                plan.storage_extent.width,
                plan.storage_extent.height,
            ],
            code_range: [code_scale, y_offset, y_scale, chroma_offset],
            chroma_matrix0: [chroma_scale, chroma_origin[0], chroma_origin[1], r_cr],
            matrix1: [g_cb, g_cr, b_cb, 0.0],
        }
    }

    #[cfg(test)]
    fn decode_code_values(self, y_code: f32, cb_code: f32, cr_code: f32) -> [f32; 3] {
        let y = (y_code - self.code_range[1]) * self.code_range[2];
        let cb = (cb_code - self.code_range[3]) * self.chroma_matrix0[0];
        let cr = (cr_code - self.code_range[3]) * self.chroma_matrix0[0];
        [
            y + self.chroma_matrix0[3] * cr,
            y + self.matrix1[0] * cb + self.matrix1[1] * cr,
            y + self.matrix1[2] * cb,
        ]
    }
}

/// Prepared immutable bindings for a reusable native YUV surface and plan.
pub struct GpuNativeYuvPreparedPass {
    contract: GpuNativeYuvDecodeContract,
    bind_group: wgpu::BindGroup,
    _uniform_buffer: wgpu::Buffer,
}

/// Renderer runtime for native YUV to encoded-float RGB conversion.
pub struct GpuNativeYuvDecoder {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl GpuNativeYuvDecoder {
    /// Create the device-owned shader pipeline. The pipeline is format-stable
    /// and can serve both NV12 and P010 plans.
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.native-video.yuv-decode.shader"),
            source: wgpu::ShaderSource::Wgsl(YUV_DECODE_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.native-video.yuv-decode.bindings"),
            entries: &[
                texture_binding(0),
                texture_binding(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
            label: Some("mondrian.native-video.yuv-decode.layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.native-video.yuv-decode.pipeline"),
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
                    format: wgpu::TextureFormat::Rgba16Float,
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

    /// Allocate the encoded-float output resource declared by a plan.
    pub fn allocate_output(
        device: &wgpu::Device,
        plan: &GpuNativeYuvDecodePlan,
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        GpuColorFrameUploader::allocate(
            device,
            &GpuColorFrameAllocationPlan::for_handle(plan.output.clone()),
        )
    }

    /// Prepare immutable plane bindings and conversion constants for reuse
    /// across frames with the same decoder surface contract.
    pub fn prepare_pass(
        &self,
        device: &wgpu::Device,
        plan: &GpuNativeYuvDecodePlan,
        planes: GpuNativeYuvPlaneViews<'_>,
    ) -> GpuNativeYuvPreparedPass {
        let uniforms = GpuNativeYuvDecodeUniforms::from_plan(plan);
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.native-video.yuv-decode.uniforms"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.native-video.yuv-decode.bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(planes.luma),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(planes.chroma),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        GpuNativeYuvPreparedPass {
            contract: plan.contract(),
            bind_group,
            _uniform_buffer: uniform_buffer,
        }
    }

    /// Record one decode draw into the plan's encoded source resource.
    pub fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        plan: &GpuNativeYuvDecodePlan,
        prepared: &GpuNativeYuvPreparedPass,
        output: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
    ) -> Result<(), GpuNativeYuvDecodeRecordError> {
        if prepared.contract != plan.contract() {
            return Err(GpuNativeYuvDecodeRecordError::PreparedPassMismatch);
        }
        if output.handle().contract() != plan.output.contract() {
            return Err(GpuNativeYuvDecodeRecordError::OutputContractMismatch {
                expected: plan.output.contract(),
                actual: output.handle().contract(),
            });
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.native-video.yuv-decode.pass"),
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

/// Error recording a prepared native YUV decode pass.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum GpuNativeYuvDecodeRecordError {
    /// Prepared bindings came from another plan/surface contract.
    #[error("prepared native YUV pass does not match the decode plan")]
    PreparedPassMismatch,
    /// Render target does not match the plan's encoded source frame.
    #[error("native YUV output resource contract does not match the decode plan")]
    OutputContractMismatch {
        /// Planned target contract.
        expected: GpuColorFrameContract,
        /// Actual target contract.
        actual: GpuColorFrameContract,
    },
}

fn texture_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ColorFrameDescriptor, GpuColorFrameId, GpuColorFrameIdAllocator,
        GpuNativeDecodedFrameImportContract, GpuNativeDecodedFrameImportSupport,
    };
    use mondrian_core::types::{ColorEngine, ColorSpace};
    use mondrian_core::ColorTransferCharacteristic;
    use mondrian_media::DecodedGpuFrameHandleKind;

    #[test]
    fn limited_range_black_and_white_decode_exactly() {
        let plan = decode_plan(
            GpuNativeDecodedFrameTextureFormat::Nv12,
            GpuVideoRange::Limited,
            ColorMatrixCoefficients::Bt709,
            8,
            GpuVideoChromaLocation::Left,
        );
        let uniforms = GpuNativeYuvDecodeUniforms::from_plan(&plan);
        assert_rgb_close(uniforms.decode_code_values(16.0, 128.0, 128.0), [0.0; 3]);
        assert_rgb_close(uniforms.decode_code_values(235.0, 128.0, 128.0), [1.0; 3]);
    }

    #[test]
    fn p010_normalization_recovers_ten_bit_code_values() {
        let plan = decode_plan(
            GpuNativeDecodedFrameTextureFormat::P010,
            GpuVideoRange::Limited,
            ColorMatrixCoefficients::Bt2020NonConstant,
            10,
            GpuVideoChromaLocation::TopLeft,
        );
        let uniforms = GpuNativeYuvDecodeUniforms::from_plan(&plan);
        let sampled_white = 940.0 * 64.0 / 65_535.0;
        let recovered = sampled_white * uniforms.code_range[0];
        assert!((recovered - 940.0).abs() < 1.0e-5);
        assert_rgb_close(
            uniforms.decode_code_values(recovered, 512.0, 512.0),
            [1.0; 3],
        );
    }

    #[test]
    fn matrix_coefficients_match_bt709_reference_red() {
        let plan = decode_plan(
            GpuNativeDecodedFrameTextureFormat::Nv12,
            GpuVideoRange::Full,
            ColorMatrixCoefficients::Bt709,
            8,
            GpuVideoChromaLocation::Center,
        );
        let uniforms = GpuNativeYuvDecodeUniforms::from_plan(&plan);
        let kr = 0.2126_f32;
        let kb = 0.0722_f32;
        let y = kr * 255.0;
        let cb = (128.0 - 0.5 * kr / (1.0 - kb) * 255.0).round();
        let cr = (128.0_f32 + 0.5 * 255.0).round();
        let decoded = uniforms.decode_code_values(y, cb, cr);
        assert!((decoded[0] - 1.0).abs() < 0.01);
        assert!(decoded[1].abs() < 0.01);
        assert!(decoded[2].abs() < 0.01);
    }

    #[test]
    fn import_plan_has_distinct_encoded_and_working_frames() {
        let mut ids = GpuColorFrameIdAllocator::new(40);
        let import = GpuNativeDecodedFrameImportPlan::from_contract(
            &mut ids,
            import_contract(),
            &GpuNativeDecodedFrameImportSupport::ready(
                vec![DecodedGpuFrameHandleKind::D3D11Texture2D],
                vec![GpuNativeDecodedFrameTextureFormat::P010],
            ),
        )
        .expect("valid native import plan");
        let plan = GpuNativeYuvDecodePlan::from_import_plan(
            &import,
            GpuNativeVideoExtent { width: 1920, height: 1088 },
        )
        .expect("valid YUV decode plan");

        assert_eq!(plan.output, import.encoded_source_frame);
        assert_eq!(plan.output.id().raw(), 40);
        assert_eq!(import.working_frame.id().raw(), 41);
        assert_eq!(
            plan.output.descriptor().encoding,
            ColorFrameEncoding::EncodedFloat
        );
        assert_eq!(
            import.working_frame.descriptor().encoding,
            ColorFrameEncoding::LinearFloat
        );
    }

    #[test]
    fn decode_plan_rejects_linear_output_contract() {
        let output = GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(1),
            ColorFrameDescriptor {
                width: 1920,
                height: 1080,
                color_space: ColorSpace::Rec2100Pq,
                domain: ColorFrameDomain::Source,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
            },
            GpuColorFrameTextureFormat::Rgba16Float,
            "invalid-linear-source",
        )
        .expect("handle structure is independently valid");
        let error = GpuNativeYuvDecodePlan::new(
            GpuNativeDecodedFrameTextureFormat::P010,
            GpuNativeVideoExtent { width: 1920, height: 1080 },
            GpuNativeVideoExtent { width: 1920, height: 1088 },
            sampling(
                GpuVideoRange::Limited,
                ColorMatrixCoefficients::Bt2020NonConstant,
                10,
                GpuVideoChromaLocation::Left,
            ),
            output,
        )
        .expect_err("encoded YUV must not be labeled linear");
        assert!(matches!(
            error,
            GpuNativeYuvDecodePlanError::InvalidOutputContract { .. }
        ));
    }

    #[tokio::test]
    async fn nv12_shader_decodes_limited_range_on_real_wgpu_device() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping native YUV shader test: no GPU adapter available");
            return;
        };
        let luma = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian-test-native-yuv-luma"),
            size: wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let chroma = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian-test-native-yuv-chroma"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rg8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_test_texture(&context.queue, &luma, 2, 2, 2, &[16, 235, 126, 71]);
        write_test_texture(&context.queue, &chroma, 1, 1, 2, &[128, 128]);
        let output_handle = GpuColorFrameHandle::new(
            GpuColorFrameId::from_raw(80),
            ColorFrameDescriptor {
                width: 2,
                height: 2,
                color_space: ColorSpace::Rec709,
                domain: ColorFrameDomain::Source,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
            },
            GpuColorFrameTextureFormat::Rgba16Float,
            "native-yuv-shader-output",
        )
        .expect("encoded output handle");
        let plan = GpuNativeYuvDecodePlan::new(
            GpuNativeDecodedFrameTextureFormat::Nv12,
            GpuNativeVideoExtent { width: 2, height: 2 },
            GpuNativeVideoExtent { width: 2, height: 2 },
            GpuNativeDecodedFrameVideoSampling {
                range: GpuVideoRange::Limited,
                matrix: ColorMatrixCoefficients::Bt709,
                transfer: ColorTransferCharacteristic::Bt709,
                bit_depth: 8,
                chroma_location: GpuVideoChromaLocation::Left,
            },
            output_handle,
        )
        .expect("valid shader decode plan");
        let decoder = GpuNativeYuvDecoder::new(&context.device);
        let output = GpuNativeYuvDecoder::allocate_output(&context.device, &plan);
        let luma_view = luma.create_view(&wgpu::TextureViewDescriptor::default());
        let chroma_view = chroma.create_view(&wgpu::TextureViewDescriptor::default());
        let prepared = decoder.prepare_pass(
            &context.device,
            &plan,
            GpuNativeYuvPlaneViews { luma: &luma_view, chroma: &chroma_view },
        );
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-native-yuv-decode"),
        });
        decoder
            .record(&mut encoder, &plan, &prepared, &output)
            .expect("record YUV decode");
        let readback_plan =
            crate::GpuColorFrameReadbackPlan::encoded_rgba16float(output.handle().clone())
                .expect("Rgba16Float readback plan");
        let readback = crate::GpuColorFrameReadback::record_copy(
            &context.device,
            &mut encoder,
            &readback_plan,
            output.resource(),
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_readback_buffer(&context.device, &readback);
        let actual =
            readback_plan.unpack_mapped_rgba16float(&mapped).expect("unpack shader output");
        readback.unmap();

        let expected_luma = [0.0, 1.0, 110.0 / 219.0, 55.0 / 219.0];
        for (pixel, expected) in actual.chunks_exact(4).zip(expected_luma) {
            for channel in &pixel[..3] {
                assert!(
                    (*channel - expected).abs() < 0.0015,
                    "expected neutral {expected}, got {pixel:?}"
                );
            }
            assert!((pixel[3] - 1.0).abs() < 0.001);
        }
    }

    fn decode_plan(
        format: GpuNativeDecodedFrameTextureFormat,
        range: GpuVideoRange,
        matrix: ColorMatrixCoefficients,
        bit_depth: u8,
        chroma_location: GpuVideoChromaLocation,
    ) -> GpuNativeYuvDecodePlan {
        let (color_space, transfer) = match matrix {
            ColorMatrixCoefficients::Bt709 => {
                (ColorSpace::Rec709, ColorTransferCharacteristic::Bt709)
            }
            ColorMatrixCoefficients::Bt2020NonConstant => {
                (ColorSpace::Rec2100Pq, ColorTransferCharacteristic::Pq)
            }
            _ => panic!("test helper requires a supported YUV matrix"),
        };
        GpuNativeYuvDecodePlan::new(
            format,
            GpuNativeVideoExtent { width: 1920, height: 1080 },
            GpuNativeVideoExtent { width: 1920, height: 1088 },
            GpuNativeDecodedFrameVideoSampling {
                range,
                matrix,
                transfer,
                bit_depth,
                chroma_location,
            },
            GpuColorFrameHandle::new(
                GpuColorFrameId::from_raw(1),
                ColorFrameDescriptor {
                    width: 1920,
                    height: 1080,
                    color_space,
                    domain: ColorFrameDomain::Source,
                    encoding: ColorFrameEncoding::EncodedFloat,
                    residency: ColorFrameResidency::Gpu,
                },
                GpuColorFrameTextureFormat::Rgba16Float,
                "encoded-source",
            )
            .expect("encoded source handle"),
        )
        .expect("valid YUV decode plan")
    }

    fn sampling(
        range: GpuVideoRange,
        matrix: ColorMatrixCoefficients,
        bit_depth: u8,
        chroma_location: GpuVideoChromaLocation,
    ) -> GpuNativeDecodedFrameVideoSampling {
        GpuNativeDecodedFrameVideoSampling {
            range,
            matrix,
            transfer: ColorTransferCharacteristic::Pq,
            bit_depth,
            chroma_location,
        }
    }

    fn import_contract() -> GpuNativeDecodedFrameImportContract {
        GpuNativeDecodedFrameImportContract {
            width: 1920,
            height: 1080,
            source_color_space: ColorSpace::Rec2100Pq,
            input_transform: crate::RenderInputTransform::to_working_gpu(
                ColorSpace::Rec2020,
                true,
                ColorEngine::MondrianSmart,
            ),
            handle_kind: DecodedGpuFrameHandleKind::D3D11Texture2D,
            source_texture_format: GpuNativeDecodedFrameTextureFormat::P010,
            video_sampling: GpuNativeDecodedFrameVideoSampling::from_source_color_space(
                ColorSpace::Rec2100Pq,
                GpuVideoRange::Limited,
                10,
                GpuVideoChromaLocation::Left,
            ),
            working_texture_format: GpuColorFrameTextureFormat::Rgba16Float,
            label: "native-working".to_owned(),
        }
    }

    fn assert_rgb_close(actual: [f32; 3], expected: [f32; 3]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1.0e-5, "{actual} != {expected}");
        }
    }

    fn write_test_texture(
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
