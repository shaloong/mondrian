//! Float32 raster pass for an extent-bound mask deformation.

use crate::gpu_command::GpuCommandEncoder;
use crate::spatial_pass_gpu::EffectSpatialGpuError;
use mondrian_effects::mask_deformation::MaskDeformationRaster;
use wgpu::util::DeviceExt as _;

const HEADER_WORDS: usize = 8;

pub(crate) struct MaskDeformationGpu {
    pipeline: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
}

impl MaskDeformationGpu {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.mask-deformation.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.mask-deformation.bindings"),
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
            label: Some("mondrian.mask-deformation.layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.mask-deformation.pipeline"),
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
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
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
        field: &MaskDeformationRaster,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        let extent = field.extent();
        if extent.is_empty() {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        for texture in [source, destination] {
            if texture.width() != extent.width()
                || texture.height() != extent.height()
                || texture.format() != wgpu::TextureFormat::Rgba32Float
            {
                return Err(EffectSpatialGpuError::UnsupportedPass(
                    "mask_deformation_raster",
                ));
            }
        }
        let [x, y] = field.axes();
        let x_at = HEADER_WORDS + field.offsets().len() * 2;
        let y_at = x_at + x.len() * 2;
        let rows_at = y_at + y.len() * 2;
        let spans_at = rows_at + field.row_offsets().len();
        let words = spans_at + field.spans().len() * 2;
        let limits = device.limits();
        if words as u64 * 4 > limits.max_storage_buffer_binding_size.min(limits.max_buffer_size) {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "mask_deformation_storage",
            ));
        }
        let mut payload = Vec::with_capacity(words);
        payload.extend([
            field.lattice_side(),
            extent.width(),
            extent.height(),
            field.offset_bits(),
            x_at as u32,
            y_at as u32,
            rows_at as u32,
            spans_at as u32,
        ]);
        payload.extend_from_slice(bytemuck::cast_slice(field.offsets()));
        payload.extend_from_slice(bytemuck::cast_slice(x));
        payload.extend_from_slice(bytemuck::cast_slice(y));
        payload.extend_from_slice(field.row_offsets());
        payload.extend_from_slice(bytemuck::cast_slice(field.spans()));
        let storage = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.mask-deformation.field"),
            contents: bytemuck::cast_slice(&payload),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let input = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output = destination.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.mask-deformation.bind-group"),
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
            label: Some("mondrian.mask-deformation.pass"),
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
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
        Ok(destination.clone())
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<storage, read> field: array<u32>;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(positions[index], 0.0, 1.0);
}

fn inside(pixel: vec2<u32>) -> bool {
    let rows = field[6];
    let spans = field[7];
    let end = field[rows + pixel.y + 1u];
    var left = field[rows + pixel.y];
    var right = end;
    while left < right {
        let middle = left + (right - left) / 2u;
        if field[spans + middle * 2u + 1u] <= pixel.x {
            left = middle + 1u;
        } else {
            right = middle;
        }
    }
    if left == end { return false; }
    return field[spans + left * 2u] <= pixel.x;
}

fn offset_pair(at: u32) -> vec2<i32> {
    return vec2(bitcast<i32>(field[at]), bitcast<i32>(field[at + 1u]));
}

fn fixed_mix(a: vec2<i32>, b: vec2<i32>, weight: u32) -> vec2<i32> {
    let delta = b - a;
    let low = (bitcast<vec2<u32>>(delta) & vec2(65535u)) * weight + vec2(32768u);
    return a + (delta >> vec2(16u)) * i32(weight) + vec2<i32>(low >> vec2(16u));
}

fn associate(p: vec4<f32>) -> vec4<f32> {
    let a = clamp(p.a, 0.0, 1.0);
    return vec4(p.rgb * a, a);
}

fn sample_picture(pixel: vec2<u32>, offset: vec2<i32>, dims: vec2<u32>) -> vec4<f32> {
    let bits = field[3];
    let unit = 1u << bits;
    let base = vec2<i32>(pixel) + (offset >> vec2(bits));
    let limit = vec2<i32>(dims) - vec2(1);
    let lo = clamp(base, vec2(0), limit);
    let hi = clamp(base + vec2(1), vec2(0), limit);
    // Match the CPU's clamped coordinate before computing the fraction.
    let t = select(vec2(0.0), vec2<f32>(offset & vec2(i32(unit - 1u))) / f32(unit), lo != hi);
    let a = associate(textureLoad(source, lo, 0));
    let b = associate(textureLoad(source, vec2(hi.x, lo.y), 0));
    let c = associate(textureLoad(source, vec2(lo.x, hi.y), 0));
    let d = associate(textureLoad(source, hi, 0));
    let top = a * (1.0 - t.x) + b * t.x;
    let bottom = c * (1.0 - t.x) + d * t.x;
    let rgba = top * (1.0 - t.y) + bottom * t.y;
    let alpha = clamp(rgba.a, 0.0, 1.0);
    if alpha <= 0.0 { return vec4(0.0); }
    return vec4(rgba.rgb / alpha, alpha);
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let side = field[0];
    let pixel = vec2<u32>(position.xy);
    let dims = vec2(field[1], field[2]);
    if side == 0u || !inside(pixel) {
        return textureLoad(source, vec2<i32>(pixel), 0);
    }
    let axis_x = field[4] + pixel.x * 2u;
    let axis_y = field[5] + pixel.y * 2u;
    let base = 8u + (field[axis_y] * side + field[axis_x]) * 2u;
    let top = fixed_mix(offset_pair(base), offset_pair(base + 2u), field[axis_x + 1u]);
    let bottom = fixed_mix(offset_pair(base + side * 2u), offset_pair(base + side * 2u + 2u), field[axis_x + 1u]);
    let offset = fixed_mix(top, bottom, field[axis_y + 1u]);
    if all(offset == vec2(0)) { return textureLoad(source, vec2<i32>(pixel), 0); }
    return sample_picture(pixel, offset, dims);
}
"#;
