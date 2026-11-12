//! Extent-bound GPU pyramid for Detail-preserving Upscale.

use mondrian_effects::detail_preserving_upscale::DetailPreservingUpscaleGrade;
use mondrian_effects::{EffectFrameExtent, EffectRasterUpscaleStage};
use wgpu::util::DeviceExt as _;

use crate::gpu_command::GpuCommandEncoder;
use crate::spatial_pass_gpu::EffectSpatialGpuError;

const LABEL: &str = "mondrian.detail-preserving-upscale";
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const HEADER_BYTES: u64 = 16;

pub(crate) struct UpscaleGpuPlan {
    source: EffectFrameExtent,
    destination: EffectFrameExtent,
    levels: Vec<EffectRasterUpscaleStage>,
    controls: [u32; 3],
    pub(crate) passes: u32,
    pub(crate) draw_pixels: u64,
    pub(crate) working_bytes: u64,
    pub(crate) scratch_bytes: u64,
    largest_binding: u64,
}

impl UpscaleGpuPlan {
    pub(crate) fn new(
        grade: DetailPreservingUpscaleGrade,
        source: EffectFrameExtent,
    ) -> Option<Self> {
        if source.is_empty() || grade.is_identity() {
            return None;
        }
        let levels = grade.scale().stages(source)?;
        let destination = levels.last().map_or(source, |level| level.output_extent());
        let controls = grade.gpu_controls();
        let denoise = controls[2] != 0;
        let mut textures = raster_bytes(source)?;
        let source_bytes = textures;
        let mut table_bytes = 0_u64;
        let mut largest_binding = 0;
        if denoise {
            textures = textures.checked_add(raster_bytes(source)?)?;
            table_bytes = HEADER_BYTES;
            largest_binding = HEADER_BYTES;
        }
        for level in &levels {
            let extent = level.output_extent();
            textures = textures.checked_add(raster_bytes(extent)?)?;
            let binding =
                HEADER_BYTES + 8 * (u64::from(extent.width()) + u64::from(extent.height()));
            table_bytes = table_bytes.checked_add(binding)?;
            largest_binding = largest_binding.max(binding);
        }
        let passes = levels.len() as u32 + u32::from(denoise);
        let draw_pixels = (textures - source_bytes) / 16;
        let working_bytes = textures.checked_add(table_bytes)?;
        let scratch_bytes = working_bytes - raster_bytes(source)? - raster_bytes(destination)?;
        Some(Self {
            source,
            destination,
            levels,
            controls,
            passes,
            draw_pixels,
            working_bytes,
            scratch_bytes,
            largest_binding,
        })
    }

    pub(crate) fn fits(&self, limits: &wgpu::Limits) -> bool {
        self.largest_binding <= limits.max_storage_buffer_binding_size.min(limits.max_buffer_size)
            && [self.source, self.destination].into_iter().all(|extent| {
                extent.width() <= limits.max_texture_dimension_2d
                    && extent.height() <= limits.max_texture_dimension_2d
            })
    }
}

fn raster_bytes(extent: EffectFrameExtent) -> Option<u64> {
    u64::from(extent.width())
        .checked_mul(u64::from(extent.height()))?
        .checked_mul(16)
}

pub(crate) struct DetailPreservingUpscaleGpu {
    upscale: wgpu::RenderPipeline,
    denoise: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
}

impl DetailPreservingUpscaleGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(LABEL),
            source: wgpu::ShaderSource::Wgsl(include_str!("detail_preserving_upscale.wgsl").into()),
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(LABEL),
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
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
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
        let pipeline = |fragment| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(LABEL),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(fragment),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            upscale: pipeline("fs_upscale"),
            denoise: pipeline("fs_denoise"),
            bindings,
        }
    }

    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        plan: &UpscaleGpuPlan,
    ) -> Result<(), EffectSpatialGpuError> {
        if !plan.fits(&device.limits())
            || !texture_matches(source, plan.source)
            || !texture_matches(destination, plan.destination)
        {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "detail_preserving_upscale_raster",
            ));
        }
        let [detail, alpha, noise] = plan.controls;
        let mut current = source.clone();
        if noise != 0 {
            let output = if plan.levels.is_empty() {
                destination.clone()
            } else {
                intermediate(device, plan.source)
            };
            self.draw(
                device,
                encoder,
                &self.denoise,
                &current,
                &output,
                &[plan.source.width(), detail, alpha, noise],
            );
            current = output;
        }
        for (index, level) in plan.levels.iter().copied().enumerate() {
            let extent = level.output_extent();
            let output = if index + 1 == plan.levels.len() {
                destination.clone()
            } else {
                intermediate(device, extent)
            };
            let mut payload =
                Vec::with_capacity(4 + 2 * (extent.width() as usize + extent.height() as usize));
            payload.extend([extent.width(), detail, alpha, noise]);
            for sample in level.column_samples().chain(level.row_samples()) {
                payload.extend([sample.base, sample.fraction.to_bits()]);
            }
            self.draw(device, encoder, &self.upscale, &current, &output, &payload);
            current = output;
        }
        Ok(())
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        payload: &[u32],
    ) {
        let storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(LABEL),
            contents: bytemuck::cast_slice(payload),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let input = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output = destination.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(LABEL),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input),
                },
                wgpu::BindGroupEntry { binding: 1, resource: storage.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(LABEL),
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn texture_matches(texture: &wgpu::Texture, extent: EffectFrameExtent) -> bool {
    texture.width() == extent.width()
        && texture.height() == extent.height()
        && texture.format() == FORMAT
}

fn intermediate(device: &wgpu::Device, extent: EffectFrameExtent) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(LABEL),
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
