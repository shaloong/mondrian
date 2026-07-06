//! GPU-resident working-space compositing.
//!
//! This module owns the native wgpu path for preview/playback compositing when
//! a layer stack is simple enough to stay on GPU: affine transforms, Normal
//! blend mode, no effect graphs, and a bounded layer count. Unsupported layer
//! shapes are rejected with typed blockers so callers can fall back to the CPU
//! reference compositor without losing diagnostic evidence.

use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    GpuColorFrameAllocationPlan, GpuColorFrameHandle, GpuColorFrameIdAllocator,
    GpuColorFrameResource, GpuColorFrameResourceTable, GpuColorFrameTextureFormat,
    GpuColorFrameUploadPlan, GpuColorFrameUploader, GpuColorFrameWgpuResource,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::types::{BlendMode, Color, ColorSpace};
use serde::{Deserialize, Serialize};
use wgpu::util::DeviceExt;

const MAX_GPU_COMPOSITE_LAYERS: usize = 5;
const GPU_COMPOSITOR_SHADER: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct CompositeUniforms {
    opacity: f32,
    source_kind: u32,
    _pad0: vec2<u32>,
    solid_color: vec4<f32>,
    inv_transform0: vec4<f32>,
    inv_transform1: vec4<f32>,
    geometry: vec4<f32>,
};

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(0) @binding(1) var accum_tex: texture_2d<f32>;
@group(0) @binding(2) var linear_sampler: sampler;
@group(1) @binding(0) var<uniform> uniforms: CompositeUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var uvs = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn over_straight_alpha(base_px: vec4<f32>, blend_px: vec4<f32>, opacity: f32) -> vec4<f32> {
    let base_alpha = clamp(base_px.a, 0.0, 1.0);
    let blend_alpha = clamp(blend_px.a * clamp(opacity, 0.0, 1.0), 0.0, 1.0);
    if (blend_alpha <= 0.0001) {
        return base_px;
    }
    if (base_alpha <= 0.0001) {
        return vec4<f32>(blend_px.rgb, blend_alpha);
    }
    let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
    if (out_alpha <= 0.0001) {
        return vec4<f32>(0.0);
    }
    let premul = blend_px.rgb * blend_alpha + base_px.rgb * base_alpha * (1.0 - blend_alpha);
    return vec4<f32>(premul / out_alpha, out_alpha);
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let base_px = textureSample(accum_tex, linear_sampler, in.uv);
    let layer_px = select(
        sample_layer(in.uv),
        uniforms.solid_color,
        uniforms.source_kind == 1u,
    );
    return over_straight_alpha(base_px, layer_px, uniforms.opacity);
}

fn sample_layer(dst_uv: vec2<f32>) -> vec4<f32> {
    let dst_center = vec2<f32>(
        dst_uv.x * uniforms.geometry.x,
        dst_uv.y * uniforms.geometry.y,
    );
    let src_center = vec2<f32>(
        uniforms.inv_transform0.x * dst_center.x +
            uniforms.inv_transform0.y * dst_center.y +
            uniforms.inv_transform0.z,
        uniforms.inv_transform0.w * dst_center.x +
            uniforms.inv_transform1.x * dst_center.y +
            uniforms.inv_transform1.y,
    );
    let src_size = uniforms.geometry.zw;
    if (src_center.x < 0.5 ||
        src_center.y < 0.5 ||
        src_center.x >= src_size.x + 0.5 ||
        src_center.y >= src_size.y + 0.5) {
        return vec4<f32>(0.0);
    }
    return textureSample(layer_tex, linear_sampler, src_center / src_size);
}
"#;

/// Classification of whether GPU compositing is possible for a set of layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuCompositingCapability {
    /// GPU compositing is possible: all layers are GPU-resident, use only
    /// supported blend modes, and have no effect graphs requiring CPU execution.
    GpuNative,
    /// GPU compositing is possible for the layer structure, but layers must
    /// be uploaded from CPU first. The composited result stays on GPU.
    GpuWithUpload,
    /// GPU compositing is not possible. Falls back to CPU compositing with
    /// upload of the final result.
    CpuFallback {
        /// First reason preventing GPU compositing.
        reason: GpuCompositingBlockerReason,
    },
}

/// Reasons GPU compositing cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GpuCompositingBlockerReason {
    /// An effect graph requires CPU execution or has not been lowered to a GPU shader.
    EffectRequiresCpu,
    /// A blend mode other than Normal is used.
    UnsupportedBlendMode,
    /// The layer has a transform the GPU compositor cannot sample correctly.
    UnsupportedTransform,
    /// A source frame is not already GPU-resident and uploads were disallowed.
    FrameNotGpuResident,
    /// Too many layers for the bounded GPU composite path.
    TooManyLayers,
    /// GPU compositor is not initialized (device/queue unavailable).
    GpuUnavailable,
}

impl GpuCompositingBlockerReason {
    /// Machine-readable code for diagnostics.
    pub fn code(&self) -> &'static str {
        match self {
            Self::EffectRequiresCpu => "effect_requires_cpu",
            Self::UnsupportedBlendMode => "unsupported_blend_mode",
            Self::UnsupportedTransform => "unsupported_transform",
            Self::FrameNotGpuResident => "frame_not_gpu_resident",
            Self::TooManyLayers => "too_many_layers",
            Self::GpuUnavailable => "gpu_unavailable",
        }
    }

    /// Human-readable description.
    pub fn description(&self) -> &'static str {
        match self {
            Self::EffectRequiresCpu => {
                "Effect graph requires CPU execution or has no GPU shader lowering"
            }
            Self::UnsupportedBlendMode => "Blend mode not supported by GPU compositor",
            Self::UnsupportedTransform => "Transform cannot be represented by GPU compositor",
            Self::FrameNotGpuResident => "Frame requires CPU-to-GPU upload before compositing",
            Self::TooManyLayers => "Too many layers for bounded GPU compositing",
            Self::GpuUnavailable => "GPU device/queue not available for compositing",
        }
    }
}

/// Structured diagnostics for GPU vs CPU compositing path selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositingDiagnostics {
    /// Number of compositing operations that used the GPU-native path.
    pub gpu_native_composites: u64,
    /// Number of compositing operations that uploaded CPU layers for GPU compositing.
    pub gpu_with_upload_composites: u64,
    /// Number of compositing operations that fell back to CPU compositing.
    pub cpu_fallback_composites: u64,
    /// Total pixels processed through GPU compositing.
    pub gpu_composited_pixels: u64,
    /// Total pixels processed through CPU compositing.
    pub cpu_composited_pixels: u64,
    /// First blocker reason observed (for health reports).
    pub first_blocker: Option<GpuCompositingBlockerReason>,
}

impl GpuCompositingDiagnostics {
    /// Accumulate another diagnostics snapshot.
    pub fn accumulate(&mut self, other: Self) {
        self.gpu_native_composites =
            self.gpu_native_composites.saturating_add(other.gpu_native_composites);
        self.gpu_with_upload_composites =
            self.gpu_with_upload_composites.saturating_add(other.gpu_with_upload_composites);
        self.cpu_fallback_composites =
            self.cpu_fallback_composites.saturating_add(other.cpu_fallback_composites);
        self.gpu_composited_pixels =
            self.gpu_composited_pixels.saturating_add(other.gpu_composited_pixels);
        self.cpu_composited_pixels =
            self.cpu_composited_pixels.saturating_add(other.cpu_composited_pixels);
        if self.first_blocker.is_none() {
            self.first_blocker = other.first_blocker;
        }
    }

    /// Whether any compositing used the GPU-native path.
    pub fn has_gpu_native(&self) -> bool {
        self.gpu_native_composites > 0
    }

    /// Whether any compositing fell back to CPU.
    pub fn has_cpu_fallback(&self) -> bool {
        self.cpu_fallback_composites > 0
    }
}

/// Evaluate GPU compositing capability for a set of timeline layers.
///
/// Returns the capability classification and structured diagnostics about
/// why GPU compositing is or is not possible.
pub fn evaluate_gpu_compositing_capability(
    layer_count: usize,
    has_any_effect_graph: bool,
    has_any_unsupported_transform: bool,
    has_any_non_normal_blend_mode: bool,
    all_frames_gpu_resident: bool,
) -> GpuCompositingCapability {
    if layer_count > MAX_GPU_COMPOSITE_LAYERS {
        return GpuCompositingCapability::CpuFallback {
            reason: GpuCompositingBlockerReason::TooManyLayers,
        };
    }
    if has_any_effect_graph {
        return GpuCompositingCapability::CpuFallback {
            reason: GpuCompositingBlockerReason::EffectRequiresCpu,
        };
    }
    if has_any_non_normal_blend_mode {
        return GpuCompositingCapability::CpuFallback {
            reason: GpuCompositingBlockerReason::UnsupportedBlendMode,
        };
    }
    if has_any_unsupported_transform {
        return GpuCompositingCapability::CpuFallback {
            reason: GpuCompositingBlockerReason::UnsupportedTransform,
        };
    }
    if all_frames_gpu_resident {
        GpuCompositingCapability::GpuNative
    } else {
        GpuCompositingCapability::GpuWithUpload
    }
}

/// Source layer accepted by the native GPU working-space compositor.
#[derive(Debug, Clone, Copy)]
pub enum GpuCompositeLayerSource<'a> {
    /// CPU working-space frame that will be uploaded to an Rgba32Float texture.
    CpuFrame(&'a CpuColorFrame),
    /// GPU-resident working-space frame that will be sampled directly.
    GpuFrame(&'a GpuColorFrameHandle),
    /// Solid working-space color drawn directly by shader uniform.
    SolidColor(Color),
}

/// One layer in a GPU working-space composite request.
#[derive(Debug, Clone, Copy)]
pub struct GpuCompositeLayer<'a> {
    /// Source pixels or procedural solid.
    pub source: GpuCompositeLayerSource<'a>,
    /// Straight-alpha layer opacity.
    pub opacity: f32,
    /// Timeline blend mode. Only `Normal` is accepted by the current GPU path.
    pub blend_mode: BlendMode,
    /// Timeline affine transform. Only identity is accepted by the current GPU path.
    pub transform: [f32; 6],
    /// Whether this layer has any effect graph work that is not already baked
    /// into the source frame.
    pub has_effect_graph: bool,
}

/// Request for recording a GPU working-space composite.
pub struct GpuCompositeRequest<'a> {
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Working color space of the composite output.
    pub working_color_space: ColorSpace,
    /// Layers in bottom-to-top order.
    pub layers: &'a [GpuCompositeLayer<'a>],
}

/// Result of recording a GPU working-space composite.
pub struct GpuCompositeRecord {
    /// GPU handle for the working-space composite texture.
    pub output: GpuColorFrameHandle,
    /// Diagnostics proving which compositing path executed.
    pub diagnostics: GpuCompositingDiagnostics,
}

/// Errors returned by native GPU working-space compositing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GpuCompositeError {
    /// Output dimensions must be non-zero.
    #[error("GPU composite output dimensions must be non-zero, got {width}x{height}")]
    EmptyExtent {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// A layer shape requires CPU fallback.
    #[error("GPU composite is blocked by {reason:?}")]
    Blocked {
        /// First blocker reason.
        reason: GpuCompositingBlockerReason,
    },
    /// A source frame has the wrong descriptor for this composite.
    #[error("GPU composite source frame descriptor mismatch")]
    SourceDescriptorMismatch {
        /// Expected descriptor.
        expected: ColorFrameDescriptor,
        /// Actual descriptor.
        actual: ColorFrameDescriptor,
    },
    /// A renderer frame handle could not be created.
    #[error("GPU composite output handle error: {0}")]
    OutputHandle(#[from] crate::GpuColorFrameHandleError),
    /// Uploading a CPU source layer failed.
    #[error("GPU composite layer upload error: {0:?}")]
    Upload(crate::GpuColorFrameUploadError),
    /// The shared resource table rejected an inserted resource.
    #[error("GPU composite resource table error: {0:?}")]
    ResourceTable(crate::GpuColorFrameResourceTableError),
}

/// Runtime for recording GPU working-space composites.
pub struct GpuFrameCompositor {
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    uniform_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuCompositeUniforms {
    opacity: f32,
    source_kind: u32,
    _pad0: [u32; 2],
    solid_color: [f32; 4],
    inv_transform0: [f32; 4],
    inv_transform1: [f32; 4],
    geometry: [f32; 4],
}

impl GpuFrameCompositor {
    /// Create a GPU working-space compositor runtime for a wgpu device.
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian_gpu_working_compositor_shader"),
            source: wgpu::ShaderSource::Wgsl(GPU_COMPOSITOR_SHADER.into()),
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_textures"),
            entries: &[
                texture_binding(0),
                texture_binding(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_layout"),
            bind_group_layouts: &[Some(&texture_layout), Some(&uniform_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian_gpu_working_compositor_pipeline"),
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
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mondrian_gpu_working_compositor_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        Self { pipeline, texture_layout, uniform_layout, sampler }
    }

    /// Record a GPU working-space composite into the supplied command encoder
    /// and resource table.
    pub fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        request: GpuCompositeRequest<'_>,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_request(&request)?;
        let width = request.width;
        let height = request.height;
        let output_descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space: request.working_color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let target_a =
            create_working_resource(device, ids, output_descriptor, "gpu-composite-accum-a")?;
        let target_b =
            create_working_resource(device, ids, output_descriptor, "gpu-composite-accum-b")?;
        clear_working_texture(
            encoder,
            &target_a.resource().texture_view,
            wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
        );

        let mut transient_uploads = Vec::new();
        let mut uploaded_cpu_layers = false;
        let mut src_is_a = true;
        for (index, layer) in request.layers.iter().enumerate() {
            let (accum, dst) = if src_is_a {
                (&target_a, &target_b)
            } else {
                (&target_b, &target_a)
            };
            let (layer_view, source_kind, solid_color, source_size) = match layer.source {
                GpuCompositeLayerSource::CpuFrame(frame) => {
                    uploaded_cpu_layers = true;
                    let descriptor = frame.descriptor();
                    let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
                        ids.allocate(),
                        frame,
                        GpuColorFrameTextureFormat::Rgba32Float,
                        format!("gpu-composite-layer-{index}"),
                    )
                    .map_err(GpuCompositeError::Upload)?;
                    let uploaded = GpuColorFrameUploader::upload(device, queue, &upload);
                    transient_uploads.push(uploaded);
                    (
                        &transient_uploads
                            .last()
                            .expect("uploaded layer just pushed")
                            .resource()
                            .texture_view,
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::GpuFrame(handle) => {
                    let resource = table.get(handle).map_err(GpuCompositeError::ResourceTable)?;
                    let descriptor = handle.descriptor();
                    (
                        &resource.resource().texture_view,
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::SolidColor(color) => (
                    &accum.resource().texture_view,
                    1,
                    [color.r, color.g, color.b, color.a],
                    [width as f32, height as f32],
                ),
            };
            let inv_transform = invert_affine(layer.transform)
                .expect("validate_request rejects unsupported transforms");
            self.record_layer_pass(
                device,
                encoder,
                &accum.resource().texture_view,
                &dst.resource().texture_view,
                layer_view,
                GpuCompositeUniforms {
                    opacity: layer.opacity.clamp(0.0, 1.0),
                    source_kind,
                    _pad0: [0, 0],
                    solid_color,
                    inv_transform0: [
                        inv_transform[0],
                        inv_transform[1],
                        inv_transform[2],
                        inv_transform[3],
                    ],
                    inv_transform1: [inv_transform[4], inv_transform[5], 0.0, 0.0],
                    geometry: [width as f32, height as f32, source_size[0], source_size[1]],
                },
            );
            src_is_a = !src_is_a;
        }

        let (output_resource, retained_resource) = if src_is_a {
            (target_a, target_b)
        } else {
            (target_b, target_a)
        };
        let output = output_resource.handle().clone();
        table.insert(retained_resource).map_err(GpuCompositeError::ResourceTable)?;
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        let mut diagnostics = GpuCompositingDiagnostics {
            gpu_composited_pixels: u64::from(width).saturating_mul(u64::from(height)),
            ..GpuCompositingDiagnostics::default()
        };
        if uploaded_cpu_layers {
            diagnostics.gpu_with_upload_composites = 1;
        } else {
            diagnostics.gpu_native_composites = 1;
        }
        Ok(GpuCompositeRecord { output, diagnostics })
    }

    fn record_layer_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        accum_view: &wgpu::TextureView,
        dst_view: &wgpu::TextureView,
        layer_view: &wgpu::TextureView,
        uniforms: GpuCompositeUniforms,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian_gpu_working_compositor_layer_uniform"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_texture_bind_group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(accum_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_uniform_bind_group"),
            layout: &self.uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian_gpu_working_compositor_pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &texture_bind_group, &[]);
        pass.set_bind_group(1, &uniform_bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}

fn validate_request(request: &GpuCompositeRequest<'_>) -> Result<(), GpuCompositeError> {
    if request.width == 0 || request.height == 0 {
        return Err(GpuCompositeError::EmptyExtent {
            width: request.width,
            height: request.height,
        });
    }
    let capability = evaluate_gpu_compositing_capability(
        request.layers.len(),
        request.layers.iter().any(|layer| layer.has_effect_graph),
        request.layers.iter().any(|layer| !gpu_transform_supported(layer)),
        request.layers.iter().any(|layer| layer.blend_mode != BlendMode::Normal),
        request
            .layers
            .iter()
            .all(|layer| !matches!(layer.source, GpuCompositeLayerSource::CpuFrame(_))),
    );
    if let GpuCompositingCapability::CpuFallback { reason } = capability {
        return Err(GpuCompositeError::Blocked { reason });
    }
    for layer in request.layers {
        if let Some(actual) = layer_source_descriptor(layer.source) {
            let expected_residency = match layer.source {
                GpuCompositeLayerSource::CpuFrame(_) => ColorFrameResidency::Cpu,
                GpuCompositeLayerSource::GpuFrame(_) => ColorFrameResidency::Gpu,
                GpuCompositeLayerSource::SolidColor(_) => unreachable!("solid has no descriptor"),
            };
            let expected = ColorFrameDescriptor {
                width: actual.width,
                height: actual.height,
                color_space: request.working_color_space,
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: expected_residency,
            };
            if actual != expected {
                return Err(GpuCompositeError::SourceDescriptorMismatch { expected, actual });
            }
        }
    }
    Ok(())
}

fn gpu_transform_supported(layer: &GpuCompositeLayer<'_>) -> bool {
    match layer.source {
        GpuCompositeLayerSource::CpuFrame(_) | GpuCompositeLayerSource::GpuFrame(_) => {
            invert_affine(layer.transform).is_some()
        }
        GpuCompositeLayerSource::SolidColor(_) => is_identity_transform(layer.transform),
    }
}

fn layer_source_descriptor(source: GpuCompositeLayerSource<'_>) -> Option<ColorFrameDescriptor> {
    match source {
        GpuCompositeLayerSource::CpuFrame(frame) => Some(frame.descriptor()),
        GpuCompositeLayerSource::GpuFrame(handle) => Some(handle.descriptor()),
        GpuCompositeLayerSource::SolidColor(_) => None,
    }
}

fn is_identity_transform(transform: [f32; 6]) -> bool {
    const EPSILON: f32 = 1.0e-6;
    (transform[0] - 1.0).abs() <= EPSILON
        && transform[1].abs() <= EPSILON
        && transform[2].abs() <= EPSILON
        && transform[3].abs() <= EPSILON
        && (transform[4] - 1.0).abs() <= EPSILON
        && transform[5].abs() <= EPSILON
}

fn invert_affine(transform: [f32; 6]) -> Option<[f32; 6]> {
    let a = transform[0];
    let c = transform[1];
    let tx = transform[2];
    let b = transform[3];
    let d = transform[4];
    let ty = transform[5];
    let det = a * d - b * c;
    if det.abs() <= 1.0e-8 {
        return None;
    }
    let inv_det = 1.0 / det;
    let ia = d * inv_det;
    let ic = -c * inv_det;
    let ib = -b * inv_det;
    let id = a * inv_det;
    let itx = -(ia * tx + ic * ty);
    let ity = -(ib * tx + id * ty);
    Some([ia, ic, itx, ib, id, ity])
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

fn create_working_resource(
    device: &wgpu::Device,
    ids: &mut GpuColorFrameIdAllocator,
    descriptor: ColorFrameDescriptor,
    label: &'static str,
) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuCompositeError> {
    let handle = GpuColorFrameHandle::new(
        ids.allocate(),
        descriptor,
        GpuColorFrameTextureFormat::Rgba32Float,
        label,
    )?;
    let allocation = GpuColorFrameAllocationPlan::for_handle(handle);
    Ok(GpuColorFrameUploader::allocate(device, &allocation))
}

fn clear_working_texture(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    color: wgpu::Color,
) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mondrian_gpu_working_compositor_clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::RgbaF32Frame;

    #[test]
    fn gpu_compositor_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(GPU_COMPOSITOR_SHADER)
            .expect("GPU compositor WGSL should parse");
    }

    #[test]
    fn gpu_compositing_capability_classifies_single_layer() {
        let cap = evaluate_gpu_compositing_capability(1, false, false, false, true);
        assert_eq!(cap, GpuCompositingCapability::GpuNative);
    }

    #[test]
    fn gpu_compositing_capability_classifies_upload_needed() {
        let cap = evaluate_gpu_compositing_capability(1, false, false, false, false);
        assert_eq!(cap, GpuCompositingCapability::GpuWithUpload);
    }

    #[test]
    fn gpu_compositing_capability_rejects_effect_graph() {
        let cap = evaluate_gpu_compositing_capability(1, true, false, false, true);
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::EffectRequiresCpu
            }
        ));
    }

    #[test]
    fn gpu_compositing_capability_rejects_non_normal_blend() {
        let cap = evaluate_gpu_compositing_capability(2, false, false, true, true);
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::UnsupportedBlendMode
            }
        ));
    }

    #[test]
    fn gpu_compositing_capability_rejects_unsupported_transform() {
        let cap = evaluate_gpu_compositing_capability(1, false, true, false, true);
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::UnsupportedTransform
            }
        ));
    }

    #[test]
    fn gpu_compositing_capability_rejects_too_many_layers() {
        let cap = evaluate_gpu_compositing_capability(
            MAX_GPU_COMPOSITE_LAYERS + 1,
            false,
            false,
            false,
            true,
        );
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::TooManyLayers
            }
        ));
    }

    #[test]
    fn gpu_compositing_diagnostics_accumulate() {
        let mut a = GpuCompositingDiagnostics {
            gpu_native_composites: 3,
            gpu_composited_pixels: 1000,
            ..GpuCompositingDiagnostics::default()
        };
        let b = GpuCompositingDiagnostics {
            cpu_fallback_composites: 1,
            cpu_composited_pixels: 500,
            first_blocker: Some(GpuCompositingBlockerReason::EffectRequiresCpu),
            ..GpuCompositingDiagnostics::default()
        };
        a.accumulate(b);
        assert_eq!(a.gpu_native_composites, 3);
        assert_eq!(a.cpu_fallback_composites, 1);
        assert_eq!(a.gpu_composited_pixels, 1000);
        assert_eq!(a.cpu_composited_pixels, 500);
        assert_eq!(
            a.first_blocker,
            Some(GpuCompositingBlockerReason::EffectRequiresCpu)
        );
    }

    #[test]
    fn gpu_compositing_blocker_codes_are_stable() {
        let reasons = [
            GpuCompositingBlockerReason::EffectRequiresCpu,
            GpuCompositingBlockerReason::UnsupportedBlendMode,
            GpuCompositingBlockerReason::UnsupportedTransform,
            GpuCompositingBlockerReason::FrameNotGpuResident,
            GpuCompositingBlockerReason::TooManyLayers,
            GpuCompositingBlockerReason::GpuUnavailable,
        ];
        let mut codes: Vec<_> = reasons.iter().map(|r| r.code()).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(codes.len(), reasons.len(), "blocker codes must be unique");
        for reason in &reasons {
            assert!(!reason.code().is_empty());
            assert!(!reason.description().is_empty());
        }
    }

    #[test]
    fn gpu_composite_request_rejects_unsupported_blend_mode() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
            opacity: 1.0,
            blend_mode: BlendMode::Multiply,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            has_effect_graph: false,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: ColorSpace::Rec709,
            layers: &[layer],
        };

        let err = validate_request(&request).expect_err("multiply should be CPU fallback");

        assert_eq!(
            err,
            GpuCompositeError::Blocked {
                reason: GpuCompositingBlockerReason::UnsupportedBlendMode
            }
        );
    }

    #[test]
    fn gpu_composite_request_accepts_media_extent_mismatch_with_affine_transform() {
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: 8,
            height: 8,
            color_space: ColorSpace::Rec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            has_effect_graph: false,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: ColorSpace::Rec709,
            layers: &[layer],
        };

        validate_request(&request).expect("GPU compositor should support affine media sampling");
    }

    #[test]
    fn gpu_composite_request_accepts_gpu_resident_media_frame() {
        let handle = gpu_working_handle(10, ColorSpace::Rec709);
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            has_effect_graph: false,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: ColorSpace::Rec709,
            layers: &[layer],
        };

        validate_request(&request).expect("GPU compositor should accept GPU-resident media layer");
    }

    #[test]
    fn gpu_composite_request_rejects_gpu_frame_color_space_mismatch() {
        let handle = gpu_working_handle(11, ColorSpace::DciP3);
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            has_effect_graph: false,
        };
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: ColorSpace::Rec709,
            layers: &[layer],
        };

        let err =
            validate_request(&request).expect_err("GPU frame color-space mismatch should fail");

        assert!(matches!(
            err,
            GpuCompositeError::SourceDescriptorMismatch { .. }
        ));
    }

    #[test]
    fn gpu_composite_request_rejects_singular_media_transform() {
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: 8,
            height: 8,
            color_space: ColorSpace::Rec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            has_effect_graph: false,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: ColorSpace::Rec709,
            layers: &[layer],
        };

        let err = validate_request(&request).expect_err("singular transform should be blocked");

        assert_eq!(
            err,
            GpuCompositeError::Blocked {
                reason: GpuCompositingBlockerReason::UnsupportedTransform
            }
        );
    }

    #[test]
    fn gpu_composite_request_rejects_source_color_space_mismatch() {
        let frame = CpuColorFrame::working(RgbaF32Frame {
            width: 8,
            height: 8,
            color_space: ColorSpace::DciP3,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            has_effect_graph: false,
        };
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: ColorSpace::Rec709,
            layers: &[layer],
        };

        let err = validate_request(&request).expect_err("color space mismatch should be blocked");

        assert!(matches!(
            err,
            GpuCompositeError::SourceDescriptorMismatch { .. }
        ));
    }

    fn gpu_working_handle(id: u64, color_space: ColorSpace) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(id),
            ColorFrameDescriptor {
                width: 8,
                height: 8,
                color_space,
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
            },
            GpuColorFrameTextureFormat::Rgba16Float,
            "test-gpu-working-layer",
        )
        .expect("test GPU handle")
    }
}
