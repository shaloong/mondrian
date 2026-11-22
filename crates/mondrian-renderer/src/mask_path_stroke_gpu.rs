//! Direct Float32 path painting from the shared extent-bound geometry payload.

use crate::gpu_command::GpuCommandEncoder;
use crate::spatial_pass_gpu::EffectSpatialGpuError;
use mondrian_effects::{mask_path_stroke::MaskPathStrokeLayout, EffectPixelRoi};
use wgpu::util::DeviceExt as _;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StrokeUniform {
    ink: [f32; 4],
    profile: [f32; 4],
    pixel_scale: [f32; 2],
    pixel_origin: [u32; 2],
    control: [u32; 4],
}

pub(crate) fn check_limits(
    layout: &MaskPathStrokeLayout,
    limits: &wgpu::Limits,
) -> Result<(), EffectSpatialGpuError> {
    if layout.is_empty() {
        return Ok(());
    }
    let binding_limit = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
    let uniform_bytes = std::mem::size_of::<StrokeUniform>() as u64;
    if limits.max_storage_buffers_per_shader_stage < 2
        || uniform_bytes > limits.max_uniform_buffer_binding_size
        || uniform_bytes > limits.max_buffer_size
        || layout.gpu_storage_bytes().iter().any(|&bytes| bytes > binding_limit)
    {
        return Err(EffectSpatialGpuError::UnsupportedPass(
            "mask_path_stroke_storage",
        ));
    }
    Ok(())
}

/// Charged GPU bytes, including both geometry bindings and the uniform.
pub(crate) fn payload_bytes(layout: &MaskPathStrokeLayout) -> u64 {
    if layout.is_empty() {
        0
    } else {
        layout.gpu_storage_bytes().into_iter().sum::<u64>()
            + std::mem::size_of::<StrokeUniform>() as u64
    }
}

pub(crate) struct MaskPathStrokeGpu {
    pipeline: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
}

impl MaskPathStrokeGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.mask-path-stroke.shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mask_path_stroke.wgsl").into()),
        });
        let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.mask-path-stroke.bindings"),
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
                buffer(1, wgpu::BufferBindingType::Uniform),
                buffer(2, wgpu::BufferBindingType::Storage { read_only: true }),
                buffer(3, wgpu::BufferBindingType::Storage { read_only: true }),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.mask-path-stroke.layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.mask-path-stroke.pipeline"),
            layout: Some(&pipeline_layout),
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
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, bindings }
    }

    /// Both textures are local to `roi`; geometry and pixel centers stay in
    /// the layout's full input grid. Production currently supplies a full ROI.
    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        layout: &MaskPathStrokeLayout,
        roi: EffectPixelRoi,
    ) -> Result<(), EffectSpatialGpuError> {
        debug_assert!(!layout.is_empty());
        let extent = layout.extent();
        if roi.is_empty()
            || u64::from(roi.x()) + u64::from(roi.width()) > u64::from(extent.width())
            || u64::from(roi.y()) + u64::from(roi.height()) > u64::from(extent.height())
            || !source.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING)
            || !destination.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            || [source, destination].into_iter().any(|texture| {
                texture.width() != roi.width()
                    || texture.height() != roi.height()
                    || texture.depth_or_array_layers() != 1
                    || texture.sample_count() != 1
                    || texture.dimension() != wgpu::TextureDimension::D2
                    || texture.format() != wgpu::TextureFormat::Rgba32Float
            })
        {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "mask_path_stroke_raster",
            ));
        }
        check_limits(layout, &device.limits())?;
        let grade = layout.grade();
        let (control, ink, _) = grade.gpu_controls();
        let uniform = StrokeUniform {
            ink,
            profile: [grade.radius(), grade.solid(), grade.softness(), 0.0],
            pixel_scale: layout.pixel_scale(),
            pixel_origin: [roi.x(), roi.y()],
            control: [control[0], layout.nodes().len() as u32, 0, 0],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.mask-path-stroke.uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        // Pack into mapped GPU buffers directly: no coverage image or second
        // CPU-side geometry allocation. Alignment of Rust types is immaterial.
        let nodes = upload_words(
            device,
            "mondrian.mask-path-stroke.nodes",
            layout.nodes().iter().map(|node| node.storage_words()),
        )?;
        let primitives = upload_words(
            device,
            "mondrian.mask-path-stroke.primitives",
            layout
                .primitives()
                .iter()
                .map(|primitive| primitive.endpoints.map(f32::to_bits)),
        )?;
        let input = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output = destination.create_view(&wgpu::TextureViewDescriptor::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.mask-path-stroke.bind-group"),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input),
                },
                wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: nodes.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: primitives.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.mask-path-stroke.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &output,
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
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..3, 0..1);
        Ok(())
    }
}

fn upload_words<const N: usize>(
    device: &wgpu::Device,
    label: &str,
    rows: impl ExactSizeIterator<Item = [u32; N]>,
) -> Result<wgpu::Buffer, EffectSpatialGpuError> {
    let row_bytes = std::mem::size_of::<[u32; N]>();
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (rows.len() * row_bytes) as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: true,
    });
    {
        let mut mapped = buffer.slice(..).get_mapped_range_mut().map_err(|_| {
            EffectSpatialGpuError::UnsupportedPass("mask_path_stroke_storage_upload")
        })?;
        for (index, words) in rows.enumerate() {
            let start = index * row_bytes;
            mapped
                .slice(start..start + row_bytes)
                .copy_from_slice(bytemuck::cast_slice(&words));
        }
    }
    buffer.unmap();
    Ok(buffer)
}
