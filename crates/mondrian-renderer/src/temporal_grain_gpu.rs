//! Three-texture Float32 pass for a frozen temporal grain step.

use mondrian_effects::grain_temporal::GrainTemporalGrade;
use wgpu::util::DeviceExt;

const LABEL: &str = "mondrian.temporal-grain";

pub(crate) struct TemporalGrainPipeline {
    bindings: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl TemporalGrainPipeline {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(LABEL),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(LABEL),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(LABEL),
            source: wgpu::ShaderSource::Wgsl(include_str!("temporal_grain.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(LABEL),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("full_screen"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            multisample: Default::default(),
            depth_stencil: None,
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_temporal_grain"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba32Float.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { bindings, pipeline }
    }

    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        [current, previous, following]: [&wgpu::Texture; 3],
        output: &wgpu::Texture,
        grade: GrainTemporalGrade,
    ) {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(LABEL),
            contents: bytemuck::bytes_of(&grade.gpu_controls()),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let views = [current, previous, following, output]
            .map(|texture| texture.create_view(&Default::default()));
        let texture = |binding: u32| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(&views[binding as usize]),
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(LABEL),
            layout: &self.bindings,
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                wgpu::BindGroupEntry { binding: 3, resource: uniform.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(LABEL),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &views[3],
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
