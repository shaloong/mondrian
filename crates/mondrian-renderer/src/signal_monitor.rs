//! Fused GPU Viewer pass for false color, zebra, and gamut alarms.

use std::sync::Arc;

use mondrian_core::{
    ProgramScopesTap, ProgramSignalColorimetry, SignalComplianceContract, SignalComplianceError,
    SignalMonitoringSettings,
};
use thiserror::Error;

use crate::{
    GpuColorFrameAllocationPlan, GpuColorFrameHandle, GpuColorFrameHandleError,
    GpuColorFrameIdAllocationError, GpuColorFrameIdAllocator, GpuColorFrameResource,
    GpuColorFrameTextureFormat, GpuColorFrameWgpuResource, GpuColorFrameWgpuResourcePool,
};

/// One validated Viewer warning request.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuSignalMonitorRequest {
    /// Viewer boundary sampled for classification.
    pub tap: ProgramScopesTap,
    /// Signal identity sampled for classification.
    pub compliance: SignalComplianceContract,
    /// Fused warning controls.
    pub settings: SignalMonitoringSettings,
}

impl GpuSignalMonitorRequest {
    /// Construct and validate an active monitoring request.
    pub fn new(
        compliance: SignalComplianceContract,
        settings: SignalMonitoringSettings,
        tap: ProgramScopesTap,
    ) -> Result<Self, GpuSignalMonitorError> {
        Self { tap, compliance, settings }.validate()
    }

    /// Revalidate a deserialized or directly constructed request at execution.
    pub fn validate(self) -> Result<Self, GpuSignalMonitorError> {
        self.compliance.validate()?;
        self.settings.validate()?;
        if !self.settings.is_active() {
            return Err(GpuSignalMonitorError::InactiveRequest);
        }
        Ok(self)
    }
}

/// Retained signal-monitoring output and pipeline caches.
pub struct GpuSignalMonitorRuntime {
    pipeline: Option<GpuSignalMonitorPipeline>,
    pipeline_format: Option<GpuColorFrameTextureFormat>,
    output: Option<GpuColorFrameResource<GpuColorFrameWgpuResource>>,
    resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ids: GpuColorFrameIdAllocator,
    diagnostics: GpuSignalMonitorRuntimeDiagnostics,
}

/// Cumulative cache and recording evidence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuSignalMonitorRuntimeDiagnostics {
    /// Pipeline builds after output-format changes.
    pub pipeline_builds: u64,
    /// Fused monitoring passes recorded.
    pub records: u64,
}

impl GpuSignalMonitorRuntime {
    /// Create a runtime backed by the Viewer owner's texture pool.
    pub fn with_resource_pool(
        resource_pool: Arc<GpuColorFrameWgpuResourcePool>,
    ) -> Result<Self, GpuColorFrameIdAllocationError> {
        Ok(Self {
            pipeline: None,
            pipeline_format: None,
            output: None,
            resource_pool,
            ids: GpuColorFrameIdAllocator::new(1)?,
            diagnostics: GpuSignalMonitorRuntimeDiagnostics::default(),
        })
    }

    /// Classify from `signal_input` while coloring the monitor-adapted presentation input.
    pub fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        signal_input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        presentation_input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        request: GpuSignalMonitorRequest,
    ) -> Result<GpuColorFrameHandle, GpuSignalMonitorError> {
        let request = request.validate()?;
        self.clear_frame_resources();
        let signal_descriptor = signal_input.handle().descriptor();
        let presentation_descriptor = presentation_input.handle().descriptor();
        if signal_descriptor.width != presentation_descriptor.width
            || signal_descriptor.height != presentation_descriptor.height
        {
            return Err(GpuSignalMonitorError::ExtentMismatch);
        }
        if signal_descriptor.color_space.color() != Some(request.compliance.signal_color_space) {
            return Err(GpuSignalMonitorError::SignalIdentityMismatch);
        }
        let output_format = presentation_input.handle().texture_format();
        if self.pipeline_format != Some(output_format) {
            self.pipeline = Some(GpuSignalMonitorPipeline::new(device, output_format));
            self.pipeline_format = Some(output_format);
            self.diagnostics.pipeline_builds = self.diagnostics.pipeline_builds.saturating_add(1);
        }
        let output = GpuColorFrameHandle::new(
            self.ids.allocate()?,
            presentation_descriptor,
            output_format,
            "viewer-signal-monitor-output",
        )?;
        let output_resource = self.resource_pool.acquire(
            device,
            &GpuColorFrameAllocationPlan::for_handle(output.clone()),
        );
        self.pipeline
            .as_ref()
            .ok_or(GpuSignalMonitorError::InternalPipelineMissing)?
            .record(
                device,
                queue,
                encoder,
                signal_input,
                presentation_input,
                &output_resource,
                request,
            )?;
        self.output = Some(output_resource);
        self.diagnostics.records = self.diagnostics.records.saturating_add(1);
        Ok(output)
    }

    /// Resolve the retained output.
    pub fn output(
        &self,
        handle: &GpuColorFrameHandle,
    ) -> Option<&GpuColorFrameResource<GpuColorFrameWgpuResource>> {
        self.output.as_ref().filter(|output| output.handle() == handle)
    }

    /// Transfer the exact presentation output once.
    pub fn take_output(
        &mut self,
        handle: &GpuColorFrameHandle,
    ) -> Option<GpuColorFrameResource<GpuColorFrameWgpuResource>> {
        self.output
            .as_ref()
            .is_some_and(|output| output.handle() == handle)
            .then(|| self.output.take())
            .flatten()
    }

    /// Return the current frame output to the shared idle pool.
    pub fn clear_frame_resources(&mut self) {
        if let Some(output) = self.output.take() {
            self.resource_pool.release(output);
        }
    }

    /// Return retained runtime evidence.
    pub const fn diagnostics(&self) -> GpuSignalMonitorRuntimeDiagnostics {
        self.diagnostics
    }

    /// Invalidate device objects and per-frame output.
    pub fn clear(&mut self) {
        self.clear_frame_resources();
        self.pipeline = None;
        self.pipeline_format = None;
    }
}

struct GpuSignalMonitorPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
}

impl GpuSignalMonitorPipeline {
    fn new(device: &wgpu::Device, output_format: GpuColorFrameTextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.signal-monitor.shader"),
            source: wgpu::ShaderSource::Wgsl(SIGNAL_MONITOR_SHADER.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.signal-monitor.bindings"),
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
            label: Some("mondrian.signal-monitor.layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.signal-monitor.pipeline"),
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
                    format: output_format.to_wgpu(),
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian.signal-monitor.uniforms"),
            size: std::mem::size_of::<SignalMonitorUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { pipeline, bind_group_layout, uniform_buffer }
    }

    fn record(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        signal_input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        presentation_input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        output: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        request: GpuSignalMonitorRequest,
    ) -> Result<(), GpuSignalMonitorError> {
        let colorimetry =
            ProgramSignalColorimetry::for_color_space(request.compliance.signal_color_space)
                .map_err(|_| SignalComplianceError::UnsupportedSignalColorSpace {
                    color_space: request.compliance.signal_color_space,
                })?;
        let flags = u32::from(request.settings.false_color)
            | (u32::from(request.settings.zebra) << 1)
            | (u32::from(request.settings.gamut_alarm) << 2);
        let uniforms = SignalMonitorUniforms {
            flags,
            width: signal_input.handle().descriptor().width,
            zebra_lower: f32::from(request.settings.zebra_lower_per_mille) / 1_000.0,
            zebra_upper: f32::from(request.settings.zebra_upper_per_mille) / 1_000.0,
            kr: colorimetry.kr(),
            kb: colorimetry.kb(),
            _padding: [0; 2],
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.signal-monitor.bind-group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &signal_input.resource().texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &presentation_input.resource().texture_view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.signal-monitor.pass"),
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
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
        Ok(())
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SignalMonitorUniforms {
    flags: u32,
    width: u32,
    zebra_lower: f32,
    zebra_upper: f32,
    kr: f32,
    kb: f32,
    _padding: [u32; 2],
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

/// Signal-monitoring failures remain distinct from creative/output color failures.
#[derive(Debug, Error)]
pub enum GpuSignalMonitorError {
    /// Shared signal contract rejected the request.
    #[error(transparent)]
    Compliance(#[from] SignalComplianceError),
    /// No overlays were enabled.
    #[error("signal-monitoring request has no active overlay")]
    InactiveRequest,
    /// Classification and presentation inputs must describe the same raster.
    #[error("signal-monitoring classification and presentation extents differ")]
    ExtentMismatch,
    /// The input handle does not carry the declared classification identity.
    #[error("signal-monitoring input identity differs from its compliance contract")]
    SignalIdentityMismatch,
    /// Renderer frame ids are exhausted.
    #[error(transparent)]
    FrameId(#[from] GpuColorFrameIdAllocationError),
    /// Output handle construction failed.
    #[error(transparent)]
    OutputHandle(#[from] GpuColorFrameHandleError),
    /// Runtime cache invariant failed.
    #[error("signal-monitoring runtime is missing its pipeline")]
    InternalPipelineMissing,
}

const SIGNAL_MONITOR_SHADER: &str = r#"
struct Uniforms {
    flags: u32,
    width: u32,
    zebra_lower: f32,
    zebra_upper: f32,
    kr: f32,
    kb: f32,
    _padding: vec2<u32>,
};

@group(0) @binding(0) var signal_input: texture_2d<f32>;
@group(0) @binding(1) var presentation_input: texture_2d<f32>;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0)
    );
    var out: VertexOutput;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = positions[index] * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    return out;
}

fn false_color(luma: f32) -> vec3<f32> {
    if luma < 0.02 { return vec3<f32>(0.45, 0.0, 0.65); }
    if luma < 0.10 { return vec3<f32>(0.0, 0.15, 0.8); }
    if luma < 0.40 { return vec3<f32>(0.05, 0.55, 0.75); }
    if luma < 0.55 { return vec3<f32>(0.18, 0.72, 0.28); }
    if luma < 0.70 { return vec3<f32>(0.72, 0.68, 0.28); }
    if luma < 0.90 { return vec3<f32>(0.95, 0.42, 0.08); }
    return vec3<f32>(0.9, 0.05, 0.05);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let dimensions = textureDimensions(signal_input);
    let coord = min(vec2<i32>(input.uv * vec2<f32>(dimensions)), vec2<i32>(dimensions) - 1);
    let signal = textureLoad(signal_input, coord, 0);
    let presentation = textureLoad(presentation_input, coord, 0);
    let luma = dot(signal.rgb, vec3<f32>(uniforms.kr, 1.0 - uniforms.kr - uniforms.kb, uniforms.kb));
    var rgb = presentation.rgb;
    if (uniforms.flags & 1u) != 0u {
        rgb = false_color(luma);
    }
    if (uniforms.flags & 2u) != 0u && luma >= uniforms.zebra_lower && luma <= uniforms.zebra_upper {
        let stripe = ((u32(coord.x) + u32(coord.y)) / 4u) & 1u;
        if stripe == 0u { rgb = mix(rgb, vec3<f32>(1.0), 0.8); }
    }
    if (uniforms.flags & 4u) != 0u && (any(signal.rgb < vec3<f32>(0.0)) || any(signal.rgb > vec3<f32>(1.0))) {
        rgb = vec3<f32>(1.0, 0.0, 1.0);
    }
    return vec4<f32>(rgb, presentation.a);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ColorFrameAlpha, ColorFrameDescriptor, ColorFrameDomain, ColorFrameEncoding,
        ColorFrameResidency, ColorFrameSpace, GpuColorFrameReadback, GpuColorFrameReadbackPlan,
        GpuColorFrameUploader,
    };
    use mondrian_core::{ColorSpace, ProgramScopesTap};

    #[tokio::test]
    async fn fused_gpu_monitor_prioritizes_gamut_alarm_and_preserves_alpha() {
        let Ok(context) = crate::GpuContext::new().await else {
            eprintln!("skipping signal-monitor GPU test: no adapter available");
            return;
        };
        let signal = upload_frame(
            &context,
            10,
            [[1.2, 0.5, 0.5, 0.25], [0.95, 0.95, 0.95, 0.75]],
        );
        let presentation =
            upload_frame(&context, 11, [[0.2, 0.3, 0.4, 0.25], [0.2, 0.3, 0.4, 0.75]]);
        let pool = Arc::new(GpuColorFrameWgpuResourcePool::default());
        let mut runtime =
            GpuSignalMonitorRuntime::with_resource_pool(Arc::clone(&pool)).expect("runtime");
        let request = GpuSignalMonitorRequest::new(
            SignalComplianceContract::normalized_rgb(ColorSpace::Rec709).expect("contract"),
            SignalMonitoringSettings {
                false_color: true,
                zebra: true,
                gamut_alarm: true,
                ..Default::default()
            },
            ProgramScopesTap::ProgramOutput,
        )
        .expect("monitor request");
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("signal-monitor-test"),
        });
        let output = runtime
            .record(
                &context.device,
                &context.queue,
                &mut encoder,
                &signal,
                &presentation,
                request,
            )
            .expect("record monitor");
        let output_resource = runtime.output(&output).expect("output resource");
        let readback_plan = GpuColorFrameReadbackPlan::encoded_rgba32float(output.clone())
            .expect("float readback plan");
        let readback = GpuColorFrameReadback::record_copy(
            &context.device,
            &mut encoder,
            &readback_plan,
            output_resource,
        )
        .expect("readback copy");
        context.queue.submit(std::iter::once(encoder.finish()));
        let mapped = map_readback_buffer(&context.device, &readback);
        let actual = readback_plan.unpack_mapped_rgba32float(&mapped).expect("unpack");
        readback.unmap();

        assert_eq!(&actual[0..3], &[1.0, 0.0, 1.0]);
        assert!((actual[3] - 0.25).abs() < 1.0e-6);
        assert!((actual[7] - 0.75).abs() < 1.0e-6);
        assert_eq!(runtime.diagnostics().pipeline_builds, 1);
        assert_eq!(runtime.diagnostics().records, 1);
    }

    #[test]
    fn inactive_or_mismatched_monitoring_contracts_fail_closed() {
        let compliance =
            SignalComplianceContract::normalized_rgb(ColorSpace::Rec709).expect("contract");
        assert!(matches!(
            GpuSignalMonitorRequest::new(
                compliance,
                SignalMonitoringSettings::default(),
                ProgramScopesTap::ProgramOutput,
            ),
            Err(GpuSignalMonitorError::InactiveRequest)
        ));
    }

    fn upload_frame(
        context: &crate::GpuContext,
        id: u64,
        pixels: [[f32; 4]; 2],
    ) -> GpuColorFrameResource<GpuColorFrameWgpuResource> {
        let handle = GpuColorFrameHandle::new(
            crate::GpuColorFrameId::from_raw(id),
            ColorFrameDescriptor {
                width: 2,
                height: 1,
                color_space: ColorFrameSpace::Color(ColorSpace::Rec709),
                domain: ColorFrameDomain::Display,
                encoding: ColorFrameEncoding::EncodedFloat,
                residency: ColorFrameResidency::Gpu,
                alpha: ColorFrameAlpha::StraightCoverage,
            },
            GpuColorFrameTextureFormat::Rgba32Float,
            "signal-monitor-test-input",
        )
        .expect("input handle");
        let resource = GpuColorFrameUploader::allocate(
            &context.device,
            &GpuColorFrameAllocationPlan::for_handle(handle),
        );
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &resource.resource().texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(32),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: 2, height: 1, depth_or_array_layers: 1 },
        );
        resource
    }

    fn map_readback_buffer(device: &wgpu::Device, readback: &wgpu::Buffer) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = readback.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        rx.recv().expect("readback callback").expect("readback map");
        slice.get_mapped_range().expect("mapped range").to_vec()
    }
}
