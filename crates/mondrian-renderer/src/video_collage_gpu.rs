//! Record all collage tiles into one cleared working texture.

use crate::gpu_command::GpuCommandEncoder;
use crate::spatial_pass_gpu::EffectSpatialGpuError;
use mondrian_core::ImageSampling;
use mondrian_effects::{EffectFrameExtent, EffectGpuSpatialPass, EffectPixelRoi};
use wgpu::util::DeviceExt as _;

const LABEL: &str = "mondrian.video-collage";
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const CONTROL_BYTES: u64 = 96;

pub(crate) struct VideoCollageGpuPlan {
    tiles: Vec<EffectPixelRoi>,
    pub(crate) draws: u32,
    pub(crate) draw_pixels: u64,
    pub(crate) controls_bytes: u64,
    pub(crate) working_bytes: u64,
}

impl VideoCollageGpuPlan {
    pub(crate) fn new(
        pass: &EffectGpuSpatialPass,
        extent: EffectFrameExtent,
    ) -> Result<Self, EffectSpatialGpuError> {
        let EffectGpuSpatialPass::VideoCollage { grade, slots, sampling } = pass else {
            return Err(EffectSpatialGpuError::UnsupportedPass("video_collage"));
        };
        let sampling = sampling.ok_or(EffectSpatialGpuError::MissingImageSampling)?;
        if slots.len() != grade.tile_count() || extent.is_empty() {
            return Err(mondrian_effects::VideoCollageError::Geometry.into());
        }
        let tiles = grade.tiles(extent, sampling);
        let draws = tiles.iter().filter(|tile| !tile.is_empty()).count() as u32;
        let draw_pixels = tiles
            .iter()
            .map(|tile| u64::from(tile.width()) * u64::from(tile.height()))
            .sum();
        let controls_bytes = u64::from(draws) * CONTROL_BYTES;
        let working_bytes = u64::from(extent.width())
            .saturating_mul(u64::from(extent.height()))
            .saturating_mul(32)
            .saturating_add(controls_bytes);
        Ok(Self {
            tiles,
            draws,
            draw_pixels,
            controls_bytes,
            working_bytes,
        })
    }
}

pub(crate) struct VideoCollageGpu {
    bindings: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}

impl VideoCollageGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let taps = mondrian_effects::BILINEAR_TAPS
            .map(|[x, y]| format!("vec2<i32>({x}, {y})"))
            .join(", ");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(LABEL),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                "const PIXEL_CENTRE: f32 = {:?};\nconst TAPS = array<vec2<i32>, 4>({taps});\n{}",
                mondrian_effects::PIXEL_CENTRE, include_str!("video_collage.wgsl"),
            )
                .into(),
            ),
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
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(CONTROL_BYTES),
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
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { bindings, pipeline }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record<'a>(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        pass: &EffectGpuSpatialPass,
        primary: &wgpu::Texture,
        output: &wgpu::Texture,
        mut resolve: impl FnMut(u32) -> Option<(&'a wgpu::Texture, ImageSampling, [f32; 6])>,
    ) -> Result<(), EffectSpatialGpuError> {
        let extent = EffectFrameExtent::new(output.width(), output.height());
        let plan = VideoCollageGpuPlan::new(pass, extent)?;
        let EffectGpuSpatialPass::VideoCollage { grade, slots, sampling: Some(sampling) } = pass
        else {
            return Err(EffectSpatialGpuError::UnsupportedPass("video_collage"));
        };
        if (primary.width(), primary.height()) != (output.width(), output.height())
            || output.format() != FORMAT
            || plan.working_bytes > crate::spatial_driver::GPU_SPATIAL_SCRATCH_BUDGET_BYTES
        {
            return Err(mondrian_effects::VideoCollageError::Geometry.into());
        }
        let mut draws = Vec::with_capacity(plan.draws as usize);
        for (tile, slot) in plan.tiles.into_iter().zip(slots.iter()) {
            if tile.is_empty() {
                continue;
            }
            let (texture, source_sampling, [a, b, tx, c, d, ty]) = match slot {
                Some(slot) => resolve(*slot).ok_or(EffectSpatialGpuError::MissingAuxiliaryInput)?,
                None => (primary, *sampling, mondrian_effects::IDENTITY_AFFINE),
            };
            if texture.format() != FORMAT {
                return Err(EffectSpatialGpuError::UnsupportedPass(
                    "video_collage_format",
                ));
            }
            let controls = grade.gpu_controls(
                tile,
                *sampling,
                EffectFrameExtent::new(texture.width(), texture.height()),
                source_sampling,
            )?;
            let [x, y] = controls.origin;
            let [sx, sy] = controls.pixel_size;
            let [hx, hy] = controls.half_size;
            let [cx, cy] = controls.crop;
            // Affine rows fit into the trailing components of the same block.
            let data = [
                [x, y, sx, sy],
                [hx, hy, cx, cy],
                [
                    controls.radius,
                    controls.border_width,
                    controls.feather,
                    0.0,
                ],
                controls.border,
                [a, b, tx, c],
                [d, ty, 0.0, 0.0],
            ];
            let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(LABEL),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
            let view = texture.create_view(&Default::default());
            let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(LABEL),
                layout: &self.bindings,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                ],
            });
            draws.push((tile, bindings));
        }
        let view = output.create_view(&Default::default());
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(LABEL),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        for (tile, bindings) in &draws {
            pass.set_scissor_rect(tile.x(), tile.y(), tile.width(), tile.height());
            pass.set_bind_group(0, bindings, &[]);
            pass.draw(0..3, 0..1);
        }
        Ok(())
    }
}
