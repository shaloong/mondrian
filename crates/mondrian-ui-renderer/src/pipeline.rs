//! 2D UI 渲染管线
//!
//! 包含顶点/片段 shader 和 bind group layout。
//! 支持 SDF 几何、纹理图集图像、字形图像和小型 SVG raster atlas 绘制。

use crate::shape::RectVertex;

/// UI 渲染 shader 源代码
pub const UI_VERTEX_SHADER: &str = include_str!("../shaders/ui_vertex.wgsl");
pub const UI_FRAGMENT_SHADER: &str = include_str!("../shaders/ui_fragment.wgsl");

fn ui_primitive_state() -> wgpu::PrimitiveState {
    wgpu::PrimitiveState {
        topology: wgpu::PrimitiveTopology::TriangleList,
        cull_mode: None,
        ..Default::default()
    }
}

/// 2D UI 渲染管线
pub struct UiPipeline {
    pub render_pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub texture_bind_group_layout: wgpu::BindGroupLayout,
}

impl UiPipeline {
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        sample_count: u32,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui_shader"),
            source: wgpu::ShaderSource::Wgsl(UI_VERTEX_SHADER.into()),
        });
        let frag = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ui_frag"),
            source: wgpu::ShaderSource::Wgsl(UI_FRAGMENT_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ui_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("ui_texture_bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ui_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout), Some(&texture_bind_group_layout)],
            immediate_size: 0,
        });

        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ui_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("main"),
                buffers: &[RectVertex::layout()],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &frag,
                entry_point: Some("main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: ui_primitive_state(),
            multisample: wgpu::MultisampleState { count: sample_count, ..Default::default() },
            depth_stencil: None,
            multiview_mask: None,
            cache: None,
        });

        Self {
            render_pipeline,
            bind_group_layout,
            texture_bind_group_layout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{LINE_SHADER_AA_EDGE_SCALE, LINE_SHADER_AA_MAX_PX, LINE_SHADER_AA_MIN_PX};

    #[test]
    fn ui_shaders_parse_and_validate() {
        validate_wgsl(UI_VERTEX_SHADER);
        validate_wgsl(UI_FRAGMENT_SHADER);
    }

    #[test]
    fn line_shader_aa_contract_matches_cpu_coverage_tests() {
        let fragment = UI_FRAGMENT_SHADER.split_whitespace().collect::<Vec<_>>().join(" ");
        let clamp_expr = format!(
            "clamp(fwidth(d), {}, {})",
            LINE_SHADER_AA_MIN_PX, LINE_SHADER_AA_MAX_PX
        );
        let smoothstep_expr = format!(
            "smoothstep(aa * {}, -aa * {}, d)",
            LINE_SHADER_AA_EDGE_SCALE, LINE_SHADER_AA_EDGE_SCALE
        );

        assert!(
            fragment.contains(&clamp_expr),
            "fragment shader must keep line AA clamp {clamp_expr:?} in sync with CPU coverage tests"
        );
        assert!(
            fragment.contains(&smoothstep_expr),
            "fragment shader must keep line alpha edge scale {smoothstep_expr:?} in sync with CPU coverage tests"
        );
    }

    #[test]
    fn soft_shadow_shader_contract_uses_blur_distance_field() {
        let fragment = UI_FRAGMENT_SHADER.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(fragment.contains("const RENDER_MODE_SOFT_SHADOW: u32 = 4u;"));
        assert!(fragment.contains("@location(5) @interpolate(flat) blur_radius_px: f32"));
        assert!(fragment.contains("smoothstep(0.0, blur, outside)"));
    }

    #[test]
    fn ui_pipeline_2d_primitives_do_not_depend_on_backface_culling() {
        let primitive = ui_primitive_state();

        assert_eq!(primitive.topology, wgpu::PrimitiveTopology::TriangleList);
        assert_eq!(primitive.cull_mode, None);
    }

    fn validate_wgsl(source: &str) {
        let module = naga::front::wgsl::parse_str(source).expect("WGSL should parse");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .expect("WGSL should validate");
    }
}
