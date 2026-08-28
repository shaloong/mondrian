//! Working-linear spatial processing for Viewer presentation.
//!
//! Spatial filtering must happen before an OCIO display/output transform. This
//! module therefore accepts and produces only GPU-resident RGBA32F working
//! frames. Downscales build a bounded box-prefilter pyramid before a separable
//! Lanczos3 reconstruction pass, keeping the final filter footprint bounded.

use std::sync::Arc;

use crate::{
    ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding,
    ColorFrameResidency, ColorFrameSpace, GpuColorFrameAllocationPlan, GpuColorFrameHandle,
    GpuColorFrameHandleError, GpuColorFrameIdAllocationError, GpuColorFrameIdAllocator,
    GpuColorFrameResource, GpuColorFrameTextureFormat, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool,
};
use bytemuck::{Pod, Zeroable};
use thiserror::Error;
use wgpu::util::DeviceExt;

const MAX_LANCZOS_SAMPLES: u32 = 32;
const VIEWER_SPATIAL_SHADER: &str = r#"
struct VsOut {
    @builtin(position) position: vec4<f32>,
};

struct SpatialUniforms {
    input_output_size: vec4<u32>,
    axis_flags: vec4<u32>,
    source_rect: vec4<f32>,
};

@group(0) @binding(0) var source_tex: texture_2d<f32>;
@group(0) @binding(1) var<uniform> uniforms: SpatialUniforms;

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn to_premultiplied(sample: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(sample.rgb * clamp(sample.a, 0.0, 1.0), clamp(sample.a, 0.0, 1.0));
}

fn from_premultiplied(sample: vec4<f32>) -> vec4<f32> {
    let raw_alpha = sample.a;
    if (raw_alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sample.rgb / raw_alpha, clamp(raw_alpha, 0.0, 1.0));
}

fn sinc(value: f32) -> f32 {
    if (abs(value) < 0.000001) {
        return 1.0;
    }
    let angle = value * 3.141592653589793;
    return sin(angle) / angle;
}

fn lanczos3(distance: f32, filter_scale: f32) -> f32 {
    let scaled = distance * filter_scale;
    if (abs(scaled) >= 3.0) {
        return 0.0;
    }
    return sinc(scaled) * sinc(scaled / 3.0) * filter_scale;
}

@fragment
fn fs_downsample(in: VsOut) -> @location(0) vec4<f32> {
    let input_size = vec2<i32>(uniforms.input_output_size.xy);
    let dst = vec2<i32>(in.position.xy);
    let base = dst * 2;
    var sum = vec4<f32>(0.0);
    var count = 0.0;
    for (var y = 0; y < 2; y = y + 1) {
        for (var x = 0; x < 2; x = x + 1) {
            let coordinate = base + vec2<i32>(x, y);
            if (coordinate.x < input_size.x && coordinate.y < input_size.y) {
                sum += to_premultiplied(textureLoad(source_tex, coordinate, 0));
                count += 1.0;
            }
        }
    }
    return from_premultiplied(sum / max(count, 1.0));
}

@fragment
fn fs_lanczos(in: VsOut) -> @location(0) vec4<f32> {
    let input_size = vec2<i32>(uniforms.input_output_size.xy);
    let output_size = vec2<f32>(uniforms.input_output_size.zw);
    let axis = uniforms.axis_flags.x;
    let input_is_premultiplied = uniforms.axis_flags.y != 0u;
    let output_is_premultiplied = uniforms.axis_flags.z != 0u;
    let dst = vec2<i32>(in.position.xy);

    let input_axis_length = select(f32(input_size.x), f32(input_size.y), axis == 1u);
    let output_axis_length = select(output_size.x, output_size.y, axis == 1u);
    let source_origin = select(
        uniforms.source_rect.x * f32(input_size.x),
        uniforms.source_rect.y * f32(input_size.y),
        axis == 1u,
    );
    let source_length = select(
        uniforms.source_rect.z * f32(input_size.x),
        uniforms.source_rect.w * f32(input_size.y),
        axis == 1u,
    );
    let destination_axis = select(f32(dst.x), f32(dst.y), axis == 1u);
    let source_position = source_origin
        + (destination_axis + 0.5) * source_length / output_axis_length - 0.5;
    let filter_scale = min(output_axis_length / source_length, 1.0);
    let support = 3.0 / filter_scale;
    var first = i32(ceil(source_position - support));
    let last = i32(floor(source_position + support));
    if (last - first + 1 > 32) {
        first = i32(floor(source_position)) - 15;
    }

    var weighted = vec4<f32>(0.0);
    var weight_sum = 0.0;
    for (var tap = 0u; tap < 32u; tap = tap + 1u) {
        let sample_axis = first + i32(tap);
        if (sample_axis > last) {
            break;
        }
        let clamped_axis = clamp(sample_axis, 0, i32(input_axis_length) - 1);
        let coordinate = select(
            vec2<i32>(clamped_axis, dst.y),
            vec2<i32>(dst.x, clamped_axis),
            axis == 1u,
        );
        var sample = textureLoad(source_tex, coordinate, 0);
        if (!input_is_premultiplied) {
            sample = to_premultiplied(sample);
        }
        let weight = lanczos3(f32(sample_axis) - source_position, filter_scale);
        weighted += sample * weight;
        weight_sum += weight;
    }
    let normalized = weighted / max(abs(weight_sum), 0.0000001);
    if (output_is_premultiplied) {
        return normalized;
    }
    return from_premultiplied(normalized);
}
"#;

/// Normalized source region presented in the Viewer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerSourceRect {
    /// Left edge in normalized source coordinates.
    pub x: f32,
    /// Top edge in normalized source coordinates.
    pub y: f32,
    /// Width in normalized source coordinates.
    pub width: f32,
    /// Height in normalized source coordinates.
    pub height: f32,
}

impl ViewerSourceRect {
    /// The complete source image.
    pub const FULL: Self = Self { x: 0.0, y: 0.0, width: 1.0, height: 1.0 };

    fn validate(self) -> Result<(), GpuViewerSpatialPlanError> {
        if ![self.x, self.y, self.width, self.height].into_iter().all(f32::is_finite) {
            return Err(GpuViewerSpatialPlanError::NonFiniteSourceRect);
        }
        if self.x < 0.0
            || self.y < 0.0
            || self.width <= 0.0
            || self.height <= 0.0
            || self.x + self.width > 1.0
            || self.y + self.height > 1.0
        {
            return Err(GpuViewerSpatialPlanError::SourceRectOutOfBounds);
        }
        Ok(())
    }
}

/// Validated working-linear Viewer spatial pass.
#[derive(Debug, Clone)]
pub struct GpuViewerSpatialPlan {
    /// GPU-resident working-linear input.
    pub input: GpuColorFrameHandle,
    /// GPU-resident working-linear output entering the display boundary.
    pub output: GpuColorFrameHandle,
    /// Visible normalized region of the input.
    pub source_rect: ViewerSourceRect,
}

impl GpuViewerSpatialPlan {
    /// Validate a Viewer spatial request and allocate its typed output identity.
    pub fn new(
        ids: &mut GpuColorFrameIdAllocator,
        input: GpuColorFrameHandle,
        source_rect: ViewerSourceRect,
        output_width: u32,
        output_height: u32,
    ) -> Result<Self, GpuViewerSpatialPlanError> {
        let descriptor =
            validate_spatial_request(&input, source_rect, output_width, output_height)?;
        let output = GpuColorFrameHandle::new(
            ids.allocate()?,
            ColorFrameDescriptor {
                width: output_width,
                height: output_height,
                ..descriptor
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "viewer-working-spatial-output",
        )?;
        Ok(Self { input, output, source_rect })
    }
}

fn validate_spatial_request(
    input: &GpuColorFrameHandle,
    source_rect: ViewerSourceRect,
    output_width: u32,
    output_height: u32,
) -> Result<ColorFrameDescriptor, GpuViewerSpatialPlanError> {
    source_rect.validate()?;
    if output_width == 0 || output_height == 0 {
        return Err(GpuViewerSpatialPlanError::EmptyOutputExtent {
            width: output_width,
            height: output_height,
        });
    }
    let descriptor = input.descriptor();
    if descriptor.domain != ColorFrameDomain::Working
        || descriptor.encoding != ColorFrameEncoding::LinearFloat
        || descriptor.residency != ColorFrameResidency::Gpu
        || !matches!(descriptor.color_space, ColorFrameSpace::Working(_))
    {
        return Err(GpuViewerSpatialPlanError::InputNotWorkingLinear { actual: descriptor });
    }
    if input.texture_format() != GpuColorFrameTextureFormat::Rgba32Float {
        return Err(GpuViewerSpatialPlanError::InputNotRgba32Float {
            actual: input.texture_format(),
        });
    }
    if !descriptor.alpha.is_straight_compatible() {
        return Err(GpuViewerSpatialPlanError::InputNotStraightCompatibleAlpha {
            actual: descriptor.alpha,
        });
    }
    let source_width = f64::from(descriptor.width) * f64::from(source_rect.width);
    let source_height = f64::from(descriptor.height) * f64::from(source_rect.height);
    let scale_x = f64::from(output_width) / source_width;
    let scale_y = f64::from(output_height) / source_height;
    let anisotropy = scale_x.max(scale_y) / scale_x.min(scale_y);
    if !anisotropy.is_finite() || anisotropy > 2.0 {
        return Err(GpuViewerSpatialPlanError::ExcessiveScaleAnisotropy);
    }
    Ok(descriptor)
}

/// Viewer spatial planning failure.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum GpuViewerSpatialPlanError {
    /// Source crop contains a non-finite component.
    #[error("Viewer source rect contains a non-finite component")]
    NonFiniteSourceRect,
    /// Source crop is empty or outside normalized source bounds.
    #[error("Viewer source rect must be non-empty and contained in normalized source bounds")]
    SourceRectOutOfBounds,
    /// Target dimensions are empty.
    #[error("Viewer spatial output extent must be non-zero, got {width}x{height}")]
    EmptyOutputExtent {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
    },
    /// Input does not carry a GPU working-linear descriptor.
    #[error("Viewer spatial input is not a GPU working-linear frame: {actual:?}")]
    InputNotWorkingLinear {
        /// Rejected input descriptor.
        actual: ColorFrameDescriptor,
    },
    /// Working storage is not the mandatory 32-bit float format.
    #[error("Viewer spatial input must use RGBA32F, got {actual:?}")]
    InputNotRgba32Float {
        /// Rejected storage format.
        actual: GpuColorFrameTextureFormat,
    },
    /// Public Viewer spatial input must use straight or opaque coverage.
    #[error("Viewer spatial input must carry straight-compatible coverage, got {actual:?}")]
    InputNotStraightCompatibleAlpha {
        /// Rejected RGB/coverage association.
        actual: ColorFrameAlpha,
    },
    /// Viewer reconstruction is intentionally aspect-preserving.
    #[error("Viewer spatial scale anisotropy exceeds the supported 2:1 bound")]
    ExcessiveScaleAnisotropy,
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// Output handle creation failed.
    #[error(transparent)]
    OutputHandle(#[from] GpuColorFrameHandleError),
}

/// Cumulative runtime evidence for Viewer spatial processing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GpuViewerSpatialRuntimeDiagnostics {
    /// Spatial pipeline sets constructed.
    pub pipeline_builds: u64,
    /// Frames spatially processed.
    pub records: u64,
    /// Full-frame identity requests that reused their input working texture.
    pub passthrough_frames: u64,
    /// Box-prefilter passes recorded across frames.
    pub prefilter_passes: u64,
    /// Lanczos passes recorded across frames.
    pub lanczos_passes: u64,
    /// Output pixels produced across frames.
    pub output_pixels: u64,
}

/// Renderer-owned Viewer spatial pipeline and per-frame resources.
#[derive(Default)]
pub struct GpuViewerSpatialRuntime {
    pipeline: Option<GpuViewerSpatialPipeline>,
    prefilters: Vec<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    horizontal: Option<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    output: Option<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    diagnostics: GpuViewerSpatialRuntimeDiagnostics,
}

impl GpuViewerSpatialRuntime {
    /// Create a spatial runtime that shares a device-scoped texture pool with
    /// compositing and color-output stages.
    pub fn with_resource_pool(resource_pool: Arc<GpuColorFrameWgpuResourcePool>) -> Self {
        Self { resource_pool, ..Self::default() }
    }

    /// Reuse an identity input or record the required crop/resize passes.
    ///
    /// A reused output remains owned by the caller's resource table. A
    /// materialized output remains owned by this runtime until `take_output`.
    pub fn record_for_presentation(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        source_rect: ViewerSourceRect,
        output_width: u32,
        output_height: u32,
    ) -> Result<GpuViewerSpatialRecord, GpuViewerSpatialRuntimeError> {
        if spatial_request_is_identity(input.handle(), source_rect, output_width, output_height) {
            self.clear_frame_resources();
            validate_spatial_request(input.handle(), source_rect, output_width, output_height)?;
            self.diagnostics.records = self.diagnostics.records.saturating_add(1);
            self.diagnostics.passthrough_frames =
                self.diagnostics.passthrough_frames.saturating_add(1);
            self.diagnostics.output_pixels = self
                .diagnostics
                .output_pixels
                .saturating_add(u64::from(output_width).saturating_mul(u64::from(output_height)));
            return Ok(GpuViewerSpatialRecord::Reused(input.handle().clone()));
        }
        self.record(
            device,
            encoder,
            ids,
            input,
            source_rect,
            output_width,
            output_height,
        )
        .map(GpuViewerSpatialRecord::Materialized)
    }

    /// Record working-linear crop and resize passes and retain the output resource.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        source_rect: ViewerSourceRect,
        output_width: u32,
        output_height: u32,
    ) -> Result<GpuColorFrameHandle, GpuViewerSpatialRuntimeError> {
        self.clear_frame_resources();
        let limit = device.limits().max_texture_dimension_2d;
        if output_width > limit || output_height > limit {
            return Err(GpuViewerSpatialRuntimeError::OutputExceedsDeviceLimit {
                width: output_width,
                height: output_height,
                limit,
            });
        }
        let plan = GpuViewerSpatialPlan::new(
            ids,
            input.handle().clone(),
            source_rect,
            output_width,
            output_height,
        )?;
        if self.pipeline.is_none() {
            self.pipeline = Some(GpuViewerSpatialPipeline::new(device));
            self.diagnostics.pipeline_builds = self.diagnostics.pipeline_builds.saturating_add(1);
        }
        let pipeline =
            self.pipeline
                .as_ref()
                .ok_or(GpuViewerSpatialRuntimeError::InternalResourceMissing(
                    "pipeline",
                ))?;

        let mut selected_view = &input.resource().texture_view;
        let mut selected_width = plan.input.descriptor().width;
        let mut selected_height = plan.input.descriptor().height;
        let mut selected_alpha = plan.input.descriptor().alpha;
        while should_prefilter(
            selected_width,
            selected_height,
            source_rect,
            output_width,
            output_height,
        ) {
            let next_width = selected_width.div_ceil(2);
            let next_height = selected_height.div_ceil(2);
            let resource = allocate_private_working_resource(
                device,
                &self.resource_pool,
                ids,
                plan.input.descriptor().color_space,
                selected_alpha,
                next_width,
                next_height,
                "viewer-working-spatial-prefilter",
            )?;
            pipeline.record_downsample(
                device,
                encoder,
                selected_view,
                selected_width,
                selected_height,
                &resource.resource().texture_view,
            );
            self.prefilters.push(resource);
            selected_view = &self
                .prefilters
                .last()
                .ok_or(GpuViewerSpatialRuntimeError::InternalResourceMissing(
                    "prefilter",
                ))?
                .resource()
                .texture_view;
            selected_width = next_width;
            selected_height = next_height;
            selected_alpha = self
                .prefilters
                .last()
                .ok_or(GpuViewerSpatialRuntimeError::InternalResourceMissing(
                    "prefilter",
                ))?
                .handle()
                .descriptor()
                .alpha;
            self.diagnostics.prefilter_passes = self.diagnostics.prefilter_passes.saturating_add(1);
        }

        let horizontal = allocate_private_working_resource(
            device,
            &self.resource_pool,
            ids,
            plan.input.descriptor().color_space,
            ColorFrameAlpha::PremultipliedCoverage,
            output_width,
            selected_height,
            "viewer-working-spatial-horizontal",
        )?;
        let horizontal_alpha = horizontal.handle().descriptor().alpha;
        pipeline.record_lanczos(
            device,
            encoder,
            selected_view,
            (selected_width, selected_height),
            &horizontal.resource().texture_view,
            (output_width, selected_height),
            source_rect,
            SpatialAxis::Horizontal,
            selected_alpha,
            horizontal_alpha,
        );
        self.horizontal = Some(horizontal);

        let output = self.resource_pool.acquire(
            device,
            &GpuColorFrameAllocationPlan::for_handle(plan.output.clone()),
        );
        let horizontal_alpha = self
            .horizontal
            .as_ref()
            .ok_or(GpuViewerSpatialRuntimeError::InternalResourceMissing(
                "horizontal",
            ))?
            .handle()
            .descriptor()
            .alpha;
        let output_alpha = output.handle().descriptor().alpha;
        pipeline.record_lanczos(
            device,
            encoder,
            &self
                .horizontal
                .as_ref()
                .ok_or(GpuViewerSpatialRuntimeError::InternalResourceMissing(
                    "horizontal",
                ))?
                .resource()
                .texture_view,
            (output_width, selected_height),
            &output.resource().texture_view,
            (output_width, output_height),
            source_rect,
            SpatialAxis::Vertical,
            horizontal_alpha,
            output_alpha,
        );
        self.output = Some(output);
        self.diagnostics.records = self.diagnostics.records.saturating_add(1);
        self.diagnostics.lanczos_passes = self.diagnostics.lanczos_passes.saturating_add(2);
        self.diagnostics.output_pixels = self
            .diagnostics
            .output_pixels
            .saturating_add(u64::from(output_width).saturating_mul(u64::from(output_height)));
        Ok(plan.output)
    }

    /// Resolve a retained output by exact typed handle contract.
    pub fn output(
        &self,
        handle: &GpuColorFrameHandle,
    ) -> Option<&GpuColorFrameResource<GpuColorFrameWgpuResource>> {
        self.output.as_ref().filter(|resource| resource.handle() == handle)
    }

    /// Transfer the exact spatial output into a downstream renderer resource table.
    pub fn take_output(
        &mut self,
        handle: &GpuColorFrameHandle,
    ) -> Option<GpuColorFrameResource<GpuColorFrameWgpuResource>> {
        if self.output.as_ref().is_some_and(|resource| resource.handle() == handle) {
            self.output.take()
        } else {
            None
        }
    }

    /// Drop frame-local intermediate and output textures.
    pub fn clear_frame_resources(&mut self) {
        for resource in self.prefilters.drain(..) {
            self.resource_pool.release(resource);
        }
        if let Some(resource) = self.horizontal.take() {
            self.resource_pool.release(resource);
        }
        if let Some(resource) = self.output.take() {
            self.resource_pool.release(resource);
        }
    }

    /// Drop the spatial pipeline and return frame resources to the shared pool.
    /// The device owner is responsible for clearing that pool after all stages
    /// have returned their resources during device replacement.
    pub fn clear(&mut self) {
        self.clear_frame_resources();
        self.pipeline = None;
    }

    /// Return cumulative runtime evidence.
    pub const fn diagnostics(&self) -> GpuViewerSpatialRuntimeDiagnostics {
        self.diagnostics
    }
}

/// Ownership result for one Viewer presentation spatial request.
pub enum GpuViewerSpatialRecord {
    /// The input handle already has the exact presentation geometry.
    Reused(GpuColorFrameHandle),
    /// The spatial runtime owns a newly materialized output.
    Materialized(GpuColorFrameHandle),
}

impl GpuViewerSpatialRecord {
    /// Borrow the working handle entering the display boundary.
    pub const fn output(&self) -> &GpuColorFrameHandle {
        match self {
            Self::Reused(output) | Self::Materialized(output) => output,
        }
    }
}

fn spatial_request_is_identity(
    input: &GpuColorFrameHandle,
    source_rect: ViewerSourceRect,
    output_width: u32,
    output_height: u32,
) -> bool {
    let descriptor = input.descriptor();
    source_rect == ViewerSourceRect::FULL
        && descriptor.width == output_width
        && descriptor.height == output_height
}

/// Viewer spatial runtime failure.
#[derive(Debug, Error)]
pub enum GpuViewerSpatialRuntimeError {
    /// Typed request validation failed.
    #[error(transparent)]
    Plan(#[from] GpuViewerSpatialPlanError),
    /// Requested output cannot be allocated on the active device.
    #[error("Viewer spatial output {width}x{height} exceeds device limit {limit}")]
    OutputExceedsDeviceLimit {
        /// Requested width.
        width: u32,
        /// Requested height.
        height: u32,
        /// Active device limit.
        limit: u32,
    },
    /// Renderer frame identity allocation is exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// A private working handle could not be created.
    #[error(transparent)]
    PrivateHandle(#[from] GpuColorFrameHandleError),
    /// An internal runtime invariant was violated.
    #[error("Viewer spatial runtime missing internal {0}")]
    InternalResourceMissing(&'static str),
}

struct GpuViewerSpatialPipeline {
    downsample: wgpu::RenderPipeline,
    lanczos: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
}

impl GpuViewerSpatialPipeline {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.viewer-spatial.shader"),
            source: wgpu::ShaderSource::Wgsl(VIEWER_SPATIAL_SHADER.into()),
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.viewer-spatial.bindings"),
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
            label: Some("mondrian.viewer-spatial.layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let make_pipeline = |label: &'static str, entry_point: &'static str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry_point),
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
                    cull_mode: None,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let downsample = make_pipeline("mondrian.viewer-spatial.downsample", "fs_downsample");
        let lanczos = make_pipeline("mondrian.viewer-spatial.lanczos3", "fs_lanczos");
        Self { downsample, lanczos, bindings }
    }

    fn record_downsample(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        input_width: u32,
        input_height: u32,
        output: &wgpu::TextureView,
    ) {
        let uniforms = SpatialUniforms {
            input_output_size: [
                input_width,
                input_height,
                input_width.div_ceil(2),
                input_height.div_ceil(2),
            ],
            axis_flags: [0; 4],
            source_rect: [0.0, 0.0, 1.0, 1.0],
        };
        self.record_pass(device, encoder, input, output, &uniforms, &self.downsample);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_lanczos(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        input_size: (u32, u32),
        output: &wgpu::TextureView,
        output_size: (u32, u32),
        source_rect: ViewerSourceRect,
        axis: SpatialAxis,
        input_alpha: ColorFrameAlpha,
        output_alpha: ColorFrameAlpha,
    ) {
        let uniforms = SpatialUniforms {
            input_output_size: [input_size.0, input_size.1, output_size.0, output_size.1],
            axis_flags: spatial_axis_flags(axis, input_alpha, output_alpha),
            source_rect: [
                source_rect.x,
                source_rect.y,
                source_rect.width,
                source_rect.height,
            ],
        };
        self.record_pass(device, encoder, input, output, &uniforms, &self.lanczos);
    }

    fn record_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &SpatialUniforms,
        pipeline: &wgpu::RenderPipeline,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.viewer-spatial.uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.viewer-spatial.bind-group"),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(input),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.viewer-spatial.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: output,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SpatialUniforms {
    input_output_size: [u32; 4],
    axis_flags: [u32; 4],
    source_rect: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
enum SpatialAxis {
    Horizontal = 0,
    Vertical = 1,
}

fn spatial_axis_flags(
    axis: SpatialAxis,
    input_alpha: ColorFrameAlpha,
    output_alpha: ColorFrameAlpha,
) -> [u32; 4] {
    [
        axis as u32,
        u32::from(input_alpha.is_premultiplied()),
        u32::from(output_alpha.is_premultiplied()),
        MAX_LANCZOS_SAMPLES,
    ]
}

fn should_prefilter(
    input_width: u32,
    input_height: u32,
    source_rect: ViewerSourceRect,
    output_width: u32,
    output_height: u32,
) -> bool {
    let source_width = input_width as f64 * f64::from(source_rect.width);
    let source_height = input_height as f64 * f64::from(source_rect.height);
    (source_width > f64::from(output_width) * 4.0 || source_height > f64::from(output_height) * 4.0)
        && input_width > 1
        && input_height > 1
}

fn allocate_private_working_resource(
    device: &wgpu::Device,
    resource_pool: &GpuColorFrameWgpuResourcePool,
    ids: &mut GpuColorFrameIdAllocator,
    color_space: ColorFrameSpace,
    alpha: ColorFrameAlpha,
    width: u32,
    height: u32,
    label: &'static str,
) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, GpuViewerSpatialRuntimeError> {
    let handle = GpuColorFrameHandle::new(
        ids.allocate()?,
        ColorFrameDescriptor {
            width,
            height,
            color_space,
            domain: ColorFrameDomain::Working,
            encoding: ColorFrameEncoding::LinearFloat,
            residency: ColorFrameResidency::Gpu,
            alpha,
        },
        GpuColorFrameTextureFormat::Rgba32Float,
        label,
    )?;
    Ok(resource_pool.acquire(device, &GpuColorFrameAllocationPlan::for_handle(handle)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        GpuColorFrameReadback, GpuColorFrameReadbackPlan, GpuColorFrameUploadPlan,
        GpuColorFrameUploader,
    };
    use mondrian_core::{
        ensure_mondrian_default_ocio_loaded, ColorEngine, ColorSpace, WorkingColorSpace,
        WorkingRgbaF32Frame,
    };

    #[test]
    fn plan_rejects_encoded_and_half_float_inputs() {
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame id allocator");
        let encoded = GpuColorFrameHandle::new(
            ids.allocate().expect("encoded frame id"),
            ColorFrameDescriptor {
                width: 4,
                height: 4,
                color_space: ColorFrameSpace::Color(mondrian_core::ColorSpace::Srgb),
                domain: ColorFrameDomain::Display,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::StraightCoverage,
            },
            GpuColorFrameTextureFormat::Rgba16Float,
            "encoded",
        )
        .expect("encoded handle");
        assert!(matches!(
            GpuViewerSpatialPlan::new(&mut ids, encoded, ViewerSourceRect::FULL, 2, 2),
            Err(GpuViewerSpatialPlanError::InputNotWorkingLinear { .. })
        ));

        let half = working_handle(&mut ids, 4, 4, GpuColorFrameTextureFormat::Rgba16Float);
        assert!(matches!(
            GpuViewerSpatialPlan::new(&mut ids, half, ViewerSourceRect::FULL, 2, 2),
            Err(GpuViewerSpatialPlanError::InputNotRgba32Float { .. })
        ));

        let anisotropic =
            working_handle(&mut ids, 100, 100, GpuColorFrameTextureFormat::Rgba32Float);
        assert!(matches!(
            GpuViewerSpatialPlan::new(&mut ids, anisotropic, ViewerSourceRect::FULL, 1, 100,),
            Err(GpuViewerSpatialPlanError::ExcessiveScaleAnisotropy)
        ));
    }

    #[test]
    fn plan_rejects_premultiplied_public_working_input() {
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame id allocator");
        let input = GpuColorFrameHandle::new(
            ids.allocate().expect("input frame id"),
            ColorFrameDescriptor {
                width: 4,
                height: 4,
                color_space: ColorFrameSpace::Working(WorkingColorSpace::LinearRec709),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::PremultipliedCoverage,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "premultiplied-public-input",
        )
        .expect("structurally valid GPU handle");

        assert!(matches!(
            GpuViewerSpatialPlan::new(&mut ids, input, ViewerSourceRect::FULL, 2, 2),
            Err(GpuViewerSpatialPlanError::InputNotStraightCompatibleAlpha {
                actual: ColorFrameAlpha::PremultipliedCoverage
            })
        ));
    }

    #[test]
    fn shader_alpha_flags_are_derived_from_frame_contracts() {
        assert_eq!(
            spatial_axis_flags(
                SpatialAxis::Horizontal,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::PremultipliedCoverage,
            ),
            [0, 0, 1, MAX_LANCZOS_SAMPLES]
        );
        assert_eq!(
            spatial_axis_flags(
                SpatialAxis::Vertical,
                ColorFrameAlpha::PremultipliedCoverage,
                ColorFrameAlpha::StraightCoverage,
            ),
            [1, 1, 0, MAX_LANCZOS_SAMPLES]
        );
        assert_eq!(
            spatial_axis_flags(
                SpatialAxis::Vertical,
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::Opaque,
            ),
            [1, 0, 0, MAX_LANCZOS_SAMPLES]
        );
    }

    #[test]
    fn prefilter_selection_bounds_final_lanczos_footprint() {
        let rect = ViewerSourceRect::FULL;
        assert!(should_prefilter(7680, 4320, rect, 480, 270));
        assert!(!should_prefilter(1920, 1080, rect, 480, 270));
        assert!(!should_prefilter(960, 540, rect, 480, 270));
    }

    #[test]
    fn presentation_identity_requires_full_rect_and_matching_extent() {
        let mut ids = GpuColorFrameIdAllocator::new(1).expect("frame id allocator");
        let input = working_handle(&mut ids, 960, 540, GpuColorFrameTextureFormat::Rgba32Float);

        assert!(spatial_request_is_identity(
            &input,
            ViewerSourceRect::FULL,
            960,
            540
        ));
        assert!(!spatial_request_is_identity(
            &input,
            ViewerSourceRect::FULL,
            480,
            270
        ));
        assert!(!spatial_request_is_identity(
            &input,
            ViewerSourceRect { x: 0.0, y: 0.0, width: 0.5, height: 1.0 },
            960,
            540,
        ));
    }

    #[tokio::test]
    async fn gpu_identity_resize_preserves_working_linear_samples() {
        let Some((actual, diagnostics)) = run_spatial(
            3,
            2,
            vec![
                0.0, 0.1, 0.2, 1.0, 0.3, 0.4, 0.5, 1.0, 0.6, 0.7, 0.8, 1.0, 1.0, 0.9, 0.8, 1.0,
                0.7, 0.6, 0.5, 1.0, 0.4, 0.3, 0.2, 1.0,
            ],
            ViewerSourceRect::FULL,
            3,
            2,
        )
        .await
        else {
            return;
        };
        let expected = [
            0.0, 0.1, 0.2, 1.0, 0.3, 0.4, 0.5, 1.0, 0.6, 0.7, 0.8, 1.0, 1.0, 0.9, 0.8, 1.0, 0.7,
            0.6, 0.5, 1.0, 0.4, 0.3, 0.2, 1.0,
        ];
        for (observed, reference) in actual.iter().zip(expected) {
            assert!((observed - reference).abs() < 1.0e-5);
        }
        assert_eq!(diagnostics.prefilter_passes, 0);
        assert_eq!(diagnostics.lanczos_passes, 2);
    }

    #[tokio::test]
    async fn gpu_spatial_round_trip_preserves_positive_sixteen_bit_coverage() {
        let edge = 1.0 / 65_535.0;
        let expected_pixel = [1.25, -0.25, 0.5, edge];
        let Some((actual, diagnostics)) =
            run_spatial(2, 2, expected_pixel.repeat(4), ViewerSourceRect::FULL, 2, 2).await
        else {
            return;
        };

        for (pixel_index, pixel) in actual.chunks_exact(4).enumerate() {
            for channel in 0..4 {
                assert!(
                    (pixel[channel] - expected_pixel[channel]).abs() <= 2.0e-6,
                    "pixel {pixel_index}, channel {channel}: expected {}, got {}",
                    expected_pixel[channel],
                    pixel[channel]
                );
            }
        }
        assert_eq!(diagnostics.prefilter_passes, 0);
        assert_eq!(diagnostics.lanczos_passes, 2);
    }

    #[tokio::test]
    async fn gpu_downscale_prefilters_in_working_linear_space() {
        let mut data = Vec::with_capacity(8 * 8 * 4);
        for y in 0..8 {
            for x in 0..8 {
                let value = if (x + y) % 2 == 0 { 0.0 } else { 1.0 };
                data.extend_from_slice(&[value, value, value, 1.0]);
            }
        }
        let Some((actual, diagnostics)) =
            run_spatial(8, 8, data, ViewerSourceRect::FULL, 1, 1).await
        else {
            return;
        };
        for channel in actual.iter().take(3) {
            assert!((*channel - 0.5).abs() < 1.0e-5);
        }
        assert!((actual[3] - 1.0).abs() < 1.0e-5);
        assert_eq!(diagnostics.prefilter_passes, 1);
    }

    #[tokio::test]
    async fn gpu_crop_outputs_only_requested_source_region() {
        let mut data = Vec::with_capacity(4 * 4 * 4);
        for _y in 0..4 {
            for x in 0..4 {
                let value = x as f32 / 3.0;
                data.extend_from_slice(&[value, 0.0, 0.0, 1.0]);
            }
        }
        let rect = ViewerSourceRect { x: 0.25, y: 0.0, width: 0.5, height: 1.0 };
        let Some((actual, _)) = run_spatial(4, 4, data, rect, 2, 4).await else {
            return;
        };
        for row in actual.chunks_exact(8) {
            assert!((row[0] - 1.0 / 3.0).abs() < 1.0e-5);
            assert!((row[4] - 2.0 / 3.0).abs() < 1.0e-5);
        }
    }

    #[tokio::test]
    async fn transferred_spatial_output_feeds_ocio_display_boundary() {
        ensure_mondrian_default_ocio_loaded().expect("default OCIO config");
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Viewer spatial-to-OCIO test: no adapter available");
            return;
        };
        let mut pixels = Vec::with_capacity(8 * 8);
        for y in 0..8 {
            for x in 0..8 {
                let value = if (x + y) % 2 == 0 { 0.0 } else { 1.0 };
                pixels.push([value, value, value, 1.0]);
            }
        }
        let frame = crate::CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            data: pixels,
            color_space: WorkingColorSpace::LinearRec709,
        });
        let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
            crate::GpuColorFrameId::from_raw(10_000),
            &frame,
            GpuColorFrameTextureFormat::Rgba32Float,
            "viewer-spatial-ocio-input",
        )
        .expect("upload plan");
        let input = GpuColorFrameUploader::upload(&context.device, &context.queue, &upload);
        let mut spatial = GpuViewerSpatialRuntime::default();
        let mut output_runtime = crate::RenderGpuOutputBoundaryRuntime::with_first_frame_id(20_000)
            .expect("GPU output runtime");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-spatial-ocio-encoder"),
        });
        let spatial_output = spatial
            .record(
                &context.device,
                &mut encoder,
                output_runtime.frame_ids_mut(),
                &input,
                ViewerSourceRect::FULL,
                1,
                1,
            )
            .expect("spatial record");
        let spatial_resource =
            spatial.take_output(&spatial_output).expect("spatial output transfer");
        output_runtime
            .frame_table_mut()
            .insert(spatial_resource)
            .expect("shared output table insertion");
        let boundary = crate::RenderOutputColorBoundary::display(
            ColorSpace::Srgb,
            false,
            ColorEngine::mondrian_standard(),
        );
        let record = output_runtime
            .record_wgpu_output_boundary_gpu_frame_owned_backend(
                &boundary,
                &spatial_output,
                GpuColorFrameTextureFormat::Rgba8Unorm,
                crate::RenderColorTransformGpuOptions {
                    output_residency: ColorFrameResidency::Cpu,
                    ..crate::RenderColorTransformGpuOptions::default()
                },
                crate::RenderGpuOutputBoundaryRuntimeOwnedBackendContext {
                    device: &context.device,
                    queue: &context.queue,
                    encoder: &mut encoder,
                    load_op: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                },
            )
            .expect("spatial output should feed OCIO boundary");
        let readback = record.readback_buffer.expect("encoded output readback");
        context.queue.submit(std::iter::once(encoder.finish()));
        let plan = GpuColorFrameReadbackPlan::encoded_rgba8(record.materialized.output)
            .expect("encoded output readback plan");
        let mapped = map_readback_buffer(&context.device, &readback);
        let actual = plan.unpack_mapped_rgba8(&mapped).expect("encoded output");
        readback.unmap();

        let reference = crate::execute_cpu_output_boundary_rgba8(
            &crate::CpuColorFrame::working(WorkingRgbaF32Frame {
                width: 1,
                height: 1,
                data: vec![[0.5, 0.5, 0.5, 1.0]],
                color_space: WorkingColorSpace::LinearRec709,
            }),
            &boundary,
        )
        .expect("CPU display reference");
        for (observed, expected) in actual.rgba().iter().zip(reference.rgba.iter()) {
            assert!(u8::abs_diff(*observed, *expected) <= 1);
        }
    }

    #[tokio::test]
    async fn spatial_runtime_reuses_exact_contract_textures_across_submitted_frames() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Viewer spatial pool test: no adapter available");
            return;
        };
        let frame = crate::CpuColorFrame::working(WorkingRgbaF32Frame {
            width: 8,
            height: 8,
            data: vec![[0.25, 0.5, 0.75, 1.0]; 64],
            color_space: WorkingColorSpace::LinearRec709,
        });
        let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
            crate::GpuColorFrameId::from_raw(10_000),
            &frame,
            GpuColorFrameTextureFormat::Rgba32Float,
            "viewer-spatial-pool-input",
        )
        .expect("upload plan");
        let input = GpuColorFrameUploader::upload(&context.device, &context.queue, &upload);
        let pool = std::sync::Arc::new(GpuColorFrameWgpuResourcePool::default());
        let mut runtime = GpuViewerSpatialRuntime::with_resource_pool(std::sync::Arc::clone(&pool));
        let mut ids = GpuColorFrameIdAllocator::new(20_000).expect("frame id allocator");

        for frame_index in 0..2 {
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewer-spatial-pool-encoder"),
                });
            let output = runtime
                .record(
                    &context.device,
                    &mut encoder,
                    &mut ids,
                    &input,
                    ViewerSourceRect::FULL,
                    4,
                    4,
                )
                .expect("record Viewer spatial pass");
            let output = runtime.take_output(&output).expect("take spatial output");
            context.queue.submit(std::iter::once(encoder.finish()));
            pool.release(output);
            runtime.clear_frame_resources();

            let diagnostics = pool.diagnostics();
            if frame_index == 0 {
                assert_eq!(diagnostics.hits, 0);
                assert_eq!(diagnostics.misses, 2);
            }
        }

        let diagnostics = pool.diagnostics();
        assert_eq!(diagnostics.hits, 2);
        assert_eq!(diagnostics.misses, 2);
        assert_eq!(diagnostics.retained_resources, 2);
    }

    async fn run_spatial(
        width: u32,
        height: u32,
        data: Vec<f32>,
        source_rect: ViewerSourceRect,
        output_width: u32,
        output_height: u32,
    ) -> Option<(Vec<f32>, GpuViewerSpatialRuntimeDiagnostics)> {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping Viewer spatial GPU test: no adapter available");
            return None;
        };
        let frame = crate::CpuColorFrame::working(WorkingRgbaF32Frame {
            width,
            height,
            data: data
                .chunks_exact(4)
                .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
                .collect(),
            color_space: WorkingColorSpace::LinearRec709,
        });
        let mut upload_ids =
            GpuColorFrameIdAllocator::new(10_000).expect("upload frame id allocator");
        let upload = GpuColorFrameUploadPlan::from_cpu_color_frame(
            upload_ids.allocate().expect("upload frame id"),
            &frame,
            GpuColorFrameTextureFormat::Rgba32Float,
            "viewer-spatial-test-input",
        )
        .expect("upload plan");
        let input = GpuColorFrameUploader::upload(&context.device, &context.queue, &upload);
        let mut runtime = GpuViewerSpatialRuntime::default();
        let mut spatial_ids =
            GpuColorFrameIdAllocator::new(20_000).expect("spatial frame id allocator");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewer-spatial-test-encoder"),
        });
        let output = runtime
            .record(
                &context.device,
                &mut encoder,
                &mut spatial_ids,
                &input,
                source_rect,
                output_width,
                output_height,
            )
            .expect("record Viewer spatial pass");
        assert!(runtime.output(&output).is_some());
        let output_resource =
            runtime.take_output(&output).expect("transfer retained spatial output");
        assert!(runtime.output(&output).is_none());
        let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba32float(output)
            .expect("working output readback plan");
        let readback = GpuColorFrameReadback::record_copy(
            &context.device,
            &mut encoder,
            &readback_plan,
            &output_resource,
        )
        .expect("record Viewer spatial readback");
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_readback_buffer(&context.device, &readback);
        let actual = readback_plan
            .unpack_mapped_rgba32float(&mapped)
            .expect("unpack Viewer spatial output");
        readback.unmap();
        Some((actual, runtime.diagnostics()))
    }

    fn working_handle(
        ids: &mut GpuColorFrameIdAllocator,
        width: u32,
        height: u32,
        format: GpuColorFrameTextureFormat,
    ) -> GpuColorFrameHandle {
        GpuColorFrameHandle::new(
            ids.allocate().expect("working frame id"),
            ColorFrameDescriptor {
                width,
                height,
                color_space: ColorFrameSpace::Working(WorkingColorSpace::LinearRec709),
                domain: ColorFrameDomain::Working,
                encoding: ColorFrameEncoding::LinearFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::StraightCoverage,
            },
            format,
            "working",
        )
        .expect("working handle")
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
