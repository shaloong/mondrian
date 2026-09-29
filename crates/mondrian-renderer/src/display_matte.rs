//! Presentation-only linear-light matting before device-specific ICC calibration.

use crate::{
    ColorFrameAlpha, ColorFrameDomain, ColorFrameEncoding, ColorFrameSpace,
    GpuColorFrameAllocationPlan, GpuColorFrameHandle, GpuColorFrameIdAllocator,
    GpuColorFrameResource, GpuColorFrameTextureFormat, GpuColorFrameWgpuResource,
    GpuColorFrameWgpuResourcePool,
};
use mondrian_core::types::ColorSpace;
use mondrian_core::{Color, ColorEngine, OcioColorSpaceIdentity, WorkingColorSpace};
use wgpu::util::DeviceExt;

/// Opaque Viewer canvas with two sRGB overlays, matching the UI paint order.
/// Geometry is expressed in canvas points, independently of Retina scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuDisplayCalibrationBackdrop {
    /// Opaque sRGB canvas color.
    pub canvas: Color,
    /// sRGB overlay covering the canvas.
    pub dark: Color,
    /// sRGB overlay covering the even checker tiles.
    pub light: Color,
    /// Logical checker tile size.
    pub tile_size: f32,
    /// Visible texture origin relative to the complete canvas, in points.
    pub offset: [f32; 2],
    /// Logical points per output texture pixel.
    pub pixel_step: [f32; 2],
}

impl GpuDisplayCalibrationBackdrop {
    /// Constant opaque background for headless presentation and validation.
    pub const fn solid(color: Color) -> Self {
        Self {
            canvas: color,
            dark: Color::TRANSPARENT,
            light: Color::TRANSPARENT,
            tile_size: 16.0,
            offset: [0.0; 2],
            pixel_step: [1.0; 2],
        }
    }

    /// Exact bit identity for a physical presentation cache entry.
    pub fn key_suffix(self) -> String {
        self.components().into_iter().map(|v| format!("{:08x}", v.to_bits())).collect()
    }

    fn components(self) -> [f32; 17] {
        [
            self.canvas.r,
            self.canvas.g,
            self.canvas.b,
            self.canvas.a,
            self.dark.r,
            self.dark.g,
            self.dark.b,
            self.dark.a,
            self.light.r,
            self.light.g,
            self.light.b,
            self.light.a,
            self.tile_size,
            self.offset[0],
            self.offset[1],
            self.pixel_step[0],
            self.pixel_step[1],
        ]
    }

    pub(crate) fn validate(self) -> Result<(), String> {
        if !self.components().into_iter().all(f32::is_finite)
            || self.canvas.a != 1.0
            || self.tile_size <= 0.0
            || self.pixel_step.into_iter().any(|v| v <= 0.0)
            || [self.canvas, self.dark, self.light]
                .into_iter()
                .any(|c| [c.r, c.g, c.b, c.a].into_iter().any(|v| !(0.0..=1.0).contains(&v)))
        {
            return Err("ICC presentation backdrop must be finite, normalized and opaque, with positive sampling geometry".to_owned());
        }
        Ok(())
    }

    fn uniforms(self) -> Result<MatteUniforms, String> {
        self.validate()?;
        let mut colors = [
            self.canvas.r,
            self.canvas.g,
            self.canvas.b,
            self.canvas.a,
            self.dark.r,
            self.dark.g,
            self.dark.b,
            self.dark.a,
            self.light.r,
            self.light.g,
            self.light.b,
            self.light.a,
        ];
        // These are standardized sRGB UI colors, never scene content or a
        // creative view. Reuse the core's immutable OCIO identity processor.
        ColorEngine::mondrian_standard().convert_identity_float(
            &mut colors,
            OcioColorSpaceIdentity::Color(ColorSpace::Srgb),
            OcioColorSpaceIdentity::Working(WorkingColorSpace::LinearRec709),
        )?;
        Ok(MatteUniforms {
            canvas: colors[0..4].try_into().unwrap(),
            dark: colors[4..8].try_into().unwrap(),
            light: colors[8..12].try_into().unwrap(),
            geometry: [
                self.offset[0],
                self.offset[1],
                self.pixel_step[0],
                self.pixel_step[1],
            ],
            tile: [self.tile_size, 0.0, 0.0, 0.0],
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MatteUniforms {
    canvas: [f32; 4],
    dark: [f32; 4],
    light: [f32; 4],
    geometry: [f32; 4],
    tile: [f32; 4],
}

pub(crate) struct GpuDisplayMattePipeline {
    pipeline: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
    backdrop: Option<(GpuDisplayCalibrationBackdrop, MatteUniforms)>,
}

impl GpuDisplayMattePipeline {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.display-matte.shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.display-matte.bindings"),
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
            label: Some("mondrian.display-matte.layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.display-matte.pipeline"),
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
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, bindings, backdrop: None }
    }

    pub(crate) fn record(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        ids: &mut GpuColorFrameIdAllocator,
        pool: &GpuColorFrameWgpuResourcePool,
        input: &GpuColorFrameResource<GpuColorFrameWgpuResource>,
        backdrop: GpuDisplayCalibrationBackdrop,
    ) -> Result<GpuColorFrameResource<GpuColorFrameWgpuResource>, String> {
        let descriptor = input.handle().descriptor();
        if descriptor.domain != ColorFrameDomain::Display
            || descriptor.encoding != ColorFrameEncoding::LinearFloat
            || descriptor.color_space != ColorFrameSpace::Working(WorkingColorSpace::LinearRec709)
            || !descriptor.alpha.is_straight_compatible()
        {
            return Err("ICC matting requires a straight display-linear Rec.709 signal".to_owned());
        }
        if self.backdrop.as_ref().map(|(key, _)| *key) != Some(backdrop) {
            self.backdrop = Some((backdrop, backdrop.uniforms()?));
        }
        let params = self.backdrop.as_ref().unwrap().1;
        // Each record owns immutable parameters. Rewriting a shared buffer
        // could change a previously encoded, not-yet-completed frame.
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.display-matte.params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.display-matte.frame"),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input.resource().texture_view),
                },
                wgpu::BindGroupEntry { binding: 1, resource: buffer.as_entire_binding() },
            ],
        });
        let handle = GpuColorFrameHandle::new(
            ids.allocate().map_err(|e| e.to_string())?,
            crate::ColorFrameDescriptor { alpha: ColorFrameAlpha::Opaque, ..descriptor },
            GpuColorFrameTextureFormat::Rgba32Float,
            "viewer-display-matte",
        )
        .map_err(|e| e.to_string())?;
        let output = pool.acquire(device, &GpuColorFrameAllocationPlan::for_handle(handle));
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.display-matte.pass"),
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
        pass.set_bind_group(0, &bindings, &[]);
        pass.draw(0..4, 0..1);
        drop(pass);
        Ok(output)
    }
}

const SHADER: &str = r#"
@group(0) @binding(0) var signal: texture_2d<f32>;
struct Params { canvas: vec4<f32>, dark: vec4<f32>, light: vec4<f32>, geometry: vec4<f32>, tile: vec4<f32> };
@group(0) @binding(1) var<uniform> params: Params;
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    var positions = array<vec2<f32>, 4>(vec2(-1.0,-1.0),vec2(1.0,-1.0),vec2(-1.0,1.0),vec2(1.0,1.0));
    return vec4(positions[index], 0.0, 1.0);
}
@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let point = params.geometry.xy + position.xy * params.geometry.zw;
    let cell = vec2<i32>(floor(point / params.tile.x));
    var background = mix(params.canvas.rgb, params.dark.rgb, params.dark.a);
    if ((cell.x + cell.y) % 2 == 0) { background = mix(background, params.light.rgb, params.light.a); }
    let source = textureLoad(signal, vec2<i32>(floor(position.xy)), 0);
    let alpha = clamp(source.a, 0.0, 1.0);
    if (!(alpha > 0.0)) { return vec4(background, 1.0); }
    return vec4(mix(background, source.rgb, alpha), 1.0);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_nonfinite_transparent_or_invalid_sampling_backdrops() {
        let valid = GpuDisplayCalibrationBackdrop::solid(Color::BLACK);
        assert!(valid.validate().is_ok());
        for invalid in [
            GpuDisplayCalibrationBackdrop { canvas: Color::TRANSPARENT, ..valid },
            GpuDisplayCalibrationBackdrop { offset: [f32::NAN, 0.0], ..valid },
            GpuDisplayCalibrationBackdrop { pixel_step: [1.0, 0.0], ..valid },
            GpuDisplayCalibrationBackdrop { tile_size: -1.0, ..valid },
            GpuDisplayCalibrationBackdrop {
                dark: Color { r: f32::INFINITY, ..Color::BLACK },
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err(), "{invalid:?}");
        }
    }
}
