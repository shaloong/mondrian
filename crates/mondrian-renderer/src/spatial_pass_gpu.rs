//! GPU execution of built-in spatial effect passes.
//!
//! The effect graph lowering decides *which* spatial passes run and in which
//! order (`mondrian_effects::CompiledEffectGpuPipeline`). This module turns one
//! [`EffectGpuSpatialPass`] into render passes over working-format textures so a
//! spatial effect never has to leave the GPU when it owns GPU mathematics.
//!
//! Contract:
//! - Input and output are the product working format (`Rgba32Float`). The pass
//!   never quantizes to 8-bit and never changes the color domain.
//! - The working seam is straight alpha. Box blur is a linear filter, so the
//!   executor premultiplies on the first pass, keeps the intermediate
//!   premultiplied, and unpremultiplies on the last pass. That is exactly what
//!   the CPU kernel does and keeps transparent neighbors from tinting visible
//!   pixels.
//! - Edge behavior follows the authored `repeat_edge_pixels` flag: out-of-frame
//!   samples either repeat the edge pixel or count as zero coverage, and the
//!   divisor always uses the authored normalization `2r + 1 + 2f`.
//! - Directional blur is intentionally unsupported here: it is `O(taps)` per
//!   pixel, and the measured CPU row-band implementation already beats a
//!   gather-based GPU pass. Callers must keep that pass on the CPU.

// The layer compositor owns the execution seam and starts driving these passes
// in the follow-up slice; until then the executor is exercised by its own GPU
// parity tests only.
#![allow(dead_code)]

use mondrian_effects::EffectGpuSpatialPass;
use wgpu::util::DeviceExt as _;

/// One GPU spatial pass could not be executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum EffectSpatialGpuError {
    /// The pass has no GPU mathematics yet.
    #[error("spatial pass `{0}` has no GPU implementation")]
    UnsupportedPass(&'static str),
    /// The frame geometry is empty.
    #[error("spatial pass needs a non-empty frame")]
    EmptyFrame,
}

/// Reusable pipelines for GPU spatial effect passes.
pub(crate) struct EffectSpatialGpu {
    box_blur: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
}

impl EffectSpatialGpu {
    /// Build the spatial pass pipelines for one device.
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.shader"),
            source: wgpu::ShaderSource::Wgsl(SPATIAL_EFFECT_SHADER.into()),
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.effect-spatial.bindings"),
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
            label: Some("mondrian.effect-spatial.layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let box_blur = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.box-blur"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_box_blur"),
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
        });
        Self { box_blur, bindings }
    }

    /// Number of texture passes one spatial pass expands into.
    ///
    /// Box blur expands per axis and per iteration; callers use this to size a
    /// ping-pong scratch pool before recording.
    pub(crate) fn texture_pass_count(
        pass: &EffectGpuSpatialPass,
    ) -> Result<u32, EffectSpatialGpuError> {
        match pass {
            EffectGpuSpatialPass::BoxBlur { direction, iterations, .. } => {
                let axes =
                    u32::from(direction.blurs_horizontal()) + u32::from(direction.blurs_vertical());
                Ok(axes * (*iterations).max(1))
            }
            EffectGpuSpatialPass::DirectionalBlur { .. } => {
                Err(EffectSpatialGpuError::UnsupportedPass("directional_blur"))
            }
        }
    }

    /// Record every texture pass of one spatial effect.
    ///
    /// `source` is the straight-alpha working frame entering the effect;
    /// `scratch` must hold at least [`Self::texture_pass_count`] textures of the
    /// same geometry, and the recorded chain ping-pongs between them. The last
    /// written texture is returned.
    pub(crate) fn record(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        pass: &EffectGpuSpatialPass,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let EffectGpuSpatialPass::BoxBlur { radius, iterations, direction, repeat_edge_pixels } =
            pass
        else {
            return Err(EffectSpatialGpuError::UnsupportedPass(pass.name()));
        };
        let box_kernel = BoxKernel::new(*radius);
        let iterations = (*iterations).max(1);
        let mut axes = Vec::with_capacity(2);
        if direction.blurs_horizontal() {
            axes.push(0_u32);
        }
        if direction.blurs_vertical() {
            axes.push(1_u32);
        }
        let total_passes = (axes.len() as u32 * iterations) as usize;
        if scratch.len() < total_passes {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "box_blur_scratch_geometry",
            ));
        }
        let mut written = 0_usize;
        for iteration in 0..iterations {
            for axis in &axes {
                let uniform = SpatialPassUniform {
                    size: [width as f32, height as f32, 0.0, 0.0],
                    params: [
                        0.0,
                        *axis as f32,
                        box_kernel.whole_radius as f32,
                        box_kernel.fraction,
                    ],
                    extra: [
                        box_kernel.normalization(),
                        f32::from(u8::from(*repeat_edge_pixels)),
                        f32::from(u8::from(written == 0)),
                        f32::from(u8::from(
                            written + 1 == total_passes && iteration + 1 == iterations,
                        )),
                    ],
                    aces: [0.0; 4],
                };
                let input_view_owner;
                let input_view = if written == 0 {
                    input_view_owner = source.create_view(&wgpu::TextureViewDescriptor::default());
                    &input_view_owner
                } else {
                    input_view_owner =
                        scratch[written - 1].create_view(&wgpu::TextureViewDescriptor::default());
                    &input_view_owner
                };
                let output = &scratch[written];
                let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
                self.record_pass(device, encoder, input_view, &output_view, &uniform);
                written += 1;
            }
        }
        Ok(scratch[written - 1].clone())
    }

    fn record_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &SpatialPassUniform,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.effect-spatial.uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.effect-spatial.bind-group"),
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
            label: Some("mondrian.effect-spatial.pass"),
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
        pass.set_pipeline(&self.box_blur);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
}

/// Integer/fractional split of an authored box blur radius.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BoxKernel {
    whole_radius: u32,
    fraction: f32,
}

impl BoxKernel {
    fn new(radius: f32) -> Self {
        let radius = radius.max(0.0);
        let whole = radius.floor();
        Self {
            whole_radius: whole as u32,
            fraction: radius - whole,
        }
    }

    /// Total authored weight: `2r + 1 + 2f`, matching the CPU kernel divisor.
    fn normalization(self) -> f32 {
        2.0 * self.whole_radius as f32 + 1.0 + 2.0 * self.fraction
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpatialPassUniform {
    size: [f32; 4],
    params: [f32; 4],
    extra: [f32; 4],
    aces: [f32; 4],
}

const SPATIAL_EFFECT_SHADER: &str = r#"
struct Uniforms {
    size: vec4<f32>,
    params: vec4<f32>,
    extra: vec4<f32>,
    aces: vec4<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> uniforms: Uniforms;

struct VsOut {
    @builtin(position) position: vec4<f32>,
};

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

fn frame_dims() -> vec2<i32> {
    return vec2<i32>(uniforms.size.xy);
}

fn load_clamped(p: vec2<i32>) -> vec4<f32> {
    let limit = frame_dims() - vec2<i32>(1);
    return textureLoad(src, clamp(p, vec2<i32>(0), limit), 0);
}

fn load_window(p: vec2<i32>, repeat_edge: bool) -> vec4<f32> {
    let dims = frame_dims();
    if (!repeat_edge && (p.x < 0 || p.y < 0 || p.x >= dims.x || p.y >= dims.y)) {
        return vec4<f32>(0.0);
    }
    return load_clamped(p);
}

fn premultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    return vec4<f32>(sample.rgb * alpha, alpha);
}

fn unpremultiply(sample: vec4<f32>) -> vec4<f32> {
    if (sample.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sample.rgb / sample.a, sample.a);
}

@fragment
fn fs_box_blur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let axis = uniforms.params.y;
    let whole = i32(uniforms.params.z);
    let fraction = uniforms.params.w;
    let repeat_edge = uniforms.extra.y > 0.5;
    let premultiply_input = uniforms.extra.z > 0.5;

    var sum = vec4<f32>(0.0);
    for (var offset = -whole; offset <= whole; offset = offset + 1) {
        var sample_at = pixel;
        if (axis < 0.5) {
            sample_at.x = sample_at.x + offset;
        } else {
            sample_at.y = sample_at.y + offset;
        }
        var sample = load_window(sample_at, repeat_edge);
        if (premultiply_input) {
            sample = premultiply(sample);
        }
        sum = sum + sample;
    }
    if (fraction > 0.0) {
        var outer_a = pixel;
        var outer_b = pixel;
        if (axis < 0.5) {
            outer_a.x = outer_a.x + whole + 1;
            outer_b.x = outer_b.x - whole - 1;
        } else {
            outer_a.y = outer_a.y + whole + 1;
            outer_b.y = outer_b.y - whole - 1;
        }
        var sample_a = load_window(outer_a, repeat_edge);
        var sample_b = load_window(outer_b, repeat_edge);
        if (premultiply_input) {
            sample_a = premultiply(sample_a);
            sample_b = premultiply(sample_b);
        }
        sum = sum + fraction * (sample_a + sample_b);
    }
    var result = sum / max(uniforms.extra.x, 1.0);
    if (uniforms.extra.w > 0.5) {
        result = unpremultiply(result);
    }
    return result;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::{box_blur, BoxBlurDirection};
    use pollster::block_on;

    fn upload(
        context: &crate::GpuContext,
        pixels: &[[f32; 4]],
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        let bytes: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| pixel.iter().flat_map(|value| value.to_le_bytes()))
            .collect();
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mondrian.effect-spatial.source"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        context.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 16),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        texture
    }

    fn scratch(
        context: &crate::GpuContext,
        width: u32,
        height: u32,
        count: usize,
    ) -> Vec<wgpu::Texture> {
        (0..count)
            .map(|_| {
                context.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("mondrian.effect-spatial.scratch"),
                    size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba32Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            })
            .collect()
    }

    fn readback(
        context: &crate::GpuContext,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<[f32; 4]> {
        let unpadded = width * 16;
        let padded = unpadded.div_ceil(256) * 256;
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian.effect-spatial.readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.readback-encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        context.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = context
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
        let data = slice.get_mapped_range().expect("mapped readback range");
        let mut pixels = Vec::with_capacity((width * height) as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            for column in 0..width as usize {
                let base = start + column * 16;
                let mut pixel = [0.0_f32; 4];
                for (component, value) in pixel.iter_mut().enumerate() {
                    let offset = base + component * 4;
                    *value = f32::from_le_bytes([
                        data[offset],
                        data[offset + 1],
                        data[offset + 2],
                        data[offset + 3],
                    ]);
                }
                pixels.push(pixel);
            }
        }
        drop(data);
        buffer.unmap();
        pixels
    }

    fn run_box_blur_on_gpu(
        context: &crate::GpuContext,
        pixels: &[[f32; 4]],
        width: u32,
        height: u32,
        radius: f32,
        iterations: u32,
        direction: BoxBlurDirection,
        repeat_edge_pixels: bool,
    ) -> Vec<[f32; 4]> {
        let executor = EffectSpatialGpu::new(&context.device);
        let pass =
            EffectGpuSpatialPass::BoxBlur { radius, iterations, direction, repeat_edge_pixels };
        let count =
            EffectSpatialGpu::texture_pass_count(&pass).expect("box blur pass count") as usize;
        let source = upload(context, pixels, width, height);
        let scratch = scratch(context, width, height, count);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.encoder"),
        });
        let output = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &scratch,
                width,
                height,
            )
            .expect("recorded box blur");
        context.queue.submit(Some(encoder.finish()));
        readback(context, &output, width, height)
    }

    #[test]
    fn box_blur_matches_the_cpu_kernel_for_a_single_pass() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU box blur parity test: no GPU adapter available");
            return;
        };
        let width = 4_u32;
        let height = 1_u32;
        let source = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.0, 2.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 4.0, 0.0, 1.0],
        ];
        let mut expected = source.clone();
        assert!(box_blur::box_blur_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            1.0,
            1,
            BoxBlurDirection::Horizontal,
            true,
        ));
        let actual = run_box_blur_on_gpu(
            &context,
            &source,
            width,
            height,
            1.0,
            1,
            BoxBlurDirection::Horizontal,
            true,
        );
        for (index, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
            for component in 0..4 {
                assert!(
                    (expected[component] - actual[component]).abs() <= 1.0e-4,
                    "pixel {index} component {component}: CPU {} vs GPU {}",
                    expected[component],
                    actual[component]
                );
            }
        }
    }

    #[test]
    fn box_blur_keeps_transparent_neighbours_from_tinting_visible_pixels() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU box blur transparency test: no GPU adapter available");
            return;
        };
        let width = 3_u32;
        let height = 1_u32;
        let source = vec![
            [1.0_f32, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
        let mut expected = source.clone();
        assert!(box_blur::box_blur_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            1.0,
            1,
            BoxBlurDirection::Horizontal,
            false,
        ));
        let actual = run_box_blur_on_gpu(
            &context,
            &source,
            width,
            height,
            1.0,
            1,
            BoxBlurDirection::Horizontal,
            false,
        );
        for (index, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
            for component in 0..4 {
                assert!(
                    (expected[component] - actual[component]).abs() <= 1.0e-4,
                    "pixel {index} component {component}: CPU {} vs GPU {}",
                    expected[component],
                    actual[component]
                );
            }
        }
    }

    #[test]
    fn directional_blur_reports_that_it_has_no_gpu_math() {
        let pass = EffectGpuSpatialPass::DirectionalBlur { length: 8.0, direction_degrees: 30.0 };
        assert_eq!(
            EffectSpatialGpu::texture_pass_count(&pass),
            Err(EffectSpatialGpuError::UnsupportedPass("directional_blur"))
        );
    }

    #[test]
    fn box_kernel_normalization_matches_the_authored_weight_sum() {
        let kernel = BoxKernel::new(4.5);
        assert_eq!(kernel.whole_radius, 4);
        assert!((kernel.fraction - 0.5).abs() <= f32::EPSILON);
        assert!((kernel.normalization() - 10.0).abs() <= 1.0e-6);
    }

    #[test]
    fn box_blur_fractional_radius_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU fractional box blur test: no GPU adapter available");
            return;
        };
        let width = 5_u32;
        let height = 1_u32;
        let source = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.5, 0.0, 1.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut expected = source.clone();
        assert!(box_blur::box_blur_frame_f32(
            &mut expected,
            width as usize,
            height as usize,
            1.5,
            1,
            BoxBlurDirection::Horizontal,
            true,
        ));
        let actual = run_box_blur_on_gpu(
            &context,
            &source,
            width,
            height,
            1.5,
            1,
            BoxBlurDirection::Horizontal,
            true,
        );
        for (index, (expected, actual)) in expected.iter().zip(actual.iter()).enumerate() {
            for component in 0..4 {
                assert!(
                    (expected[component] - actual[component]).abs() <= 1.0e-4,
                    "pixel {index} component {component}: CPU {} vs GPU {}",
                    expected[component],
                    actual[component]
                );
            }
        }
    }
}
