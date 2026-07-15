//! GPU-resident working-space compositing.
//!
//! This module owns the native wgpu path for preview/playback compositing when
//! a layer stack is simple enough to stay on GPU: affine transforms, fused
//! pointwise effect graphs, Normal blend mode, and a bounded layer count.
//! Unsupported layer shapes are rejected with typed blockers so callers can
//! fall back to the CPU reference compositor without losing diagnostic evidence.

use crate::{
    ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding, ColorFrameResidency, CpuColorFrame,
    GpuColorFrameAllocationPlan, GpuColorFrameHandle, GpuColorFrameIdAllocator,
    GpuColorFrameResource, GpuColorFrameResourceTable, GpuColorFrameTextureFormat,
    GpuColorFrameUploadPlan, GpuColorFrameUploader, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool,
};
use bytemuck::{Pod, Zeroable};
use mondrian_core::types::{BlendMode, Color};
use mondrian_effects::{
    CompiledEffectGpuPlan, EffectColorDomain, EffectGpuPointOp, MAX_FUSED_GPU_EFFECT_OPS,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_GPU_COMPOSITE_LAYERS: usize = 5;
const GPU_COMPOSITOR_UNIFORM_ARENA_SLOTS: u32 = 128;
static NEXT_GPU_COMPOSITOR_BINDING_CACHE_KEY: AtomicU64 = AtomicU64::new(1);
const GPU_COMPOSITOR_SHADER: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct CompositeUniforms {
    opacity: f32,
    source_kind: u32,
    effect_count: u32,
    frame_seed: u32,
    solid_color: vec4<f32>,
    inv_transform0: vec4<f32>,
    inv_transform1: vec4<f32>,
    geometry: vec4<f32>,
    effects: array<EffectUniform, 8>,
};

struct EffectUniform {
    header: vec4<u32>,
    params: vec4<f32>,
};

@group(0) @binding(0) var layer_tex: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(1) @binding(0) var accum_tex: texture_2d<f32>;
@group(2) @binding(0) var<uniform> uniforms: CompositeUniforms;

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
    let source_position = source_coordinate(in.uv);
    var layer_px: vec4<f32>;
    if (uniforms.source_kind == 1u || uniforms.source_kind == 4u) {
        layer_px = uniforms.solid_color;
    } else if (uniforms.source_kind == 2u) {
        layer_px = base_px;
    } else {
        layer_px = sample_layer(source_position);
    }
    let effect_position = select(
        source_position - vec2<f32>(0.5),
        in.uv * uniforms.geometry.zw - vec2<f32>(0.5),
        uniforms.source_kind == 1u,
    );
    layer_px = apply_effects(layer_px, effect_position);
    if (uniforms.source_kind == 3u || uniforms.source_kind == 4u) {
        return layer_px;
    }
    return over_straight_alpha(base_px, layer_px, uniforms.opacity);
}

fn source_coordinate(dst_uv: vec2<f32>) -> vec2<f32> {
    let dst_center = vec2<f32>(
        dst_uv.x * uniforms.geometry.x,
        dst_uv.y * uniforms.geometry.y,
    );
    return vec2<f32>(
        uniforms.inv_transform0.x * dst_center.x +
            uniforms.inv_transform0.y * dst_center.y +
            uniforms.inv_transform0.z,
        uniforms.inv_transform0.w * dst_center.x +
            uniforms.inv_transform1.x * dst_center.y +
            uniforms.inv_transform1.y,
    );
}

fn sample_layer(src_center: vec2<f32>) -> vec4<f32> {
    let src_size = uniforms.geometry.zw;
    if (src_center.x < 0.5 ||
        src_center.y < 0.5 ||
        src_center.x >= src_size.x + 0.5 ||
        src_center.y >= src_size.y + 0.5) {
        return vec4<f32>(0.0);
    }
    return textureSample(layer_tex, linear_sampler, src_center / src_size);
}

fn grain_noise(position: vec2<f32>) -> f32 {
    var value = u32(position.x) * 1973u + u32(position.y) * 9277u +
        uniforms.frame_seed * 26699u + 0x68bc21ebu;
    value = value ^ (value << 13u);
    value = value ^ (value >> 17u);
    value = value ^ (value << 5u);
    return f32(value) / 4294967295.0 * 2.0 - 1.0;
}

fn apply_effects(input: vec4<f32>, position: vec2<f32>) -> vec4<f32> {
    if (input.a <= 0.000001) { return input; }
    var pixel = input;
    for (var index = 0u; index < 8u; index = index + 1u) {
        if (index >= uniforms.effect_count) { break; }
        let effect = uniforms.effects[index];
        if (effect.header.x == 1u) {
            let exposure = exp2(clamp(effect.params.x, -4.0, 4.0));
            let contrast = clamp(effect.params.y, 0.0, 3.0);
            let saturation = clamp(effect.params.z, 0.0, 3.0);
            var rgb = (pixel.rgb * exposure - vec3<f32>(0.5)) * contrast + vec3<f32>(0.5);
            let luma = dot(rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
            pixel = vec4<f32>(vec3<f32>(luma) + (rgb - vec3<f32>(luma)) * saturation, pixel.a);
        } else if (effect.header.x == 2u) {
            let temperature = clamp(effect.params.x, -1.0, 1.0);
            let tint = clamp(effect.params.y, -1.0, 1.0);
            pixel.r = pixel.r + temperature * 0.12 - tint * 0.04;
            pixel.g = pixel.g + tint * 0.05;
            pixel.b = pixel.b - temperature * 0.12 - tint * 0.02;
        } else if (effect.header.x == 3u) {
            let center = max((uniforms.geometry.zw - vec2<f32>(1.0)) * 0.5, vec2<f32>(1.0));
            let normalized = (position - center) / center;
            let distance = min(length(normalized), 1.0);
            let feather = clamp(effect.params.y, 0.05, 1.0);
            let gain = 1.0 - smoothstep(1.0 - feather * 0.85, 1.0, distance) * effect.params.x;
            pixel = vec4<f32>(pixel.rgb * gain, pixel.a);
        } else if (effect.header.x == 4u) {
            let noise = grain_noise(position) * clamp(effect.params.x, 0.0, 1.0) * 0.18;
            pixel = vec4<f32>(pixel.rgb + vec3<f32>(noise), pixel.a);
        }
    }
    return pixel;
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
    /// Single GPU working layers reused without recording a composite pass.
    pub gpu_passthrough_frames: u64,
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
        self.gpu_passthrough_frames =
            self.gpu_passthrough_frames.saturating_add(other.gpu_passthrough_frames);
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
        self.gpu_passthrough_frames > 0 || self.gpu_native_composites > 0
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
    has_any_unsupported_transform: bool,
    has_any_non_normal_blend_mode: bool,
    all_frames_gpu_resident: bool,
) -> GpuCompositingCapability {
    if layer_count > MAX_GPU_COMPOSITE_LAYERS {
        return GpuCompositingCapability::CpuFallback {
            reason: GpuCompositingBlockerReason::TooManyLayers,
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
    /// Adjustment layer that processes the current working-space accumulator.
    Adjustment,
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
    /// Timeline affine transform. Media accepts invertible transforms; solids require identity.
    pub transform: [f32; 6],
    /// Lowered pointwise effect plan evaluated in working-linear space.
    pub effect_plan: Option<&'a CompiledEffectGpuPlan>,
    /// Stable timeline frame seed used by temporal effect operations.
    pub frame_seed: i64,
}

/// Request for recording a GPU working-space composite.
pub struct GpuCompositeRequest<'a> {
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Working color space of the composite output.
    pub working_color_space: mondrian_core::WorkingColorSpace,
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

/// Result of recording one standalone GPU point-effect pass.
pub struct GpuPointEffectRecord {
    /// GPU-resident frame carrying the same effect-domain identity as the input.
    pub output: GpuColorFrameHandle,
    /// Number of pixels evaluated by the point-effect shader.
    pub processed_pixels: u64,
}

/// Result of materializing one procedural solid into a GPU working frame.
pub struct GpuSolidSourceRecord {
    /// GPU-resident working-linear frame containing the unblended solid color.
    pub output: GpuColorFrameHandle,
    /// Number of pixels generated by the procedural source pass.
    pub materialized_pixels: u64,
}

/// Bounded uniform-arena evidence for the compositor hot path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositorUniformArenaDiagnostics {
    /// Persistent GPU buffers created for this arena.
    pub buffer_creations: u64,
    /// Uniform payloads written into reserved slots.
    pub uniform_writes: u64,
    /// Peak slots reserved within one frame lifetime.
    pub high_watermark_slots: u32,
    /// Frame-lifetime resets after ordered submission.
    pub frame_resets: u64,
    /// Attempts rejected because callers did not reset the bounded arena.
    pub exhaustions: u64,
}

/// Point-in-time evidence for compositor texture-binding object reuse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuCompositorTextureBindingDiagnostics {
    /// Texture bind groups created since compositor construction.
    pub bind_group_creations: u64,
    /// Existing texture bind groups reused without backend object creation.
    pub cache_hits: u64,
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
    /// An adjustment layer did not provide a non-identity GPU effect plan.
    #[error("GPU adjustment layer requires a non-identity effect plan")]
    AdjustmentMissingEffectPlan,
    /// An effect must be surrounded by renderer-owned OCIO passes before composition.
    #[error("GPU working compositor cannot execute effect domain {domain:?} inline")]
    EffectDomainRequiresExternalPass {
        /// Exact domain that the external pass sequence must materialize.
        domain: EffectColorDomain,
    },
    /// A standalone point pass was given a scene, data, or alpha domain.
    #[error("GPU point-effect pass requires an RGB effect intermediate, got {domain:?}")]
    PointEffectRequiresRgbIntermediate {
        /// Unsupported plan domain.
        domain: EffectColorDomain,
    },
    /// The intermediate frame does not match the effect plan's exact domain contract.
    #[error("GPU point-effect intermediate descriptor mismatch")]
    PointEffectDescriptorMismatch {
        /// Descriptor required by the plan.
        expected: ColorFrameDescriptor,
        /// Supplied frame descriptor.
        actual: ColorFrameDescriptor,
    },
    /// The point pass requires a floating-point sampled texture.
    #[error("GPU point-effect input texture must be floating point, got {texture_format:?}")]
    PointEffectTextureFormatUnsupported {
        /// Unsupported input texture format.
        texture_format: GpuColorFrameTextureFormat,
    },
    /// The bounded per-frame uniform arena was not reset after ordered submission.
    #[error("GPU compositor uniform arena exhausted at {capacity} slots")]
    UniformArenaExhausted {
        /// Fixed slot capacity.
        capacity: u32,
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
    layer_texture_layout: wgpu::BindGroupLayout,
    accum_texture_layout: wgpu::BindGroupLayout,
    layer_texture_cache_key: u64,
    accum_texture_cache_key: u64,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    uniform_stride: u64,
    uniform_arena: Mutex<GpuCompositeUniformArenaState>,
    texture_bind_group_creations: AtomicU64,
    texture_bind_group_cache_hits: AtomicU64,
    sampler: wgpu::Sampler,
    procedural_layer_bind_group: wgpu::BindGroup,
    procedural_accum_bind_group: wgpu::BindGroup,
}

#[derive(Clone, Copy)]
enum GpuCompositeTextureBinding<'a> {
    Resource(&'a GpuColorFrameWgpuResource),
    ProceduralDummy,
}

struct GpuCompositeUniformArenaState {
    next_slot: u32,
    diagnostics: GpuCompositorUniformArenaDiagnostics,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuCompositeUniforms {
    opacity: f32,
    source_kind: u32,
    effect_count: u32,
    frame_seed: u32,
    solid_color: [f32; 4],
    inv_transform0: [f32; 4],
    inv_transform1: [f32; 4],
    geometry: [f32; 4],
    effects: [GpuEffectUniform; MAX_FUSED_GPU_EFFECT_OPS],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct GpuEffectUniform {
    header: [u32; 4],
    params: [f32; 4],
}

impl GpuFrameCompositor {
    /// Create a GPU working-space compositor runtime for a wgpu device.
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian_gpu_working_compositor_shader"),
            source: wgpu::ShaderSource::Wgsl(GPU_COMPOSITOR_SHADER.into()),
        });
        let layer_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian_gpu_working_compositor_layer_texture"),
                entries: &[
                    texture_binding(0),
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                        count: None,
                    },
                ],
            });
        let accum_texture_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian_gpu_working_compositor_accum_texture"),
                entries: &[texture_binding(0)],
            });
        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(
                        std::mem::size_of::<GpuCompositeUniforms>() as u64,
                    ),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian_gpu_working_compositor_layout"),
            bind_group_layouts: &[
                Some(&layer_texture_layout),
                Some(&accum_texture_layout),
                Some(&uniform_layout),
            ],
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
        let uniform_size = std::mem::size_of::<GpuCompositeUniforms>() as u64;
        let uniform_alignment =
            u64::from(device.limits().min_uniform_buffer_offset_alignment.max(1));
        let uniform_stride = uniform_size.div_ceil(uniform_alignment) * uniform_alignment;
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian_gpu_working_compositor_uniform_arena"),
            size: uniform_stride.saturating_mul(u64::from(GPU_COMPOSITOR_UNIFORM_ARENA_SLOTS)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_uniform_arena_bind_group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniform_buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(uniform_size),
                }),
            }],
        });
        let procedural_dummy = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian_gpu_procedural_dummy"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let procedural_dummy_view =
            procedural_dummy.create_view(&wgpu::TextureViewDescriptor::default());
        let procedural_layer_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_procedural_layer_binding"),
            layout: &layer_texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&procedural_dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let procedural_accum_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian_gpu_working_compositor_procedural_accum_binding"),
            layout: &accum_texture_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&procedural_dummy_view),
            }],
        });
        let layer_texture_cache_key =
            NEXT_GPU_COMPOSITOR_BINDING_CACHE_KEY.fetch_add(2, Ordering::Relaxed);
        Self {
            pipeline,
            layer_texture_layout,
            accum_texture_layout,
            layer_texture_cache_key,
            accum_texture_cache_key: layer_texture_cache_key.saturating_add(1),
            uniform_buffer,
            uniform_bind_group,
            uniform_stride,
            uniform_arena: Mutex::new(GpuCompositeUniformArenaState {
                next_slot: 0,
                diagnostics: GpuCompositorUniformArenaDiagnostics {
                    buffer_creations: 1,
                    ..GpuCompositorUniformArenaDiagnostics::default()
                },
            }),
            texture_bind_group_creations: AtomicU64::new(2),
            texture_bind_group_cache_hits: AtomicU64::new(0),
            sampler,
            procedural_layer_bind_group,
            procedural_accum_bind_group,
        }
    }

    /// Reset frame-local uniform slots after the caller has ordered submission.
    pub fn clear_frame_resources(&self) {
        let mut arena = self.uniform_arena.lock();
        arena.next_slot = 0;
        arena.diagnostics.frame_resets = arena.diagnostics.frame_resets.saturating_add(1);
    }

    /// Return point-in-time evidence for persistent uniform-arena reuse.
    pub fn uniform_arena_diagnostics(&self) -> GpuCompositorUniformArenaDiagnostics {
        self.uniform_arena.lock().diagnostics
    }

    /// Return point-in-time compositor texture-binding reuse evidence.
    pub fn texture_binding_diagnostics(&self) -> GpuCompositorTextureBindingDiagnostics {
        GpuCompositorTextureBindingDiagnostics {
            bind_group_creations: self.texture_bind_group_creations.load(Ordering::Relaxed),
            cache_hits: self.texture_bind_group_cache_hits.load(Ordering::Relaxed),
        }
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
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        request: GpuCompositeRequest<'_>,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        validate_request(&request)?;
        if let Some(output) = single_layer_gpu_passthrough(&request) {
            return Ok(GpuCompositeRecord {
                output: output.clone(),
                diagnostics: GpuCompositingDiagnostics {
                    gpu_passthrough_frames: 1,
                    ..GpuCompositingDiagnostics::default()
                },
            });
        }
        let width = request.width;
        let height = request.height;
        let output_descriptor = ColorFrameDescriptor {
            width,
            height,
            color_space: request.working_color_space.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let target_a = create_working_resource(
            device,
            ids,
            output_descriptor,
            "gpu-composite-accum-a",
            resource_pool,
        )?;
        let target_b = create_working_resource(
            device,
            ids,
            output_descriptor,
            "gpu-composite-accum-b",
            resource_pool,
        )?;
        clear_working_texture(
            encoder,
            &target_a.resource().texture_view,
            wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 },
        );

        let mut transient_uploads = Vec::new();
        let mut uploaded_cpu_layers = false;
        let mut src_is_a = true;
        for (index, layer) in request.layers.iter().enumerate() {
            if layer_has_zero_contribution(layer) {
                continue;
            }
            let (accum, dst) = if src_is_a {
                (&target_a, &target_b)
            } else {
                (&target_b, &target_a)
            };
            let (layer_binding, source_kind, solid_color, source_size) = match layer.source {
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
                        GpuCompositeTextureBinding::Resource(
                            transient_uploads
                                .last()
                                .expect("uploaded layer just pushed")
                                .resource(),
                        ),
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::GpuFrame(handle) => {
                    let resource = table.get(handle).map_err(GpuCompositeError::ResourceTable)?;
                    let descriptor = handle.descriptor();
                    (
                        GpuCompositeTextureBinding::Resource(resource.resource()),
                        0,
                        [0.0, 0.0, 0.0, 0.0],
                        [descriptor.width as f32, descriptor.height as f32],
                    )
                }
                GpuCompositeLayerSource::SolidColor(color) => (
                    GpuCompositeTextureBinding::ProceduralDummy,
                    1,
                    [color.r, color.g, color.b, color.a],
                    [width as f32, height as f32],
                ),
                GpuCompositeLayerSource::Adjustment => (
                    GpuCompositeTextureBinding::ProceduralDummy,
                    2,
                    [0.0, 0.0, 0.0, 0.0],
                    [width as f32, height as f32],
                ),
            };
            let inv_transform = invert_affine(layer.transform)
                .expect("validate_request rejects unsupported transforms");
            self.record_layer_pass(
                device,
                queue,
                encoder,
                GpuCompositeTextureBinding::Resource(accum.resource()),
                &dst.resource().texture_view,
                layer_binding,
                GpuCompositeUniforms {
                    opacity: layer.opacity.clamp(0.0, 1.0),
                    source_kind,
                    effect_count: layer
                        .effect_plan
                        .map_or(0, |plan| plan.operations().len() as u32),
                    frame_seed: layer.frame_seed as u32,
                    solid_color,
                    inv_transform0: [
                        inv_transform[0],
                        inv_transform[1],
                        inv_transform[2],
                        inv_transform[3],
                    ],
                    inv_transform1: [inv_transform[4], inv_transform[5], 0.0, 0.0],
                    geometry: [width as f32, height as f32, source_size[0], source_size[1]],
                    effects: effect_uniforms(layer.effect_plan),
                },
            )?;
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

    /// Record a standalone point-effect pass over a typed effect-domain frame.
    ///
    /// The pass does not blend or relabel pixels. Renderer-owned OCIO passes
    /// are responsible for entering and leaving this exact intermediate domain.
    pub fn record_point_effect_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        input: &GpuColorFrameHandle,
        plan: &CompiledEffectGpuPlan,
        frame_seed: i64,
    ) -> Result<GpuPointEffectRecord, GpuCompositeError> {
        validate_point_effect_input(input, plan)?;
        let input_resource = table.get(input).map_err(GpuCompositeError::ResourceTable)?;
        let descriptor = input.descriptor();
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-point-effect-output",
            resource_pool,
        )?;
        let width = descriptor.width;
        let height = descriptor.height;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::ProceduralDummy,
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(input_resource.resource()),
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 3,
                effect_count: plan.operations().len() as u32,
                frame_seed: frame_seed as u32,
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [width as f32, height as f32, width as f32, height as f32],
                effects: effect_uniforms(Some(plan)),
            },
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuPointEffectRecord {
            output,
            processed_pixels: u64::from(width).saturating_mul(u64::from(height)),
        })
    }

    /// Blend an already processed adjustment frame over its original accumulator.
    ///
    /// Unlike a two-layer general composite, this graph node samples the base
    /// accumulator directly and therefore records one full-frame pass and one
    /// output allocation.
    #[allow(clippy::too_many_arguments)]
    pub fn record_adjustment_blend_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        base: &GpuColorFrameHandle,
        processed: &GpuColorFrameHandle,
        opacity: f32,
        blend_mode: BlendMode,
        working_color_space: mondrian_core::WorkingColorSpace,
    ) -> Result<GpuCompositeRecord, GpuCompositeError> {
        let validation_layers = [
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(base),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
            GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(processed),
                opacity,
                blend_mode,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            },
        ];
        let descriptor = base.descriptor();
        validate_request(&GpuCompositeRequest {
            width: descriptor.width,
            height: descriptor.height,
            working_color_space,
            layers: &validation_layers,
        })?;
        let base_resource = table.get(base).map_err(GpuCompositeError::ResourceTable)?;
        let processed_resource = table.get(processed).map_err(GpuCompositeError::ResourceTable)?;
        let output_resource = create_working_resource(
            device,
            ids,
            descriptor,
            "gpu-adjustment-blend-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::Resource(base_resource.resource()),
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::Resource(processed_resource.resource()),
            GpuCompositeUniforms {
                opacity: opacity.clamp(0.0, 1.0),
                source_kind: 0,
                effect_count: 0,
                frame_seed: 0,
                solid_color: [0.0; 4],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [
                    descriptor.width as f32,
                    descriptor.height as f32,
                    descriptor.width as f32,
                    descriptor.height as f32,
                ],
                effects: effect_uniforms(None),
            },
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuCompositeRecord {
            output,
            diagnostics: GpuCompositingDiagnostics {
                gpu_native_composites: 1,
                gpu_composited_pixels: u64::from(descriptor.width)
                    .saturating_mul(u64::from(descriptor.height)),
                ..GpuCompositingDiagnostics::default()
            },
        })
    }

    /// Materialize an unblended procedural solid as a working-linear GPU frame.
    ///
    /// Layer opacity, affine transformation, blending, and effects are
    /// intentionally deferred to later graph nodes so their authored order is
    /// preserved when an OCIO effect-domain round trip is inserted.
    #[allow(clippy::too_many_arguments)]
    pub fn record_solid_source_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
        resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
        width: u32,
        height: u32,
        working_color_space: mondrian_core::WorkingColorSpace,
        color: Color,
    ) -> Result<GpuSolidSourceRecord, GpuCompositeError> {
        if width == 0 || height == 0 {
            return Err(GpuCompositeError::EmptyExtent { width, height });
        }
        let output_resource = create_working_resource(
            device,
            ids,
            ColorFrameDescriptor {
                width,
                height,
                color_space: working_color_space.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
            },
            "gpu-solid-source-output",
            resource_pool,
        )?;
        self.record_layer_pass(
            device,
            queue,
            encoder,
            GpuCompositeTextureBinding::ProceduralDummy,
            &output_resource.resource().texture_view,
            GpuCompositeTextureBinding::ProceduralDummy,
            GpuCompositeUniforms {
                opacity: 1.0,
                source_kind: 4,
                effect_count: 0,
                frame_seed: 0,
                solid_color: [color.r, color.g, color.b, color.a],
                inv_transform0: [1.0, 0.0, 0.0, 0.0],
                inv_transform1: [1.0, 0.0, 0.0, 0.0],
                geometry: [width as f32, height as f32, width as f32, height as f32],
                effects: effect_uniforms(None),
            },
        )?;
        let output = output_resource.handle().clone();
        table.insert(output_resource).map_err(GpuCompositeError::ResourceTable)?;
        Ok(GpuSolidSourceRecord {
            output,
            materialized_pixels: u64::from(width).saturating_mul(u64::from(height)),
        })
    }

    fn record_layer_pass(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        accum_binding: GpuCompositeTextureBinding<'_>,
        dst_view: &wgpu::TextureView,
        layer_binding: GpuCompositeTextureBinding<'_>,
        uniforms: GpuCompositeUniforms,
    ) -> Result<(), GpuCompositeError> {
        let uniform_offset = {
            let mut arena = self.uniform_arena.lock();
            if arena.next_slot >= GPU_COMPOSITOR_UNIFORM_ARENA_SLOTS {
                arena.diagnostics.exhaustions = arena.diagnostics.exhaustions.saturating_add(1);
                return Err(GpuCompositeError::UniformArenaExhausted {
                    capacity: GPU_COMPOSITOR_UNIFORM_ARENA_SLOTS,
                });
            }
            let slot = arena.next_slot;
            arena.next_slot = arena.next_slot.saturating_add(1);
            arena.diagnostics.uniform_writes = arena.diagnostics.uniform_writes.saturating_add(1);
            arena.diagnostics.high_watermark_slots =
                arena.diagnostics.high_watermark_slots.max(arena.next_slot);
            u64::from(slot).saturating_mul(self.uniform_stride)
        };
        queue.write_buffer(
            &self.uniform_buffer,
            uniform_offset,
            bytemuck::bytes_of(&uniforms),
        );
        let layer_bind_group = match layer_binding {
            GpuCompositeTextureBinding::Resource(resource) => {
                let (bind_group, cache_hit) =
                    resource.cached_bind_group(self.layer_texture_cache_key, |texture_view| {
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("mondrian_gpu_working_compositor_layer_binding"),
                            layout: &self.layer_texture_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(texture_view),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                                },
                            ],
                        })
                    });
                self.record_texture_binding_cache_result(cache_hit);
                bind_group
            }
            GpuCompositeTextureBinding::ProceduralDummy => {
                self.texture_bind_group_cache_hits.fetch_add(1, Ordering::Relaxed);
                self.procedural_layer_bind_group.clone()
            }
        };
        let accum_bind_group = match accum_binding {
            GpuCompositeTextureBinding::Resource(resource) => {
                let (bind_group, cache_hit) =
                    resource.cached_bind_group(self.accum_texture_cache_key, |texture_view| {
                        device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("mondrian_gpu_working_compositor_accum_binding"),
                            layout: &self.accum_texture_layout,
                            entries: &[wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(texture_view),
                            }],
                        })
                    });
                self.record_texture_binding_cache_result(cache_hit);
                bind_group
            }
            GpuCompositeTextureBinding::ProceduralDummy => {
                self.texture_bind_group_cache_hits.fetch_add(1, Ordering::Relaxed);
                self.procedural_accum_bind_group.clone()
            }
        };
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
        pass.set_bind_group(0, &layer_bind_group, &[]);
        pass.set_bind_group(1, &accum_bind_group, &[]);
        pass.set_bind_group(2, &self.uniform_bind_group, &[uniform_offset as u32]);
        pass.draw(0..4, 0..1);
        Ok(())
    }

    fn record_texture_binding_cache_result(&self, cache_hit: bool) {
        if cache_hit {
            self.texture_bind_group_cache_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.texture_bind_group_creations.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn validate_point_effect_input(
    input: &GpuColorFrameHandle,
    plan: &CompiledEffectGpuPlan,
) -> Result<(), GpuCompositeError> {
    let actual = input.descriptor();
    let (color_space, encoding) = match plan.processing_domain() {
        EffectColorDomain::LogPerceptualRgb { color_space }
        | EffectColorDomain::DisplayEncodedRgb { color_space } => {
            (color_space, ColorFrameEncoding::EncodedFloat)
        }
        EffectColorDomain::DisplayLinearRgb { color_space } => {
            (color_space, ColorFrameEncoding::LinearFloat)
        }
        domain @ (EffectColorDomain::SceneLinearRgb
        | EffectColorDomain::Data
        | EffectColorDomain::AlphaMask) => {
            return Err(GpuCompositeError::PointEffectRequiresRgbIntermediate { domain });
        }
    };
    let expected = ColorFrameDescriptor {
        width: actual.width,
        height: actual.height,
        color_space: color_space.into(),
        domain: ColorFrameDomain::Effect,
        encoding,
        residency: ColorFrameResidency::Gpu,
    };
    if actual != expected {
        return Err(GpuCompositeError::PointEffectDescriptorMismatch { expected, actual });
    }
    if input.texture_format() == GpuColorFrameTextureFormat::Rgba8Unorm {
        return Err(GpuCompositeError::PointEffectTextureFormatUnsupported {
            texture_format: input.texture_format(),
        });
    }
    Ok(())
}

fn single_layer_gpu_passthrough<'a>(
    request: &'a GpuCompositeRequest<'a>,
) -> Option<&'a GpuColorFrameHandle> {
    let mut contributing_layers =
        request.layers.iter().filter(|layer| !layer_has_zero_contribution(layer));
    let layer = contributing_layers.next()?;
    if contributing_layers.next().is_some() {
        return None;
    }
    let GpuCompositeLayerSource::GpuFrame(handle) = layer.source else {
        return None;
    };
    let descriptor = handle.descriptor();
    let preserves_pixels = layer.opacity.clamp(0.0, 1.0) == 1.0
        && layer.blend_mode == BlendMode::Normal
        && is_identity_transform(layer.transform)
        && layer.effect_plan.is_none_or(CompiledEffectGpuPlan::is_identity)
        && descriptor.width == request.width
        && descriptor.height == request.height
        && handle.texture_format() == GpuColorFrameTextureFormat::Rgba32Float;
    preserves_pixels.then_some(handle)
}

fn effect_uniforms(
    plan: Option<&CompiledEffectGpuPlan>,
) -> [GpuEffectUniform; MAX_FUSED_GPU_EFFECT_OPS] {
    let mut uniforms = [GpuEffectUniform::zeroed(); MAX_FUSED_GPU_EFFECT_OPS];
    let Some(plan) = plan else { return uniforms };
    for (target, operation) in uniforms.iter_mut().zip(plan.operations()) {
        *target = match *operation {
            EffectGpuPointOp::ColorAdjust { exposure, contrast, saturation } => GpuEffectUniform {
                header: [1, 0, 0, 0],
                params: [exposure, contrast, saturation, 0.0],
            },
            EffectGpuPointOp::WhiteBalance { temperature, tint } => GpuEffectUniform {
                header: [2, 0, 0, 0],
                params: [temperature, tint, 0.0, 0.0],
            },
            EffectGpuPointOp::Vignette { intensity, feather } => GpuEffectUniform {
                header: [3, 0, 0, 0],
                params: [intensity, feather, 0.0, 0.0],
            },
            EffectGpuPointOp::Grain { amount } => GpuEffectUniform {
                header: [4, 0, 0, 0],
                params: [amount, 0.0, 0.0, 0.0],
            },
        };
    }
    uniforms
}

fn validate_request(request: &GpuCompositeRequest<'_>) -> Result<(), GpuCompositeError> {
    if request.width == 0 || request.height == 0 {
        return Err(GpuCompositeError::EmptyExtent {
            width: request.width,
            height: request.height,
        });
    }
    let contributing_layers =
        request.layers.iter().filter(|layer| !layer_has_zero_contribution(layer));
    let capability = evaluate_gpu_compositing_capability(
        contributing_layers.clone().count(),
        contributing_layers.clone().any(|layer| !gpu_transform_supported(layer)),
        contributing_layers.clone().any(|layer| layer.blend_mode != BlendMode::Normal),
        contributing_layers
            .clone()
            .all(|layer| !matches!(layer.source, GpuCompositeLayerSource::CpuFrame(_))),
    );
    if let GpuCompositingCapability::CpuFallback { reason } = capability {
        return Err(GpuCompositeError::Blocked { reason });
    }
    for layer in request.layers {
        if layer_has_zero_contribution(layer) {
            continue;
        }
        if let Some(plan) = layer.effect_plan {
            let domain = plan.processing_domain();
            if domain != EffectColorDomain::SceneLinearRgb {
                return Err(GpuCompositeError::EffectDomainRequiresExternalPass { domain });
            }
        }
        if matches!(layer.source, GpuCompositeLayerSource::Adjustment)
            && layer.effect_plan.is_none_or(CompiledEffectGpuPlan::is_identity)
        {
            return Err(GpuCompositeError::AdjustmentMissingEffectPlan);
        }
        if let Some(actual) = layer_source_descriptor(layer.source) {
            let expected_residency = match layer.source {
                GpuCompositeLayerSource::CpuFrame(_) => ColorFrameResidency::Cpu,
                GpuCompositeLayerSource::GpuFrame(_) => ColorFrameResidency::Gpu,
                GpuCompositeLayerSource::SolidColor(_) | GpuCompositeLayerSource::Adjustment => {
                    unreachable!("procedural layers have no descriptor")
                }
            };
            let expected = ColorFrameDescriptor {
                width: actual.width,
                height: actual.height,
                color_space: request.working_color_space.into(),
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

fn layer_has_zero_contribution(layer: &GpuCompositeLayer<'_>) -> bool {
    layer.opacity.clamp(0.0, 1.0) == 0.0
}

fn gpu_transform_supported(layer: &GpuCompositeLayer<'_>) -> bool {
    match layer.source {
        GpuCompositeLayerSource::CpuFrame(_) | GpuCompositeLayerSource::GpuFrame(_) => {
            invert_affine(layer.transform).is_some()
        }
        GpuCompositeLayerSource::SolidColor(_) | GpuCompositeLayerSource::Adjustment => {
            is_identity_transform(layer.transform)
        }
    }
}

fn layer_source_descriptor(source: GpuCompositeLayerSource<'_>) -> Option<ColorFrameDescriptor> {
    match source {
        GpuCompositeLayerSource::CpuFrame(frame) => Some(frame.descriptor()),
        GpuCompositeLayerSource::GpuFrame(handle) => Some(handle.descriptor()),
        GpuCompositeLayerSource::SolidColor(_) | GpuCompositeLayerSource::Adjustment => None,
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
    resource_pool: Option<&GpuColorFrameWgpuResourcePool>,
) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuCompositeError> {
    let handle = GpuColorFrameHandle::new(
        ids.allocate(),
        descriptor,
        GpuColorFrameTextureFormat::Rgba32Float,
        label,
    )?;
    let allocation = GpuColorFrameAllocationPlan::for_handle(handle);
    Ok(resource_pool.map_or_else(
        || GpuColorFrameUploader::allocate(device, &allocation),
        |pool| pool.acquire(device, &allocation),
    ))
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
    use mondrian_core::{WorkingColorSpace, WorkingRgbaF32Frame};

    #[test]
    fn gpu_compositor_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(GPU_COMPOSITOR_SHADER)
            .expect("GPU compositor WGSL should parse");
    }

    #[test]
    fn gpu_compositing_capability_classifies_single_layer() {
        let cap = evaluate_gpu_compositing_capability(1, false, false, true);
        assert_eq!(cap, GpuCompositingCapability::GpuNative);
    }

    #[test]
    fn gpu_compositing_capability_classifies_upload_needed() {
        let cap = evaluate_gpu_compositing_capability(1, false, false, false);
        assert_eq!(cap, GpuCompositingCapability::GpuWithUpload);
    }

    #[test]
    fn gpu_compositing_capability_rejects_non_normal_blend() {
        let cap = evaluate_gpu_compositing_capability(2, false, true, true);
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::UnsupportedBlendMode
            }
        ));
    }

    #[test]
    fn gpu_compositing_capability_rejects_unsupported_transform() {
        let cap = evaluate_gpu_compositing_capability(1, true, false, true);
        assert!(matches!(
            cap,
            GpuCompositingCapability::CpuFallback {
                reason: GpuCompositingBlockerReason::UnsupportedTransform
            }
        ));
    }

    #[test]
    fn gpu_compositing_capability_rejects_too_many_layers() {
        let cap =
            evaluate_gpu_compositing_capability(MAX_GPU_COMPOSITE_LAYERS + 1, false, false, true);
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
            gpu_passthrough_frames: 2,
            gpu_native_composites: 3,
            gpu_composited_pixels: 1000,
            ..GpuCompositingDiagnostics::default()
        };
        let b = GpuCompositingDiagnostics {
            gpu_passthrough_frames: 1,
            cpu_fallback_composites: 1,
            cpu_composited_pixels: 500,
            first_blocker: Some(GpuCompositingBlockerReason::EffectRequiresCpu),
            ..GpuCompositingDiagnostics::default()
        };
        a.accumulate(b);
        assert_eq!(a.gpu_passthrough_frames, 3);
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
    fn single_gpu_working_layer_passthrough_requires_pixel_identity() {
        let handle = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(99),
            ColorFrameDescriptor {
                width: 8,
                height: 8,
                color_space: WorkingColorSpace::LinearRec709.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "passthrough-test",
        )
        .expect("valid passthrough handle");
        let mut layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };

        let layers = [layer];
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &layers,
        };
        assert_eq!(single_layer_gpu_passthrough(&request), Some(&handle));

        layer.opacity = 0.5;
        let layers = [layer];
        let request = GpuCompositeRequest { layers: &layers, ..request };
        assert_eq!(single_layer_gpu_passthrough(&request), None);

        layer.opacity = 1.0;
        layer.transform[2] = 1.0;
        let layers = [layer];
        let request = GpuCompositeRequest { layers: &layers, ..request };
        assert_eq!(single_layer_gpu_passthrough(&request), None);
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
    fn gpu_effect_uniforms_preserve_plan_order_and_parameters() {
        use mondrian_effects::{
            get_or_compile_scheduled_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::WhiteBalance { temperature: 0.25, tint: -0.5 });
        builder.append_unary(EffectRenderOp::Vignette { intensity: 0.7, feather: 0.4 });
        let graph = get_or_compile_scheduled_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point chain");

        let uniforms = effect_uniforms(Some(&plan));

        assert_eq!(uniforms[0].header[0], 2);
        assert_eq!(uniforms[0].params, [0.25, -0.5, 0.0, 0.0]);
        assert_eq!(uniforms[1].header[0], 3);
        assert_eq!(uniforms[1].params, [0.7, 0.4, 0.0, 0.0]);
        assert!(uniforms[2..].iter().all(|uniform| uniform.header[0] == 0));
    }

    #[test]
    fn gpu_composite_request_rejects_adjustment_without_effect_plan() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 4,
            height: 4,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        assert_eq!(
            validate_request(&request),
            Err(GpuCompositeError::AdjustmentMissingEffectPlan)
        );
    }

    #[test]
    fn working_compositor_rejects_effect_plan_declared_in_external_color_domain() {
        use mondrian_effects::{
            compile_scheduled_effect_graph_in_domain, lower_effect_graph_to_gpu_plan,
            EffectColorDomain, EffectColorDomainContract, EffectRenderOp, EffectRenderPlan,
        };

        let domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid domain graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU point plan");
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 4,
            height: 4,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        assert_eq!(
            validate_request(&request),
            Err(GpuCompositeError::EffectDomainRequiresExternalPass { domain })
        );
    }

    #[test]
    fn point_effect_pass_accepts_matching_display_encoded_intermediate() {
        use mondrian_effects::{
            compile_scheduled_effect_graph_in_domain, lower_effect_graph_to_gpu_plan,
            EffectColorDomain, EffectColorDomainContract, EffectRenderOp, EffectRenderPlan,
        };

        let domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 0.25,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid domain graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU point plan");
        let input = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(120),
            ColorFrameDescriptor {
                width: 1920,
                height: 1080,
                color_space: mondrian_core::ColorSpace::Rec709.into(),
                domain: ColorFrameDomain::Effect,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "display-effect-input",
        )
        .expect("effect input");

        validate_point_effect_input(&input, &plan).expect("matching point effect input");
    }

    #[tokio::test]
    async fn gpu_point_effects_match_cpu_float_reference_on_real_wgpu_device() {
        use crate::color_accuracy::{
            compare_linear_rgba, LinearAccuracyBudget, LinearRgbaAccuracyBudget,
        };
        use mondrian_effects::{
            apply_compiled_effect_graph_pass_rgba_f32, apply_compiled_effect_graph_rgba_f32,
            get_or_compile_scheduled_render_graph, lower_effect_graph_to_gpu_plan,
            EffectGraphBuilderState, EffectRenderOp,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping GPU point-effect parity test: no GPU adapter available");
            return;
        };
        let data = (0..16)
            .map(|index| {
                let value = index as f32 / 15.0;
                [
                    -0.15 + value * 1.6,
                    1.3 - value * 1.4,
                    0.05 + value * 1.2,
                    1.0,
                ]
            })
            .collect::<Vec<_>>();
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709,
            data,
        });
        let mut builder = EffectGraphBuilderState::new();
        builder.append_unary(EffectRenderOp::ColorAdjust {
            exposure: 0.35,
            contrast: 1.15,
            saturation: 0.8,
        });
        builder.append_unary(EffectRenderOp::WhiteBalance { temperature: 0.2, tint: -0.15 });
        builder.append_unary(EffectRenderOp::Vignette { intensity: 0.45, feather: 0.7 });
        builder.append_unary(EffectRenderOp::Grain { amount: 0.1 });
        let graph = get_or_compile_scheduled_render_graph(builder.finish()).expect("valid graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("supported point effects");
        let expected =
            apply_compiled_effect_graph_rgba_f32(&frame.rgba_f32().data, 4, 4, &graph, 23)
                .expect("CPU float reference");

        let media_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 23,
        };
        let actual = readback_test_composite(&context, &[media_layer]);
        assert_test_pixels_accurate(&expected, &actual);

        let adjustment_opacity = 0.55;
        let expected_adjustment = apply_compiled_effect_graph_pass_rgba_f32(
            &frame.rgba_f32().data,
            4,
            4,
            &graph,
            adjustment_opacity,
            None,
            23,
        )
        .expect("CPU float adjustment reference");
        let base_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let adjustment_layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::Adjustment,
            opacity: adjustment_opacity,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: Some(&plan),
            frame_seed: 23,
        };
        let actual_adjustment = readback_test_composite(&context, &[base_layer, adjustment_layer]);
        assert_test_pixels_accurate(&expected_adjustment, &actual_adjustment);

        fn assert_test_pixels_accurate(expected: &[[f32; 4]], actual: &[[f32; 4]]) {
            let report = compare_linear_rgba(
                expected,
                actual,
                LinearRgbaAccuracyBudget {
                    rgb: LinearAccuracyBudget::finite(3.0e-5, 1.0e-5, 2.0e-5),
                    alpha: LinearAccuracyBudget::finite(1.0e-6, 1.0e-7, 1.0e-6),
                },
            )
            .expect("matching GPU and CPU frame shapes");
            assert!(
                report.within_budget,
                "GPU point-effect accuracy budget exceeded: {report:#?}"
            );
        }
    }

    #[tokio::test]
    async fn solid_source_materialization_preserves_linear_rgba() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping solid source materialization test: no GPU adapter available");
            return;
        };
        let color = Color { r: 1.25, g: -0.125, b: 0.375, a: 0.625 };
        let compositor = GpuFrameCompositor::new(&context.device);
        let mut ids = GpuColorFrameIdAllocator::new(950);
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("solid-source-materialization"),
        });
        let record = compositor
            .record_solid_source_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                4,
                4,
                WorkingColorSpace::LinearRec709,
                color,
            )
            .expect("record solid source materialization");
        assert_eq!(record.materialized_pixels, 16);
        assert_eq!(
            record.output.descriptor(),
            ColorFrameDescriptor {
                width: 4,
                height: 4,
                color_space: WorkingColorSpace::LinearRec709.into(),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
            }
        );
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("solid-source-materialization-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("solid source output");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        for row in mapped.chunks_exact(256).take(4) {
            for pixel in bytemuck::cast_slice::<u8, f32>(&row[..64]).chunks_exact(4) {
                assert_eq!(pixel, &[color.r, color.g, color.b, color.a]);
            }
        }
        readback.unmap();
    }

    #[tokio::test]
    async fn compositor_uniform_arena_reuses_one_buffer_across_submitted_frames() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping compositor uniform arena test: no GPU adapter available");
            return;
        };
        let compositor = GpuFrameCompositor::new(&context.device);
        let mut ids = GpuColorFrameIdAllocator::new(975);
        let mut table = GpuColorFrameResourceTable::new();

        for frame in 0..2 {
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("compositor-uniform-arena-reuse"),
                });
            compositor
                .record_solid_source_pass(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    4,
                    4,
                    WorkingColorSpace::LinearRec709,
                    Color { r: frame as f32, g: 0.25, b: 0.5, a: 1.0 },
                )
                .expect("record solid through uniform arena");
            context.queue.submit(std::iter::once(encoder.finish()));
            compositor.clear_frame_resources();
        }

        let diagnostics = compositor.uniform_arena_diagnostics();
        assert_eq!(diagnostics.buffer_creations, 1);
        assert_eq!(diagnostics.uniform_writes, 2);
        assert_eq!(diagnostics.high_watermark_slots, 1);
        assert_eq!(diagnostics.frame_resets, 2);
        assert_eq!(diagnostics.exhaustions, 0);
        assert_eq!(
            compositor.texture_binding_diagnostics().bind_group_creations,
            2
        );
        assert_eq!(compositor.texture_binding_diagnostics().cache_hits, 4);
    }

    #[tokio::test]
    async fn compositor_reuses_bind_groups_with_pooled_input_textures() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping compositor texture-binding cache test: no GPU adapter available");
            return;
        };
        fn insert_inputs(
            device: &wgpu::Device,
            ids: &mut GpuColorFrameIdAllocator,
            table: &mut GpuColorFrameResourceTable<GpuColorFrameWgpuResource>,
            pool: &GpuColorFrameWgpuResourcePool,
            descriptor: ColorFrameDescriptor,
        ) -> [GpuColorFrameHandle; 2] {
            ["binding-cache-input-a", "binding-cache-input-b"].map(|label| {
                let handle = GpuColorFrameHandle::new(
                    ids.allocate(),
                    descriptor,
                    GpuColorFrameTextureFormat::Rgba32Float,
                    label,
                )
                .expect("working input handle");
                let allocation = GpuColorFrameAllocationPlan::for_handle(handle.clone());
                table.insert(pool.acquire(device, &allocation)).expect("insert working input");
                handle
            })
        }

        let compositor = GpuFrameCompositor::new(&context.device);
        let mut ids = GpuColorFrameIdAllocator::new(1_025);
        let mut table = GpuColorFrameResourceTable::new();
        let pool = GpuColorFrameWgpuResourcePool::default();
        let descriptor = ColorFrameDescriptor {
            width: 4,
            height: 4,
            color_space: WorkingColorSpace::LinearRec709.into(),
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let mut inputs = insert_inputs(&context.device, &mut ids, &mut table, &pool, descriptor);
        let mut after_first = None;

        for frame in 0..2 {
            let layer = |handle| GpuCompositeLayer {
                source: GpuCompositeLayerSource::GpuFrame(handle),
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
                transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                effect_plan: None,
                frame_seed: 0,
            };
            let layers = [layer(&inputs[0]), layer(&inputs[1])];
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("compositor-texture-binding-cache"),
                });
            compositor
                .record(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &mut ids,
                    &mut table,
                    None,
                    GpuCompositeRequest {
                        width: 4,
                        height: 4,
                        working_color_space: WorkingColorSpace::LinearRec709,
                        layers: &layers,
                    },
                )
                .expect("record pooled-input composite");
            context.queue.submit(std::iter::once(encoder.finish()));
            compositor.clear_frame_resources();
            if frame == 0 {
                after_first = Some(compositor.texture_binding_diagnostics());
                for handle in &inputs {
                    let resource =
                        table.remove(handle.id()).expect("remove submitted input resource");
                    pool.release(resource);
                }
                inputs = insert_inputs(&context.device, &mut ids, &mut table, &pool, descriptor);
            }
        }

        let after_first = after_first.expect("first frame diagnostics");
        let after_second = compositor.texture_binding_diagnostics();
        assert_eq!(
            after_second.bind_group_creations - after_first.bind_group_creations,
            2,
            "only the two new accumulator textures should need bindings"
        );
        assert_eq!(
            after_second.cache_hits - after_first.cache_hits,
            2,
            "both pooled input textures should reuse their layer bindings"
        );
        assert_eq!(pool.diagnostics().hits, 2);
    }

    #[tokio::test]
    async fn standalone_effect_domain_point_pass_matches_numeric_reference() {
        use mondrian_effects::{
            compile_scheduled_effect_graph_in_domain, lower_effect_graph_to_gpu_plan,
            EffectColorDomain, EffectColorDomainContract, EffectRenderOp, EffectRenderPlan,
        };

        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping standalone point-effect test: no GPU adapter available");
            return;
        };
        let domain =
            EffectColorDomain::DisplayEncodedRgb { color_space: mondrian_core::ColorSpace::Rec709 };
        let graph = compile_scheduled_effect_graph_in_domain(
            &EffectRenderPlan {
                ops: vec![EffectRenderOp::ColorAdjust {
                    exposure: 1.0,
                    contrast: 1.0,
                    saturation: 1.0,
                }],
            },
            EffectColorDomainContract::preserving(domain),
        )
        .expect("valid display-domain graph");
        let plan = lower_effect_graph_to_gpu_plan(&graph).expect("GPU point plan");
        let data = (0..16)
            .map(|index| {
                let value = index as f32 / 32.0;
                [value, value * 0.75, value * 0.5, 1.0]
            })
            .collect::<Vec<_>>();
        let expected = data
            .iter()
            .map(|pixel| [pixel[0] * 2.0, pixel[1] * 2.0, pixel[2] * 2.0, pixel[3]])
            .collect::<Vec<_>>();
        let descriptor = ColorFrameDescriptor {
            width: 4,
            height: 4,
            color_space: mondrian_core::ColorSpace::Rec709.into(),
            domain: ColorFrameDomain::Effect,
            encoding: ColorFrameEncoding::EncodedFloat,
            residency: ColorFrameResidency::Gpu,
        };
        let input = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(900),
            descriptor,
            GpuColorFrameTextureFormat::Rgba32Float,
            "standalone-effect-input",
        )
        .expect("effect input");
        let input_allocation = GpuColorFrameAllocationPlan::for_handle(input.clone());
        let input_resource = GpuColorFrameUploader::allocate(&context.device, &input_allocation);
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &input_resource.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&data),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(64),
                rows_per_image: Some(4),
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        let compositor = GpuFrameCompositor::new(&context.device);
        let mut ids = GpuColorFrameIdAllocator::new(901);
        let mut table = GpuColorFrameResourceTable::new();
        table.insert(input_resource).expect("insert effect input");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("standalone-effect-domain-point-pass"),
        });
        let record = compositor
            .record_point_effect_pass(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                &input,
                &plan,
                0,
            )
            .expect("record standalone point pass");
        assert_eq!(record.output.descriptor(), descriptor);
        assert_eq!(record.processed_pixels, 16);
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("standalone-effect-domain-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("point pass output");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        let actual = mapped
            .chunks_exact(256)
            .take(4)
            .flat_map(|row| {
                bytemuck::cast_slice::<u8, f32>(&row[..64])
                    .chunks_exact(4)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            })
            .collect::<Vec<_>>();
        readback.unmap();

        for (expected, actual) in expected.iter().zip(actual.iter()) {
            for channel in 0..4 {
                assert!((expected[channel] - actual[channel]).abs() <= 1.0e-6);
            }
        }
    }

    fn readback_test_composite(
        context: &crate::GpuContext,
        layers: &[GpuCompositeLayer<'_>],
    ) -> Vec<[f32; 4]> {
        let compositor = GpuFrameCompositor::new(&context.device);
        let mut ids = GpuColorFrameIdAllocator::default();
        let mut table = GpuColorFrameResourceTable::new();
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian-test-gpu-point-effect-parity"),
        });
        let record = compositor
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                &mut ids,
                &mut table,
                None,
                GpuCompositeRequest {
                    width: 4,
                    height: 4,
                    working_color_space: WorkingColorSpace::LinearRec709,
                    layers,
                },
            )
            .expect("record GPU effect composite");
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian-test-gpu-point-effect-readback"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let output = table.get(&record.output).expect("composite output resource");
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &output.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(4),
                },
            },
            wgpu::Extent3d { width: 4, height: 4, depth_or_array_layers: 1 },
        );
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_test_readback(&context.device, &readback);
        let mut actual = Vec::with_capacity(16);
        for row in mapped.chunks_exact(256).take(4) {
            actual.extend(
                bytemuck::cast_slice::<u8, f32>(&row[..64])
                    .chunks_exact(4)
                    .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]]),
            );
        }
        readback.unmap();
        actual
    }

    fn map_test_readback(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        rx.recv().expect("readback callback").expect("readback mapping");
        slice.get_mapped_range().expect("mapped readback range").to_vec()
    }

    #[test]
    fn gpu_composite_request_rejects_unsupported_blend_mode() {
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::SolidColor(Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 }),
            opacity: 1.0,
            blend_mode: BlendMode::Multiply,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
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
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request).expect("GPU compositor should support affine media sampling");
    }

    #[test]
    fn gpu_composite_request_accepts_gpu_resident_media_frame() {
        let handle = gpu_working_handle(10, WorkingColorSpace::LinearRec709);
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [2.0, 0.0, 0.0, 0.0, 2.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        validate_request(&request).expect("GPU compositor should accept GPU-resident media layer");
    }

    #[test]
    fn gpu_composite_request_rejects_gpu_frame_color_space_mismatch() {
        let handle = gpu_working_handle(11, WorkingColorSpace::LinearP3D65);
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::GpuFrame(&handle),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
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
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearRec709,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 16,
            height: 16,
            working_color_space: WorkingColorSpace::LinearRec709,
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
        let frame = CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            color_space: WorkingColorSpace::LinearP3D65,
            data: vec![[0.0, 0.0, 0.0, 1.0]; 64],
        });
        let layer = GpuCompositeLayer {
            source: GpuCompositeLayerSource::CpuFrame(&frame),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            transform: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            effect_plan: None,
            frame_seed: 0,
        };
        let request = GpuCompositeRequest {
            width: 8,
            height: 8,
            working_color_space: WorkingColorSpace::LinearRec709,
            layers: &[layer],
        };

        let err = validate_request(&request).expect_err("color space mismatch should be blocked");

        assert!(matches!(
            err,
            GpuCompositeError::SourceDescriptorMismatch { .. }
        ));
    }

    fn gpu_working_handle(id: u64, color_space: WorkingColorSpace) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(id),
            ColorFrameDescriptor {
                width: 8,
                height: 8,
                color_space: color_space.into(),
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
