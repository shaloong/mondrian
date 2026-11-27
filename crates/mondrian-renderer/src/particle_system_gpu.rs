//! Float32 particle compositing on a birth-ordered, actual-grid tile index.

use crate::{gpu_command::GpuCommandEncoder, spatial_pass_gpu::EffectSpatialGpuError};
use mondrian_effects::{
    particle_system::{ParticleBlend, ParticleRaster, ParticleTileIndex, PARTICLE_TILE_SIZE},
    EffectFrameExtent,
};
use wgpu::util::DeviceExt as _;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleRecord {
    center_radius: [f32; 4],
    orientation: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ParticleUniform {
    physical_per_sample: [f32; 2],
    antialias_radius: f32,
    ellipse_aspect: f32,
    tile_cols: u32,
    tile_size: u32,
    blend: u32,
    _padding: u32,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParticleGpuLayout {
    extent: EffectFrameExtent,
    physical_per_sample: [f32; 2],
    antialias_radius: f32,
    records: Vec<ParticleRecord>,
    index: ParticleTileIndex,
    blend: ParticleBlend,
    cpu_coverage_pixels: u64,
}

impl ParticleGpuLayout {
    pub(crate) fn new(
        raster: &ParticleRaster,
        index: ParticleTileIndex,
        blend: ParticleBlend,
    ) -> Self {
        let records = raster
            .particles()
            .iter()
            .map(|p| ParticleRecord {
                center_radius: [p.center[0], p.center[1], p.radius, 0.0],
                orientation: [p.orientation[0], p.orientation[1], 0.0, 0.0],
                color: p.color,
            })
            .collect();
        let cpu_coverage_pixels = raster
            .particles()
            .iter()
            .map(|p| u64::from(p.bounds.width()) * u64::from(p.bounds.height()))
            .sum();
        Self {
            extent: raster.extent(),
            physical_per_sample: raster.physical_per_sample(),
            antialias_radius: raster.antialias_radius(),
            records,
            index,
            blend,
            cpu_coverage_pixels,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn storage_bytes(&self) -> [u64; 2] {
        [
            std::mem::size_of_val(&*self.records) as u64,
            std::mem::size_of_val(self.index.words()) as u64,
        ]
    }

    pub(crate) fn payload_bytes(&self) -> u64 {
        if self.is_empty() {
            0
        } else {
            self.storage_bytes().into_iter().sum::<u64>()
                + std::mem::size_of::<ParticleUniform>() as u64
        }
    }

    pub(crate) fn coverage_work(&self) -> (u64, u64) {
        let offsets = self.index.words().first().copied().unwrap_or(0);
        let entries = self.index.words().len() as u64 - u64::from(offsets);
        (
            entries * u64::from(PARTICLE_TILE_SIZE).pow(2),
            self.cpu_coverage_pixels,
        )
    }

    pub(crate) fn check_limits(&self, limits: &wgpu::Limits) -> Result<(), EffectSpatialGpuError> {
        if self.is_empty() {
            return Ok(());
        }
        let binding_limit = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let uniform_bytes = std::mem::size_of::<ParticleUniform>() as u64;
        if limits.max_storage_buffers_per_shader_stage < 2
            || uniform_bytes > limits.max_uniform_buffer_binding_size
            || uniform_bytes > limits.max_buffer_size
            || self.storage_bytes().iter().any(|&bytes| bytes > binding_limit)
        {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "particle_system_storage",
            ));
        }
        Ok(())
    }
}

pub(crate) struct ParticleGpu {
    pipeline: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
}

impl ParticleGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.particle-system.shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("particle_system.wgsl").into()),
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
            label: Some("mondrian.particle-system.bindings"),
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
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.particle-system.layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.particle-system.pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, bindings }
    }

    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        layout: &ParticleGpuLayout,
    ) -> Result<(), EffectSpatialGpuError> {
        let extent = layout.extent;
        if layout.is_empty()
            || !source.usage().contains(wgpu::TextureUsages::TEXTURE_BINDING)
            || !destination.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            || [source, destination].into_iter().any(|texture| {
                texture.width() != extent.width()
                    || texture.height() != extent.height()
                    || texture.depth_or_array_layers() != 1
                    || texture.sample_count() != 1
                    || texture.dimension() != wgpu::TextureDimension::D2
                    || texture.format() != wgpu::TextureFormat::Rgba32Float
            })
        {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "particle_system_raster",
            ));
        }
        layout.check_limits(&device.limits())?;
        let uniforms = ParticleUniform {
            physical_per_sample: layout.physical_per_sample,
            antialias_radius: layout.antialias_radius,
            ellipse_aspect: mondrian_effects::particle_system::PARTICLE_ELLIPSE_ASPECT,
            tile_cols: layout.index.columns(),
            tile_size: PARTICLE_TILE_SIZE,
            blend: layout.blend.gpu_code(),
            _padding: 0,
        };
        let upload = |label, contents, usage| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage,
            })
        };
        let uniform = upload(
            "mondrian.particle-system.uniform",
            bytemuck::bytes_of(&uniforms),
            wgpu::BufferUsages::UNIFORM,
        );
        let records = upload(
            "mondrian.particle-system.live",
            bytemuck::cast_slice(&layout.records),
            wgpu::BufferUsages::STORAGE,
        );
        let index = upload(
            "mondrian.particle-system.tiles",
            bytemuck::cast_slice(layout.index.words()),
            wgpu::BufferUsages::STORAGE,
        );
        let input = source.create_view(&Default::default());
        let output = destination.create_view(&Default::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.particle-system.bind-group"),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input),
                },
                wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: records.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: index.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.particle-system.pass"),
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
