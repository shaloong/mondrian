//! GPU height pyramid and lighting draw on a layer's own raster.

use crate::gpu_command::GpuCommandEncoder;
use crate::spatial_pass_gpu::EffectSpatialGpuError;
use mondrian_effects::{
    AuxiliaryCoordinateMapping, AuxiliaryFilter, AuxiliaryOutOfBounds, AuxiliarySampling,
    EffectFrameExtent, EffectGpuSpatialPass, EffectRasterBounds, GlassGpuControls, GlassHeightPlan,
};
use wgpu::util::DeviceExt as _;

const LABEL: &str = "mondrian.glass";
const HEIGHT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const CONTROL_BYTES: u64 = 176;

pub(crate) struct GlassGpuPlan {
    extent: EffectFrameExtent,
    controls: GlassGpuControls,
    pub(crate) draws: u32,
    pub(crate) draw_pixels: u64,
    pub(crate) working_bytes: u64,
    pub(crate) scratch_bytes: u64,
    light_bytes: u64,
}

impl GlassGpuPlan {
    pub(crate) fn new(
        pass: &EffectGpuSpatialPass,
        primary: EffectFrameExtent,
        stage: EffectRasterBounds,
    ) -> Result<Self, EffectSpatialGpuError> {
        let EffectGpuSpatialPass::Glass { grade, lighting, projection, sampling, .. } = pass else {
            return Err(EffectSpatialGpuError::UnsupportedPass(pass.name()));
        };
        let sampling = sampling.ok_or(EffectSpatialGpuError::MissingImageSampling)?;
        let controls =
            grade.gpu_controls(sampling, primary, stage, lighting.as_deref(), *projection)?;
        let extent = stage.extent();
        let area = |width, height| u64::from(width) * u64::from(height);
        let mut size = [extent.width(), extent.height()];
        let pixels = area(size[0], size[1]);
        let mut draw_pixels = pixels.saturating_mul(2);
        let mut height_pixels = pixels;
        let mut draws = 2;
        let mut factor = 1;
        let height = controls.height_plan();
        while factor < height.factor {
            size = size.map(|axis| axis.div_ceil(2));
            let pixels = area(size[0], size[1]);
            height_pixels = height_pixels.saturating_add(pixels);
            draw_pixels = draw_pixels.saturating_add(pixels);
            draws += 1;
            factor *= 2;
        }
        for radius in height.radius {
            if radius > 0.0 {
                let pixels = area(size[0], size[1]);
                height_pixels = height_pixels.saturating_add(pixels);
                draw_pixels = draw_pixels.saturating_add(pixels);
                draws += 1;
            }
        }
        let light_bytes = light_buffer_bytes(controls.packed_lights().len());
        let scratch_bytes = height_pixels
            .saturating_mul(4)
            .saturating_add(CONTROL_BYTES * u64::from(draws))
            .saturating_add(light_bytes);
        let working_bytes = pixels.saturating_mul(32).saturating_add(scratch_bytes);
        Ok(Self {
            extent,
            controls,
            draws,
            draw_pixels,
            working_bytes,
            scratch_bytes,
            light_bytes,
        })
    }

    pub(crate) fn fits(&self, limits: &wgpu::Limits) -> bool {
        self.light_bytes <= limits.max_storage_buffer_binding_size.min(limits.max_buffer_size)
            && CONTROL_BYTES <= limits.max_uniform_buffer_binding_size
            && self.extent.width() <= limits.max_texture_dimension_2d
            && self.extent.height() <= limits.max_texture_dimension_2d
    }

    /// Before source sampling is available, bound all retained scalar layouts.
    pub(crate) fn recording_bound(extent: EffectFrameExtent, light_count: usize) -> (u64, u64) {
        let mut size = [extent.width(), extent.height()];
        let area = |[w, h]: [u32; 2]| u64::from(w) * u64::from(h);
        let mut height_pixels = area(size);
        let mut peak = height_pixels.saturating_mul(3);
        for _ in 3..GlassHeightPlan::MAX_PLANES {
            size = size.map(|axis| axis.div_ceil(2));
            let reduced = area(size);
            height_pixels = height_pixels.saturating_add(reduced);
            peak = peak.max(height_pixels.saturating_add(reduced.saturating_mul(2)));
        }
        let planes = u64::from(GlassHeightPlan::MAX_PLANES);
        let bytes = peak
            .saturating_mul(4)
            .saturating_add(CONTROL_BYTES * (planes + 1))
            .saturating_add(light_buffer_bytes(light_count));
        (bytes, planes)
    }
}

fn light_buffer_bytes(count: usize) -> u64 {
    (count.max(1) as u64).saturating_mul(64)
}

pub(crate) struct GlassGpu {
    bindings: wgpu::BindGroupLayout,
    height: wgpu::RenderPipeline,
    reduce: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    shade: wgpu::RenderPipeline,
}

impl GlassGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let taps = mondrian_effects::BILINEAR_TAPS
            .map(|[x, y]| format!("vec2<i32>({x}, {y})"))
            .join(", ");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(LABEL),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "const PIXEL_CENTRE: f32 = {:?};\n\
                 const GLASS_FINITE_MAX: f32 = {:e};\n\
                 const GLASS_MIN_POSITIVE: f32 = {:e};\n\
                 const BILINEAR_TAPS = array<vec2<i32>, 4>({taps});\n{}",
                    mondrian_effects::PIXEL_CENTRE,
                    f64::from(f32::MAX),
                    f64::from(f32::MIN_POSITIVE),
                    include_str!("glass.wgsl")
                )
                .into(),
            ),
        });
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
        let buffer = |binding, ty, minimum| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(minimum),
            },
            count: None,
        };
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(LABEL),
            entries: &[
                texture(0),
                texture(1),
                texture(2),
                buffer(3, wgpu::BufferBindingType::Uniform, CONTROL_BYTES),
                buffer(4, wgpu::BufferBindingType::Storage { read_only: true }, 64),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(LABEL),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = |entry, format| {
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
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
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
        let height = pipeline("fs_height", HEIGHT_FORMAT);
        let reduce = pipeline("fs_reduce", HEIGHT_FORMAT);
        let blur = pipeline("fs_blur", HEIGHT_FORMAT);
        let shade = pipeline("fs_glass", OUTPUT_FORMAT);
        Self { bindings, height, reduce, blur, shade }
    }

    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        auxiliary: Option<(&wgpu::Texture, [f32; 6], AuxiliarySampling)>,
        plan: &GlassGpuPlan,
    ) -> Result<(), EffectSpatialGpuError> {
        let valid_picture = |texture: &wgpu::Texture| texture.format() == OUTPUT_FORMAT;
        let expected = (plan.extent.width(), plan.extent.height());
        if !plan.fits(&device.limits())
            || plan.working_bytes > crate::spatial_driver::GPU_SPATIAL_SCRATCH_BUDGET_BYTES
            || [source, destination].into_iter().any(|texture| {
                !valid_picture(texture) || (texture.width(), texture.height()) != expected
            })
            || auxiliary.is_some_and(|(map, transform, _)| {
                !valid_picture(map) || !transform.into_iter().all(f32::is_finite)
            })
        {
            return Err(EffectSpatialGpuError::UnsupportedPass("glass_working_set"));
        }
        let dummy = [[0.0_f32; 16]];
        let lights = if plan.controls.packed_lights().is_empty() {
            dummy.as_slice()
        } else {
            plan.controls.packed_lights()
        };
        let lights = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(LABEL),
            contents: bytemuck::cast_slice(lights),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let (map, [a, b, tx, c, d, ty], sampling) = auxiliary.unwrap_or((
            source,
            mondrian_effects::IDENTITY_AFFINE,
            AuxiliarySampling::PRIMARY_GRID_LINEAR_CLAMP,
        ));
        let mapping = if auxiliary.is_some() {
            if sampling.mapping == AuxiliaryCoordinateMapping::FrameTransform {
                2.0
            } else {
                1.0
            }
        } else {
            0.0
        };
        let mut payload = [[0.0; 4]; 11];
        payload[..8].copy_from_slice(plan.controls.uniform_words());
        payload[8] = [a, b, tx, mapping];
        payload[9] = [
            c,
            d,
            ty,
            f32::from(sampling.out_of_bounds == AuxiliaryOutOfBounds::Transparent),
        ];
        payload[10] = [
            0.0,
            0.0,
            f32::from(sampling.filter == AuxiliaryFilter::Linear),
            plan.controls.packed_lights().len() as f32,
        ];

        let source = source.create_view(&wgpu::TextureViewDescriptor::default());
        let map = map.create_view(&wgpu::TextureViewDescriptor::default());
        let draw = |encoder: &mut dyn GpuCommandEncoder,
                    pipeline,
                    plane: &wgpu::TextureView,
                    target: &wgpu::Texture,
                    payload: &[[f32; 4]; 11]| {
            let output = target.create_view(&wgpu::TextureViewDescriptor::default());
            self.draw(
                device,
                encoder,
                pipeline,
                [&source, plane, &map, &output],
                &lights,
                payload,
            );
        };
        let mut size = [expected.0, expected.1];
        let mut plane = height_plane(device, size);
        draw(encoder, &self.height, &source, &plane, &payload);
        let mut factor = 1;
        let height = plan.controls.height_plan();
        while factor < height.factor {
            size = size.map(|axis| axis.div_ceil(2));
            let next = height_plane(device, size);
            draw(
                encoder,
                &self.reduce,
                &plane.create_view(&wgpu::TextureViewDescriptor::default()),
                &next,
                &payload,
            );
            plane = next;
            factor *= 2;
        }
        for (axis, radius) in height.radius.into_iter().enumerate() {
            if radius > 0.0 {
                let next = height_plane(device, size);
                payload[10][0] = radius;
                payload[10][1] = axis as f32;
                draw(
                    encoder,
                    &self.blur,
                    &plane.create_view(&wgpu::TextureViewDescriptor::default()),
                    &next,
                    &payload,
                );
                plane = next;
            }
        }
        draw(
            encoder,
            &self.shade,
            &plane.create_view(&wgpu::TextureViewDescriptor::default()),
            destination,
            &payload,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn GpuCommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        [original, plane, map, destination]: [&wgpu::TextureView; 4],
        lights: &wgpu::Buffer,
        payload: &[[f32; 4]; 11],
    ) {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(LABEL),
            contents: bytemuck::cast_slice(payload),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(LABEL),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(original),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(plane),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(map),
                },
                wgpu::BindGroupEntry { binding: 3, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: lights.as_entire_binding() },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(LABEL),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: destination,
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

fn height_plane(device: &wgpu::Device, [width, height]: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(LABEL),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HEIGHT_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
