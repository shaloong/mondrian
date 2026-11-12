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

use mondrian_effects::{
    AuxiliaryCoordinateMapping, AuxiliaryFilter, AuxiliaryOutOfBounds, EffectGpuSpatialPass,
};
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
    /// This kernel requires a second raster supplied by its caller.
    #[error("spatial pass requires an auxiliary raster")]
    MissingAuxiliaryInput,
}

/// Reusable pipelines for GPU spatial effect passes.
pub(crate) struct EffectSpatialGpu {
    bilateral: std::sync::OnceLock<crate::bilateral_gpu::BilateralGpu>,
    box_blur: wgpu::RenderPipeline,
    find_edges: wgpu::RenderPipeline,
    emboss: wgpu::RenderPipeline,
    color_emboss: wgpu::RenderPipeline,
    radial_blur: wgpu::RenderPipeline,
    offset: wgpu::RenderPipeline,
    twirl: wgpu::RenderPipeline,
    spherize: wgpu::RenderPipeline,
    vr_rotate_sphere: wgpu::RenderPipeline,
    vr_sphere_to_plane: wgpu::RenderPipeline,
    vr_project_2d: wgpu::RenderPipeline,
    magnify: wgpu::RenderPipeline,
    dent: wgpu::RenderPipeline,
    warp: wgpu::RenderPipeline,
    grid_warp: wgpu::RenderPipeline,
    wire_removal: wgpu::RenderPipeline,
    roughen_edges: wgpu::RenderPipeline,
    mirror: wgpu::RenderPipeline,
    polar_coordinates: wgpu::RenderPipeline,
    lens_distortion: wgpu::RenderPipeline,
    chromatic_aberration_removal: wgpu::RenderPipeline,
    basic_3d: wgpu::RenderPipeline,
    camera_view: wgpu::RenderPipeline,
    grow_bounds: wgpu::RenderPipeline,
    blanking_fill_horizontal: wgpu::RenderPipeline,
    blanking_fill_vertical: wgpu::RenderPipeline,
    blanking_fill_warp: wgpu::RenderPipeline,
    flip: wgpu::RenderPipeline,
    ripple: wgpu::RenderPipeline,
    wave_warp: wgpu::RenderPipeline,
    transform: wgpu::RenderPipeline,
    corner_pin: wgpu::RenderPipeline,
    turbulent_displace: wgpu::RenderPipeline,
    lens_flare: wgpu::RenderPipeline,
    bulge: wgpu::RenderPipeline,
    split: wgpu::RenderPipeline,
    linear_color_key: wgpu::RenderPipeline,
    color_offset: wgpu::RenderPipeline,
    convolution: wgpu::RenderPipeline,
    deband: wgpu::RenderPipeline,
    glow_extract: wgpu::RenderPipeline,
    glow_composite: wgpu::RenderPipeline,
    fractal_noise: wgpu::RenderPipeline,
    keylight_key: wgpu::RenderPipeline,
    keylight_erode: wgpu::RenderPipeline,
    keylight_composite: wgpu::RenderPipeline,
    matte_refine_cleanup: wgpu::RenderPipeline,
    matte_refine_smooth: wgpu::RenderPipeline,
    matte_refine_feather: wgpu::RenderPipeline,
    matte_refine_composite: wgpu::RenderPipeline,
    choker_composite: wgpu::RenderPipeline,
    key_cleaner_composite: wgpu::RenderPipeline,
    smart_blur: wgpu::RenderPipeline,
    minimax: wgpu::RenderPipeline,
    channel_blur: wgpu::RenderPipeline,
    median: wgpu::RenderPipeline,
    dust_scratches: wgpu::RenderPipeline,
    selective_color: wgpu::RenderPipeline,
    stroke: wgpu::RenderPipeline,
    bezier_warp: wgpu::RenderPipeline,
    fill_matte: wgpu::RenderPipeline,
    channel_combiner_layer: wgpu::RenderPipeline,
    displacement_map: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
    composite_bindings: wgpu::BindGroupLayout,
    fractal_noise_bindings: wgpu::BindGroupLayout,
    selective_color_bindings: wgpu::BindGroupLayout,
    stroke_bindings: wgpu::BindGroupLayout,
    bezier_warp_bindings: wgpu::BindGroupLayout,
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
        let find_edges = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.find-edges"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_find_edges"),
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
        let emboss = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.emboss"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_emboss"),
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
        let color_emboss = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.color-emboss"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_color_emboss"),
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
        let convolution = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.convolution"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_convolution"),
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
        let radial_blur = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.radial-blur"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_radial_blur"),
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
        let offset = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.offset"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_offset"),
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
        let twirl = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.twirl"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_twirl"),
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
        let spherize = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.spherize"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_spherize"),
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
        let vr_rotate_sphere = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.vr_rotate_sphere"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_vr_rotate_sphere"),
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
        let vr_sphere_to_plane = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.vr_sphere_to_plane"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_vr_sphere_to_plane"),
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
        let vr_project_2d = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.vr_project_2d"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_vr_project_2d"),
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
        let magnify = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.magnify"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_magnify"),
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
        let dent = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.dent"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_dent"),
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
        let warp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.warp"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_warp"),
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
        let grid_warp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.grid-warp"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_grid_warp"),
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
        let wire_removal = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.wire-removal"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_wire_removal"),
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
        let roughen_edges = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.roughen-edges"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_roughen_edges"),
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
        let mirror = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.mirror"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mirror"),
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
        let polar_coordinates = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.polar-coordinates"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_polar"),
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
        let lens_distortion = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.lens-distortion"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_lens_distortion"),
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
        let chromatic_aberration_removal =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mondrian.effect-spatial.chromatic-aberration-removal"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_chromatic_aberration_removal"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    cull_mode: None,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        let basic_3d = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.basic-3d"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_basic_3d"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let camera_view = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.camera-view"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_camera_view"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let flip = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.flip"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_flip"),
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
        let ripple = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.ripple"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_ripple"),
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
        let wave_warp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.wave-warp"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_wave_warp"),
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
        let transform = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.transform"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_transform"),
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
        let corner_pin = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.corner-pin"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_corner_pin"),
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
        let turbulent_displace = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.turbulent-displace"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_turbulent_displace"),
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
        let lens_flare = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.lens-flare"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_lens_flare"),
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
        let bulge = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.bulge"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_bulge"),
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
        let linear_color_key = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.linear_color_key"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_linear_color_key"),
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
        let color_offset = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.color_offset"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_color_offset"),
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
        let grow_bounds = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.grow-bounds"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_grow_bounds"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let split = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.split"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_split"),
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
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.composite-shader"),
            source: wgpu::ShaderSource::Wgsl(spatial_composite_shader_source().into()),
        });
        let composite_bindings =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian.effect-spatial.composite-bindings"),
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
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
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
        let composite_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.effect-spatial.composite-layout"),
            bind_group_layouts: &[Some(&composite_bindings)],
            immediate_size: 0,
        });
        let blanking_fill_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.blanking-fill"),
            source: wgpu::ShaderSource::Wgsl(blanking_fill_shader_source().into()),
        });
        let blanking_fill_pipeline = |label, entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&composite_layout),
                vertex: wgpu::VertexState {
                    module: &blanking_fill_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &blanking_fill_shader,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let blanking_fill_horizontal = blanking_fill_pipeline(
            "mondrian.effect-spatial.blanking-fill-horizontal",
            "fs_horizontal",
        );
        let blanking_fill_vertical = blanking_fill_pipeline(
            "mondrian.effect-spatial.blanking-fill-vertical",
            "fs_vertical",
        );
        let blanking_fill_warp =
            blanking_fill_pipeline("mondrian.effect-spatial.blanking-fill-warp", "fs_warp");
        let auxiliary_pipeline = |label, entry_point| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&composite_layout),
                vertex: wgpu::VertexState {
                    module: &composite_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_shader,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba32Float,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..wgpu::PrimitiveState::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let fill_matte = auxiliary_pipeline("mondrian.effect-spatial.fill-matte", "fs_fill_matte");
        let channel_combiner_layer = auxiliary_pipeline(
            "mondrian.effect-spatial.channel-combiner-layer",
            "fs_channel_combiner_layer",
        );
        let displacement_map = auxiliary_pipeline(
            "mondrian.effect-spatial.displacement-map",
            "fs_displacement_map",
        );
        let deband = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.deband"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_deband"),
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
        let glow_extract = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.glow-extract"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_glow_extract"),
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
        let glow_composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.glow-composite"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_glow_composite"),
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
        // Keylight keys the source into a matte, erodes that matte with a
        // separable min filter, and composites the despilled source against it.
        let keylight_key = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.keylight-key"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_keylight_key"),
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
        let keylight_erode = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.keylight-erode"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_keylight_erode"),
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
        let keylight_composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.keylight-composite"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_keylight_composite"),
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
        // Matte refine cleans the edge colour, smooths and shifts the carried
        // matte, feathers it, and composites the cleaned source against it.
        let matte_refine_cleanup = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.matte-refine-cleanup"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_matte_refine_cleanup"),
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
        let matte_refine_smooth = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.matte-refine-smooth"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_matte_refine_smooth"),
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
        let matte_refine_feather = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.matte-refine-feather"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_matte_refine_feather"),
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
        let matte_refine_composite =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mondrian.effect-spatial.matte-refine-composite"),
                layout: Some(&composite_layout),
                vertex: wgpu::VertexState {
                    module: &composite_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_shader,
                    entry_point: Some("fs_matte_refine_composite"),
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
        // The choker composite keeps the source colour verbatim and swaps in the
        // morphed alpha: no view map, no contrast, no premultiply in the shader.
        let choker_composite = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.choker-composite"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_choker_composite"),
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
        // The key cleaner composite finishes the separable chain: it averages the
        // eroded matte down the column, re-steepens it and blends it back by
        // strength, while the source colour passes through untouched.
        let key_cleaner_composite =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("mondrian.effect-spatial.key-cleaner-composite"),
                layout: Some(&composite_layout),
                vertex: wgpu::VertexState {
                    module: &composite_shader,
                    entry_point: Some("vs_main"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_shader,
                    entry_point: Some("fs_key_cleaner_composite"),
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
        // Smart blur runs one separable axis per draw: the gate always reads the
        // untouched source (binding 0) while the average reads the pass input
        // (binding 1), so the horizontal draw binds the source twice.
        let smart_blur = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.smart-blur"),
            layout: Some(&composite_layout),
            vertex: wgpu::VertexState {
                module: &composite_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &composite_shader,
                entry_point: Some("fs_smart_blur"),
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
        // Minimax runs one separable minimum or maximum step per pass and writes
        // straight into the scratch ping-pong: no separate composite stage.
        let minimax = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.minimax"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_minimax"),
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
        // Channel blur runs one separable box-blur step per pass and writes
        // straight into the scratch ping-pong: no separate composite stage.
        let channel_blur = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.channel-blur"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_channel_blur"),
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
        // Median writes the selected window median straight into the scratch
        // frame: one pass, no composite stage.
        let median = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.median"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_median"),
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
        // Dust & scratches reuses the median gather and gates its result, so it
        // writes the repaired pixel straight into the scratch frame as well.
        let dust_scratches = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.dust-scratches"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_dust_scratches"),
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
        // Fractal noise needs twenty-six scalars, so the shared uniform struct is
        // too small: the stage gets its own shader module, bind group layout, and
        // uniform payload instead of a sixth `extra` vector.
        let fractal_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.fractal-noise-shader"),
            source: wgpu::ShaderSource::Wgsl(SPATIAL_FRACTAL_NOISE_SHADER.into()),
        });
        let fractal_noise_bindings =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian.effect-spatial.fractal-noise-bindings"),
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
        let fractal_noise_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.effect-spatial.fractal-noise-layout"),
            bind_group_layouts: &[Some(&fractal_noise_bindings)],
            immediate_size: 0,
        });
        let fractal_noise = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.fractal-noise"),
            layout: Some(&fractal_noise_layout),
            vertex: wgpu::VertexState {
                module: &fractal_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fractal_shader,
                entry_point: Some("fs_fractal_noise"),
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
        // Selective colour needs a 160-byte payload: sixteen control bytes and
        // the nine folded delta rows. The shared uniform struct is far too
        // small, so the stage gets its own shader module, bind group layout, and
        // uniform instead of another `extra` vector.
        let selective_color_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.selective-color-shader"),
            source: wgpu::ShaderSource::Wgsl(SPATIAL_SELECTIVE_COLOR_SHADER.into()),
        });
        let selective_color_bindings =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian.effect-spatial.selective-color-bindings"),
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
                            min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                SelectiveColorUniform,
                            >()
                                as u64),
                        },
                        count: None,
                    },
                ],
            });
        let selective_color_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("mondrian.effect-spatial.selective-color-layout"),
                bind_group_layouts: &[Some(&selective_color_bindings)],
                immediate_size: 0,
            });
        let selective_color = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.selective-color"),
            layout: Some(&selective_color_layout),
            vertex: wgpu::VertexState {
                module: &selective_color_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &selective_color_shader,
                entry_point: Some("fs_selective_color"),
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
        // The stroke band reads a neighbourhood and carries a forty-eight byte
        // payload that the shared uniform cannot hold, so the stage gets its own
        // shader module, bind group layout, and pipeline.
        let stroke_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.stroke-shader"),
            source: wgpu::ShaderSource::Wgsl(SPATIAL_STROKE_SHADER.into()),
        });
        let stroke_bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mondrian.effect-spatial.stroke-bindings"),
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
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<StrokeUniform>() as u64,
                        ),
                    },
                    count: None,
                },
            ],
        });
        let stroke_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.effect-spatial.stroke-layout"),
            bind_group_layouts: &[Some(&stroke_bindings)],
            immediate_size: 0,
        });
        let stroke = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.stroke"),
            layout: Some(&stroke_layout),
            vertex: wgpu::VertexState {
                module: &stroke_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &stroke_shader,
                entry_point: Some("fs_stroke"),
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
        // The bezier warp inverts a Coons patch per fragment and carries a
        // hundred-and-twenty-eight byte payload, so it also gets its own shader
        // module, bind group layout, and pipeline.
        let bezier_warp_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.effect-spatial.bezier-warp-shader"),
            source: wgpu::ShaderSource::Wgsl(SPATIAL_BEZIER_WARP_SHADER.into()),
        });
        let bezier_warp_bindings =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("mondrian.effect-spatial.bezier-warp-bindings"),
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
                            min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<
                                BezierWarpUniform,
                            >()
                                as u64),
                        },
                        count: None,
                    },
                ],
            });
        let bezier_warp_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mondrian.effect-spatial.bezier-warp-layout"),
            bind_group_layouts: &[Some(&bezier_warp_bindings)],
            immediate_size: 0,
        });
        let bezier_warp = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mondrian.effect-spatial.bezier-warp"),
            layout: Some(&bezier_warp_layout),
            vertex: wgpu::VertexState {
                module: &bezier_warp_shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &bezier_warp_shader,
                entry_point: Some("fs_bezier_warp"),
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
        Self {
            box_blur,
            emboss,
            color_emboss,
            radial_blur,
            offset,
            twirl,
            spherize,
            vr_rotate_sphere,
            vr_sphere_to_plane,
            vr_project_2d,
            magnify,
            dent,
            warp,
            grid_warp,
            wire_removal,
            roughen_edges,
            mirror,
            polar_coordinates,
            lens_distortion,
            chromatic_aberration_removal,
            basic_3d,
            camera_view,
            grow_bounds,
            blanking_fill_horizontal,
            blanking_fill_vertical,
            blanking_fill_warp,
            flip,
            ripple,
            wave_warp,
            transform,
            corner_pin,
            turbulent_displace,
            lens_flare,
            bulge,
            split,
            linear_color_key,
            color_offset,
            convolution,
            deband,
            glow_extract,
            glow_composite,
            fractal_noise,
            find_edges,
            keylight_key,
            keylight_erode,
            keylight_composite,
            matte_refine_cleanup,
            matte_refine_smooth,
            matte_refine_feather,
            matte_refine_composite,
            choker_composite,
            key_cleaner_composite,
            smart_blur,
            bilateral: std::sync::OnceLock::new(),
            minimax,
            channel_blur,
            median,
            dust_scratches,
            selective_color,
            stroke,
            bezier_warp,
            fill_matte,
            channel_combiner_layer,
            displacement_map,
            bindings,
            composite_bindings,
            fractal_noise_bindings,
            selective_color_bindings,
            stroke_bindings,
            bezier_warp_bindings,
        }
    }

    /// Records the bulge magnification stage.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_bulge(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.bulge,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    /// Records the linear colour key stage.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_linear_color_key(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.linear_color_key,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    /// Records the colour offset stage.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_color_offset(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.color_offset,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    /// Records the split stage.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_split(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.split,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the flip pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_flip(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.flip,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }
    /// Records the ripple pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_ripple(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.ripple,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }
    /// Records the wave warp pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_wave_warp(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.wave_warp,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }
    /// Records the transform pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_transform(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: &mondrian_effects::TransformGrade,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let Some((params, extra)) = grade.gpu_controls(width, height) else {
            // A degenerate transform has no inverse matrix to sample with, so
            // the frame passes through unchanged instead of being resampled.
            return Ok(source.clone());
        };
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.transform,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }
    /// Records the corner pin pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_corner_pin(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: &mondrian_effects::CornerPinGrade,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let Some((params, extra)) = grade.gpu_controls(width, height) else {
            // A degenerate quad has no homography to sample with, so the frame
            // passes through unchanged instead of being resampled.
            return Ok(source.clone());
        };
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.corner_pin,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }
    /// Records the turbulent displace pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_turbulent_displace(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.turbulent_displace,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }
    /// Records the fractal noise generator pass.
    ///
    /// The stage reads only the pixel it writes, so the source is sampled
    /// per-pixel and no scratch texture or halo is involved.
    #[allow(clippy::too_many_arguments)]
    fn record_fractal_noise(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        uniform: &FractalNoiseUniform,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_fractal_noise_pass(device, encoder, &input_view, &output_view, uniform);
        Ok(destination.clone())
    }
    /// Records the selective colour pass.
    ///
    /// The kernel is a per-pixel point that reads no neighbour, so one
    /// full-screen draw covers the whole frame.
    pub(crate) fn record_selective_color(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::SelectiveColorGrade,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let (deltas, method) = grade.gpu_controls();
        let uniform = SelectiveColorUniform { control: [method, 0, 0, 0], deltas };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_selective_color_pass(device, encoder, &input_view, &output_view, &uniform);
        Ok(destination.clone())
    }
    /// Records the stroke pass.
    ///
    /// Every fragment resolves its own band, so one full-screen draw covers the
    /// whole frame even though the kernel reads a neighbourhood.
    pub(crate) fn record_stroke(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::StrokeGrade,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let (control, color, geometry) = grade.gpu_controls();
        let uniform = StrokeUniform { control, color, geometry };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_stroke_pass(device, encoder, &input_view, &output_view, &uniform);
        Ok(destination.clone())
    }
    /// Records the bezier warp pass.
    ///
    /// Every fragment inverts the Coons patch for its own target, so one
    /// full-screen draw covers the whole frame.
    pub(crate) fn record_bezier_warp(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::BezierWarpGrade,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let (points, control) = grade.gpu_controls();
        let uniform = BezierWarpUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            points,
            control,
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_bezier_warp_pass(device, encoder, &input_view, &output_view, &uniform);
        Ok(destination.clone())
    }
    /// Records the lens distortion pass.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_lens_distortion(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniforms = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.lens_distortion,
            &input_view,
            &output_view,
            &uniforms,
        );
        Ok(destination.clone())
    }

    fn record_chromatic_aberration_removal(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::ChromaticAberrationRemovalGrade,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
    ) -> wgpu::Texture {
        let (params, extra) = grade.gpu_controls();
        let uniforms = SpatialPassUniform {
            size: [source.width() as f32, source.height() as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.chromatic_aberration_removal,
            &input_view,
            &output_view,
            &uniforms,
        );
        destination.clone()
    }

    /// Records the lens flare stage.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_lens_flare(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.lens_flare,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    /// Sample an independent texture while processing the primary frame.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_auxiliary(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        pass: &EffectGpuSpatialPass,
        primary: &wgpu::Texture,
        auxiliary: &wgpu::Texture,
        destination: &wgpu::Texture,
        transform: [f32; 6],
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        let (pipeline, sampling, mut params, extra) = match *pass {
            EffectGpuSpatialPass::FillMatte { grade, sampling, .. } => {
                let [red, green, blue] = grade.color();
                (
                    &self.fill_matte,
                    sampling,
                    [
                        grade.feather_x(),
                        grade.feather_y(),
                        f32::from(u8::from(grade.invert())),
                        0.0,
                    ],
                    [red, green, blue, grade.opacity()],
                )
            }
            EffectGpuSpatialPass::ChannelCombinerLayer { grade, sampling, .. } => {
                let ([_, slots, flags, _], controls) = grade.gpu_controls();
                (
                    &self.channel_combiner_layer,
                    sampling,
                    [slots as f32, flags as f32, 0.0, 0.0],
                    controls,
                )
            }
            EffectGpuSpatialPass::DisplacementMap { grade, sampling, .. } => {
                let (controls, mut modes) = grade.gpu_controls();
                modes[2] = f32::from(u8::from(grade.is_identity()));
                (&self.displacement_map, sampling, modes, controls)
            }
            _ => return Err(EffectSpatialGpuError::UnsupportedPass(pass.name())),
        };
        if primary.width() == 0
            || primary.height() == 0
            || auxiliary.width() == 0
            || auxiliary.height() == 0
        {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        if transform.iter().any(|component| !component.is_finite()) {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "auxiliary_transform",
            ));
        }
        params[3] = f32::from(u8::from(sampling.filter == AuxiliaryFilter::Linear));
        let [a, b, tx, c, d, ty] = transform;
        let input_extent =
            mondrian_effects::EffectFrameExtent::new(primary.width(), primary.height());
        let bounds = mondrian_effects::EffectRasterBounds::from_extent(input_extent)
            .checked_outset(pass.output_outset())
            .ok_or(EffectSpatialGpuError::UnsupportedPass(
                "auxiliary_output_extent",
            ))?;
        if (destination.width(), destination.height())
            != (bounds.extent().width(), bounds.extent().height())
        {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "auxiliary_output_extent",
            ));
        }
        let [origin_x, origin_y] = bounds.origin();
        let uniforms = SpatialPassUniform {
            size: [
                primary.width() as f32,
                primary.height() as f32,
                f32::from(u8::from(
                    sampling.out_of_bounds == AuxiliaryOutOfBounds::Transparent,
                )),
                f32::from(u8::from(
                    sampling.mapping == AuxiliaryCoordinateMapping::FrameTransform,
                )),
            ],
            params,
            extra,
            aces: [a, b, tx, origin_x as f32],
            extra1: [c, d, ty, origin_y as f32],
        };
        let primary = primary.create_view(&wgpu::TextureViewDescriptor::default());
        let auxiliary = auxiliary.create_view(&wgpu::TextureViewDescriptor::default());
        let output = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device, encoder, pipeline, &primary, &auxiliary, &output, &uniforms,
        );
        Ok(destination.clone())
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
            // Every single-pass kernel needs one texture pass.
            EffectGpuSpatialPass::FindEdges { .. }
            | EffectGpuSpatialPass::Emboss { .. }
            | EffectGpuSpatialPass::ColorEmboss { .. }
            | EffectGpuSpatialPass::RadialBlur { .. }
            | EffectGpuSpatialPass::Offset { .. }
            | EffectGpuSpatialPass::Twirl { .. }
            | EffectGpuSpatialPass::Spherize { .. }
            | EffectGpuSpatialPass::VrRotateSphere { .. }
            | EffectGpuSpatialPass::VrSphereToPlane { .. }
            | EffectGpuSpatialPass::VrProject2D { .. }
            | EffectGpuSpatialPass::Magnify { .. }
            | EffectGpuSpatialPass::Dent { .. }
            | EffectGpuSpatialPass::Warp { .. }
            | EffectGpuSpatialPass::GridWarp { .. }
            | EffectGpuSpatialPass::WireRemoval { .. }
            | EffectGpuSpatialPass::RoughenEdges { .. }
            | EffectGpuSpatialPass::Convolution { .. }
            | EffectGpuSpatialPass::Mirror { .. }
            | EffectGpuSpatialPass::PolarCoordinates { .. } => Ok(1),
            EffectGpuSpatialPass::Flip { .. } => Ok(1),
            EffectGpuSpatialPass::Ripple { .. } => Ok(1),
            EffectGpuSpatialPass::WaveWarp { .. } => Ok(1),
            EffectGpuSpatialPass::Transform { .. } => Ok(1),
            EffectGpuSpatialPass::CornerPin { .. } => Ok(1),
            EffectGpuSpatialPass::TurbulentDisplace { .. } => Ok(1),
            EffectGpuSpatialPass::FractalNoise { .. } => Ok(1),
            // Two blur passes feed one composite pass.
            EffectGpuSpatialPass::Deband { .. } => Ok(3),
            // One extract, one blur pass per axis and iteration, one composite.
            EffectGpuSpatialPass::Glow { grade } => {
                Ok(2 + grade.dimensions().axis_count() * mondrian_effects::GLOW_BLUR_ITERATIONS)
            }
            // One key pass, two passes per enabled morphology stage, one
            // composite. The preblur, the shrink, and the feather are all
            // optional, so the count follows the authored grade.
            EffectGpuSpatialPass::Keylight { grade } => Ok(1
                + u32::from(grade.preblur() > 0.0) * 2
                + u32::from(grade.edge_shrink() > 0.0) * 2
                + u32::from(grade.edge_feather() > 0.0) * 2
                + 1),
            // Matte refine cleans the source, smooths and shifts the matte,
            // feathers it, and composites. Every stage is optional and the two
            // views fold into the composite, so the count follows the grade.
            EffectGpuSpatialPass::MatteRefine { grade } => Ok(grade.pass_count()),
            // The choker runs one horizontal and one vertical morphology pass;
            // the composite reuses the texture the horizontal pass wrote.
            EffectGpuSpatialPass::Choker { .. } => Ok(2),
            // Two erosion passes, the horizontal half of the softening box, and
            // the composite. The softening pass is optional, but declaring four
            // would cost every frame the third plane even when it is unused, so
            // the count stays at three and `record_key_cleaner` refuses a pool
            // that cannot hold them. That makes a DCI 4K frame
            // (4096x2160x16 bytes x 4 textures = 566,231,040 bytes) exceed the
            // 512 MiB spatial scratch budget; UHD (530,841,600 bytes) still fits.
            EffectGpuSpatialPass::KeyCleaner { .. } => Ok(3),
            // One draw per axis: the horizontal pass writes the first plane and
            // the vertical pass finishes it into the second. Two textures is the
            // smallest pool the record method accepts, so a DCI 4K frame
            // (4096x2160x16 bytes x 3 textures = 424,673,280 bytes) fits the
            // 512 MiB spatial scratch budget while a three-pass declaration
            // would not.
            EffectGpuSpatialPass::SmartBlur { .. } => Ok(2),
            EffectGpuSpatialPass::BilateralBlur { .. } => Ok(1),
            // The chain is one pass per operation step and axis: 1, 2 or 4.
            // `record_minimax` executes exactly the same table.
            EffectGpuSpatialPass::Minimax { grade } => Ok(grade.pass_count()),
            EffectGpuSpatialPass::ChannelBlur { grade } => {
                Ok(u32::from(grade.direction().blurs_horizontal())
                    + u32::from(grade.direction().blurs_vertical()))
            }
            // The median is one gather pass. The radius gate lives here, on the
            // admission path, so a too-wide window is rejected while the chain is
            // still being planned instead of failing mid-recording.
            EffectGpuSpatialPass::Median { grade } => {
                if grade.gpu_supported() {
                    Ok(1)
                } else {
                    Err(EffectSpatialGpuError::UnsupportedPass("median_radius"))
                }
            }
            // The repair is one gather pass as well, and the same admission
            // gate: a window wider than the GPU cap is refused while the chain
            // is still being planned.
            EffectGpuSpatialPass::DustScratches { grade } => {
                if grade.gpu_supported() {
                    Ok(1)
                } else {
                    Err(EffectSpatialGpuError::UnsupportedPass(
                        "dust_scratches_radius",
                    ))
                }
            }
            // The dedicated pass writes the whole frame in one full-screen
            // draw, so the scratch pool needs the source and the destination.
            EffectGpuSpatialPass::SelectiveColor { .. } => Ok(1),
            // One draw resolves the whole band; only the source and the
            // destination are needed.
            EffectGpuSpatialPass::Stroke { .. } => Ok(1),
            // The bezier warp inverts the patch per fragment in a single draw.
            EffectGpuSpatialPass::BezierWarp { .. } => Ok(1),
            EffectGpuSpatialPass::LensDistortion { .. } => Ok(1),
            EffectGpuSpatialPass::ChromaticAberrationRemoval { .. } => Ok(1),
            EffectGpuSpatialPass::Basic3D { .. } => Ok(1),
            EffectGpuSpatialPass::CameraView { .. } => Ok(1),
            EffectGpuSpatialPass::GrowBounds { .. } => Ok(1),
            EffectGpuSpatialPass::BlankingFill { grade } => {
                Ok(if grade.warps_top_layer() { 1 } else { 2 })
            }
            EffectGpuSpatialPass::LensFlare { .. } => Ok(1),
            EffectGpuSpatialPass::Bulge { .. } => Ok(1),
            EffectGpuSpatialPass::Split { .. } => Ok(1),
            EffectGpuSpatialPass::LinearColorKey { .. } => Ok(1),
            EffectGpuSpatialPass::ColorOffset { .. } => Ok(1),
            EffectGpuSpatialPass::FillMatte { .. }
            | EffectGpuSpatialPass::ChannelCombinerLayer { .. }
            | EffectGpuSpatialPass::DisplacementMap { .. } => Ok(1),
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
        // Single-pass kernels write one texture and need no scratch pool.
        match pass {
            EffectGpuSpatialPass::BilateralBlur { grade } => {
                let output = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                self.record_bilateral(device, encoder, source, output, *grade)?;
                return Ok(output.clone());
            }
            EffectGpuSpatialPass::FillMatte { .. }
            | EffectGpuSpatialPass::ChannelCombinerLayer { .. }
            | EffectGpuSpatialPass::DisplacementMap { .. } => {
                return Err(EffectSpatialGpuError::MissingAuxiliaryInput);
            }
            EffectGpuSpatialPass::FindEdges { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_find_edges(
                    device,
                    encoder,
                    grade.blend_with_original(),
                    grade.invert(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Emboss { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_emboss(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    grade.luminance_coefficients(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::ColorEmboss { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_color_emboss(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::RadialBlur { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_radial_blur(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Offset { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_offset(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Twirl { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_twirl(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Spherize { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_spherize(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::VrRotateSphere { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, aces, _) = grade.gpu_controls();
                let (eye_width, eye_height) = grade.eye_size(width as usize, height as usize);
                return self.record_vr_rotate_sphere(
                    device,
                    encoder,
                    params,
                    extra,
                    aces,
                    [0.0, 0.0, eye_width as f32, eye_height as f32],
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::VrSphereToPlane { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, aces, _) =
                    grade.gpu_controls_for_frame(width as usize, height as usize);
                return self.record_vr_sphere_to_plane(
                    device,
                    encoder,
                    params,
                    extra,
                    aces,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::VrProject2D { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, aces, extra1) = grade.gpu_controls();
                return self.record_vr_project_2d(
                    device,
                    encoder,
                    params,
                    extra,
                    aces,
                    extra1,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Magnify { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, aces, _) = grade.gpu_controls();
                return self.record_magnify(
                    device,
                    encoder,
                    params,
                    extra,
                    aces,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Dent { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_dent(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Warp { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_warp(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::GridWarp { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::UnsupportedPass(
                    "grid_warp_scratch_geometry",
                ))?;
                let (params, extra) = grade.gpu_controls(width as usize, height as usize);
                return self.record_grid_warp(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::WireRemoval { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_wire_removal(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::RoughenEdges { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, extra1) = grade.gpu_controls();
                return self.record_roughen_edges(
                    device,
                    encoder,
                    params,
                    extra,
                    grade.aces_controls(),
                    extra1,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Convolution { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, aces) = grade.gpu_controls();
                return self.record_convolution(
                    device,
                    encoder,
                    params,
                    extra,
                    aces,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Mirror { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_mirror(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::PolarCoordinates { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_polar_coordinates(
                    device,
                    encoder,
                    grade.gpu_controls(),
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Bulge { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_bulge(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::LinearColorKey { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_linear_color_key(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::ColorOffset { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_color_offset(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Split { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_split(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Flip { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_flip(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Ripple { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_ripple(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::WaveWarp { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_wave_warp(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Transform { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_transform(
                    device,
                    encoder,
                    grade,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::CornerPin { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_corner_pin(
                    device,
                    encoder,
                    grade,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::TurbulentDisplace { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra, aces) = grade.gpu_controls();
                return self.record_turbulent_displace(
                    device,
                    encoder,
                    params,
                    extra,
                    aces,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::LensDistortion { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_lens_distortion(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::LensFlare { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let (params, extra) = grade.gpu_controls();
                return self.record_lens_flare(
                    device,
                    encoder,
                    params,
                    extra,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::ChromaticAberrationRemoval { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return Ok(self.record_chromatic_aberration_removal(
                    device,
                    encoder,
                    *grade,
                    source,
                    destination,
                ));
            }
            EffectGpuSpatialPass::Basic3D { grade } => {
                let Some([params, extra, aces, extra1]) =
                    grade.gpu_controls(source.width(), source.height())
                else {
                    return Ok(source.clone());
                };
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let uniforms = SpatialPassUniform {
                    size: [source.width() as f32, source.height() as f32, 0.0, 0.0],
                    params,
                    extra,
                    aces,
                    extra1,
                };
                let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
                let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
                self.record_pass(
                    device,
                    encoder,
                    &self.basic_3d,
                    &input_view,
                    &output_view,
                    &uniforms,
                );
                return Ok(destination.clone());
            }
            EffectGpuSpatialPass::CameraView { grade } => {
                let Some([params, extra, aces, extra1]) =
                    grade.gpu_controls(source.width(), source.height())
                else {
                    return Ok(source.clone());
                };
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let uniforms = SpatialPassUniform {
                    size: [source.width() as f32, source.height() as f32, 0.0, 0.0],
                    params,
                    extra,
                    aces,
                    extra1,
                };
                let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
                let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
                self.record_pass(
                    device,
                    encoder,
                    &self.camera_view,
                    &input_view,
                    &output_view,
                    &uniforms,
                );
                return Ok(destination.clone());
            }
            EffectGpuSpatialPass::BlankingFill { grade } => {
                return self.record_blanking_fill(device, encoder, *grade, source, scratch);
            }
            EffectGpuSpatialPass::GrowBounds { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let input_extent =
                    mondrian_effects::EffectFrameExtent::new(source.width(), source.height());
                let extent = mondrian_effects::EffectRasterBounds::from_extent(input_extent)
                    .checked_outset(grade.output_outset())
                    .map(mondrian_effects::EffectRasterBounds::extent);
                if extent != Some(mondrian_effects::EffectFrameExtent::new(width, height))
                    || (destination.width(), destination.height()) != (width, height)
                {
                    return Err(EffectSpatialGpuError::UnsupportedPass(
                        "grow_bounds_output_geometry",
                    ));
                }
                let uniforms = SpatialPassUniform {
                    size: [source.width() as f32, source.height() as f32, 0.0, 0.0],
                    params: grade.gpu_controls(),
                    extra: [0.0; 4],
                    aces: [0.0; 4],
                    extra1: [0.0; 4],
                };
                let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
                let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
                self.record_pass(
                    device,
                    encoder,
                    &self.grow_bounds,
                    &input_view,
                    &output_view,
                    &uniforms,
                );
                return Ok(destination.clone());
            }
            EffectGpuSpatialPass::Deband { grade } => {
                return self.record_deband(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::Glow { grade } => {
                return self.record_glow(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::FractalNoise { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                let controls = grade.gpu_controls(width, height);
                let uniform = FractalNoiseUniform {
                    size: controls.size,
                    params: controls.params,
                    extra0: controls.extra0,
                    extra1: controls.extra1,
                    extra2: controls.extra2,
                    extra3: controls.extra3,
                    extra4: controls.extra4,
                    extra5: controls.extra5,
                };
                return self.record_fractal_noise(
                    device,
                    encoder,
                    &uniform,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::SelectiveColor { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_selective_color(
                    device,
                    encoder,
                    *grade,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Stroke { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_stroke(
                    device,
                    encoder,
                    *grade,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::BezierWarp { grade } => {
                let destination = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
                return self.record_bezier_warp(
                    device,
                    encoder,
                    *grade,
                    source,
                    destination,
                    width,
                    height,
                );
            }
            EffectGpuSpatialPass::Keylight { grade } => {
                return self
                    .record_keylight(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::MatteRefine { grade } => {
                return self
                    .record_matte_refine(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::Choker { grade } => {
                return self.record_choker(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::Minimax { grade } => {
                return self
                    .record_minimax(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::ChannelBlur { grade } => {
                return self
                    .record_channel_blur(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::Median { grade } => {
                return self.record_median(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::DustScratches { grade } => {
                return self.record_dust_scratches(
                    device, encoder, *grade, source, scratch, width, height,
                );
            }
            EffectGpuSpatialPass::KeyCleaner { grade } => {
                return self
                    .record_key_cleaner(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::SmartBlur { grade } => {
                return self
                    .record_smart_blur(device, encoder, *grade, source, scratch, width, height);
            }
            EffectGpuSpatialPass::BoxBlur { .. } => {}
            EffectGpuSpatialPass::DirectionalBlur { .. } => {
                return Err(EffectSpatialGpuError::UnsupportedPass(pass.name()));
            }
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
                    extra1: [0.0; 4],
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
                self.record_pass(
                    device,
                    encoder,
                    &self.box_blur,
                    input_view,
                    &output_view,
                    &uniform,
                );
                written += 1;
            }
        }
        Ok(scratch[written - 1].clone())
    }

    /// Records the find edges pass: one fullscreen pass over the frame.
    pub(crate) fn record_find_edges(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        blend_with_original: f32,
        invert: bool,
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: [
                blend_with_original.clamp(0.0, 1.0),
                f32::from(u8::from(invert)),
                0.0,
                0.0,
            ],
            extra: [1.0, 1.0, 0.0, 1.0],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.find_edges,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    pub(crate) fn record_emboss(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        controls: [f32; 4],
        luminance_coefficients: [f32; 3],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: controls,
            extra: [
                luminance_coefficients[0],
                luminance_coefficients[1],
                luminance_coefficients[2],
                0.0,
            ],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.emboss,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    pub(crate) fn record_color_emboss(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        controls: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: controls,
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.color_emboss,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    /// Records the radial blur pass.
    ///
    /// `params` carries the centre, the tap count and the mode tag (`1` spin,
    /// `2` zoom); `extra` carries the authored amount and the jitter seed. The
    /// tap positions and the jitter hash are byte-for-byte the CPU kernel's, so
    /// the two paths sample the same points.
    /// Records the sub-pixel translation.
    ///
    /// `params` carries the shift and the blend with the original. Sampling
    /// wraps around the frame exactly like the CPU kernel, and the blend works
    /// on premultiplied colour so transparent neighbours cannot tint the
    /// result.
    /// Records the spherize distortion.
    ///
    /// `params` carries the centre and the sphere radius in pixels. The
    /// inverse mapping uses the same `asin` falloff as the CPU kernel and
    /// returns pixels at or beyond the rim untouched.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_spherize(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.spherize,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the virtual-reality sphere rotation.
    ///
    /// `params` carries the horizontal and vertical fields of view in radians
    /// followed by the pitch and yaw, `extra` the roll, the direction switch,
    /// the frame layout code and the wrap flag, and `extra1` the eye sub-frame
    /// size. The eye size is divided with integer arithmetic on the CPU so both
    /// eyes split on exactly the same boundary as the kernel; the shader only
    /// compares against it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_vr_rotate_sphere(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        extra1: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1,
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.vr_rotate_sphere,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the equirectangular sphere to perspective plane projection.
    ///
    /// `params` carries the horizontal field of view in radians, its tangent,
    /// and the pitch and yaw in radians; `extra` carries the roll, the frame
    /// aspect ratio and the direction switch. The shader receives the whole
    /// frame: there is always a lens, so the tap is never a straight copy.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_vr_sphere_to_plane(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.vr_sphere_to_plane,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the perspective card to panorama sphere projection.
    ///
    /// `params` carries `[h_fov_rad, v_fov_rad, tan(scale / 2),
    /// tan(stereo_disparity)]`, `extra` the source card pose and the feather
    /// fraction, `extra1` the projection pose and the frame layout code. The
    /// shader receives the whole frame and derives the eye size with the same
    /// integer division as [`VrFrameLayout::eye_size`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_vr_project_2d(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        extra1: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1,
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.vr_project_2d,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the lens magnification.
    ///
    /// `params` carries `[centre_x, centre_y, radius, magnification]`, `extra`
    /// carries `[feather, opacity, shape_flag, supersample_factor]` and `aces.x`
    /// carries the engine blend-mode index. The shader accumulates the
    /// supersampled footprint in premultiplied space, un-premultiplies once and
    /// composites in the same order as `blend_rgba_f32_pixel`, so the pass
    /// matches the CPU kernel.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_magnify(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.magnify,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the circular dent distortion.
    ///
    /// `params` carries `[centre_x, centre_y, size_fraction, strength]` and
    /// `extra` carries `[kind, edge, 0, 0]`. The inverse mapping uses the same
    /// `(1 - u^2)^2` falloff as the CPU kernel and returns pixels at or beyond
    /// the rim untouched.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_dent(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.dent,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the arc warp.
    ///
    /// `params` carries `[bend, horizontal_distortion, vertical_distortion,
    /// style]` and `extra` carries `[axis, 0, 0, 0]`. The inverse mapping
    /// mirrors the CPU kernel step for step, so the two produce the same
    /// sample position for the same authored controls.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_warp(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.warp,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the checkerboard grid warp.
    ///
    /// `params` carries `[cell_width, cell_height, blend, amplitude_ratio]`
    /// and `extra` carries `[cells_x, cells_y, 0, 0]`. The shader repeats the
    /// CPU mapping expression for expression, so both produce the same sample
    /// position for the same authored controls.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_grid_warp(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.grid_warp,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records wire removal: clones pixels from a parallel displacement across
    /// the authored line.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_wire_removal(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.wire_removal,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records roughen edges: grows the silhouette along its own alpha gradient
    /// and styles the new rim.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_roughen_edges(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        extra1: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1,
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.roughen_edges,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the polar coordinate conversion.
    ///
    /// `params` carries `[conversion, interpolation, 0, 0]`: one converts
    /// rectangular to polar coordinates, two the inverse, and an interpolation
    /// below one half selects the nearest texel instead of a bilinear mix.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_polar_coordinates(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.polar_coordinates,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the mirror reflection.    ///
    /// `params` carries the axis centre and the axis angle in degrees. The
    /// normal of the axis is derived with the same arithmetic as the CPU
    /// kernel: pixels with a non-negative signed distance keep their own
    /// colour, and the opposite half is filled with the bilinear reflection.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_mirror(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.mirror,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    /// Records the twirl distortion.
    ///
    /// `params` carries the centre, the radius as a fraction of the shorter
    /// frame side and the authored angle in degrees. The falloff and the
    /// rotation are evaluated with the same arithmetic as the CPU kernel, and
    /// pixels outside the radius are returned untouched instead of being
    /// resampled.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_twirl(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.twirl,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_offset(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.offset,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_radial_blur(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.radial_blur,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }

    /// Records the general 3×3 convolution.
    ///
    /// `params` and `extra` carry the first eight kernel cells, `aces` carries
    /// the ninth cell, the absolute flag and the blend. The pass samples the
    /// same clamped neighbourhood as the CPU kernel, filters premultiplied
    /// colour, and returns to straight alpha.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_convolution(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        params: [f32; 4],
        extra: [f32; 4],
        aces: [f32; 4],
        source: &wgpu::Texture,
        destination: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if width == 0 || height == 0 {
            return Err(EffectSpatialGpuError::EmptyFrame);
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = destination.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.convolution,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(destination.clone())
    }
    fn record_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        pipeline: &wgpu::RenderPipeline,
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    /// Records one fractal noise pass with the stage's own bind group layout.
    fn record_fractal_noise_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &FractalNoiseUniform,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.effect-spatial.fractal-noise-uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.effect-spatial.fractal-noise-bind-group"),
            layout: &self.fractal_noise_bindings,
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
            label: Some("mondrian.effect-spatial.fractal-noise-pass"),
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
        pass.set_pipeline(&self.fractal_noise);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    fn record_selective_color_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &SelectiveColorUniform,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.effect-spatial.selective-color-uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.effect-spatial.selective-color-bind-group"),
            layout: &self.selective_color_bindings,
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
            label: Some("mondrian.effect-spatial.selective-color-pass"),
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
        pass.set_pipeline(&self.selective_color);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    fn record_stroke_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &StrokeUniform,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.effect-spatial.stroke-uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.effect-spatial.stroke-bind-group"),
            layout: &self.stroke_bindings,
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
            label: Some("mondrian.effect-spatial.stroke-pass"),
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
        pass.set_pipeline(&self.stroke);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    fn record_bezier_warp_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        input: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &BezierWarpUniform,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.effect-spatial.bezier-warp-uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.effect-spatial.bezier-warp-bind-group"),
            layout: &self.bezier_warp_bindings,
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
            label: Some("mondrian.effect-spatial.bezier-warp-pass"),
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
        pass.set_pipeline(&self.bezier_warp);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..4, 0..1);
    }
    /// Records the three-pass deband chain.
    ///
    /// The horizontal and vertical box blur passes reuse [`Self::box_blur`] and
    /// the exact uniform packing of the standalone box blur, except that the
    /// last pass deliberately does **not** unpremultiply: the merge needs the
    /// blurred copy in the same premultiplied plane as the original. The
    /// composite pass then writes `scratch[0]`, the texture the framework hands
    /// back, while `scratch[1]` and `scratch[2]` hold the blur intermediates.
    fn record_deband(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::DebandGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.len() < 3 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "deband_scratch_geometry",
            ));
        }
        let box_kernel = BoxKernel::new(grade.radius());
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let horizontal_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());
        let vertical_view = scratch[2].create_view(&wgpu::TextureViewDescriptor::default());
        let horizontal = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: [
                0.0,
                0.0,
                box_kernel.whole_radius as f32,
                box_kernel.fraction,
            ],
            extra: [
                box_kernel.normalization(),
                // Repeat the edge pixels, premultiply on the first pass, and never
                // unpremultiply: the auxiliary texture stays premultiplied.
                1.0,
                1.0,
                0.0,
            ],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        self.record_pass(
            device,
            encoder,
            &self.box_blur,
            &source_view,
            &horizontal_view,
            &horizontal,
        );
        let vertical = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: [
                0.0,
                1.0,
                box_kernel.whole_radius as f32,
                box_kernel.fraction,
            ],
            extra: [box_kernel.normalization(), 1.0, 0.0, 0.0],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        self.record_pass(
            device,
            encoder,
            &self.box_blur,
            &horizontal_view,
            &vertical_view,
            &vertical,
        );
        let (params, extra) = grade.gpu_controls();
        let composite = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let destination_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.deband,
            &source_view,
            &vertical_view,
            &destination_view,
            &composite,
        );
        Ok(scratch[0].clone())
    }

    /// Records the glow extract, blur, and composite stages.
    ///
    /// `scratch[0]` receives the merged frame, `scratch[1]` holds the extracted
    /// glow layer, and the blur passes ping-pong between `scratch[1]` and
    /// `scratch[2]`. The blur passes reuse [`Self::box_blur`] and the exact
    /// uniform packing of the standalone box blur: the first pass premultiplies
    /// the straight-alpha extraction and the last pass unpremultiplies it again,
    /// so the merge reads straight-alpha glow.
    fn record_glow(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::GlowGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.len() < 3 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "glow_scratch_geometry",
            ));
        }
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let (params, extra, aces, extra1) = grade.extract_gpu_controls();
        let extract = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1,
        };
        let glow_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.glow_extract,
            &source_view,
            &glow_view,
            &extract,
        );

        let box_kernel = BoxKernel::new(grade.radius());
        let dimensions = grade.dimensions();
        let mut axes = Vec::with_capacity(2);
        if dimensions.blurs_horizontal() {
            axes.push(0_u32);
        }
        if dimensions.blurs_vertical() {
            axes.push(1_u32);
        }
        let iterations = mondrian_effects::GLOW_BLUR_ITERATIONS;
        let total_passes = axes.len() * iterations as usize;
        // The extract wrote `scratch[1]`, and every blur pass writes the buffer
        // the previous pass did not read.
        let mut written = 1_usize;
        let mut pass_index = 0_usize;
        for _ in 0..iterations {
            for axis in &axes {
                let output = if written == 1 { 2 } else { 1 };
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
                        // Repeat the edge pixels; premultiply the straight-alpha
                        // extraction on the first pass and unpremultiply it on the
                        // last, so the merge reads straight-alpha glow.
                        1.0,
                        f32::from(u8::from(pass_index == 0)),
                        f32::from(u8::from(pass_index + 1 == total_passes)),
                    ],
                    aces: [0.0; 4],
                    extra1: [0.0; 4],
                };
                let input_view =
                    scratch[written].create_view(&wgpu::TextureViewDescriptor::default());
                let output_view =
                    scratch[output].create_view(&wgpu::TextureViewDescriptor::default());
                self.record_pass(
                    device,
                    encoder,
                    &self.box_blur,
                    &input_view,
                    &output_view,
                    &uniform,
                );
                written = output;
                pass_index += 1;
            }
        }

        let (params, extra, aces, extra1) = grade.composite_gpu_controls();
        let composite = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params,
            extra,
            aces,
            extra1,
        };
        let glow_view = scratch[written].create_view(&wgpu::TextureViewDescriptor::default());
        let destination_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.glow_composite,
            &source_view,
            &glow_view,
            &destination_view,
            &composite,
        );
        Ok(scratch[0].clone())
    }

    /// Records one fullscreen pass that reads the original and one auxiliary
    /// texture and writes the destination.
    /// Records the keylight stages and returns the merged frame.
    ///
    /// `scratch[0]` receives the merged frame, `scratch[1]` and `scratch[2]`
    /// carry the preblur and the matte ping-pong, and `scratch[3]` holds the
    /// keyed matte. The preblur and the feather reuse [`Self::box_blur`] with the
    /// exact uniform packing of the standalone box blur; the shrink reuses
    /// [`Self::keylight_erode`], which upsamples nothing and only reads `.x`.
    fn record_keylight(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::KeylightGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.len() < 4 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "keylight_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());

        // The preblur softens the source before the key reads it, so the matte
        // follows the blurred separation instead of the sharp one.
        let mut key_input = source;
        if grade.preblur() > 0.0 {
            let box_kernel = BoxKernel::new(grade.preblur());
            for pass_index in 0..2_usize {
                let axis = pass_index as f32;
                let uniform = SpatialPassUniform {
                    size,
                    params: [
                        0.0,
                        axis,
                        box_kernel.whole_radius as f32,
                        box_kernel.fraction,
                    ],
                    extra: [
                        box_kernel.normalization(),
                        1.0,
                        f32::from(u8::from(pass_index == 0)),
                        f32::from(u8::from(pass_index == 1)),
                    ],
                    aces: [0.0; 4],
                    extra1: [0.0; 4],
                };
                let input_view = if pass_index == 0 {
                    source_view.clone()
                } else {
                    scratch[2].create_view(&wgpu::TextureViewDescriptor::default())
                };
                let output_view = if pass_index == 0 {
                    scratch[2].create_view(&wgpu::TextureViewDescriptor::default())
                } else {
                    scratch[1].create_view(&wgpu::TextureViewDescriptor::default())
                };
                self.record_pass(
                    device,
                    encoder,
                    &self.box_blur,
                    &input_view,
                    &output_view,
                    &uniform,
                );
            }
            key_input = &scratch[1];
        }

        // The key pass writes the matte into every channel of `scratch[3]`.
        let (params, extra, aces, extra1) = grade.key_gpu_controls();
        let key = SpatialPassUniform { size, params, extra, aces, extra1 };
        let key_input_view = key_input.create_view(&wgpu::TextureViewDescriptor::default());
        let matte_view = scratch[3].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.keylight_key,
            &key_input_view,
            &matte_view,
            &key,
        );

        // The morphology ping-pongs between `scratch[3]` and `scratch[2]` and
        // always ends on `scratch[3]`.
        let written = 3_usize;
        if grade.shrink_radius() > 0 {
            let uniform = SpatialPassUniform {
                size,
                params: [grade.edge_shrink().round(), 0.0, 0.0, 0.0],
                extra: [0.0; 4],
                aces: [0.0; 4],
                extra1: [0.0; 4],
            };
            let destination = 2_usize;
            let input_view = scratch[written].create_view(&wgpu::TextureViewDescriptor::default());
            let output_view =
                scratch[destination].create_view(&wgpu::TextureViewDescriptor::default());
            self.record_pass(
                device,
                encoder,
                &self.keylight_erode,
                &input_view,
                &output_view,
                &uniform,
            );
            let uniform = SpatialPassUniform {
                size,
                params: [grade.edge_shrink().round(), 1.0, 0.0, 0.0],
                extra: [0.0; 4],
                aces: [0.0; 4],
                extra1: [0.0; 4],
            };
            let input_view =
                scratch[destination].create_view(&wgpu::TextureViewDescriptor::default());
            let output_view = scratch[written].create_view(&wgpu::TextureViewDescriptor::default());
            self.record_pass(
                device,
                encoder,
                &self.keylight_erode,
                &input_view,
                &output_view,
                &uniform,
            );
        }
        if grade.edge_feather() > 0.0 {
            let box_kernel = BoxKernel::new(grade.edge_feather());
            let destination = 2_usize;
            for pass_index in 0..2_usize {
                let uniform = SpatialPassUniform {
                    size,
                    params: [
                        0.0,
                        pass_index as f32,
                        box_kernel.whole_radius as f32,
                        box_kernel.fraction,
                    ],
                    extra: [
                        box_kernel.normalization(),
                        1.0,
                        f32::from(u8::from(pass_index == 0)),
                        f32::from(u8::from(pass_index == 1)),
                    ],
                    aces: [0.0; 4],
                    extra1: [0.0; 4],
                };
                let input_index = if pass_index == 0 {
                    written
                } else {
                    destination
                };
                let output_index = if pass_index == 0 {
                    destination
                } else {
                    written
                };
                let input_view =
                    scratch[input_index].create_view(&wgpu::TextureViewDescriptor::default());
                let output_view =
                    scratch[output_index].create_view(&wgpu::TextureViewDescriptor::default());
                self.record_pass(
                    device,
                    encoder,
                    &self.box_blur,
                    &input_view,
                    &output_view,
                    &uniform,
                );
            }
        }

        let (params, extra, aces, extra1) = grade.composite_gpu_controls();
        let composite = SpatialPassUniform { size, params, extra, aces, extra1 };
        let matte_view = scratch[written].create_view(&wgpu::TextureViewDescriptor::default());
        let destination_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.keylight_composite,
            &source_view,
            &matte_view,
            &destination_view,
            &composite,
        );
        Ok(scratch[0].clone())
    }

    /// Records the matte refine stages and returns the merged frame.
    ///
    /// `scratch[0]` receives the merged frame, `scratch[1]` the optionally
    /// cleaned source, and `scratch[2]`/`scratch[3]` ping-pong the matte. Every
    /// matte stage is two passes, so the matte always lands back on
    /// `scratch[2]`; when no stage runs the composite reads the frame alpha.
    fn record_matte_refine(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::MatteRefineGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.len() < 4 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "matte_refine_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());

        // The cleanup replaces the colour fringe of the soft pixels and keeps
        // the alpha, so every later matte pass can still read the frame alpha.
        if grade.cleans_edge_color() {
            let uniform = SpatialPassUniform {
                size,
                params: grade.cleanup_gpu_controls(),
                extra: [0.0; 4],
                aces: [0.0; 4],
                extra1: [0.0; 4],
            };
            let output_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());
            self.record_pass(
                device,
                encoder,
                &self.matte_refine_cleanup,
                &source_view,
                &output_view,
                &uniform,
            );
        }
        let colour_view = if grade.cleans_edge_color() {
            scratch[1].create_view(&wgpu::TextureViewDescriptor::default())
        } else {
            source_view.clone()
        };

        let mut matte = None;
        let mut written = 3_usize;
        let iterations = grade.blur_iterations();
        let no_extra = [0.0_f32; 4];
        if grade.smooths() && grade.extend_smooth() {
            for _ in 0..iterations {
                for axis in 0..2_u32 {
                    self.record_matte_pass(
                        device,
                        encoder,
                        &self.matte_refine_smooth,
                        source,
                        scratch,
                        &mut matte,
                        &mut written,
                        size,
                        grade.smooth_gpu_controls(axis as f32, false),
                        no_extra,
                        no_extra,
                        MatteReadFlag::Params,
                    );
                }
            }
        }
        if grade.grows() {
            for axis in 0..2_u32 {
                self.record_matte_pass(
                    device,
                    encoder,
                    &self.keylight_erode,
                    source,
                    scratch,
                    &mut matte,
                    &mut written,
                    size,
                    grade.morphology_gpu_controls(grade.grow_radius(), axis as f32, true, false),
                    no_extra,
                    no_extra,
                    MatteReadFlag::Params,
                );
            }
        }
        if grade.shrinks() {
            for axis in 0..2_u32 {
                self.record_matte_pass(
                    device,
                    encoder,
                    &self.keylight_erode,
                    source,
                    scratch,
                    &mut matte,
                    &mut written,
                    size,
                    grade.morphology_gpu_controls(grade.shrink_radius(), axis as f32, false, false),
                    no_extra,
                    no_extra,
                    MatteReadFlag::Params,
                );
            }
        }
        if grade.smooths() && !grade.extend_smooth() {
            for _ in 0..iterations {
                for axis in 0..2_u32 {
                    self.record_matte_pass(
                        device,
                        encoder,
                        &self.matte_refine_smooth,
                        source,
                        scratch,
                        &mut matte,
                        &mut written,
                        size,
                        grade.smooth_gpu_controls(axis as f32, false),
                        no_extra,
                        no_extra,
                        MatteReadFlag::Params,
                    );
                }
            }
        }
        if grade.feathers() {
            let box_kernel = BoxKernel::new(grade.feather());
            for _ in 0..iterations {
                for axis in 0..2_u32 {
                    let (params, extra, extra1) = grade.feather_gpu_controls(
                        axis as f32,
                        box_kernel.whole_radius,
                        box_kernel.fraction,
                        box_kernel.normalization(),
                        false,
                    );
                    self.record_matte_pass(
                        device,
                        encoder,
                        &self.matte_refine_feather,
                        source,
                        scratch,
                        &mut matte,
                        &mut written,
                        size,
                        params,
                        extra,
                        extra1,
                        MatteReadFlag::Extra1,
                    );
                }
            }
        }

        let read_frame = matte.is_none();
        let (params, extra1) = grade.composite_gpu_controls(read_frame);
        let composite = SpatialPassUniform {
            size,
            params,
            extra: no_extra,
            aces: [0.0; 4],
            extra1,
        };
        let auxiliary_view = match matte {
            Some(index) => scratch[index].create_view(&wgpu::TextureViewDescriptor::default()),
            None => source_view.clone(),
        };
        let destination_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.matte_refine_composite,
            &colour_view,
            &auxiliary_view,
            &destination_view,
            &composite,
        );
        Ok(scratch[0].clone())
    }

    /// Records the choker morphology chain.
    ///
    /// `scratch[1]` receives the horizontal min/max of the frame alpha,
    /// `scratch[0]` the vertical pass over that intermediate, which is the matte
    /// plane the composite reads. The composite keeps the source colour and only
    /// replaces the alpha, so it writes back into `scratch[1]`.
    fn record_choker(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::ChokerGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.len() < 2 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "choker_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        // `[radius, dilate, 0, 0]`; the axis and the "read the frame alpha" flag
        // are per pass, because only the horizontal pass starts from the frame.
        let (stage, extra) = grade.gpu_controls();
        let no_extra = [0.0_f32; 4];
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());

        let horizontal = SpatialPassUniform {
            size,
            params: [stage[0], 0.0, stage[1], 1.0],
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let horizontal_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.keylight_erode,
            &source_view,
            &horizontal_view,
            &horizontal,
        );

        let vertical = SpatialPassUniform {
            size,
            params: [stage[0], 1.0, stage[1], 0.0],
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let matte_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.keylight_erode,
            &horizontal_view,
            &matte_view,
            &vertical,
        );

        let composite = SpatialPassUniform {
            size,
            params: no_extra,
            extra: no_extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let auxiliary_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        let destination_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.choker_composite,
            &source_view,
            &auxiliary_view,
            &destination_view,
            &composite,
        );
        Ok(scratch[1].clone())
    }

    /// Records the key cleaner morphology chain.
    ///
    /// `scratch[0]` receives the horizontal minimum, `scratch[1]` the vertical
    /// one. When the chatter softening is on, the horizontal half of the box
    /// average writes `scratch[2]`; the composite then runs the vertical half,
    /// the contrast curve and the strength blend, reading whichever matte plane
    /// the chain produced. It writes into `scratch[0]` and keeps the source
    /// colour, because the effect never reads it.
    ///
    /// Both erosion passes always run, even at radius zero: the one-tap window
    /// is then just the shader's clamp to `<= 1`, which is what keeps an
    /// out-of-range authored alpha identical on the CPU and GPU paths.
    fn record_key_cleaner(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::KeyCleanerGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        // Two matte planes are not enough: the horizontal box would have to
        // overwrite a plane the composite still reads.
        if scratch.len() < 3 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "key_cleaner_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        // `[radius, 0, 0, 0]` and `[chatter_pixels, contrast, strength, 0]`.
        let (stage, control) = grade.gpu_controls();
        let no_extra = [0.0_f32; 4];
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let horizontal_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        let vertical_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());

        let horizontal = SpatialPassUniform {
            size,
            params: [stage[0], 0.0, 0.0, 1.0],
            extra: no_extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        self.record_pass(
            device,
            encoder,
            &self.keylight_erode,
            &source_view,
            &horizontal_view,
            &horizontal,
        );

        let vertical = SpatialPassUniform {
            size,
            params: [stage[0], 1.0, 0.0, 0.0],
            extra: no_extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        self.record_pass(
            device,
            encoder,
            &self.keylight_erode,
            &horizontal_view,
            &vertical_view,
            &vertical,
        );

        let blur_pixels = control[0];
        let softened_view = if blur_pixels > 0.0 {
            let horizontal_box = SpatialPassUniform {
                size,
                // `[0, axis_horizontal, whole, fraction]`; the divisor is the
                // same `2F+1` the composite divides the vertical sum by.
                params: [0.0, 0.0, blur_pixels, 0.0],
                extra: [2.0 * blur_pixels + 1.0, 1.0, 0.0, 0.0],
                aces: [0.0; 4],
                extra1: [0.0; 4],
            };
            let softened_texture = scratch[2].create_view(&wgpu::TextureViewDescriptor::default());
            self.record_pass(
                device,
                encoder,
                &self.box_blur,
                &vertical_view,
                &softened_texture,
                &horizontal_box,
            );
            softened_texture
        } else {
            vertical_view
        };

        let composite = SpatialPassUniform {
            size,
            params: [blur_pixels, control[1], control[2], 0.0],
            extra: no_extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let destination_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.key_cleaner_composite,
            &source_view,
            &softened_view,
            &destination_view,
            &composite,
        );
        Ok(scratch[0].clone())
    }

    /// Records the smart blur: one separable gated mean per draw.
    ///
    /// The gate always compares against the untouched source, so binding 0 is
    /// the source on both draws while binding 1 is the pass input — the source
    /// again on the horizontal draw, the horizontal plane on the vertical one.
    /// The mode composite folds into the vertical draw, exactly like the CPU
    /// kernel, so the intermediate plane stays a plain premultiplied mean.
    pub(crate) fn record_bilateral(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        source: &wgpu::Texture,
        output: &wgpu::Texture,
        grade: mondrian_effects::BilateralBlurGrade,
    ) -> Result<(), EffectSpatialGpuError> {
        let limits = device.limits();
        let binding_limit = limits.max_storage_buffer_binding_size.min(limits.max_buffer_size);
        let plan =
            crate::bilateral_gpu::BilateralGpuPlan::new(grade, source.width(), source.height())
                .filter(|plan| {
                    plan.fits(binding_limit)
                        && plan.working_bytes
                            <= crate::spatial_driver::GPU_SPATIAL_SCRATCH_BUDGET_BYTES
                })
                .ok_or(EffectSpatialGpuError::UnsupportedPass(
                    "bilateral_blur_working_set",
                ))?;
        self.bilateral
            .get_or_init(|| crate::bilateral_gpu::BilateralGpu::new(device, output.format()))
            .record(device, encoder, source, output, grade, plan);
        Ok(())
    }

    fn record_smart_blur(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::SmartBlurGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        // One plane for the horizontal mean, one for the finished frame: the
        // source has to survive both draws because the gate reads it.
        if scratch.len() < 2 {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "smart_blur_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        let (params, _) = grade.gpu_controls();
        let source_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let horizontal_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        let destination_view = scratch[1].create_view(&wgpu::TextureViewDescriptor::default());

        // `params.w` is the axis selector: 0 averages along the row, 1 along the
        // column. The horizontal draw carries mode zero, which makes the shader
        // return the plain premultiplied mean instead of a mode composite.
        let horizontal = SpatialPassUniform {
            size,
            params: [params[0], params[1], 0.0, 0.0],
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        self.record_composite_pass(
            device,
            encoder,
            &self.smart_blur,
            &source_view,
            &source_view,
            &horizontal_view,
            &horizontal,
        );

        let vertical = SpatialPassUniform {
            size,
            params: [params[0], params[1], params[2], 1.0],
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        self.record_composite_pass(
            device,
            encoder,
            &self.smart_blur,
            &source_view,
            &horizontal_view,
            &destination_view,
            &vertical,
        );
        Ok(scratch[1].clone())
    }

    /// Records the minimax chain: one minimum or maximum step per pass.
    ///
    /// The chain comes straight from the grade's table, so it is 1, 2 or 4
    /// passes and always matches [`EffectSpatialGpu::texture_pass_count`]. Each
    /// pass reads the original texture (first pass) or the previous scratch
    /// frame, and writes into its own scratch frame; the last one is the result.
    fn record_minimax(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::MinimaxGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        let passes = grade.passes();
        if scratch.len() < passes.len() {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "minimax_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        // `dont_shrink_edges` travels in `extra`; the shader strips the alpha bit
        // from minimum passes itself, exactly like the CPU kernel does.
        let extra = [
            f32::from(u8::from(grade.dont_shrink_edges())),
            0.0,
            0.0,
            0.0,
        ];
        let mask = grade.channel().mask() as f32;
        for (index, pass) in passes.iter().enumerate() {
            let uniform = SpatialPassUniform {
                size,
                params: [
                    grade.radius() as f32,
                    pass.axis as f32,
                    pass.extreme as f32,
                    mask,
                ],
                extra,
                aces: [0.0; 4],
                extra1: [0.0; 4],
            };
            let input_view = if index == 0 {
                source.create_view(&wgpu::TextureViewDescriptor::default())
            } else {
                scratch[index - 1].create_view(&wgpu::TextureViewDescriptor::default())
            };
            let output_view = scratch[index].create_view(&wgpu::TextureViewDescriptor::default());
            self.record_pass(
                device,
                encoder,
                &self.minimax,
                &input_view,
                &output_view,
                &uniform,
            );
        }
        Ok(scratch[passes.len() - 1].clone())
    }

    /// Records the channel blur chain: one separable box-blur step per axis.
    ///
    /// The pass count is `1` for a single axis and `2` for both, and always
    /// matches [`EffectSpatialGpu::texture_pass_count`]. The horizontal pass
    /// reads the original texture and writes `scratch[0]`; the vertical pass
    /// reads `scratch[0]` and writes `scratch[1]`. Radii are rounded and clamped
    /// to the frame extent here so the shader sees the same integers as
    /// [`mondrian_effects::ChannelBlurGrade::radii`].
    fn record_channel_blur(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::ChannelBlurGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        let direction = grade.direction();
        let axes = [direction.blurs_horizontal(), direction.blurs_vertical()];
        let pass_count = axes.iter().filter(|blurs| **blurs).count();
        if pass_count == 0 {
            return Err(EffectSpatialGpuError::UnsupportedPass("channel_blur_axis"));
        }
        if scratch.len() < pass_count {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "channel_blur_scratch_geometry",
            ));
        }
        let size = [width as f32, height as f32, 0.0, 0.0];
        let radii = grade.radii(width as usize, height as usize);
        let params = [
            radii[0] as f32,
            radii[1] as f32,
            radii[2] as f32,
            radii[3] as f32,
        ];
        let repeat_edge = f32::from(u8::from(grade.repeat_edge_pixels()));
        let mut written = 0;
        for (axis, blurs) in axes.iter().enumerate() {
            if !blurs {
                continue;
            }
            let uniform = SpatialPassUniform {
                size,
                params,
                extra: [repeat_edge, axis as f32, 0.0, 0.0],
                aces: [0.0; 4],
                extra1: [0.0; 4],
            };
            let input_view = if written == 0 {
                source.create_view(&wgpu::TextureViewDescriptor::default())
            } else {
                scratch[written - 1].create_view(&wgpu::TextureViewDescriptor::default())
            };
            let output_view = scratch[written].create_view(&wgpu::TextureViewDescriptor::default());
            self.record_pass(
                device,
                encoder,
                &self.channel_blur,
                &input_view,
                &output_view,
                &uniform,
            );
            written += 1;
        }
        Ok(scratch[written - 1].clone())
    }

    /// Records the median pass: one gather that selects the window median.
    ///
    /// The radius is rounded and clamped to the frame extent here so the shader
    /// sees the same integer as [`mondrian_effects::MedianGrade::radius_pixels`].
    /// The admission path already rejected radii past
    /// [`mondrian_effects::MEDIAN_GPU_MAX_RADIUS`]; the check below only guards a
    /// direct call.
    fn record_median(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::MedianGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.is_empty() {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "median_scratch_geometry",
            ));
        }
        let radius = grade.radius_pixels(width, height);
        if radius > mondrian_effects::MEDIAN_GPU_MAX_RADIUS {
            return Err(EffectSpatialGpuError::UnsupportedPass("median_radius"));
        }
        let (params, extra) = grade.gpu_controls();
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: [radius as f32, params[1], 0.0, 0.0],
            extra,
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.median,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(scratch[0].clone())
    }

    /// Records the dust & scratches pass: one gather that gates the window
    /// median against the centre pixel.
    ///
    /// The radius is rounded and clamped to the frame extent here so the shader
    /// sees the same integer as
    /// [`mondrian_effects::DustScratchesGrade::radius_pixels`]. The admission
    /// path already rejected radii past
    /// [`mondrian_effects::DUST_SCRATCHES_GPU_MAX_RADIUS`]; the check below only
    /// guards a direct call.
    fn record_dust_scratches(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::DustScratchesGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        width: u32,
        height: u32,
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if scratch.is_empty() {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "dust_scratches_scratch_geometry",
            ));
        }
        let radius = grade.radius_pixels(width, height);
        if radius > mondrian_effects::DUST_SCRATCHES_GPU_MAX_RADIUS {
            return Err(EffectSpatialGpuError::UnsupportedPass(
                "dust_scratches_radius",
            ));
        }
        let uniform = SpatialPassUniform {
            size: [width as f32, height as f32, 0.0, 0.0],
            params: [
                radius as f32,
                grade.threshold(),
                f32::from(u8::from(grade.operates_on_alpha())),
                0.0,
            ],
            extra: [0.0; 4],
            aces: [0.0; 4],
            extra1: [0.0; 4],
        };
        let input_view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let output_view = scratch[0].create_view(&wgpu::TextureViewDescriptor::default());
        self.record_pass(
            device,
            encoder,
            &self.dust_scratches,
            &input_view,
            &output_view,
            &uniform,
        );
        Ok(scratch[0].clone())
    }

    /// Records one matte filter pass and advances the matte ping-pong.
    ///
    /// The first matte pass binds the frame and asks the shader for the frame
    /// alpha; every later pass binds `scratch[2]` and reads the matte channel.
    #[allow(clippy::too_many_arguments)]
    fn record_matte_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
        matte: &mut Option<usize>,
        written: &mut usize,
        size: [f32; 4],
        mut params: [f32; 4],
        extra: [f32; 4],
        mut extra1: [f32; 4],
        flag: MatteReadFlag,
    ) {
        let read_frame = f32::from(u8::from(matte.is_none()));
        match flag {
            MatteReadFlag::Params => params[3] = read_frame,
            MatteReadFlag::Extra1 => extra1[0] = read_frame,
        }
        let input_view = match *matte {
            Some(index) => scratch[index].create_view(&wgpu::TextureViewDescriptor::default()),
            None => source.create_view(&wgpu::TextureViewDescriptor::default()),
        };
        let output_view = scratch[*written].create_view(&wgpu::TextureViewDescriptor::default());
        let uniform = SpatialPassUniform { size, params, extra, aces: [0.0; 4], extra1 };
        self.record_pass(
            device,
            encoder,
            pipeline,
            &input_view,
            &output_view,
            &uniform,
        );
        *matte = Some(*written);
        *written = if *written == 3 { 2 } else { 3 };
    }

    fn record_blanking_fill(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        grade: mondrian_effects::BlankingFillGrade,
        source: &wgpu::Texture,
        scratch: &[wgpu::Texture],
    ) -> Result<wgpu::Texture, EffectSpatialGpuError> {
        if grade.is_identity() {
            return Ok(source.clone());
        }
        let first = scratch.first().ok_or(EffectSpatialGpuError::EmptyFrame)?;
        let uniforms = grade.gpu_controls(source.width(), source.height());
        let original = source.create_view(&wgpu::TextureViewDescriptor::default());
        let first_view = first.create_view(&wgpu::TextureViewDescriptor::default());
        if grade.warps_top_layer() {
            self.record_composite_pass(
                device,
                encoder,
                &self.blanking_fill_warp,
                &original,
                &original,
                &first_view,
                &uniforms,
            );
            return Ok(first.clone());
        }
        let output = scratch.get(1).ok_or(EffectSpatialGpuError::EmptyFrame)?;
        let output_view = output.create_view(&wgpu::TextureViewDescriptor::default());
        self.record_composite_pass(
            device,
            encoder,
            &self.blanking_fill_horizontal,
            &original,
            &original,
            &first_view,
            &uniforms,
        );
        self.record_composite_pass(
            device,
            encoder,
            &self.blanking_fill_vertical,
            &original,
            &first_view,
            &output_view,
            &uniforms,
        );
        Ok(output.clone())
    }

    /// Records one composite pass over the original frame and an auxiliary layer.
    #[allow(clippy::too_many_arguments)]
    fn record_composite_pass<T: bytemuck::Pod>(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        original: &wgpu::TextureView,
        auxiliary: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &T,
    ) {
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mondrian.effect-spatial.composite-uniforms"),
            contents: bytemuck::bytes_of(uniforms),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mondrian.effect-spatial.composite-bind-group"),
            layout: &self.composite_bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(original),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(auxiliary),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mondrian.effect-spatial.composite-pass"),
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
        pass.set_pipeline(pipeline);
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
    // The second spare cell, added for effects that need more than twelve
    // scalars (glow's colour pair).
    extra1: [f32; 4],
}

/// The uniform cell that carries a matte pass's "read the frame alpha" flag.
#[derive(Clone, Copy)]
enum MatteReadFlag {
    /// `params.w`, used by the smooth and morphology passes.
    Params,
    /// `extra1.x`, used by the feather pass.
    Extra1,
}

/// The dedicated uniform payload of the fractal noise generator.
///
/// Merged into [`SpatialPassUniform`] this would need eight cells, so the stage
/// carries its own struct and bind group layout instead.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FractalNoiseUniform {
    size: [f32; 4],
    params: [f32; 4],
    extra0: [f32; 4],
    extra1: [f32; 4],
    extra2: [f32; 4],
    extra3: [f32; 4],
    extra4: [f32; 4],
    extra5: [f32; 4],
}

/// The dedicated uniform payload of the selective colour pass.
///
/// Merged into [`SpatialPassUniform`] this would need ten cells, so the stage
/// carries its own struct and bind group layout instead.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SelectiveColorUniform {
    /// `x` is the method menu (1 relative, 2 absolute); the rest is reserved.
    control: [u32; 4],
    /// The nine folded CMYK delta rows, in range order.
    deltas: [[f32; 4]; 9],
}
/// The dedicated uniform payload of the stroke pass.
///
/// Forty-eight bytes: one style selector, the straight-alpha ink colour, and the
/// brush geometry. Merged into [`SpatialPassUniform`] this would need three of
/// its five cells, so the stage carries its own struct and bind group layout.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StrokeUniform {
    /// `x` is the paint style (1 on original, 2 on transparent, 3 reveal); the
    /// rest is reserved.
    control: [u32; 4],
    /// The straight-alpha ink colour; `w` is the opacity.
    color: [f32; 4],
    /// `x` is the brush size and `y` the brush hardness; the rest is reserved.
    geometry: [f32; 4],
}
/// The dedicated uniform payload of the bezier warp pass.
///
/// A hundred and twenty-eight bytes: the frame size, six cells of two control
/// points, and the solver step count in the first lane of a seventh cell. The
/// shared five-cell uniform holds twenty floats, and the twelve control points
/// alone need twenty-four, so the stage carries its own payload.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BezierWarpUniform {
    /// `xy` is the frame size in pixels; `zw` is reserved.
    size: [f32; 4],
    /// Two control points per cell, in dump order.
    points: [[f32; 4]; 6],
    /// `x` is the solver step count; the rest is reserved.
    control: [f32; 4],
}
const SPATIAL_EFFECT_SHADER: &str = r#"
struct Uniforms {
    size: vec4<f32>,
    params: vec4<f32>,
    extra: vec4<f32>,
    aces: vec4<f32>,
    extra1: vec4<f32>,
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

fn load_clamped_from(texture: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let limit = frame_dims() - vec2<i32>(1);
    return textureLoad(texture, clamp(p, vec2<i32>(0), limit), 0);
}

fn load_window(p: vec2<i32>, repeat_edge: bool) -> vec4<f32> {
    let dims = frame_dims();
    if (!repeat_edge && (p.x < 0 || p.y < 0 || p.x >= dims.x || p.y >= dims.y)) {
        return vec4<f32>(0.0);
    }
    return load_clamped(p);
}

@fragment
fn fs_grow_bounds(in: VsOut) -> @location(0) vec4<f32> {
    let padding = vec2<i32>(i32(uniforms.params.x));
    return load_window(vec2<i32>(in.position.xy) - padding, false);
}

fn premultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    return vec4<f32>(sample.rgb * alpha, alpha);
}
fn unpremultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sample.rgb / alpha, alpha);
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
// The glow extraction turns the straight-alpha source into the straight-alpha
// glow layer the blur passes then spread. Every branch mirrors
// `GlowGrade::extract` so the CPU and GPU paths evaluate the same expression.
@fragment
fn fs_glow_extract(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let sample = load_clamped(pixel);
    let alpha = sample.a;
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let luminance = dot(
        sample.rgb,
        vec3<f32>(uniforms.params.y, uniforms.params.z, uniforms.params.w),
    );
    let based_on = u32(round(uniforms.extra.x));
    let signal = select(luminance, clamp(alpha, 0.0, 1.0), based_on == 1u);
    let threshold = clamp(uniforms.params.x, 0.0, 1.0);
    var threshold_weight = 0.0;
    if (threshold < 1.0) {
        threshold_weight = clamp((signal - threshold) / (1.0 - threshold), 0.0, 1.0);
    }
    var weight = threshold_weight;
    var colour = sample.rgb;
    if (based_on == 1u) {
        colour = vec3<f32>(1.0, 1.0, 1.0);
    } else if (based_on == 2u) {
        weight = max(threshold_weight, alpha);
    }
    let colours = u32(round(uniforms.extra.y));
    let midpoint = uniforms.extra.z;
    var tint = colour;
    if (colours == 1u) {
        let ramp_low = 0.5 * signal / midpoint;
        let ramp_high = 0.5 + 0.5 * (signal - midpoint) / (1.0 - midpoint);
        let ramp = select(ramp_high, ramp_low, signal < midpoint);
        tint = mix(uniforms.aces.rgb, uniforms.extra1.rgb, ramp);
    } else if (colours == 2u) {
        let phase = uniforms.extra.w + signal * uniforms.aces.a;
        tint = mix(uniforms.aces.rgb, uniforms.extra1.rgb, fract(phase));
    }
    return vec4<f32>(tint, weight);
}
// The key pass pulls the matte out of the screen colour. Every branch mirrors
// `KeylightGrade::matte_of_rgb` so the CPU and GPU paths evaluate the same
// expression, and the matte lands in every channel so later stages can read it
// from `.x` without an alpha convention.
@fragment
fn fs_keylight_key(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let sample = load_clamped(pixel);
    let key_channel = u32(round(uniforms.extra.x));
    var other_a = 1u;
    var other_b = 2u;
    if (key_channel == 1u) {
        other_a = 0u;
        other_b = 2u;
    } else if (key_channel == 2u) {
        other_a = 0u;
        other_b = 1u;
    }
    let balance = uniforms.params.y;
    let reference = balance * sample.rgb[other_a]
        + (1.0 - balance) * sample.rgb[other_b]
        + uniforms.extra.z
        - 0.5;
    let separation = max(uniforms.aces[key_channel], 0.01);
    let raw = clamp(
        1.0 - uniforms.params.x * (sample.rgb[key_channel] - reference) / separation,
        0.0,
        1.0,
    );
    let inner = uniforms.params.z;
    let outer = uniforms.params.w;
    var matte = clamp((raw - inner) / max(outer - inner, 0.0001), 0.0, 1.0);
    if (sample.a <= 0.0) {
        matte = 0.0;
    }
    return vec4<f32>(matte, matte, matte, 1.0);
}

// The matte filters read either the frame alpha (the first matte pass, before a
// matte texture exists) or the matte channel every later pass writes.
fn matte_alpha_at(p: vec2<i32>, read_frame: bool) -> f32 {
    let sample = load_clamped(p);
    return select(sample.x, sample.a, read_frame);
}

// The same alpha gradient the CPU `matte_edge_strength` computes: the clamped
// four-neighbourhood difference drives how far the smooth radius shrinks.
fn matte_edge_strength(pixel: vec2<i32>, read_frame: bool) -> f32 {
    let left = matte_alpha_at(pixel + vec2<i32>(-1, 0), read_frame);
    let right = matte_alpha_at(pixel + vec2<i32>(1, 0), read_frame);
    let up = matte_alpha_at(pixel + vec2<i32>(0, -1), read_frame);
    let down = matte_alpha_at(pixel + vec2<i32>(0, 1), read_frame);
    let horizontal = 0.5 * (right - left);
    let vertical = 0.5 * (down - up);
    return clamp(2.0 * (abs(horizontal) + abs(vertical)), 0.0, 1.0);
}

// The edge colour cleanup samples eight taps at the authored radius and mixes
// the straight colour of the tap with the largest alpha into the soft pixel.
// `params` carries `[radius, clean_amount / 100, 0, 0]`; opaque and transparent
// pixels are returned untouched, exactly like `clean_edge_colours`.
@fragment
fn fs_matte_refine_cleanup(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped(pixel);
    if (source.a <= 0.0 || source.a >= 1.0) {
        return source;
    }
    let radius = i32(round(uniforms.params.x));
    let amount = uniforms.params.y;
    var taps = array<vec2<i32>, 8>(
        vec2<i32>(-1, 0),
        vec2<i32>(1, 0),
        vec2<i32>(0, -1),
        vec2<i32>(0, 1),
        vec2<i32>(-1, -1),
        vec2<i32>(1, -1),
        vec2<i32>(-1, 1),
        vec2<i32>(1, 1),
    );
    var best_alpha = -1.0;
    var best = source.rgb;
    for (var index = 0; index < 8; index = index + 1) {
        let tap = load_clamped(pixel + taps[index] * radius);
        if (tap.a > best_alpha) {
            best_alpha = tap.a;
            best = tap.rgb;
        }
    }
    if (best_alpha <= 0.0) {
        return source;
    }
    let cleaned = source.rgb + (best - source.rgb) * amount;
    return vec4<f32>(cleaned, source.a);
}

// The smooth pass averages the matte over a per-pixel radius. `params` carries
// `[max_radius, axis, edge_detail, read_frame_alpha]`, and the radius shrinks to
// `max * (1 - edge_detail * strength)` where the alpha gradient is strong. The
// per-pixel radius is why this cannot reuse `fs_box_blur`.
@fragment
fn fs_matte_refine_smooth(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let max_radius = uniforms.params.x;
    let axis = uniforms.params.y;
    let edge_detail = clamp(uniforms.params.z, 0.0, 1.0);
    let read_frame = uniforms.params.w > 0.5;
    let strength = matte_edge_strength(pixel, read_frame);
    let radius = i32(clamp(
        round(max_radius * (1.0 - edge_detail * strength)),
        0.0,
        max_radius,
    ));
    var sum = 0.0;
    var count = 0.0;
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        var sample_at = pixel;
        if (axis < 0.5) {
            sample_at.x = sample_at.x + offset;
        } else {
            sample_at.y = sample_at.y + offset;
        }
        sum = sum + matte_alpha_at(sample_at, read_frame);
        count = count + 1.0;
    }
    let value = sum / count;
    return vec4<f32>(value, value, value, 1.0);
}

// The feather is the same separable box kernel as `fs_box_blur`, but it reads
// the matte alpha (frame alpha on the first matte pass) and writes the matte
// back into every channel. `params` carries `[0, axis, whole, fraction]`,
// `extra` `[normalization, 1, 0, 0]`, and `extra1.x` the read-frame flag.
@fragment
fn fs_matte_refine_feather(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let axis = uniforms.params.y;
    let whole = i32(uniforms.params.z);
    let fraction = uniforms.params.w;
    let read_frame = uniforms.extra1.x > 0.5;
    var sum = 0.0;
    for (var offset = -whole; offset <= whole; offset = offset + 1) {
        var sample_at = pixel;
        if (axis < 0.5) {
            sample_at.x = sample_at.x + offset;
        } else {
            sample_at.y = sample_at.y + offset;
        }
        sum = sum + matte_alpha_at(sample_at, read_frame);
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
        sum = sum
            + fraction
                * (matte_alpha_at(outer_a, read_frame) + matte_alpha_at(outer_b, read_frame));
    }
    let value = sum / max(uniforms.extra.x, 1.0);
    return vec4<f32>(value, value, value, 1.0);
}

// The morphology pass is the separable minimum (keylight's shrink) or maximum
// (matte refine's grow) of the matte over `2r + 1` taps. `params` carries
// `[radius, axis, mode, read_frame_alpha]`; `mode` 0 is the min filter keylight
// records, 1 is the max filter matte refine uses to grow the matte. Keylight
// passes 0 in both spare cells, so its behaviour is unchanged.
@fragment
fn fs_keylight_erode(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let axis = uniforms.params.y;
    let radius = i32(round(uniforms.params.x));
    let dilate = uniforms.params.z > 0.5;
    let read_frame = uniforms.params.w > 0.5;
    var matte = select(1.0, 0.0, dilate);
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        var sample_at = pixel;
        if (axis < 0.5) {
            sample_at.x = sample_at.x + offset;
        } else {
            sample_at.y = sample_at.y + offset;
        }
        let value = matte_alpha_at(sample_at, read_frame);
        matte = select(min(matte, value), max(matte, value), dilate);
    }
    return vec4<f32>(matte, matte, matte, 1.0);
}

// The minimax pass is the separable minimum or maximum over `2r + 1` taps of
// the selected channels. `params` is `[radius, axis, extreme, channel_mask]`
// with axis 0 horizontal / 1 vertical, extreme 0 minimum / 1 maximum, and the
// mask bits 1 red, 2 green, 4 blue, 8 alpha. `extra.x` is the
// "don't shrink edges" switch: a minimum pass drops the alpha bit, so the
// matte is never eroded, while every maximum pass keeps the authored mask.
@fragment
fn fs_minimax(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let radius = i32(round(uniforms.params.x));
    let horizontal = uniforms.params.y < 0.5;
    let maximum = uniforms.params.z > 0.5;
    var mask = u32(uniforms.params.w);
    if (uniforms.extra.x >= 0.5 && !maximum) {
        mask = mask & 7u;
    }
    let step = select(vec2<i32>(0, 1), vec2<i32>(1, 0), horizontal);
    var value = load_clamped_from(src, pixel);
    for (var offset = -radius; offset <= radius; offset = offset + 1) {
        let sample_value = load_clamped_from(src, pixel + step * offset);
        if ((mask & 1u) != 0u) {
            value.r = select(min(value.r, sample_value.r), max(value.r, sample_value.r), maximum);
        }
        if ((mask & 2u) != 0u) {
            value.g = select(min(value.g, sample_value.g), max(value.g, sample_value.g), maximum);
        }
        if ((mask & 4u) != 0u) {
            value.b = select(min(value.b, sample_value.b), max(value.b, sample_value.b), maximum);
        }
        if ((mask & 8u) != 0u) {
            value.a = select(min(value.a, sample_value.a), max(value.a, sample_value.a), maximum);
        }
    }
    return value;
}

// The channel blur pass is one separable box blur over red, green, blue and
// alpha independently. `params` holds the four rounded radii in pixels and
// `extra` is `[repeat_edge, axis, 0, 0]` with axis 0 horizontal / 1 vertical.
// One traversal serves every channel: a tap joins a channel's window when its
// offset is inside that channel's radius, and the divisor stays `2r + 1` even
// when the tap falls outside the frame, so the transparent-edge mode darkens
// the border instead of renormalising it.
@fragment
fn fs_channel_blur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let radii = vec4<i32>(round(uniforms.params));
    let horizontal = uniforms.extra.y < 0.5;
    let repeat_edge = uniforms.extra.x >= 0.5;
    let axis_step = select(vec2<i32>(0, 1), vec2<i32>(1, 0), horizontal);
    let max_radius = max(max(radii.x, radii.y), max(radii.z, radii.w));
    var sums = vec4<f32>(0.0);
    for (var offset = -max_radius; offset <= max_radius; offset = offset + 1) {
        let tap = load_window(pixel + axis_step * offset, repeat_edge);
        let away = vec4<f32>(abs(f32(offset)));
        let inside = select(vec4<f32>(0.0), vec4<f32>(1.0), away <= vec4<f32>(radii));
        sums = sums + select(vec4<f32>(0.0), tap, inside > vec4<f32>(0.5));
    }
    return sums / (2.0 * vec4<f32>(radii) + vec4<f32>(1.0));
}

// The median pass selects the exact per-channel median of the `(2r + 1)^2`
// window in the 8-bit domain: `bin = round(clamp01(value) * 255)`. Selection is
// bit-serial: eight rounds build the answer from the high bit down, and each
// round counts how many taps fall below the candidate bin. The count never
// exceeds 255, so comparisons against `rank` stay exact. `params` is
// `[radius_pixels, operate_on_alpha, 0, 0]`; the alpha channel joins the
// selection only when the toggle is on, otherwise the centre alpha is copied
// bit for bit from the straight-alpha source.
fn median_bin_of(value: f32) -> u32 {
    // NaN has no ordering, so it lands in the lowest bin, like clamp01 does.
    let safe = select(value, 0.0, value != value);
    return u32(round(clamp(safe, 0.0, 1.0) * 255.0));
}

// Selects the per-channel median of the `(2 * radius + 1)^2` window centred on
// `pixel`. The bit-serial loop is the whole of the median kernel, shared by the
// median pass and the dust & scratches gate.
fn median_window(pixel: vec2<i32>, radius: i32) -> vec4<f32> {
    let side = 2 * radius + 1;
    let count = u32(side * side);
    let rank = (count + 1u) / 2u - 1u;
    var prefix = vec4<u32>(0u);
    for (var bit = 7; bit >= 0; bit = bit - 1) {
        let candidate = prefix | vec4<u32>(1u << u32(bit));
        var below = vec4<u32>(0u);
        for (var dy = -radius; dy <= radius; dy = dy + 1) {
            for (var dx = -radius; dx <= radius; dx = dx + 1) {
                let aim = load_clamped_from(src, pixel + vec2<i32>(dx, dy));
                let q = vec4<u32>(
                    median_bin_of(aim.r),
                    median_bin_of(aim.g),
                    median_bin_of(aim.b),
                    median_bin_of(aim.a),
                );
                below = below + select(vec4<u32>(0u), vec4<u32>(1u), q < candidate);
            }
        }
        prefix = select(prefix, candidate, below <= vec4<u32>(rank));
    }
    return vec4<f32>(
        f32(prefix.r),
        f32(prefix.g),
        f32(prefix.b),
        f32(prefix.a),
    ) / 255.0;
}

@fragment
fn fs_median(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let radius = i32(round(uniforms.params.x));
    let on_alpha = uniforms.params.y >= 0.5;
    let centre = load_clamped_from(src, pixel);
    let selected = median_window(pixel, radius);
    return vec4<f32>(selected.rgb, select(centre.a, selected.a, on_alpha));
}

// The dust & scratches pass keeps the centre pixel unless it deviates from the
// median of its window by more than the threshold, in which case the median
// replaces it. `params` is `[radius_pixels, threshold, operate_on_alpha, 0]`.
// The gate quantises the centre exactly like the selection does, so a threshold
// of zero repairs exactly the pixels the median itself would move.
@fragment
fn fs_dust_scratches(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let radius = i32(round(uniforms.params.x));
    let threshold = uniforms.params.y;
    let on_alpha = uniforms.params.z >= 0.5;
    let centre = load_clamped_from(src, pixel);
    let med = median_window(pixel, radius);
    let gated = select(vec4<f32>(0.0), clamp(centre, vec4<f32>(0.0), vec4<f32>(1.0)), centre == centre);
    let rgb_delta = max(max(abs(gated.r - med.r), abs(gated.g - med.g)), abs(gated.b - med.b));
    let alpha_delta = select(0.0, abs(gated.a - med.a), on_alpha);
    let repair = max(rgb_delta, alpha_delta) > threshold;
    return vec4<f32>(
        select(centre.rgb, med.rgb, repair),
        select(centre.a, med.a, repair && on_alpha),
    );
}

fn sobel_magnitude(pixel: vec2<i32>) -> f32 {
    let top_left = premultiply(load_clamped(pixel + vec2<i32>(-1, -1)));
    let top_center = premultiply(load_clamped(pixel + vec2<i32>(0, -1)));
    let top_right = premultiply(load_clamped(pixel + vec2<i32>(1, -1)));
    let middle_left = premultiply(load_clamped(pixel + vec2<i32>(-1, 0)));
    let middle_right = premultiply(load_clamped(pixel + vec2<i32>(1, 0)));
    let bottom_left = premultiply(load_clamped(pixel + vec2<i32>(-1, 1)));
    let bottom_center = premultiply(load_clamped(pixel + vec2<i32>(0, 1)));
    let bottom_right = premultiply(load_clamped(pixel + vec2<i32>(1, 1)));
    var magnitude = 0.0;
    for (var channel = 0; channel < 3; channel = channel + 1) {
        let gx = (top_right[channel] + 2.0 * middle_right[channel] + bottom_right[channel])
            - (top_left[channel] + 2.0 * middle_left[channel] + bottom_left[channel]);
        let gy = (bottom_left[channel] + 2.0 * bottom_center[channel] + bottom_right[channel])
            - (top_left[channel] + 2.0 * top_center[channel] + top_right[channel]);
        magnitude = magnitude + gx * gx + gy * gy;
    }
    return sqrt(magnitude);
}

@fragment
fn fs_find_edges(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = frame_dims();
    let pixel = vec2<i32>(floor(position.xy));
    let center = premultiply(load_clamped(pixel));
    if (center.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    var magnitude = 0.0;
    if (pixel.x > 0 && pixel.y > 0 && pixel.x + 1 < dims.x && pixel.y + 1 < dims.y) {
        magnitude = sobel_magnitude(pixel);
    }
    let edge = select(1.0 - magnitude, magnitude, uniforms.params.y > 0.5);
    let blend = clamp(uniforms.params.x, 0.0, 1.0);
    let mixed = edge * (1.0 - blend) + center.rgb * blend;
    return unpremultiply(vec4<f32>(mixed, center.a));
}

@fragment
fn fs_emboss(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = frame_dims();
    let pixel = vec2<i32>(floor(position.xy));
    let center = premultiply(load_clamped(pixel));
    if (center.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let coefficients = uniforms.extra.xyz;
    var derivative = 0.0;
    let left = premultiply(load_clamped(pixel + vec2<i32>(-1, 0)));
    let right = premultiply(load_clamped(pixel + vec2<i32>(1, 0)));
    let top = premultiply(load_clamped(pixel + vec2<i32>(0, -1)));
    let bottom = premultiply(load_clamped(pixel + vec2<i32>(0, 1)));
    let top_left = premultiply(load_clamped(pixel + vec2<i32>(-1, -1)));
    let top_right = premultiply(load_clamped(pixel + vec2<i32>(1, -1)));
    let bottom_left = premultiply(load_clamped(pixel + vec2<i32>(-1, 1)));
    let bottom_right = premultiply(load_clamped(pixel + vec2<i32>(1, 1)));
    var horizontal = 0.0;
    var vertical = 0.0;
    for (var channel = 0; channel < 3; channel = channel + 1) {
        let weight = coefficients[channel];
        horizontal = horizontal
            + weight * ((top_right[channel] + 2.0 * right[channel] + bottom_right[channel])
                - (top_left[channel] + 2.0 * left[channel] + bottom_left[channel]));
        vertical = vertical
            + weight * ((bottom_left[channel] + 2.0 * bottom[channel] + bottom_right[channel])
                - (top_left[channel] + 2.0 * top[channel] + top_right[channel]));
    }
    derivative = horizontal * uniforms.params.x - vertical * uniforms.params.y;
    let value = 0.5 + derivative * uniforms.params.z;
    let blend = clamp(uniforms.params.w, 0.0, 1.0);
    let mixed = vec3<f32>(value) * (1.0 - blend) + center.rgb * blend;
    return unpremultiply(vec4<f32>(mixed, center.a));
}

@fragment
fn fs_color_emboss(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let center = premultiply(load_clamped(pixel));
    if (center.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let left = premultiply(load_clamped(pixel + vec2<i32>(-1, 0)));
    let right = premultiply(load_clamped(pixel + vec2<i32>(1, 0)));
    let top = premultiply(load_clamped(pixel + vec2<i32>(0, -1)));
    let bottom = premultiply(load_clamped(pixel + vec2<i32>(0, 1)));
    let top_left = premultiply(load_clamped(pixel + vec2<i32>(-1, -1)));
    let top_right = premultiply(load_clamped(pixel + vec2<i32>(1, -1)));
    let bottom_left = premultiply(load_clamped(pixel + vec2<i32>(-1, 1)));
    let bottom_right = premultiply(load_clamped(pixel + vec2<i32>(1, 1)));
    let horizontal = (top_right.rgb + 2.0 * right.rgb + bottom_right.rgb)
        - (top_left.rgb + 2.0 * left.rgb + bottom_left.rgb);
    let vertical = (bottom_left.rgb + 2.0 * bottom.rgb + bottom_right.rgb)
        - (top_left.rgb + 2.0 * top.rgb + top_right.rgb);
    let derivative = horizontal * uniforms.params.x - vertical * uniforms.params.y;
    let value = vec3<f32>(0.5) + derivative * uniforms.params.z;
    let blend = clamp(uniforms.params.w, 0.0, 1.0);
    let mixed = value * (1.0 - blend) + center.rgb * blend;
    return unpremultiply(vec4<f32>(mixed, center.a));
}


fn radial_hash_jitter(seed: u32, x: u32, y: u32) -> f32 {
    var hash = seed + x * 0x9E3779B9u + y * 0x85EBCA6Bu;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    return f32((hash >> 8u) & 0xFFFFu) / 65535.0 - 0.5;
}

fn sample_bilinear_premultiplied(x: f32, y: f32) -> vec4<f32> {
    let limit = frame_dims() - vec2<i32>(1);
    let limit_f = vec2<f32>(f32(limit.x), f32(limit.y));
    let p = clamp(vec2<f32>(x, y), vec2<f32>(0.0), limit_f);
    let base = floor(p);
    let frac = p - base;
    let x0 = i32(base.x);
    let y0 = i32(base.y);
    let x1 = min(x0 + 1, limit.x);
    let y1 = min(y0 + 1, limit.y);
    let top_left = premultiply(textureLoad(src, vec2<i32>(x0, y0), 0));
    let top_right = premultiply(textureLoad(src, vec2<i32>(x1, y0), 0));
    let bottom_left = premultiply(textureLoad(src, vec2<i32>(x0, y1), 0));
    let bottom_right = premultiply(textureLoad(src, vec2<i32>(x1, y1), 0));
    let top = top_left + (top_right - top_left) * frac.x;
    let bottom = bottom_left + (bottom_right - bottom_left) * frac.x;
    return top + (bottom - top) * frac.y;
}

@fragment
fn fs_convolution(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let centre = premultiply(load_clamped(pixel));
    let kernel = array<f32, 9>(
        uniforms.params.x,
        uniforms.params.y,
        uniforms.params.z,
        uniforms.params.w,
        uniforms.extra.x,
        uniforms.extra.y,
        uniforms.extra.z,
        uniforms.extra.w,
        uniforms.aces.x,
    );
    var filtered = vec4<f32>(0.0);
    for (var index = 0; index < 9; index = index + 1) {
        let offset = vec2<i32>(index % 3 - 1, index / 3 - 1);
        filtered = filtered + kernel[index] * premultiply(load_clamped(pixel + offset));
    }
    if (uniforms.aces.y > 0.5) {
        filtered = abs(filtered);
    }
    let blend = clamp(uniforms.aces.z, 0.0, 1.0);
    let mixed = filtered * (1.0 - blend) + centre * blend;
    return unpremultiply(mixed);
}
fn wrap_index(value: i32, extent: i32) -> i32 {
    if (extent <= 1) {
        return 0;
    }
    let rest = value % extent;
    if (rest < 0) {
        return rest + extent;
    }
    return rest;
}

fn sample_bilinear_wrapped(fragment: vec2<f32>) -> vec4<f32> {
    let dims = frame_dims();
    let base = floor(fragment);
    let frac = fragment - base;
    let x0 = i32(base.x);
    let y0 = i32(base.y);
    let top_left = premultiply(textureLoad(src, vec2<i32>(wrap_index(x0, dims.x), wrap_index(y0, dims.y)), 0));
    let top_right = premultiply(textureLoad(src, vec2<i32>(wrap_index(x0 + 1, dims.x), wrap_index(y0, dims.y)), 0));
    let bottom_left = premultiply(textureLoad(src, vec2<i32>(wrap_index(x0, dims.x), wrap_index(y0 + 1, dims.y)), 0));
    let bottom_right = premultiply(textureLoad(src, vec2<i32>(wrap_index(x0 + 1, dims.x), wrap_index(y0 + 1, dims.y)), 0));
    let top = top_left + (top_right - top_left) * frac.x;
    let bottom = bottom_left + (bottom_right - bottom_left) * frac.x;
    return top + (bottom - top) * frac.y;
}

@fragment
fn fs_spherize(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = frame_dims();
    let pixel = vec2<i32>(position.xy);
    let centre = uniforms.params.xy;
    let radius = uniforms.params.z;
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let distance = length(delta);
    if (radius <= 0.0 || distance >= radius || distance <= 0.0) {
        return load_clamped(pixel);
    }
    let sample_distance = radius * asin(distance / radius) / 1.5707963267948966;
    let scale = sample_distance / distance;
    let sample_x = centre.x + delta.x * scale;
    let sample_y = centre.y + delta.y * scale;
    return unpremultiply(sample_bilinear_premultiplied(sample_x, sample_y));
}

@fragment
fn fs_vr_rotate_sphere(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let frame_layout = uniforms.extra.z;
    let eye_w = max(uniforms.extra1.z, 1.0);
    let eye_h = max(uniforms.extra1.w, 1.0);
    var origin = vec2<f32>(0.0, 0.0);
    if (frame_layout > 0.5 && frame_layout < 1.5) {
        origin = vec2<f32>(0.0, select(0.0, eye_h, position.y >= eye_h));
    } else if (frame_layout > 1.5) {
        origin = vec2<f32>(select(0.0, eye_w, position.x >= eye_w), 0.0);
    }
    // The CPU converts its pixel centre with `(column + 0.5) / eye`, so the
    // eye-local index is the same subtraction here; `position.xy` is already
    // the pixel centre, hence no extra half-pixel shift.
    let local = vec2<f32>(f32(pixel.x), f32(pixel.y)) - origin;
    let u = (local.x + 0.5) / eye_w;
    let v = (local.y + 0.5) / eye_h;
    let h_fov = uniforms.params.x;
    let v_fov = uniforms.params.y;
    let lon_dst = (u - 0.5) * h_fov;
    let lat_dst = (0.5 - v) * v_fov;
    let cos_lat = cos(lat_dst);
    let d_dst = vec3<f32>(cos_lat * sin(lon_dst), sin(lat_dst), cos_lat * cos(lon_dst));
    let pitch = uniforms.params.z;
    let yaw = uniforms.params.w;
    let roll = uniforms.extra.x;
    let sp = sin(pitch);
    let cp = cos(pitch);
    let sy = sin(yaw);
    let cy = cos(yaw);
    let sr = sin(roll);
    let cr = cos(roll);
    // R = Rz(roll) * Ry(yaw) * Rx(pitch), row by row.
    let r00 = cr * cy;
    let r01 = cr * sy * sp - sr * cp;
    let r02 = cr * sy * cp + sr * sp;
    let r10 = sr * cy;
    let r11 = sr * sy * sp + cr * cp;
    let r12 = sr * sy * cp - cr * sp;
    let r20 = -sy;
    let r21 = cy * sp;
    let r22 = cy * cp;
    // Forward samples with R (the camera-to-world rotation); inverted samples
    // with its transpose. Negating the angles would be the wrong matrix.
    let invert = uniforms.extra.y > 0.5;
    let x_forward = r00 * d_dst.x + r01 * d_dst.y + r02 * d_dst.z;
    let x_inverse = r00 * d_dst.x + r10 * d_dst.y + r20 * d_dst.z;
    let y_forward = r10 * d_dst.x + r11 * d_dst.y + r12 * d_dst.z;
    let y_inverse = r01 * d_dst.x + r11 * d_dst.y + r21 * d_dst.z;
    let z_forward = r20 * d_dst.x + r21 * d_dst.y + r22 * d_dst.z;
    let z_inverse = r02 * d_dst.x + r12 * d_dst.y + r22 * d_dst.z;
    let d_src = vec3<f32>(
        select(x_forward, x_inverse, invert),
        select(y_forward, y_inverse, invert),
        select(z_forward, z_inverse, invert),
    );
    let lon_src = atan2(d_src.x, d_src.z);
    let lat_src = asin(clamp(d_src.y, -1.0, 1.0));
    var u_src = lon_src / h_fov + 0.5;
    let v_src = 0.5 - lat_src / v_fov;
    if (uniforms.extra.w > 0.5) {
        // Wrap the column, not the longitude: folding the longitude into
        // [0, 2*pi) first would leave u in [0.5, 1.5).
        u_src = u_src - floor(u_src);
    }
    let raw = vec2<f32>(u_src * eye_w - 0.5 + origin.x, v_src * eye_h - 0.5 + origin.y);
    // Each eye samples only its own sub-frame, matching the CPU tap clamp.
    let tap = clamp(raw, origin, origin + vec2<f32>(eye_w - 1.0, eye_h - 1.0));
    return unpremultiply(sample_bilinear_premultiplied(tap.x, tap.y));
}

@fragment
fn fs_vr_sphere_to_plane(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // `position.xy` is already the pixel centre, so dividing by the frame size
    // gives the same `(column + 0.5) / width` parameterisation as the CPU.
    let size = uniforms.size.xy;
    let u = position.x / size.x;
    let v = position.y / size.y;
    // The field of view is a full angle; its tangent is computed once on the
    // CPU because the shader must not call tan().
    let focal = uniforms.params.y;
    let ax = (2.0 * u - 1.0) * focal;
    let ay = (1.0 - 2.0 * v) * focal * uniforms.extra.y;
    let ray_length = sqrt(ax * ax + ay * ay + 1.0);
    let d_dst = vec3<f32>(ax / ray_length, ay / ray_length, 1.0 / ray_length);
    let pitch = uniforms.params.z;
    let yaw = uniforms.params.w;
    let roll = uniforms.extra.x;
    let sp = sin(pitch);
    let cp = cos(pitch);
    let sy = sin(yaw);
    let cy = cos(yaw);
    let sr = sin(roll);
    let cr = cos(roll);
    // R = Rz(roll) * Ry(yaw) * Rx(pitch), row by row.
    let r00 = cr * cy;
    let r01 = cr * sy * sp - sr * cp;
    let r02 = cr * sy * cp + sr * sp;
    let r10 = sr * cy;
    let r11 = sr * sy * sp + cr * cp;
    let r12 = sr * sy * cp - cr * sp;
    let r20 = -sy;
    let r21 = cy * sp;
    let r22 = cy * cp;
    // Forward samples with R (the camera-to-world rotation); inverted samples
    // with its transpose. Negating the angles would be the wrong matrix.
    let invert = uniforms.extra.z > 0.5;
    let x_forward = r00 * d_dst.x + r01 * d_dst.y + r02 * d_dst.z;
    let x_inverse = r00 * d_dst.x + r10 * d_dst.y + r20 * d_dst.z;
    let y_forward = r10 * d_dst.x + r11 * d_dst.y + r12 * d_dst.z;
    let y_inverse = r01 * d_dst.x + r11 * d_dst.y + r21 * d_dst.z;
    let z_forward = r20 * d_dst.x + r21 * d_dst.y + r22 * d_dst.z;
    let z_inverse = r02 * d_dst.x + r12 * d_dst.y + r22 * d_dst.z;
    let d_src = vec3<f32>(
        select(x_forward, x_inverse, invert),
        select(y_forward, y_inverse, invert),
        select(z_forward, z_inverse, invert),
    );
    let lon_src = atan2(d_src.x, d_src.z);
    let lat_src = asin(clamp(d_src.y, -1.0, 1.0));
    // The source is the whole 360 x 180 equirectangular frame: the seam clamps
    // to the last column instead of wrapping around.
    let tap = vec2<f32>(
        (lon_src / 6.2831855 + 0.5) * size.x - 0.5,
        (0.5 - lat_src / 3.1415927) * size.y - 0.5,
    );
    return unpremultiply(sample_bilinear_premultiplied(tap.x, tap.y));
}

@fragment
fn fs_vr_project_2d(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let base = load_clamped(pixel);
    let frame_layout = uniforms.extra1.w;
    // `VrFrameLayout::eye_size` divides with truncation, so the shader must use
    // integer division rather than a floating point half.
    let full_width = u32(uniforms.size.x);
    let full_height = u32(uniforms.size.y);
    let eye_width = f32(select(full_width, full_width / 2u, frame_layout > 1.5));
    let eye_height = f32(select(full_height, full_height / 2u, frame_layout > 0.5 && frame_layout < 1.5));
    var origin = vec2<f32>(0.0, 0.0);
    if (frame_layout > 0.5 && frame_layout < 1.5) {
        origin = vec2<f32>(0.0, select(0.0, eye_height, position.y >= eye_height));
    } else if (frame_layout > 1.5) {
        origin = vec2<f32>(select(0.0, eye_width, position.x >= eye_width), 0.0);
    }
    let local = position.xy - origin;
    // The CPU walks only the eye sub-frame: an odd trailing row or column
    // belongs to no eye and is left bit-identical.
    if (local.x >= eye_width || local.y >= eye_height) {
        return base;
    }
    let u = local.x / eye_width;
    let v = local.y / eye_height;
    let longitude = (u - 0.5) * uniforms.params.x;
    let latitude = (0.5 - v) * uniforms.params.y;
    let cos_latitude = cos(latitude);
    let direction = vec3<f32>(
        cos_latitude * sin(longitude),
        sin(latitude),
        cos_latitude * cos(longitude),
    );
    // Projection pose, row by row: R_p = Rz(roll) * Ry(yaw) * Rx(pitch).
    let projection_pitch = uniforms.extra1.x;
    let projection_yaw = uniforms.extra1.y;
    let projection_roll = uniforms.extra1.z;
    let pp = sin(projection_pitch);
    let qp = cos(projection_pitch);
    let py = sin(projection_yaw);
    let qy = cos(projection_yaw);
    let pr = sin(projection_roll);
    let qr = cos(projection_roll);
    let p00 = qr * qy;
    let p01 = qr * py * pp - pr * qp;
    let p02 = qr * py * qp + pr * pp;
    let p10 = pr * qy;
    let p11 = pr * py * pp + qr * qp;
    let p12 = pr * py * qp - qr * pp;
    let p20 = -py;
    let p21 = qy * pp;
    let p22 = qy * qp;
    // The mapping is destination to source, so both poses sample with the real
    // transpose of the pose matrix, never with negated angles.
    let q = vec3<f32>(
        p00 * direction.x + p10 * direction.y + p20 * direction.z,
        p01 * direction.x + p11 * direction.y + p21 * direction.z,
        p02 * direction.x + p12 * direction.y + p22 * direction.z,
    );
    if (q.z <= 0.0) {
        return base;
    }
    let patch_x = q.x / q.z;
    let patch_y = q.y / q.z;
    let half_extent = max(uniforms.params.z, 1.0e-6);
    if (abs(patch_x) > half_extent || abs(patch_y) > half_extent) {
        return base;
    }
    // Source card pose, row by row: R_s = Rz(roll) * Ry(yaw) * Rx(pitch).
    let source_pitch = uniforms.extra.x;
    let source_yaw = uniforms.extra.y;
    let source_roll = uniforms.extra.z;
    let sp = sin(source_pitch);
    let cp = cos(source_pitch);
    let sy = sin(source_yaw);
    let cy = cos(source_yaw);
    let sr = sin(source_roll);
    let cr = cos(source_roll);
    let s00 = cr * cy;
    let s01 = cr * sy * sp - sr * cp;
    let s02 = cr * sy * cp + sr * sp;
    let s10 = sr * cy;
    let s11 = sr * sy * sp + cr * cp;
    let s12 = sr * sy * cp - cr * sp;
    let s20 = -sy;
    let s21 = cy * sp;
    let s22 = cy * cp;
    let s = vec3<f32>(
        s00 * patch_x + s10 * patch_y + s20,
        s01 * patch_x + s11 * patch_y + s21,
        s02 * patch_x + s12 * patch_y + s22,
    );
    if (s.z <= 0.0) {
        return base;
    }
    let source_x = s.x / s.z;
    let source_y = s.y / s.z;
    let denominator = 2.0 * half_extent;
    let u_source = 0.5 + (source_x - uniforms.params.w) / denominator;
    let v_source = 0.5 - source_y / denominator;
    let raw_tap = origin + vec2<f32>(u_source * eye_width - 0.5, v_source * eye_height - 0.5);
    // Each eye samples only its own sub-frame, matching the CPU tap clamp.
    let tap_pos = clamp(raw_tap, origin, origin + vec2<f32>(eye_width - 1.0, eye_height - 1.0));
    let patch_sample = unpremultiply(sample_bilinear_premultiplied(tap_pos.x, tap_pos.y));
    // Feather is measured on the destination patch, which is square, so the
    // normalized radius uses the larger axis instead of the Euclidean length.
    let radius = max(abs(patch_x), abs(patch_y)) / half_extent;
    let edge = 1.0 - radius;
    if (edge <= 0.0) {
        return base;
    }
    let feather = uniforms.extra.w;
    var weight = 1.0;
    if (feather > 0.0) {
        let t = clamp(edge / feather, 0.0, 1.0);
        weight = t * t * (3.0 - 2.0 * t);
    }
    if (weight <= 0.0) {
        return base;
    }
    // Same over composite as `blend_rgba_f32_pixel(..., BlendMode::Normal)`.
    let base_alpha = clamp(base.a, 0.0, 1.0);
    let blend_alpha = clamp(patch_sample.a * weight, 0.0, 1.0);
    if (blend_alpha <= 0.0) {
        return base;
    }
    if (base_alpha <= 0.0) {
        return vec4<f32>(patch_sample.rgb, blend_alpha);
    }
    let out_alpha = blend_alpha + base_alpha * (1.0 - blend_alpha);
    if (out_alpha <= 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let out_rgb =
        (patch_sample.rgb * blend_alpha + base.rgb * base_alpha * (1.0 - blend_alpha)) / out_alpha;
    return vec4<f32>(out_rgb, out_alpha);
}

fn dent_radius() -> f32 {
    return uniforms.params.z * min(uniforms.size.x, uniforms.size.y);
}

@fragment
fn fs_dent(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let centre = uniforms.params.xy;
    let radius = dent_radius();
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let distance = sqrt(dot(delta, delta));
    if (radius <= 0.0 || distance >= radius || distance <= 1.0e-6) {
        return load_clamped(pixel);
    }
    let u = distance / radius;
    let base = 1.0 - u * u;
    let falloff = select(base * base, 1.0, uniforms.extra.y > 0.5);
    let direction = select(-1.0, 1.0, uniforms.extra.x < 0.5);
    let scale = 1.0 + direction * uniforms.params.w * falloff;
    let sample = centre + delta * scale;
    return unpremultiply(sample_bilinear_premultiplied(sample.x, sample.y));
}

/// Arc profile of one warp style as a cubic in the across-axis coordinate.
///
/// `shape = (1 - v^2) * (n0 + v * (n1 + v * n2))`, matching the CPU table.
fn warp_coefficients(style: u32) -> vec3<f32> {
    switch style {
        case 0u: {
            return vec3<f32>(1.0, 0.0, 0.0);
        }
        case 1u: {
            return vec3<f32>(0.84375, -0.84375, 0.0);
        }
        case 2u: {
            return vec3<f32>(0.84375, 0.84375, 0.0);
        }
        case 3u: {
            return vec3<f32>(1.0, 0.0, 1.0);
        }
        case 4u: {
            return vec3<f32>(1.0, 0.0, 0.5);
        }
        case 5u: {
            return vec3<f32>(0.8969201, -0.8969201, -0.8969201);
        }
        case 6u: {
            return vec3<f32>(0.8969201, 0.8969201, -0.8969201);
        }
        case 7u: {
            return vec3<f32>(0.0, 2.5980762, 0.0);
        }
        case 8u: {
            return vec3<f32>(1.0, 0.0, -3.0);
        }
        case 9u: {
            return vec3<f32>(0.8802741, -0.6602056, 0.2200685);
        }
        case 10u: {
            return vec3<f32>(0.8802741, 0.6602056, 0.2200685);
        }
        case 11u: {
            return vec3<f32>(0.8888889, 0.0, 1.7777778);
        }
        case 12u: {
            return vec3<f32>(0.9807181, 0.2451795, 0.2451795);
        }
        case 13u: {
            return vec3<f32>(1.0, 0.0, -2.0);
        }
        case 14u: {
            return vec3<f32>(0.4825708, 1.9302833, -0.4825708);
        }
        default: {
            return vec3<f32>(0.0, 0.0, 0.0);
        }
    }
}

@fragment
fn fs_warp(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    if (uniforms.size.x < 2.0 || uniforms.size.y < 2.0) {
        return load_clamped(pixel);
    }
    if (abs(uniforms.params.x) <= 1.0e-6
        && abs(uniforms.params.y) <= 1.0e-6
        && abs(uniforms.params.z) <= 1.0e-6) {
        return load_clamped(pixel);
    }
    let cx = (uniforms.size.x - 1.0) * 0.5;
    let cy = (uniforms.size.y - 1.0) * 0.5;
    let swap = uniforms.extra.x >= 0.5;
    let p = vec2<f32>(pixel);
    let s = select(p.y, p.x, swap);
    let t = select(p.x, p.y, swap);
    let cs = select(cy, cx, swap);
    let ct = select(cx, cy, swap);
    let hs = cs;
    let ht = ct;
    if (hs <= 0.0 || ht <= 0.0) {
        return load_clamped(pixel);
    }
    let kt = select(uniforms.params.y, uniforms.params.z, swap);
    let ks = select(uniforms.params.z, uniforms.params.y, swap);
    let vs = (s - cs) / hs;
    let gt = 1.0 + (kt / 100.0) * 0.5 * (1.0 - vs * vs);
    let t1 = ct + (t - ct) * gt;
    let vt = (t1 - ct) / ht;
    let gs = 1.0 + (ks / 100.0) * 0.5 * (1.0 - vt * vt);
    let c = warp_coefficients(u32(uniforms.params.w));
    let w = 1.0 - vt * vt;
    let shape = w * (c.x + vt * (c.y + vt * c.z));
    let s1 = cs + (s - cs) * gs - ((uniforms.params.x / 100.0) * hs) * shape;
    let sample = select(vec2<f32>(t1, s1), vec2<f32>(s1, t1), swap);
    return unpremultiply(sample_bilinear_premultiplied(sample.x, sample.y));
}
/// Smooths the per-cell blend between the two neighbouring corner offsets.
fn grid_warp_blend(z: f32, alpha: f32) -> f32 {
    let z2 = z * z;
    return (1.0 - alpha) * z + alpha * (3.0 * z2 - 2.0 * z2 * z);
}

/// Checkerboard displacement of one cell corner, pinned on the outer ring.
fn grid_warp_vertex_displacement(
    i: i32,
    j: i32,
    nx: i32,
    ny: i32,
    amplitude: f32,
) -> vec2<f32> {
    if (i == 0 || j == 0 || i == nx || j == ny) {
        return vec2<f32>(0.0, 0.0);
    }
    let sign = select(1.0, -1.0, ((i + j) & 1) == 1);
    return vec2<f32>(amplitude * sign, -amplitude * sign);
}

@fragment
fn fs_grid_warp(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let cell_w = uniforms.params.x;
    let cell_h = uniforms.params.y;
    let nx = i32(uniforms.extra.x);
    let ny = i32(uniforms.extra.y);
    if (cell_w <= 0.0 || cell_h <= 0.0 || nx < 1 || ny < 1) {
        return load_clamped(vec2<i32>(floor(position.xy)));
    }
    let u = position.x / cell_w;
    let v = position.y / cell_h;
    let k = clamp(floor(u), 0.0, f32(nx) - 1.0);
    let l = clamp(floor(v), 0.0, f32(ny) - 1.0);
    let s = u - k;
    let t = v - l;
    let alpha = uniforms.params.z;
    let ws = grid_warp_blend(s, alpha);
    let wt = grid_warp_blend(t, alpha);
    let w0s = 1.0 - ws;
    let w1s = ws;
    let w0t = 1.0 - wt;
    let w1t = wt;
    let i0 = i32(k);
    let j0 = i32(l);
    let amplitude = uniforms.params.w * min(cell_w, cell_h);
    let d00 = grid_warp_vertex_displacement(i0, j0, nx, ny, amplitude);
    let d10 = grid_warp_vertex_displacement(i0 + 1, j0, nx, ny, amplitude);
    let d01 = grid_warp_vertex_displacement(i0, j0 + 1, nx, ny, amplitude);
    let d11 = grid_warp_vertex_displacement(i0 + 1, j0 + 1, nx, ny, amplitude);
    let disp = w0s * w0t * d00 + w1s * w0t * d10 + w0s * w1t * d01 + w1s * w1t * d11;
    let q = position.xy + disp;
    return unpremultiply(sample_bilinear_premultiplied(q.x - 0.5, q.y - 0.5));
}
fn sample_nearest_premultiplied(x: f32, y: f32) -> vec4<f32> {
    let limit = uniforms.size.xy - vec2<f32>(1.0);
    let rounded = clamp(vec2<f32>(floor(x + 0.5), floor(y + 0.5)), vec2<f32>(0.0), limit);
    return premultiply(load_clamped(vec2<i32>(rounded)));
}

/// Ellipse-normalised magnification for one bulge sample.
fn bulge_magnification(distance: f32, height: f32, taper: f32, rim_pixels: f32) -> f32 {
    let factor = exp2(height);
    if (distance <= 1.0) {
        let falloff = 1.0 - distance * distance;
        return 1.0 + (factor - 1.0) * falloff * falloff;
    }
    if (taper <= 1.0e-6 || rim_pixels <= 1.0e-6) {
        return 1.0;
    }
    let beyond = (distance - 1.0) * rim_pixels;
    if (beyond >= taper) {
        return 1.0;
    }
    let falloff = 1.0 - beyond / taper;
    return 1.0 + (factor - 1.0) * falloff * falloff;
}

/// Maps one destination coordinate back into the source frame.
fn bulge_sample_position(coordinate: vec2<f32>) -> vec2<f32> {
    let centre = uniforms.params.xy;
    let radius = vec2<f32>(max(uniforms.params.z, 1.0e-6), max(uniforms.params.w, 1.0e-6));
    let height = uniforms.extra.x;
    let taper = uniforms.extra.y;
    let pinned = uniforms.extra.w > 0.5;
    let delta = coordinate - centre;
    let normalized = delta / radius;
    let distance = length(normalized);
    if (distance <= 1.0e-6) {
        return coordinate;
    }
    if (pinned && distance > 1.0) {
        return centre + normalized / distance * radius;
    }
    let rim = min(radius.x, radius.y);
    let magnification = max(bulge_magnification(distance, height, taper, rim), 1.0e-6);
    return centre + delta / magnification;
}

/// Normalised chromaticity `(r, g) / (r + g + b)` with the same fallback as the CPU key.
fn linear_key_chromaticity(rgb: vec3<f32>) -> vec2<f32> {
    let total = rgb.r + rgb.g + rgb.b;
    if (abs(total) <= 1.0e-6) {
        return vec2<f32>(0.0, 0.0);
    }
    return vec2<f32>(rgb.r / total, rgb.g / total);
}

/// Coverage the pixel keeps: `0` fully keyed away, `1` untouched.
fn linear_key_coverage(rgb: vec3<f32>) -> f32 {
    let key = uniforms.params.xyz;
    let tolerance = uniforms.params.w;
    let softness = uniforms.extra.x;
    let mode = u32(uniforms.extra.y);
    let operation = u32(uniforms.extra.z);
    var distance = 0.0;
    if (mode == 0u) {
        let difference = abs(rgb - key);
        distance = max(difference.r, max(difference.g, difference.b));
    } else if (mode == 1u) {
        distance = abs(rgb.g - key.g);
    } else if (mode == 2u) {
        distance = abs(rgb.b - key.b);
    } else {
        distance = length(linear_key_chromaticity(rgb) - linear_key_chromaticity(key));
    }
    var near = 0.0;
    if (softness <= 0.0) {
        if (distance <= tolerance) {
            near = 0.0;
        } else {
            near = 1.0;
        }
    } else {
        let ramp = clamp((distance - tolerance) / softness, 0.0, 1.0);
        near = ramp * ramp * (3.0 - 2.0 * ramp);
    }
    if (operation == 0u) {
        return near;
    }
    return 1.0 - near;
}

/// Linear colour key: moves coverage only, leaving the authored colour untouched.
/// Resolves one source column for the colour offset, matching the CPU index rules.
fn color_offset_index(value: f32, extent: f32, overflow: f32) -> i32 {
    let limit = max(extent - 1.0, 0.0);
    if (overflow < 1.5) {
        let floored = floor(value);
        let wrapped = floored - floor(floored / extent) * extent;
        return i32(clamp(wrapped, 0.0, limit));
    }
    if (overflow < 2.5) {
        return i32(floor(clamp(value, 0.0, limit) + 0.5));
    }
    let period = max(2.0 * limit, 1.0);
    let floored = floor(value);
    let folded = floored - floor(floored / period) * period;
    if (folded <= limit) {
        return i32(clamp(folded, 0.0, limit));
    }
    return i32(clamp(period - folded, 0.0, limit));
}

/// Reads one channel at a fractional column with bilinear interpolation.
fn color_offset_channel(column: f32, row: i32, channel: i32) -> f32 {
    let width = uniforms.size.x;
    let overflow = uniforms.params.w;
    let left = floor(column);
    let fraction = column - left;
    let low = load_clamped(vec2<i32>(color_offset_index(left, width, overflow), row))[channel];
    let high = load_clamped(vec2<i32>(color_offset_index(left + 1.0, width, overflow), row))[channel];
    return mix(low, high, fraction);
}

@fragment
fn fs_color_offset(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let original = load_clamped(pixel);
    if (original.a <= 0.0) {
        return original;
    }
    let column = f32(pixel.x);
    let shifted = vec3<f32>(
        color_offset_channel(column - uniforms.params.x, pixel.y, 0),
        color_offset_channel(column - uniforms.params.y, pixel.y, 1),
        color_offset_channel(column - uniforms.params.z, pixel.y, 2),
    );
    return vec4<f32>(shifted, original.a);
}
@fragment
fn fs_linear_color_key(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(max(position.xy, vec2<f32>(0.0))));
    let sample = load_clamped(pixel);
    if (sample.a <= 0.0) {
        return sample;
    }
    return vec4<f32>(sample.rgb, sample.a * linear_key_coverage(sample.rgb));
}

/// Split: samples one side of the authored line and slides it along the normal.
@fragment
fn fs_split(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = position.xy - vec2<f32>(0.5);
    let line = vec2<f32>(
        uniforms.params.z - uniforms.params.x,
        uniforms.params.w - uniforms.params.y,
    );
    let line_length = max(length(line), 1.0e-6);
    let normal = vec2<f32>(-line.y / line_length, line.x / line_length);
    let offset = pixel - vec2<f32>(uniforms.params.x, uniforms.params.y);
    let signed = dot(offset, normal);
    let half = uniforms.extra.x * 0.5;
    let shift = select(-half, half, signed >= 0.0);
    let sampled = pixel - normal * shift;
    return unpremultiply(sample_bilinear_premultiplied(sampled.x, sampled.y));
}

/// Wire removal: clones from a parallel displacement across the authored line.
@fragment
fn fs_wire_removal(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = position.xy - vec2<f32>(0.5);
    let a = vec2<f32>(uniforms.params.x, uniforms.params.y);
    let line = vec2<f32>(
        uniforms.params.z - uniforms.params.x,
        uniforms.params.w - uniforms.params.y,
    );
    let line_length = max(length(line), 1.0e-6);
    let tangent = line / line_length;
    let normal = vec2<f32>(-tangent.y, tangent.x);
    let thickness = uniforms.extra.x;
    let signed = dot(pixel - a, normal);
    if (abs(signed) > thickness * 0.5) {
        return load_clamped(vec2<i32>(floor(pixel)));
    }
    let k = 2.0 * uniforms.extra.y - 1.0;
    let direction = normalize(normal + tangent * k);
    let displacement = thickness;
    let style = u32(uniforms.extra.w + 0.5);
    var clone = pixel + direction * displacement;
    if (style == 1u) {
        clone = pixel - direction * displacement;
    }
    if (style == 3u) {
        let side = select(-1.0, 1.0, signed >= 0.0);
        clone = pixel - side * direction * displacement;
    }
    let mirrored = pixel - 2.0 * signed * normal;
    let aim = mix(clone, mirrored, uniforms.extra.z);
    if (style == 2u) {
        let other = pixel - direction * displacement;
        let aim_b = mix(other, mirrored, uniforms.extra.z);
        return unpremultiply(
            0.5 * (sample_bilinear_premultiplied(aim.x, aim.y)
                 + sample_bilinear_premultiplied(aim_b.x, aim_b.y)),
        );
    }
    return unpremultiply(sample_bilinear_premultiplied(aim.x, aim.y));
}

// Roughen edges: grows the silhouette along its own alpha gradient and styles
// the new rim. The field and coverage arithmetic follow
// `mondrian_effects::roughen_edges` in the same f32 order.
fn re_smoothstep01(value: f32) -> f32 {
    let t = clamp(value, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

/// One octave of the roughen edges value noise: the integer octave folds into
/// the salt exactly like the CPU lattice.
fn re_value_noise(x: f32, y: f32, z: i32, salt: u32) -> f32 {
    let octave_salt = salt ^ (bitcast<u32>(z) * 0x27D4EB2Fu);
    return turbulent_value_noise(x, y, octave_salt, false);
}

fn re_fbm(u: f32, v: f32, z: i32, complexity: u32, salt: u32) -> f32 {
    var total = 0.0;
    var total_weight = 0.0;
    var amplitude = 1.0;
    var frequency = 1.0;
    for (var octave = 0u; octave < complexity; octave = octave + 1u) {
        let index = f32(octave);
        total = total
            + amplitude
                * re_value_noise(
                    u * frequency + 17.0 * index,
                    v * frequency + 31.0 * index,
                    z,
                    salt ^ (octave * 0x85EBCA6Bu),
                );
        total_weight = total_weight + amplitude;
        amplitude = amplitude * 0.5;
        frequency = frequency * 2.0;
    }
    return total / total_weight;
}

fn re_field(u: f32, v: f32, salt: u32, t: f32) -> f32 {
    let z = floor(t);
    let weight = t - z;
    let blend = weight * weight * (3.0 - 2.0 * weight);
    let base = i32(z);
    let complexity = u32(uniforms.extra.w + 0.5);
    let first = re_fbm(u, v, base, complexity, salt);
    let second = re_fbm(u, v, base + 1, complexity, salt);
    return (1.0 - blend) * first + blend * second;
}

@fragment
fn fs_roughen_edges(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = position.xy - vec2<f32>(0.5);
    let index = vec2<i32>(pixel);
    let centre = load_clamped(index);
    let border = uniforms.params.x;
    if (border <= 0.0) {
        return centre;
    }
    let left = load_clamped(index + vec2<i32>(-1, 0)).a;
    let right = load_clamped(index + vec2<i32>(1, 0)).a;
    let up = load_clamped(index + vec2<i32>(0, -1)).a;
    let down = load_clamped(index + vec2<i32>(0, 1)).a;
    let gradient_x = (right - left) * 0.5;
    let gradient_y = (down - up) * 0.5;
    let gradient = sqrt(gradient_x * gradient_x + gradient_y * gradient_y);
    if (gradient <= 1.0e-6) {
        return centre;
    }

    let feature = 100.0 * uniforms.params.w / 1000.0;
    let stretch_x = max(1.0 + uniforms.extra.x / 100.0, 0.05);
    let u = (pixel.x + uniforms.extra.y) / (feature * stretch_x);
    let v = (pixel.y + uniforms.extra.z) / feature;
    let salt = 0x8F3A2C17u ^ (u32(uniforms.aces.w + 0.5) * 0x9E3779B9u);
    var t = uniforms.aces.x / 360.0;
    if (uniforms.aces.y > 0.5) {
        let cycle = uniforms.aces.z;
        t = t - cycle * floor(t / cycle);
    }

    let field = re_field(u, v, salt, t);
    let displacement = border * (1.0 + (0.5 * (field + 1.0) - 1.0) * uniforms.params.z);
    let aim = vec2<f32>(
        pixel.x + displacement * (gradient_x / gradient),
        pixel.y + displacement * (gradient_y / gradient),
    );
    let sampled = sample_bilinear_premultiplied(aim.x, aim.y);
    let sampled_alpha = sampled.a;

    let sharpening = (uniforms.params.y - 1.0) / 9.0;
    let strength = displacement / border;
    let hardened = sampled_alpha
        + sharpening * strength * (re_smoothstep01(sampled_alpha) - sampled_alpha);
    let grown_alpha = max(centre.a, clamp(hardened, 0.0, 1.0));
    let grown = clamp(grown_alpha - centre.a, 0.0, 1.0);

    let menu = u32(uniforms.extra1.x + 0.5);
    let base = (menu - 1u) / 2u;
    var amount = grown;
    if (base == 1u) {
        let rust = re_value_noise(
            u * 3.0 + 5.0,
            v * 3.0 + 11.0,
            i32(floor(t)),
            salt ^ 0x51ED2701u,
        );
        amount = grown * (0.35 + 0.65 * (0.5 * (rust + 1.0)));
    } else if (base == 2u) {
        amount = floor(grown * 4.0 + 0.5) / 4.0;
    } else if (base == 3u) {
        amount = re_smoothstep01((grown - 0.35) / 0.30);
    }

    let final_alpha = clamp(centre.a + amount, 0.0, 1.0);
    let own = sampled.rgb / max(sampled_alpha, 1.0e-6);
    let fill = amount / max(final_alpha, 1.0e-6);
    let tint = uniforms.extra1.yzw;
    let aim_colour = select(own, tint, ((menu - 1u) % 2u) == 1u);
    let blended = final_alpha * mix(own, aim_colour, fill);
    return unpremultiply(vec4<f32>(blended, final_alpha));
}

/// Bulge: magnifies an elliptical region of the frame.
@fragment
fn fs_bulge(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = position.xy - vec2<f32>(0.5);
    let antialias = uniforms.extra.z > 0.5;
    let offsets = array<vec2<f32>, 4>(
        vec2<f32>(-0.25, -0.25),
        vec2<f32>(0.25, -0.25),
        vec2<f32>(-0.25, 0.25),
        vec2<f32>(0.25, 0.25),
    );
    var accumulated = vec4<f32>(0.0);
    var taps = 1.0;
    if (antialias) {
        taps = 4.0;
        for (var index = 0; index < 4; index = index + 1) {
            let sampled = bulge_sample_position(pixel + offsets[index]);
            accumulated += sample_bilinear_premultiplied(sampled.x, sampled.y);
        }
    } else {
        let sampled = bulge_sample_position(pixel);
        accumulated = sample_bilinear_premultiplied(sampled.x, sampled.y);
    }
    return unpremultiply(accumulated / taps);
}

/// Ghost position along the light-centre axis for one lens preset.
fn flare_ghost_position(index: i32, kind: i32) -> f32 {
    if (kind == 2) {
        let positions = array<f32, 3>(0.55, 1.15, 1.70);
        return positions[index];
    }
    if (kind == 3) {
        let positions = array<f32, 2>(0.80, 1.60);
        return positions[index];
    }
    let positions = array<f32, 5>(0.45, 0.95, 1.40, 1.85, 2.35);
    return positions[index];
}

/// Ghost radius, as a fraction of the smaller frame dimension.
fn flare_ghost_radius(index: i32, kind: i32) -> f32 {
    if (kind == 2) {
        let radii = array<f32, 3>(0.045, 0.024, 0.080);
        return radii[index];
    }
    if (kind == 3) {
        let radii = array<f32, 2>(0.120, 0.180);
        return radii[index];
    }
    let radii = array<f32, 5>(0.030, 0.016, 0.055, 0.022, 0.070);
    return radii[index];
}

/// Ghost tint for one lens preset.
fn flare_ghost_tint(index: i32, kind: i32) -> vec3<f32> {
    if (kind == 2) {
        let tints = array<vec3<f32>, 3>(
            vec3<f32>(1.00, 0.75, 0.35),
            vec3<f32>(1.00, 0.60, 0.25),
            vec3<f32>(0.95, 0.55, 0.30),
        );
        return tints[index];
    }
    if (kind == 3) {
        let tints = array<vec3<f32>, 2>(
            vec3<f32>(0.85, 0.85, 1.00),
            vec3<f32>(0.70, 0.80, 1.00),
        );
        return tints[index];
    }
    let tints = array<vec3<f32>, 5>(
        vec3<f32>(0.55, 0.70, 1.00),
        vec3<f32>(0.70, 0.85, 1.00),
        vec3<f32>(0.45, 0.75, 0.95),
        vec3<f32>(0.80, 0.90, 1.00),
        vec3<f32>(0.35, 0.55, 0.90),
    );
    return tints[index];
}

/// Number of ghosts one lens preset draws.
fn flare_ghost_count(kind: i32) -> i32 {
    if (kind == 2) {
        return 3;
    }
    if (kind == 3) {
        return 2;
    }
    return 5;
}

/// Light emitted by the flare at one pixel, in linear light.
fn flare_emission(coordinate: vec2<f32>) -> vec3<f32> {
    let dims = uniforms.size.xy;
    if (dims.x <= 0.0 || dims.y <= 0.0) {
        return vec3<f32>(0.0);
    }
    let scale = min(dims.x, dims.y);
    let frame_centre = dims * 0.5;
    let light = uniforms.params.xy;
    let kind = i32(round(uniforms.extra.x));

    var core_radius = 0.045;
    var core_tint = vec3<f32>(0.95, 0.97, 1.00);
    var streak_length = 0.35;
    var streak_thickness = 0.012;
    if (kind == 2) {
        core_radius = 0.060;
        core_tint = vec3<f32>(1.00, 0.92, 0.78);
        streak_length = 0.25;
        streak_thickness = 0.008;
    } else if (kind == 3) {
        core_radius = 0.035;
        core_tint = vec3<f32>(1.00, 0.96, 0.90);
        streak_length = 0.55;
        streak_thickness = 0.010;
    }

    var emission = vec3<f32>(0.0);

    let core = exp(-0.5 * pow(length(coordinate - light) / max(core_radius * scale, 1.0e-6), 2.0));
    emission += core_tint * core;

    let across = abs(coordinate.y - light.y) / max(streak_thickness * scale, 1.0e-6);
    let along = abs(coordinate.x - light.x) / max(streak_length * dims.x, 1.0e-6);
    if (across < 4.0 && along < 1.0) {
        let streak = exp(-0.5 * across * across) * pow(1.0 - along, 2.0);
        emission += core_tint * streak * 0.85;
    }

    let axis = light - frame_centre;
    let count = flare_ghost_count(kind);
    for (var index = 0; index < 5; index = index + 1) {
        if (index >= count) {
            break;
        }
        let position = flare_ghost_position(index, kind);
        let ghost_centre = frame_centre + axis * (position - 1.0);
        let radius = flare_ghost_radius(index, kind) * scale;
        let separation = length(coordinate - ghost_centre);
        if (separation >= radius) {
            continue;
        }
        let normalized = separation / max(radius, 1.0e-6);
        let disc = 1.0 - smoothstep(0.6, 1.0, normalized);
        emission += flare_ghost_tint(index, kind) * disc * 0.35;
    }

    return emission;
}

/// Lens flare: adds camera flare light over the source frame.
fn lens_half_extent(direction: u32) -> f32 {
    if (direction == 1u) {
        return uniforms.size.x * 0.5;
    }
    if (direction == 2u) {
        return uniforms.size.y * 0.5;
    }
    return length(uniforms.size.xy) * 0.5;
}

fn lens_map_radius(radius: f32, focal: f32, invert: bool) -> f32 {
    if (invert) {
        return focal * tan(radius / focal);
    }
    return focal * atan(radius / focal);
}

fn lens_resize_scale(focal: f32, direction: u32, resize: u32, invert: bool) -> f32 {
    if (resize == 1u) {
        return 1.0;
    }
    let corner = length(uniforms.size.xy * 0.5);
    let mapped = lens_map_radius(corner, focal, invert);
    if (mapped <= 1.0e-9) {
        return 1.0;
    }
    let gain = corner / mapped;
    if (resize == 2u) {
        return gain;
    }
    if (resize == 3u) {
        return mapped / corner;
    }
    return max(gain, 1.0);
}

fn lens_sample_position(column: f32, row: f32, focal: f32, scale: f32, invert: bool) -> vec2<f32> {
    let centre = uniforms.params.xy;
    let delta = vec2<f32>(column, row) - centre;
    let radius = length(delta);
    if (radius <= 1.0e-9) {
        return centre;
    }
    let mapped = lens_map_radius(radius, focal, invert) * scale;
    return centre + delta * (mapped / radius);
}

fn ripple_displacement(distance: f32, radius: f32) -> f32 {
    let normalized = distance / radius;
    let envelope = 1.0 - normalized * normalized;
    let wavelength = max(uniforms.extra.x, 1.0e-6);
    let angle = 6.283185307179586 * (distance / wavelength - uniforms.extra.z);
    var wave = sin(angle);
    if (uniforms.params.w < 1.5) {
        wave = 0.5 + 0.5 * wave;
    }
    return uniforms.extra.y * envelope * envelope * wave;
}

@fragment
fn fs_ripple(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let centre = uniforms.params.xy;
    let radius = uniforms.params.z * min(uniforms.size.x, uniforms.size.y);
    if (uniforms.extra.y <= 1.0e-6 || radius <= 0.0) {
        return load_clamped(pixel);
    }
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let distance = length(delta);
    if (distance <= 0.0 || distance >= radius) {
        return unpremultiply(premultiply(load_clamped(pixel)));
    }
    let displacement = ripple_displacement(distance, radius);
    let scale = (distance + displacement) / distance;
    let sample = centre + delta * scale;
    return unpremultiply(sample_bilinear_premultiplied(sample.x, sample.y));
}

@fragment
fn fs_flip(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let centre = uniforms.params.xy;
    let weight = clamp(uniforms.params.z, 0.0, 1.0);
    let axis = uniforms.params.w;
    let column = f32(pixel.x);
    let row = f32(pixel.y);
    var sample_x = column;
    var sample_y = row;
    if (axis > 1.5 && axis < 2.5) {
        sample_x = 2.0 * centre.x - column;
    } else if (axis >= 2.5) {
        sample_x = 2.0 * centre.x - column;
        sample_y = 2.0 * centre.y - row;
    } else {
        sample_y = 2.0 * centre.y - row;
    }
    let flipped = sample_bilinear_premultiplied(sample_x, sample_y);
    let original = premultiply(load_clamped(pixel));
    let mixed = original * (1.0 - weight) + flipped * weight;
    return unpremultiply(mixed);
}

fn wave_warp_smoothstep01(value: f32) -> f32 {
    let clamped = clamp(value, 0.0, 1.0);
    return clamped * clamped * (3.0 - 2.0 * clamped);
}

fn wave_warp_noise_unit(cell: i32) -> f32 {
    let mixed = bitcast<u32>(cell) * 2654435761u;
    let rotated = ((mixed << 13u) | (mixed >> 19u)) ^ 0x9e3779b9u;
    return f32(rotated >> 8u) / 16777215.0;
}

fn wave_warp_value(wave_type: u32, turns: f32) -> f32 {
    let cell = floor(turns);
    let fraction = turns - cell;
    if (wave_type == 0u) {
        return sin(6.283185307179586 * fraction);
    }
    if (wave_type == 1u) {
        if (fraction < 0.5) {
            return 1.0;
        }
        return -1.0;
    }
    if (wave_type == 2u) {
        return 1.0 - 4.0 * abs(fraction - 0.5);
    }
    if (wave_type == 3u) {
        return 2.0 * fraction - 1.0;
    }
    if (wave_type == 4u) {
        let offset = 2.0 * fraction - 1.0;
        return 2.0 * sqrt(max(1.0 - offset * offset, 0.0)) - 1.0;
    }
    if (wave_type == 5u) {
        let offset = 2.0 * fraction - 1.0;
        return sqrt(max(1.0 - offset * offset, 0.0));
    }
    if (wave_type == 6u) {
        let offset = 2.0 * fraction - 1.0;
        return 1.0 - 2.0 * sqrt(max(1.0 - offset * offset, 0.0));
    }
    if (wave_type == 7u) {
        return 2.0 * wave_warp_noise_unit(i32(cell)) - 1.0;
    }
    let left = wave_warp_noise_unit(i32(cell));
    let right = wave_warp_noise_unit(i32(cell) + 1);
    return 2.0 * (left + (right - left) * wave_warp_smoothstep01(fraction)) - 1.0;
}

fn wave_warp_pin_damping(pin: u32, coord: vec2<f32>) -> f32 {
    let dims = vec2<f32>(frame_dims());
    let normalized = (coord + vec2<f32>(0.5, 0.5)) / dims;
    let margin = 0.2;
    let left = wave_warp_smoothstep01(normalized.x / margin);
    let right = wave_warp_smoothstep01((1.0 - normalized.x) / margin);
    let top = wave_warp_smoothstep01(normalized.y / margin);
    let bottom = wave_warp_smoothstep01((1.0 - normalized.y) / margin);
    if (pin == 0u) {
        return 1.0;
    }
    if (pin == 1u) {
        return min(min(left, right), min(top, bottom));
    }
    if (pin == 2u) {
        let centre = 0.5 * (dims - vec2<f32>(1.0, 1.0));
        let radius = 0.25 * min(dims.x, dims.y);
        return wave_warp_smoothstep01(length(coord - centre) / radius);
    }
    if (pin == 3u) {
        return left;
    }
    if (pin == 4u) {
        return top;
    }
    if (pin == 5u) {
        return right;
    }
    if (pin == 6u) {
        return bottom;
    }
    if (pin == 7u) {
        return min(top, bottom);
    }
    return min(left, right);
}

@fragment
fn fs_wave_warp(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let crest = uniforms.params.x;
    if (abs(crest) <= 1.0e-9) {
        return load_clamped(pixel);
    }
    let wavelength = max(uniforms.params.y, 1.0e-6);
    let direction = uniforms.params.z;
    let phase = uniforms.params.w;
    let axis = vec2<f32>(sin(direction), -cos(direction));
    let wave_type = u32(uniforms.extra.x + 0.5);
    let pin = u32(uniforms.extra.y + 0.5);
    let samples = max(u32(uniforms.extra.z + 0.5), 1u);
    let step = 1.0 / f32(samples);
    let origin = position.xy - vec2<f32>(0.5, 0.5);
    var accumulated = vec4<f32>(0.0);
    for (var sample_row = 0u; sample_row < samples; sample_row = sample_row + 1u) {
        let offset_y = (f32(sample_row) + 0.5) * step - 0.5;
        for (var sample_column = 0u; sample_column < samples; sample_column = sample_column + 1u) {
            let offset_x = (f32(sample_column) + 0.5) * step - 0.5;
            let coord = origin + vec2<f32>(offset_x, offset_y);
            let along = dot(coord, axis);
            let damping = wave_warp_pin_damping(pin, coord);
            let displacement =
                crest * wave_warp_value(wave_type, along / wavelength - phase) * damping;
            let sample = coord + axis * displacement;
            accumulated = accumulated + sample_bilinear_premultiplied(sample.x, sample.y);
        }
    }
    return unpremultiply(accumulated * (step * step));
}

@fragment
fn fs_transform(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let samples = max(u32(uniforms.extra.w + 0.5), 1u);
    let step = 1.0 / f32(samples);
    let opacity = uniforms.extra.z;
    // `position.xy` is index + 0.5, so subtracting the half texel yields the
    // continuous pixel-centre coordinates the inverse matrix expects.
    let p = position.xy - vec2<f32>(0.5, 0.5);
    let c = 0.5 * (uniforms.size.xy - vec2<f32>(1.0, 1.0));
    let origin = p - c;
    var accumulated = vec4<f32>(0.0);
    for (var sample_row = 0u; sample_row < samples; sample_row = sample_row + 1u) {
        let offset_y = (f32(sample_row) + 0.5) * step - 0.5;
        for (var sample_column = 0u; sample_column < samples; sample_column = sample_column + 1u) {
            let offset_x = (f32(sample_column) + 0.5) * step - 0.5;
            let v = origin + vec2<f32>(offset_x, offset_y);
            let sample_x =
                uniforms.params.x * v.x + uniforms.params.y * v.y + uniforms.params.z + c.x;
            let sample_y =
                uniforms.params.w * v.x + uniforms.extra.x * v.y + uniforms.extra.y + c.y;
            accumulated = accumulated + sample_bilinear_premultiplied(sample_x, sample_y);
        }
    }
    return unpremultiply(accumulated * (step * step) * opacity);
}

@fragment
fn fs_corner_pin(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    // `position.xy` is index + 0.5, so subtracting the half texel yields the
    // continuous pixel-centre coordinates the inverse homography expects.
    let p = position.xy - vec2<f32>(0.5, 0.5);
    let denom = max(uniforms.size.xy - vec2<f32>(1.0, 1.0), vec2<f32>(1.0, 1.0));
    let d = p / denom;
    // `params` holds [i00, i01, i02, i10] and `extra` holds
    // [i11, i12, i20, i21]; `i22` is normalized to one.
    let third = uniforms.extra.z * d.x + uniforms.extra.w * d.y + 1.0;
    if (abs(third) <= 1.0e-12) {
        return vec4<f32>(0.0);
    }
    let s = vec2<f32>(
        (uniforms.params.x * d.x + uniforms.params.y * d.y + uniforms.params.z) / third,
        (uniforms.params.w * d.x + uniforms.extra.x * d.y + uniforms.extra.y) / third,
    );
    if (s.x < -1.0e-6 || s.x > 1.0 + 1.0e-6 || s.y < -1.0e-6 || s.y > 1.0 + 1.0e-6) {
        return vec4<f32>(0.0);
    }
    // Fold the denormalisation back into the numerator and sample in source
    // pixel space directly. Reconstructing `s * denom` costs a second f32
    // rounding of about one micro-pixel, which is enough for a fully
    // transparent pixel to pick up a sliver of its neighbour and unpremultiply
    // that sliver into a visible colour.
    let sample = vec2<f32>(
        (uniforms.params.x * p.x
            + uniforms.params.y * p.y * (denom.x / denom.y)
            + uniforms.params.z * denom.x)
            / third,
        (uniforms.params.w * p.x * (denom.y / denom.x)
            + uniforms.extra.x * p.y
            + uniforms.extra.y * denom.y)
            / third,
    );
    return unpremultiply(sample_bilinear_premultiplied(sample.x, sample.y));
}

// The turbulent displace field. The hash, the octave sum and the displacement
// formulas are transcribed from `mondrian_effects::turbulent_displace` in the
// same f32 arithmetic order, because a single different rounding of the field
// is amplified by the pixel-scale displacement.
fn turbulent_hash(x: u32, y: u32, salt: u32) -> u32 {
    var hash = x * 0x9E3779B9u ^ y * 0x85EBCA6Bu ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    return hash ^ (hash >> 16u);
}

fn turbulent_sample_value(hash: u32) -> f32 {
    let bits = (hash >> 8u) & 0xFFFFu;
    return f32(bits) / 65535.0 * 2.0 - 1.0;
}

fn turbulent_weight(t: f32, smoother: bool) -> f32 {
    if (smoother) {
        return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
    }
    return t * t * (3.0 - 2.0 * t);
}

fn turbulent_value_noise(x: f32, y: f32, salt: u32, smoother: bool) -> f32 {
    let cell_x = floor(x);
    let cell_y = floor(y);
    let fraction_x = x - cell_x;
    let fraction_y = y - cell_y;
    let weight_x = turbulent_weight(fraction_x, smoother);
    let weight_y = turbulent_weight(fraction_y, smoother);
    let x0 = bitcast<u32>(i32(cell_x));
    let y0 = bitcast<u32>(i32(cell_y));
    let x1 = bitcast<u32>(i32(cell_x) + 1);
    let y1 = bitcast<u32>(i32(cell_y) + 1);
    let top_left = turbulent_sample_value(turbulent_hash(x0, y0, salt));
    let top_right = turbulent_sample_value(turbulent_hash(x1, y0, salt));
    let bottom_left = turbulent_sample_value(turbulent_hash(x0, y1, salt));
    let bottom_right = turbulent_sample_value(turbulent_hash(x1, y1, salt));
    let top = top_left + (top_right - top_left) * weight_x;
    let bottom = bottom_left + (bottom_right - bottom_left) * weight_x;
    return top + (bottom - top) * weight_y;
}

fn turbulent_field(u: f32, v: f32, salt: u32, complexity: u32, smoother: bool) -> f32 {
    var octaves = max(complexity, 1u);
    if (smoother) {
        octaves = max(complexity / 2u, 1u);
    }
    var total = 0.0;
    var total_weight = 0.0;
    var amplitude = 0.5;
    var frequency = 1.0;
    for (var octave = 0u; octave < octaves; octave = octave + 1u) {
        let octave_salt = salt ^ (octave * 0x9E3779B9u);
        total = total
            + amplitude * turbulent_value_noise(u * frequency, v * frequency, octave_salt, smoother);
        total_weight = total_weight + amplitude;
        frequency = frequency * 2.0;
        amplitude = amplitude * 0.5;
    }
    return total / total_weight;
}

fn turbulent_smoothstep01(value: f32) -> f32 {
    let t = clamp(value, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn turbulent_pin_damping(pin: u32, coord: vec2<f32>) -> f32 {
    let dims = vec2<f32>(frame_dims());
    let normalized = (coord + vec2<f32>(0.5, 0.5)) / dims;
    let margin = 0.2;
    let left = turbulent_smoothstep01(normalized.x / margin);
    let right = turbulent_smoothstep01((1.0 - normalized.x) / margin);
    let top = turbulent_smoothstep01(normalized.y / margin);
    let bottom = turbulent_smoothstep01((1.0 - normalized.y) / margin);
    if (pin == 0u) {
        return 1.0;
    }
    if (pin == 1u) {
        return min(min(left, right), min(top, bottom));
    }
    if (pin == 2u) {
        let centre = 0.5 * (dims - vec2<f32>(1.0, 1.0));
        let radius = 0.25 * min(dims.x, dims.y);
        return turbulent_smoothstep01(length(coord - centre) / radius);
    }
    if (pin == 3u) {
        return left;
    }
    if (pin == 4u) {
        return top;
    }
    if (pin == 5u) {
        return right;
    }
    if (pin == 6u) {
        return bottom;
    }
    if (pin == 7u) {
        return min(top, bottom);
    }
    return min(left, right);
}

// Reflect one coordinate about the frame edges, applied once.
fn turbulent_mirror(value: f32, extent: f32) -> f32 {
    if (value < 0.0) {
        return -value;
    }
    if (value > extent) {
        return 2.0 * extent - value;
    }
    return value;
}

struct TurbulentField {
    kind: u32,
    amount: f32,
    size_: f32,
    offset: vec2<f32>,
    evolution: vec2<f32>,
    salt: u32,
    complexity: u32,
    smoother: bool,
    centre: vec2<f32>,
    pin: u32,
};

fn turbulent_domain(field: TurbulentField, coord: vec2<f32>) -> vec2<f32> {
    return (coord - field.offset) / field.size_ + field.evolution;
}

fn turbulent_eval(field: TurbulentField, u: f32, v: f32) -> f32 {
    return turbulent_field(u, v, field.salt, field.complexity, field.smoother);
}

fn turbulent_displacement(field: TurbulentField, coord: vec2<f32>) -> vec2<f32> {
    let uv = turbulent_domain(field, coord);
    let u = uv.x;
    let v = uv.y;
    if (field.kind == 0u || field.kind == 3u) {
        // The gradient of the field in domain units, so the warp follows the
        // contour of the noise instead of stretching along one axis. The
        // quotient is not multiplied by `size_`: one domain unit is `size_`
        // pixels wide, and scaling by it would turn the default grade into a
        // thousands-of-pixels smear instead of the tens `amount` promises.
        let h = 0.001;
        let difference_x = turbulent_eval(field, u + h, v) - turbulent_eval(field, u - h, v);
        let difference_y = turbulent_eval(field, u, v + h) - turbulent_eval(field, u, v - h);
        return vec2<f32>(
            field.amount * 0.5 * difference_x / (2.0 * h),
            field.amount * 0.5 * difference_y / (2.0 * h),
        );
    }
    if (field.kind == 1u || field.kind == 4u) {
        let radial = (coord - field.centre) / max(length(coord - field.centre), 1.0e-6);
        return radial * field.amount * turbulent_eval(field, u, v);
    }
    if (field.kind == 2u || field.kind == 5u) {
        let radial = (coord - field.centre) / max(length(coord - field.centre), 1.0e-6);
        let value = turbulent_eval(field, u, v);
        return vec2<f32>(-radial.y * field.amount * value, radial.x * field.amount * value);
    }
    if (field.kind == 6u) {
        return vec2<f32>(0.0, field.amount * turbulent_eval(field, u, v));
    }
    if (field.kind == 7u) {
        return vec2<f32>(field.amount * turbulent_eval(field, u, v), 0.0);
    }
    return vec2<f32>(
        field.amount * turbulent_eval(field, u, v),
        field.amount * turbulent_eval(field, u + 5.2, v + 1.3),
    );
}

@fragment
fn fs_turbulent_displace(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let amount = uniforms.params.x;
    if (amount == 0.0) {
        return load_clamped(pixel);
    }
    let dims = vec2<f32>(frame_dims());
    // `position.xy` is index + 0.5, so subtracting the half texel yields the
    // continuous pixel-centre coordinates the field is defined on.
    let p = position.xy - vec2<f32>(0.5, 0.5);
    let evolution = uniforms.extra.x;
    let cycle = uniforms.extra.z;
    var evolution_offset = vec2<f32>(evolution * 2.0, evolution * 0.618);
    if (cycle > 0.0) {
        let theta = 6.283185307179586 * evolution * cycle;
        evolution_offset = vec2<f32>(cos(theta), sin(theta));
    }
    let kind = u32(uniforms.aces.x + 0.5);
    let field = TurbulentField(
        kind,
        amount,
        uniforms.params.y,
        uniforms.params.zw,
        evolution_offset,
        0x85EBCA6Bu ^ u32(uniforms.aces.y + 0.5),
        u32(uniforms.extra.y + 0.5),
        kind == 3u || kind == 4u || kind == 5u,
        0.5 * (dims - vec2<f32>(1.0, 1.0)),
        u32(uniforms.extra.w + 0.5),
    );
    let samples = max(u32(uniforms.aces.w + 0.5), 1u);
    let step = 1.0 / f32(samples);
    let resize_layer = uniforms.aces.z;
    var accumulated = vec4<f32>(0.0);
    for (var sample_row = 0u; sample_row < samples; sample_row = sample_row + 1u) {
        let offset_y = (f32(sample_row) + 0.5) * step - 0.5;
        for (var sample_column = 0u; sample_column < samples; sample_column = sample_column + 1u) {
            let offset_x = (f32(sample_column) + 0.5) * step - 0.5;
            let coord = p + vec2<f32>(offset_x, offset_y);
            let damping = turbulent_pin_damping(field.pin, coord);
            let warped = coord + turbulent_displacement(field, coord) * damping;
            let clamped = sample_bilinear_premultiplied(warped.x, warped.y);
            var sample = clamped;
            if (resize_layer > 0.0) {
                let mirrored_target = vec2<f32>(
                    turbulent_mirror(warped.x, dims.x - 1.0),
                    turbulent_mirror(warped.y, dims.y - 1.0),
                );
                let mirrored =
                    sample_bilinear_premultiplied(mirrored_target.x, mirrored_target.y);
                if (resize_layer >= 1.0) {
                    sample = mirrored;
                } else {
                    sample = mix(clamped, mirrored, resize_layer);
                }
            }
            accumulated = accumulated + sample;
        }
    }
    return unpremultiply(accumulated * (step * step));
}

@fragment
fn fs_lens_distortion(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let original = load_clamped(pixel);
    let field_of_view = uniforms.params.z;
    if (field_of_view <= 1.0e-6) {
        return original;
    }
    let direction = u32(uniforms.extra.x + 0.5);
    let resize = u32(uniforms.extra.y + 0.5);
    let invert = uniforms.params.w > 0.5;
    let half_extent = lens_half_extent(direction);
    let half_angle = field_of_view * 3.141592653589793 / 360.0;
    let focal = half_extent / tan(half_angle);
    let scale = lens_resize_scale(focal, direction, resize, invert);
    let base = position.xy - vec2<f32>(0.5);
    let taps: i32 = select(1, 2, uniforms.extra.z > 0.5);
    let step = 1.0 / f32(taps);
    var accumulated = vec4<f32>(0.0);
    for (var sub_y = 0; sub_y < taps; sub_y = sub_y + 1) {
        for (var sub_x = 0; sub_x < taps; sub_x = sub_x + 1) {
            let offset = (vec2<f32>(f32(sub_x) + 0.5, f32(sub_y) + 0.5)) * step - vec2<f32>(0.5);
            let destination = base + offset;
            let source = lens_sample_position(destination.x, destination.y, focal, scale, invert);
            accumulated += sample_bilinear_premultiplied(source.x, source.y);
        }
    }
    return unpremultiply(accumulated / f32(taps * taps));
}
@fragment
fn fs_chromatic_aberration_removal(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = floor(position.xy);
    let original = load_clamped(vec2<i32>(pixel));
    let extent = uniforms.size.xy;
    let amounts = uniforms.params.zw;
    if (any(extent < vec2<f32>(2.0)) || all(abs(amounts) <= vec2<f32>(1.0e-6)) || !(original.a > 0.0)) {
        return original;
    }

    let centre = uniforms.params.xy * extent - vec2<f32>(0.5);
    let delta = pixel - centre;
    let distance = length(delta);
    if (distance == 0.0) {
        return original;
    }
    let edges = max(abs(centre), abs(extent - vec2<f32>(1.0) - centre));
    let radius = max(max(edges.x, edges.y), 1.0);
    let direction = delta / distance;
    let weight = 1.0 + uniforms.extra.x / 10.0 * min(distance / radius, 1.0);
    let shift = dot(amounts, direction * direction) / radius * weight;
    if (shift == 0.0) {
        return original;
    }
    let red_position = centre + delta * max(1.0 + shift, 0.25);
    let blue_position = centre + delta * max(1.0 - shift, 0.25);
    let red_sample = sample_bilinear_premultiplied(red_position.x, red_position.y);
    let blue_sample = sample_bilinear_premultiplied(blue_position.x, blue_position.y);
    var result = original;
    if (red_sample.a > 0.0) {
        result.r = red_sample.r / red_sample.a;
    }
    if (blue_sample.a > 0.0) {
        result.b = blue_sample.b / blue_sample.a;
    }
    return result;
}

@fragment
fn fs_basic_3d(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let centre = uniforms.size.xy * 0.5;
    let inverse_focal_length = uniforms.params.w;
    let pixel_centre = floor(position.xy) + vec2<f32>(0.5);
    let destination = (pixel_centre - centre) * inverse_focal_length;
    let homogeneous = vec3<f32>(destination, 1.0);
    let reciprocal_depth = dot(uniforms.aces.xyz, homogeneous);
    if (!(reciprocal_depth > 0.0 && reciprocal_depth < uniforms.aces.w)) {
        return vec4<f32>(0.0);
    }
    let source_point = vec2<f32>(
        dot(uniforms.params.xyz, homogeneous),
        dot(uniforms.extra.xyz, homogeneous)
    ) / reciprocal_depth;
    let bounds = centre * inverse_focal_length + vec2<f32>(uniforms.extra1.w);
    if (!all(abs(source_point) <= bounds)) {
        return vec4<f32>(0.0);
    }
    let focal_length = max(uniforms.size.x, uniforms.size.y);
    let texel = centre + source_point * focal_length - vec2<f32>(0.5);
    let sample = unpremultiply(sample_bilinear_premultiplied(texel.x, texel.y));
    let strength = uniforms.extra.w;
    if (strength > 0.0 && sample.a > 0.0) {
        let light_direction = normalize(vec3<f32>(0.35, -0.45, -1.0));
        let view_direction = normalize(vec3<f32>(-destination, -1.0));
        let face = uniforms.extra1.xyz;
        let normal = select(face, -face, dot(face, view_direction) < 0.0);
        let alignment = max(dot(normal, normalize(light_direction + view_direction)), 0.0);
        let squared = alignment * alignment;
        let fourth = squared * squared;
        return vec4<f32>(sample.rgb + vec3<f32>(strength * (fourth * fourth)), sample.a);
    }
    return sample;
}

@fragment
fn fs_camera_view(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let centre = uniforms.size.xy * 0.5;
    let inverse_focal_reference = uniforms.params.z;
    let min_depth = uniforms.params.w;
    let centre_texel = centre - vec2<f32>(0.5);
    let destination = floor(position.xy) - centre_texel;
    let normalized = destination * inverse_focal_reference;
    let depth = dot(uniforms.aces.xyz, vec3<f32>(normalized, 1.0));
    if (uniforms.aces.z <= min_depth || depth <= min_depth) {
        return uniforms.extra1;
    }
    let source_offset = vec2<f32>(
        dot(uniforms.params.xy, destination),
        dot(uniforms.extra.xy, destination)
    ) / depth;
    let bounds = centre + vec2<f32>(uniforms.extra.z);
    if (!all(abs(source_offset) <= bounds)) {
        return uniforms.extra1;
    }
    let texel = centre_texel + source_offset;
    return unpremultiply(sample_bilinear_premultiplied(texel.x, texel.y));
}

@fragment
fn fs_lens_flare(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let sample = load_clamped(vec2<i32>(position.xy));
    let coverage = clamp(sample.a, 0.0, 1.0);
    if (coverage <= 0.0) {
        return vec4<f32>(0.0);
    }
    // Light is added in the premultiplied domain, so transparent pixels never
    // glow and coverage is never invented; the texture carries straight alpha.
    let emission = flare_emission(position.xy) * uniforms.params.z * (1.0 - uniforms.params.w);
    return vec4<f32>(sample.rgb + emission / coverage, coverage);
}
@fragment
fn fs_polar(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let size = uniforms.size.xy;
    let centre = size * 0.5;
    let position_f = vec2<f32>(f32(pixel.x), f32(pixel.y));
    var source_position = vec2<f32>(0.0);
    if (uniforms.params.x < 1.5) {
        let angle = position_f.x / size.x * 6.283185307179586;
        let radius = position_f.y;
        source_position = centre + vec2<f32>(radius * cos(angle), radius * sin(angle));
    } else {
        let delta = position_f - centre;
        let radius = length(delta);
        var fraction = atan2(delta.y, delta.x) / 6.283185307179586;
        if (fraction < 0.0) {
            fraction = fraction + 1.0;
        }
        source_position = vec2<f32>(fraction * size.x, radius);
    }
    if (uniforms.params.y < 0.5) {
        return unpremultiply(sample_nearest_premultiplied(source_position.x, source_position.y));
    }
    return unpremultiply(sample_bilinear_premultiplied(source_position.x, source_position.y));
}
@fragment
fn fs_mirror(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let centre = uniforms.params.xy;
    let angle = radians(uniforms.params.z);
    let normal = vec2<f32>(-sin(angle), cos(angle));
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let signed = dot(delta, normal);
    if (signed >= 0.0) {
        return load_clamped(pixel);
    }
    let sample_x = f32(pixel.x) - 2.0 * signed * normal.x;
    let sample_y = f32(pixel.y) - 2.0 * signed * normal.y;
    return unpremultiply(sample_bilinear_premultiplied(sample_x, sample_y));
}
@fragment
fn fs_twirl(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = frame_dims();
    let pixel = vec2<i32>(position.xy);
    let centre = uniforms.params.xy;
    let radius = uniforms.params.z * f32(min(dims.x, dims.y));
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let distance = length(delta);
    if (radius <= 0.0 || distance >= radius) {
        return load_clamped(pixel);
    }
    let normalized = distance / radius;
    let falloff = 1.0 - normalized * normalized;
    let rotation = radians(uniforms.params.w) * falloff * falloff;
    let sine = sin(rotation);
    let cosine = cos(rotation);
    let sample_x = centre.x + delta.x * cosine + delta.y * sine;
    let sample_y = centre.y - delta.x * sine + delta.y * cosine;
    return unpremultiply(sample_bilinear_premultiplied(sample_x, sample_y));
}
@fragment
fn fs_offset(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let centre = premultiply(load_clamped(pixel));
    let fragment = vec2<f32>(f32(pixel.x), f32(pixel.y)) - uniforms.params.xy;
    let shifted = sample_bilinear_wrapped(fragment);
    let blend = clamp(uniforms.params.z, 0.0, 1.0);
    return unpremultiply(shifted * (1.0 - blend) + centre * blend);
}
@fragment
fn fs_radial_blur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let centre = uniforms.params.xy;
    let mode = uniforms.params.w;
    let amount = uniforms.extra.x;
    let seed = u32(max(uniforms.extra.y, 0.0));
    let taps = max(i32(uniforms.params.z), 1);
    let weight = 1.0 / f32(taps);
    let tap_step = 2.0 / f32(taps);
    // Half of the authored sweep, in the same units as the CPU kernel:
    // 0.36 degrees per amount unit for spin, ln(1 + 0.002 * amount) for zoom.
    let half_span = amount * 0.003141592653589793;
    let log_span = log(1.0 + amount * 0.002) * 0.5;
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let jitter = radial_hash_jitter(seed, u32(pixel.x), u32(pixel.y));
    var sum = vec4<f32>(0.0);
    for (var tap = 0; tap < taps; tap = tap + 1) {
        let base = (f32(tap) + 0.5) * tap_step - 1.0;
        let along = clamp(base + jitter * tap_step, -1.0, 1.0);
        var sample_at = vec2<f32>(0.0);
        if (mode < 1.5) {
            let angle = along * half_span;
            let c = cos(angle);
            let s = sin(angle);
            sample_at = centre + vec2<f32>(delta.x * c - delta.y * s, delta.x * s + delta.y * c);
        } else {
            let scale = exp(along * log_span);
            sample_at = centre + delta * scale;
        }
        sum = sum + sample_bilinear_premultiplied(sample_at.x, sample_at.y);
    }
    return unpremultiply(sum * weight);
}

// The blend helpers below mirror the compositor's WGSL helpers in
// `crates/mondrian-renderer/src/gpu_compositor.rs` and the CPU
// `blend_mode_rgb` they were written against; they must stay in sync.
fn blend_lum(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.299, 0.587, 0.114));
}

fn clip_blend_color(color: vec3<f32>) -> vec3<f32> {
    let luminance = blend_lum(color);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (minimum < 0.0) {
        let factor = luminance / (luminance - minimum);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    if (maximum > 1.0) {
        let factor = (1.0 - luminance) / (maximum - luminance);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    return color;
}

fn set_blend_lum(color: vec3<f32>, target_lum: f32) -> vec3<f32> {
    return clip_blend_color(color + vec3<f32>(target_lum - blend_lum(color)));
}

fn blend_sat(color: vec3<f32>) -> f32 {
    return max(color.r, max(color.g, color.b)) - min(color.r, min(color.g, color.b));
}

fn set_blend_sat(color: vec3<f32>, target_sat: f32) -> vec3<f32> {
    var result = vec3<f32>(0.0);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (maximum <= minimum) {
        return result;
    }
    let scale = target_sat / (maximum - minimum);
    if (color.r <= color.g && color.r <= color.b) {
        if (color.g <= color.b) {
            result = vec3<f32>(0.0, (color.g - minimum) * scale, target_sat);
        } else {
            result = vec3<f32>(0.0, target_sat, (color.b - minimum) * scale);
        }
    } else if (color.g <= color.r && color.g <= color.b) {
        if (color.r <= color.b) {
            result = vec3<f32>((color.r - minimum) * scale, 0.0, target_sat);
        } else {
            result = vec3<f32>(target_sat, 0.0, (color.b - minimum) * scale);
        }
    } else if (color.r <= color.g) {
        result = vec3<f32>((color.r - minimum) * scale, target_sat, 0.0);
    } else {
        result = vec3<f32>(target_sat, (color.g - minimum) * scale, 0.0);
    }
    return result;
}

fn blend_channel(mode: u32, base: f32, blend: f32) -> f32 {
    if (mode == 2u) { return base * blend; }
    if (mode == 3u) { return 1.0 - (1.0 - base) * (1.0 - blend); }
    if (mode == 4u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, base <= 0.5);
    }
    if (mode == 5u) { return min(base, blend); }
    if (mode == 6u) { return max(base, blend); }
    if (mode == 7u) {
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (1.0 - blend), 0.0, 1.0);
    }
    if (mode == 8u) {
        if (blend <= 0.001) { return 0.0; }
        return clamp(1.0 - (1.0 - base) / blend, 0.0, 1.0);
    }
    if (mode == 9u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, blend <= 0.5);
    }
    if (mode == 10u) {
        if (blend <= 0.5) { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
        var curved = ((16.0 * base - 12.0) * base + 4.0) * base;
        if (base > 0.25) { curved = sqrt(base); }
        return base + (2.0 * blend - 1.0) * (curved - base);
    }
    if (mode == 11u) { return abs(base - blend); }
    if (mode == 12u) { return base + blend - 2.0 * base * blend; }
    if (mode == 13u) { return clamp(base - blend, 0.0, 1.0); }
    if (mode == 16u) { return clamp(base + blend - 1.0, 0.0, 1.0); }
    if (mode == 17u) { return clamp(base + blend, 0.0, 1.0); }
    if (mode == 18u) {
        if (blend <= 0.5) {
            if (blend <= 0.001) { return 0.0; }
            return clamp(1.0 - (1.0 - base) / (2.0 * blend), 0.0, 1.0);
        }
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (2.0 * (1.0 - blend)), 0.0, 1.0);
    }
    if (mode == 19u) { return clamp(base + 2.0 * blend - 1.0, 0.0, 1.0); }
    if (mode == 20u) { return select(max(base, 2.0 * (blend - 0.5)), min(base, 2.0 * blend), blend <= 0.5); }
    if (mode == 21u) { return select(1.0, 0.0, base + blend < 1.0); }
    if (mode == 22u) {
        if (blend <= 0.001) { return 1.0; }
        return clamp(base / blend, 0.0, 1.0);
    }
    return blend;
}

fn blend_rgb(mode: u32, base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    if (mode == 14u) { return select(base, blend, blend_lum(blend) < blend_lum(base)); }
    if (mode == 15u) { return select(base, blend, blend_lum(blend) > blend_lum(base)); }
    if (mode == 23u) { return set_blend_lum(set_blend_sat(blend, blend_sat(base)), blend_lum(base)); }
    if (mode == 24u) { return set_blend_lum(set_blend_sat(base, blend_sat(blend)), blend_lum(base)); }
    if (mode == 25u) { return set_blend_lum(blend, blend_lum(base)); }
    if (mode == 26u) { return set_blend_lum(base, blend_lum(blend)); }
    return vec3<f32>(
        blend_channel(mode, base.r, blend.r),
        blend_channel(mode, base.g, blend.g),
        blend_channel(mode, base.b, blend.b),
    );
}

// The magnify lens mirrors `apply_magnify_f32_controlled`: the inverse map is a
// contraction by the magnification, the feather ramps inward from the rim, and
// the supersampled footprint is averaged in premultiplied space before one
// un-premultiply and the same blend composite the CPU path performs.
@fragment
fn fs_magnify(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let base = load_clamped(pixel);
    let centre = uniforms.params.xy;
    let radius = uniforms.params.z;
    let magnification = uniforms.params.w;
    let feather = uniforms.extra.x;
    let opacity = uniforms.extra.y;
    let square = uniforms.extra.z > 0.5;
    let authored_density = u32(round(uniforms.extra.w));
    let density = clamp(authored_density, 1u, 4u);
    let delta = vec2<f32>(f32(pixel.x), f32(pixel.y)) - centre;
    let distance = select(
        sqrt(delta.x * delta.x + delta.y * delta.y),
        max(abs(delta.x), abs(delta.y)),
        square,
    );
    var weight = 0.0;
    if (feather <= 0.0) {
        if (distance < radius) {
            weight = 1.0;
        }
    } else {
        let t = clamp((radius - distance) / feather, 0.0, 1.0);
        weight = t * t * (3.0 - 2.0 * t);
    }
    let blend_opacity = clamp(opacity * weight, 0.0, 1.0);
    if (weight <= 0.0 || radius <= 0.0 || magnification <= 0.0 || blend_opacity <= 0.0) {
        return base;
    }
    let inverse_magnification = 1.0 / magnification;
    var accumulated = vec4<f32>(0.0);
    let taps = f32(density * density);
    for (var index_x = 0u; index_x < density; index_x = index_x + 1u) {
        for (var index_y = 0u; index_y < density; index_y = index_y + 1u) {
            let offset_x = (f32(index_x) + 0.5) / f32(density) - 0.5;
            let offset_y = (f32(index_y) + 0.5) / f32(density) - 0.5;
            let tap_x = centre.x + (delta.x + offset_x) * inverse_magnification;
            let tap_y = centre.y + (delta.y + offset_y) * inverse_magnification;
            accumulated = accumulated + sample_bilinear_premultiplied(tap_x, tap_y);
        }
    }
    let magnified = unpremultiply(accumulated / taps);
    let base_alpha = clamp(base.a, 0.0, 1.0);
    let magnified_alpha = clamp(magnified.a * blend_opacity, 0.0, 1.0);
    if (magnified_alpha <= 0.0) {
        return base;
    }
    if (base_alpha <= 0.0) {
        return vec4<f32>(magnified.rgb, magnified_alpha);
    }
    let mode = u32(round(uniforms.aces.x));
    let blended = blend_rgb(mode, base.rgb, magnified.rgb);
    let out_alpha = magnified_alpha + base_alpha * (1.0 - magnified_alpha);
    if (out_alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let out_rgb =
        (blended * magnified_alpha + base.rgb * base_alpha * (1.0 - magnified_alpha)) / out_alpha;
    return vec4<f32>(out_rgb, out_alpha);
}
"#;

fn blanking_fill_shader_source() -> String {
    let weights = mondrian_effects::BLANKING_FILL_BLUR_WEIGHTS
        .map(|weight| format!("{weight:?}"))
        .join(", ");
    format!("const BLUR_WEIGHTS = array<f32, 9>({weights});\n{BLANKING_FILL_SHADER}",)
}

const BLANKING_FILL_SHADER: &str = r#"
struct Uniforms {
    raster: vec4<f32>,
    source_rect: vec4<f32>,
    foreground_rect: vec4<f32>,
    source_texels: vec4<f32>,
    geometry: vec4<f32>,
    fade: vec4<f32>,
    shadow: vec4<f32>,
    shadow_geometry: vec4<f32>,
    stretch: vec4<f32>,
};

@group(0) @binding(0) var original: texture_2d<f32>;
@group(0) @binding(1) var horizontal: texture_2d<f32>;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0)
    );
    return vec4<f32>(positions[index], 0.0, 1.0);
}

fn associated_sample(plane: texture_2d<f32>, texel: vec2<i32>, straight: bool) -> vec4<f32> {
    let sample = textureLoad(plane, texel, 0);
    if (straight) {
        let alpha = clamp(sample.a, 0.0, 1.0);
        return vec4<f32>(sample.rgb * alpha, alpha);
    }
    return sample;
}

fn sample_bilinear(plane: texture_2d<f32>, texel: vec2<f32>, straight: bool) -> vec4<f32> {
    let limit = vec2<i32>(textureDimensions(plane)) - vec2<i32>(1);
    let position = clamp(texel, vec2<f32>(0.0), vec2<f32>(limit));
    let lower = floor(position);
    let fraction = position - lower;
    let base = vec2<i32>(lower);
    let next = min(base + vec2<i32>(1), limit);
    let a = associated_sample(plane, base, straight);
    let b = associated_sample(plane, vec2<i32>(next.x, base.y), straight);
    let c = associated_sample(plane, vec2<i32>(base.x, next.y), straight);
    let d = associated_sample(plane, next, straight);
    let top = a + (b - a) * fraction.x;
    let bottom = c + (d - c) * fraction.x;
    return top + (bottom - top) * fraction.y;
}

fn unassociate(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sample.rgb / alpha, alpha);
}

fn sample_source(position: vec2<f32>) -> vec4<f32> {
    let texel = clamp(position - vec2<f32>(0.5), uniforms.source_texels.xy, uniforms.source_texels.zw);
    return sample_bilinear(original, texel, true);
}

fn rect_contains(rect: vec4<f32>, position: vec2<f32>) -> bool {
    return all(position >= rect.xy) && all(position <= rect.zw);
}

fn centre(rect: vec4<f32>) -> vec2<f32> {
    return (rect.xy + rect.zw) * 0.5;
}

fn foreground_position(position: vec2<f32>) -> vec2<f32> {
    let source_centre = centre(uniforms.source_rect);
    let foreground_centre = centre(uniforms.foreground_rect);
    if (uniforms.raster.z == 1.0 && all(source_centre == foreground_centre)) {
        return position;
    }
    return source_centre + (position - foreground_centre) * uniforms.raster.z;
}

fn background_position(position: vec2<f32>) -> vec2<f32> {
    if (uniforms.geometry.w == 5.0) {
        let rect = uniforms.foreground_rect;
        let size = rect.zw - rect.xy;
        let cycle = (position - rect.xy) / size;
        let wrapped = cycle - floor(cycle * 0.5) * 2.0;
        let folded = 1.0 - abs(wrapped - 1.0);
        return foreground_position(rect.xy + size * folded);
    }
    return centre(uniforms.source_rect)
        + (position - uniforms.raster.xy * 0.5) * uniforms.geometry.xy * uniforms.raster.z;
}

fn warp_position(position: vec2<f32>) -> vec2<f32> {
    let rect = uniforms.foreground_rect;
    let inner_min = rect.xy + uniforms.stretch.xy;
    let inner_max = rect.zw - uniforms.stretch.zw;
    let low = rect.xy + position / max(inner_min, vec2<f32>(1.0e-8)) * (inner_min - rect.xy);
    let high = rect.zw - (uniforms.raster.xy - position)
        / max(uniforms.raster.xy - inner_max, vec2<f32>(1.0e-8)) * (rect.zw - inner_max);
    let warped = select(position, low, position < inner_min);
    return foreground_position(select(warped, high, position > inner_max));
}

fn cubic(value: f32) -> f32 {
    return value * value * (3.0 - 2.0 * value);
}

fn foreground_weight(position: vec2<f32>) -> f32 {
    let rect = uniforms.foreground_rect;
    if (!rect_contains(rect, position)) {
        return 0.0;
    }
    let feather = uniforms.geometry.z;
    if (feather == 0.0) {
        return 1.0;
    }
    let inside = min(position - rect.xy, rect.zw - position);
    return cubic(clamp(min(inside.x, inside.y) / feather, 0.0, 1.0));
}

fn composite_background(background: vec4<f32>, position: vec2<f32>) -> vec4<f32> {
    let fade = uniforms.fade;
    let faded = background * (1.0 - fade.a) + vec4<f32>(fade.rgb, 1.0) * fade.a;
    let shadow = uniforms.shadow;
    if (shadow.a == 0.0) {
        return faded;
    }
    let rect = uniforms.foreground_rect;
    let half_size = (rect.zw - rect.xy) * 0.5;
    let delta = abs(position - (centre(rect) + uniforms.shadow_geometry.xy)) - half_size;
    let distance = length(max(delta, vec2<f32>(0.0))) + min(max(delta.x, delta.y), 0.0);
    let softness = uniforms.shadow_geometry.z;
    var coverage = select(0.0, 1.0, distance <= 0.0);
    if (softness > 0.0) {
        coverage = 1.0 - cubic(clamp(distance / softness, 0.0, 1.0));
    }
    let alpha = coverage * shadow.a;
    return vec4<f32>(shadow.rgb, 1.0) * alpha + faded * (1.0 - alpha);
}

@fragment
fn fs_horizontal(@builtin(position) fragment: vec4<f32>) -> @location(0) vec4<f32> {
    let position = floor(fragment.xy) + vec2<f32>(0.5);
    let step_size = uniforms.raster.w;
    if (step_size == 0.0) {
        return sample_source(background_position(position));
    }
    var background = vec4<f32>(0.0);
    for (var index = 0u; index < 9u; index += 1u) {
        let offset = (f32(index) - 4.0) * step_size;
        let x = clamp(position.x + offset, 0.5, uniforms.raster.x - 0.5);
        background += sample_source(background_position(vec2<f32>(x, position.y))) * BLUR_WEIGHTS[index];
    }
    return background;
}

@fragment
fn fs_vertical(@builtin(position) fragment: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = floor(fragment.xy);
    let position = texel + vec2<f32>(0.5);
    let foreground = foreground_position(position);
    let weight = foreground_weight(position);
    if (weight == 1.0) {
        if (all(foreground == position) && rect_contains(uniforms.source_texels, texel)) {
            return textureLoad(original, vec2<i32>(texel), 0);
        }
        return unassociate(sample_source(foreground));
    }
    let step_size = uniforms.raster.w;
    var background = vec4<f32>(0.0);
    if (step_size == 0.0) {
        background = textureLoad(horizontal, vec2<i32>(texel), 0);
    } else {
        for (var index = 0u; index < 9u; index += 1u) {
            let offset = (f32(index) - 4.0) * step_size;
            let sample = sample_bilinear(horizontal, texel + vec2<f32>(0.0, offset), false);
            background += sample * BLUR_WEIGHTS[index];
        }
    }
    let faded = composite_background(background, position);
    return unassociate(faded * (1.0 - weight) + sample_source(foreground) * weight);
}

@fragment
fn fs_warp(@builtin(position) fragment: vec4<f32>) -> @location(0) vec4<f32> {
    let texel = floor(fragment.xy);
    let position = texel + vec2<f32>(0.5);
    let warped = warp_position(position);
    if (all(warped == position) && rect_contains(uniforms.source_texels, texel)) {
        return textureLoad(original, vec2<i32>(texel), 0);
    }
    return unassociate(sample_source(warped));
}
"#;

fn spatial_composite_shader_source() -> String {
    let taps = mondrian_effects::BILINEAR_TAPS
        .map(|[x, y]| format!("vec2<i32>({x}, {y})"))
        .join(", ");
    format!(
        "const AUX_PIXEL_CENTRE: f32 = {:?};\n\
         const AUX_FINITE_MAX_BITS: u32 = 0x{:08x}u;\n\
         const AUX_BILINEAR_TAPS: array<vec2<i32>, 4> = array<vec2<i32>, 4>({taps});\n\
         {SPATIAL_COMPOSITE_SHADER}\n{}\n{}",
        mondrian_effects::PIXEL_CENTRE,
        f32::MAX.to_bits(),
        include_str!("channel_combiner.wgsl"),
        include_str!("displacement_map.wgsl"),
    )
}

/// Shader for passes that need the original frame and one auxiliary texture.
///
/// The primary spatial shader binds one texture plus the uniforms; this module
/// binds the original, the auxiliary and the uniforms in one group, so the
/// deband merge can compare a blurred copy against the source in a single pass.
/// The blur passes leave the auxiliary texture premultiplied, so `fs_deband`
/// premultiplies the original before the merge and unpremultiplies once at the
/// end.
const SPATIAL_COMPOSITE_SHADER: &str = r#"
struct Uniforms {
    size: vec4<f32>,
    params: vec4<f32>,
    extra: vec4<f32>,
    aces: vec4<f32>,
    extra1: vec4<f32>,
};

@group(0) @binding(0) var original_texture: texture_2d<f32>;
@group(0) @binding(1) var auxiliary_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> uniforms: Uniforms;

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

fn periodic_texel(p: vec2<i32>, dims: vec2<i32>) -> vec2<i32> {
    return (p % dims + dims) % dims;
}

fn auxiliary_texel(
    p: vec2<i32>, dims: vec2<i32>, transparent: bool, associated: bool, wrap: bool,
) -> vec4<f32> {
    if (wrap) {
        let pixel = textureLoad(auxiliary_texture, periodic_texel(p, dims), 0);
        return premultiply(pixel);
    }
    if (transparent && (any(p < vec2<i32>(0)) || any(p >= dims))) {
        return vec4<f32>(0.0);
    }
    let pixel = textureLoad(auxiliary_texture, clamp(p, vec2<i32>(0), dims - vec2<i32>(1)), 0);
    if (associated) { return premultiply(pixel); }
    return pixel;
}

fn auxiliary_weights(weight: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        (1.0 - weight.x) * (1.0 - weight.y),
        weight.x * (1.0 - weight.y),
        (1.0 - weight.x) * weight.y,
        weight.x * weight.y,
    );
}

fn sample_auxiliary(primary_uv: vec2<f32>, associated: bool, wrap: bool) -> vec4<f32> {
    let dims = vec2<i32>(textureDimensions(auxiliary_texture));
    let extent = vec2<f32>(dims);
    let transparent = uniforms.size.z != 0.0;
    var uv = primary_uv;
    if (uniforms.size.w != 0.0) {
        uv = vec2<f32>(
            uniforms.aces.x * uv.x + uniforms.aces.y * uv.y + uniforms.aces.z,
            uniforms.extra1.x * uv.x + uniforms.extra1.y * uv.y + uniforms.extra1.z,
        );
    }
    if (any(uv != uv) || any(abs(uv) > vec2<f32>(bitcast<f32>(AUX_FINITE_MAX_BITS)))) {
        return vec4<f32>(0.0);
    }
    if (wrap) {
        uv = fract(uv);
    } else if (transparent) {
        let border = vec2<f32>(AUX_PIXEL_CENTRE) / extent;
        if (any(uv < -border) || any(uv > vec2<f32>(1.0) + border)) {
            return vec4<f32>(0.0);
        }
    } else {
        uv = clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0));
    }
    let texel = uv * extent;
    if (uniforms.params.w == 0.0) {
        return auxiliary_texel(vec2<i32>(floor(texel)), dims, transparent, associated, wrap);
    }
    let position = texel - vec2<f32>(AUX_PIXEL_CENTRE);
    let origin = floor(position);
    let weights = auxiliary_weights(position - origin);
    var result = vec4<f32>(0.0);
    for (var tap = 0u; tap < 4u; tap += 1u) {
        if (weights[tap] != 0.0) {
            result += weights[tap] * auxiliary_texel(
                vec2<i32>(origin) + AUX_BILINEAR_TAPS[tap], dims, transparent, associated, wrap,
            );
        }
    }
    return result;
}

fn fill_matte_axis(inset: f32, feather: f32) -> f32 {
    if (feather <= 0.0) {
        return 1.0;
    }
    return clamp(inset / feather, 0.0, 1.0);
}

@fragment
fn fs_fill_matte(input: VsOut) -> @location(0) vec4<f32> {
    let index = floor(input.position.xy);
    let centre = index + vec2<f32>(AUX_PIXEL_CENTRE);
    let extent = uniforms.size.xy;
    let inset = min(centre, extent - centre);
    let mask = clamp(sample_auxiliary(centre / extent, false, false).a, 0.0, 1.0);
    var coverage = mask * (fill_matte_axis(inset.x, uniforms.params.x)
        * fill_matte_axis(inset.y, uniforms.params.y));
    if (uniforms.params.z != 0.0) {
        coverage = 1.0 - coverage;
    }
    let fill_alpha = clamp(coverage * uniforms.extra.w, 0.0, 1.0);
    let base = textureLoad(original_texture, vec2<i32>(index), 0);
    if (fill_alpha <= 0.0) {
        return base;
    }
    let base_alpha = clamp(base.a, 0.0, 1.0);
    if (base_alpha <= 0.0) {
        return vec4<f32>(uniforms.extra.xyz, fill_alpha);
    }
    let keep = 1.0 - fill_alpha;
    let alpha = fill_alpha + base_alpha * keep;
    let color = (uniforms.extra.xyz * fill_alpha + base.rgb * base_alpha * keep) / alpha;
    return vec4<f32>(color, alpha);
}

@fragment
fn fs_channel_combiner_layer(input: VsOut) -> @location(0) vec4<f32> {
    let index = floor(input.position.xy);
    let centre = index + vec2<f32>(AUX_PIXEL_CENTRE);
    let primary = textureLoad(original_texture, vec2<i32>(index), 0);
    let combined = channel_combiner_apply(
        primary, sample_auxiliary(centre / uniforms.size.xy, false, false),
        u32(uniforms.params.x), u32(uniforms.params.y), uniforms.extra,
    );
    return vec4<f32>(combined.rgb, clamp(combined.a, 0.0, 1.0));
}

// The auxiliary texture is read through its own binding, so the clamped load
// has to take the texture as an argument instead of closing over `src`.
fn load_clamped_from(texture: texture_2d<f32>, p: vec2<i32>) -> vec4<f32> {
    let limit = frame_dims() - vec2<i32>(1);
    return textureLoad(texture, clamp(p, vec2<i32>(0), limit), 0);
}

fn premultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    return vec4<f32>(sample.rgb * alpha, alpha);
}

fn unpremultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sample.rgb / alpha, alpha);
}

// The CPU hash is wrapping `u32` arithmetic, so the shader mirrors it bit for
// bit: same salts, same shifts, same multipliers.
fn deband_hash(x: u32, y: u32, salt: u32) -> u32 {
    var hash = (x * 0x9E3779B9u) ^ (y * 0x85EBCA6Bu) ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    return hash ^ (hash >> 16u);
}

fn deband_unit_noise(x: u32, y: u32, salt: u32) -> f32 {
    return f32(deband_hash(x, y, salt) >> 8u) / 16777216.0;
}

fn deband_luma(colour: vec3<f32>) -> f32 {
    return dot(colour, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_deband(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let original = premultiply(load_clamped_from(original_texture, pixel));
    let blurred = load_clamped_from(auxiliary_texture, pixel);
    let threshold = uniforms.params.x;
    let amplitude = uniforms.params.z;
    let difference = abs(deband_luma(original.rgb) - deband_luma(blurred.rgb));
    var weight = 1.0;
    if (threshold > 0.0) {
        weight = clamp(1.0 - difference / threshold, 0.0, 1.0);
    }
    var mixed = mix(original, blurred, weight);
    // Coverage is never a colour: the merge keeps the original alpha, so a
    // transparent pixel cannot pick up the blurred coverage.
    mixed = vec4<f32>(mixed.rgb, original.a);
    if (weight > 0.0 && mixed.a > 0.0) {
        let dither = amplitude
            * (deband_unit_noise(u32(pixel.x), u32(pixel.y), 0x9E3779B9u)
                - deband_unit_noise(u32(pixel.x), u32(pixel.y), 0x85EBCA6Bu));
        mixed = vec4<f32>(mixed.rgb + vec3<f32>(dither), mixed.a);
    }
    return unpremultiply(mixed);
}

// The blend helpers below mirror the compositor's WGSL helpers in
// `crates/mondrian-renderer/src/gpu_compositor.rs` and the CPU
// `blend_mode_rgb` they were written against; they must stay in sync.
fn blend_lum(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.299, 0.587, 0.114));
}

fn clip_blend_color(color: vec3<f32>) -> vec3<f32> {
    let luminance = blend_lum(color);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (minimum < 0.0) {
        let factor = luminance / (luminance - minimum);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    if (maximum > 1.0) {
        let factor = (1.0 - luminance) / (maximum - luminance);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    return color;
}

fn set_blend_lum(color: vec3<f32>, target_lum: f32) -> vec3<f32> {
    return clip_blend_color(color + vec3<f32>(target_lum - blend_lum(color)));
}

fn blend_sat(color: vec3<f32>) -> f32 {
    return max(color.r, max(color.g, color.b)) - min(color.r, min(color.g, color.b));
}

fn set_blend_sat(color: vec3<f32>, target_sat: f32) -> vec3<f32> {
    var result = vec3<f32>(0.0);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (maximum <= minimum) {
        return result;
    }
    let scale = target_sat / (maximum - minimum);
    if (color.r <= color.g && color.r <= color.b) {
        if (color.g <= color.b) {
            result = vec3<f32>(0.0, (color.g - minimum) * scale, target_sat);
        } else {
            result = vec3<f32>(0.0, target_sat, (color.b - minimum) * scale);
        }
    } else if (color.g <= color.r && color.g <= color.b) {
        if (color.r <= color.b) {
            result = vec3<f32>((color.r - minimum) * scale, 0.0, target_sat);
        } else {
            result = vec3<f32>(target_sat, 0.0, (color.b - minimum) * scale);
        }
    } else if (color.r <= color.g) {
        result = vec3<f32>((color.r - minimum) * scale, target_sat, 0.0);
    } else {
        result = vec3<f32>(target_sat, (color.g - minimum) * scale, 0.0);
    }
    return result;
}

fn blend_channel(mode: u32, base: f32, blend: f32) -> f32 {
    if (mode == 2u) { return base * blend; }
    if (mode == 3u) { return 1.0 - (1.0 - base) * (1.0 - blend); }
    if (mode == 4u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, base <= 0.5);
    }
    if (mode == 5u) { return min(base, blend); }
    if (mode == 6u) { return max(base, blend); }
    if (mode == 7u) {
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (1.0 - blend), 0.0, 1.0);
    }
    if (mode == 8u) {
        if (blend <= 0.001) { return 0.0; }
        return clamp(1.0 - (1.0 - base) / blend, 0.0, 1.0);
    }
    if (mode == 9u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, blend <= 0.5);
    }
    if (mode == 10u) {
        if (blend <= 0.5) { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
        var curved = ((16.0 * base - 12.0) * base + 4.0) * base;
        if (base > 0.25) { curved = sqrt(base); }
        return base + (2.0 * blend - 1.0) * (curved - base);
    }
    if (mode == 11u) { return abs(base - blend); }
    if (mode == 12u) { return base + blend - 2.0 * base * blend; }
    if (mode == 13u) { return clamp(base - blend, 0.0, 1.0); }
    if (mode == 16u) { return clamp(base + blend - 1.0, 0.0, 1.0); }
    if (mode == 17u) { return clamp(base + blend, 0.0, 1.0); }
    if (mode == 18u) {
        if (blend <= 0.5) {
            if (blend <= 0.001) { return 0.0; }
            return clamp(1.0 - (1.0 - base) / (2.0 * blend), 0.0, 1.0);
        }
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (2.0 * (1.0 - blend)), 0.0, 1.0);
    }
    if (mode == 19u) { return clamp(base + 2.0 * blend - 1.0, 0.0, 1.0); }
    if (mode == 20u) { return select(max(base, 2.0 * (blend - 0.5)), min(base, 2.0 * blend), blend <= 0.5); }
    if (mode == 21u) { return select(1.0, 0.0, base + blend < 1.0); }
    if (mode == 22u) {
        if (blend <= 0.001) { return 1.0; }
        return clamp(base / blend, 0.0, 1.0);
    }
    return blend;
}

fn blend_rgb(mode: u32, base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    if (mode == 14u) { return select(base, blend, blend_lum(blend) < blend_lum(base)); }
    if (mode == 15u) { return select(base, blend, blend_lum(blend) > blend_lum(base)); }
    if (mode == 23u) { return set_blend_lum(set_blend_sat(blend, blend_sat(base)), blend_lum(base)); }
    if (mode == 24u) { return set_blend_lum(set_blend_sat(base, blend_sat(blend)), blend_lum(base)); }
    if (mode == 25u) { return set_blend_lum(blend, blend_lum(base)); }
    if (mode == 26u) { return set_blend_lum(base, blend_lum(blend)); }
    return vec3<f32>(
        blend_channel(mode, base.r, blend.r),
        blend_channel(mode, base.g, blend.g),
        blend_channel(mode, base.b, blend.b),
    );
}

// Alpha glow gives the edge its own coverage so it is visible outside the source.
fn composite_alpha_glow(
    source: vec4<f32>,
    glow: vec4<f32>,
    intensity: f32,
    mode: u32,
    composite: u32,
) -> vec4<f32> {
    let source_alpha = clamp(source.a, 0.0, 1.0);
    let threshold = clamp(uniforms.extra.y, 0.0, 1.0);
    var center_mask = 0.0;
    if (threshold < 1.0) {
        center_mask = clamp((source_alpha - threshold) / (1.0 - threshold), 0.0, 1.0);
    }
    let edge = abs(glow.a - center_mask);
    if (edge <= 1.0e-6 || intensity == 0.0) {
        return select(source, vec4<f32>(0.0), composite == 2u);
    }
    let glow_alpha = min(edge, 1.0) * min(intensity, 1.0);
    let glow_rgb = glow.rgb * max(intensity, 1.0);
    if (composite == 2u || source_alpha == 0.0) {
        return vec4<f32>(glow_rgb, glow_alpha);
    }
    let behind = composite == 1u;
    let base = select(source.rgb, glow_rgb, behind);
    let front = select(glow_rgb, source.rgb, behind);
    let base_alpha = select(source_alpha, glow_alpha, behind);
    let front_alpha = select(glow_alpha, source_alpha, behind);
    let base_only = base_alpha * (1.0 - front_alpha);
    let front_only = front_alpha * (1.0 - base_alpha);
    let overlap = base_alpha * front_alpha;
    let alpha = front_alpha + base_only;
    let rgb = (base * base_only + front * front_only + blend_rgb(mode, base, front) * overlap)
        / alpha;
    return vec4<f32>(rgb, alpha);
}

// The colour modes preserve source coverage and blend the weighted radiance.
@fragment
fn fs_glow_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped_from(original_texture, pixel);
    let glow = load_clamped_from(auxiliary_texture, pixel);
    let intensity = uniforms.params.x;
    let mode = u32(round(uniforms.params.y));
    let composite = u32(round(uniforms.params.z));
    if (uniforms.extra.x == 1.0) {
        return composite_alpha_glow(source, glow, intensity, mode, composite);
    }
    if (source.a <= 0.0) {
        return source;
    }
    let contribution = glow.rgb * glow.a * intensity;
    var blended = contribution;
    if (composite == 0u) {
        blended = blend_rgb(mode, source.rgb, contribution);
    } else if (composite == 1u) {
        blended = blend_rgb(mode, contribution, source.rgb);
    }
    return vec4<f32>(blended, source.a);
}

// The keylight composite applies the matte to the original source. Every branch
// mirrors `KeylightGrade::composite`: a pixel without coverage is returned
// untouched, and the despill strips the key channel of the output colour.
@fragment
fn fs_keylight_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped_from(original_texture, pixel);
    let matte = load_clamped_from(auxiliary_texture, pixel).x;
    if (source.a <= 0.0) {
        return source;
    }
    let display = u32(round(uniforms.params.x));
    let despill = uniforms.params.y;
    let key_channel = u32(round(uniforms.params.z));
    if (display == 1u) {
        return vec4<f32>(matte, matte, matte, 1.0);
    }
    if (display == 3u) {
        return vec4<f32>(source.rgb, 1.0 - matte);
    }
    var other_a = 1u;
    var other_b = 2u;
    if (key_channel == 1u) {
        other_a = 0u;
        other_b = 2u;
    } else if (key_channel == 2u) {
        other_a = 0u;
        other_b = 1u;
    }
    let bias = uniforms.extra.x;
    let balance = uniforms.extra1.x;
    let reference = balance * source.rgb[other_a]
        + (1.0 - balance) * source.rgb[other_b]
        + bias;
    let stripped = max(
        source.rgb[key_channel]
            - despill * max(0.0, source.rgb[key_channel] - (reference - bias)),
        0.0,
    );
    var colour = source.rgb;
    colour[key_channel] = stripped;
    var alpha = source.a;
    if (display == 0u) {
        alpha = source.a * matte;
    } else {
        alpha = 1.0;
    }
    return vec4<f32>(colour, alpha);
}

// The choker composite keeps the source colour verbatim and swaps in the alpha
// the morphology passes produced. The matte plane replicates the coverage into
// every channel, so `.x` is the alpha and there is no view map or contrast: a
// fully transparent source pixel still receives the dilated coverage.
@fragment
fn fs_choker_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped_from(original_texture, pixel);
    let matte = load_clamped_from(auxiliary_texture, pixel).x;
    return vec4<f32>(source.rgb, matte);
}

// The key cleaner composite finishes the separable chain over the eroded matte:
// `params` carries `[blur_pixels, contrast, strength, 0]`. The vertical half of
// the box average runs here so the eroded plane only has to be written once, and
// the re-steepened matte is blended back against the source alpha by strength.
// The source colour is returned verbatim, because the effect never reads it.
//
// Every expression mirrors `key_cleaner.rs`: the sum runs from `-blur` to
// `+blur` with the same divisor, and the curve is the same clamp-then-smoothstep
// so a zero-strength frame is bit-identical to the input.
@fragment
fn fs_key_cleaner_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped_from(original_texture, pixel);
    let blur_pixels = i32(uniforms.params.x);
    let contrast = uniforms.params.y;
    let strength = uniforms.params.z;
    var sum = 0.0;
    for (var offset = -blur_pixels; offset <= blur_pixels; offset = offset + 1) {
        let sample_at = pixel + vec2<i32>(0, offset);
        sum = sum + load_clamped_from(auxiliary_texture, sample_at).x;
    }
    let divisor = f32(2 * blur_pixels + 1);
    let softened = sum / divisor;
    let t = clamp((softened - 0.25) / 0.5, 0.0, 1.0);
    let sharpened = t * t * (3.0 - 2.0 * t);
    let contrasted = softened * (1.0 - contrast) + sharpened * contrast;
    let alpha = mix(source.a, contrasted, strength);
    return vec4<f32>(source.rgb, alpha);
}

// Smart blur: one separable axis per draw. `params` carries
// `[radius, threshold_fraction, mode, axis]`. Binding 0 is always the untouched
// source, because both draws gate against it; binding 1 is the pass input.
// Every expression mirrors `smart_blur.rs`: the weight is a fractional box of
// `2*radius+1` taps, the gate is `<=`, and the divisor is the sum of the gated
// weights (the centre tap always passes, so it can never be zero).
fn smart_blur_weight(distance: f32, radius: f32) -> f32 {
    let whole = floor(radius);
    let magnitude = abs(distance);
    if (magnitude <= whole) {
        return 1.0;
    }
    if (magnitude == whole + 1.0) {
        return radius - whole;
    }
    return 0.0;
}

@fragment
fn fs_smart_blur(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let radius = clamp(uniforms.params.x, 0.1, 100.0);
    let threshold = clamp(uniforms.params.y, 0.001, 1.0);
    let mode = i32(uniforms.params.z + 0.5);
    let vertical = uniforms.params.w > 0.5;
    let whole = i32(floor(radius));
    let centre_luma = deband_luma(premultiply(load_clamped_from(original_texture, pixel)).rgb);
    let source_alpha = load_clamped_from(original_texture, pixel).a;
    var sum = vec4<f32>(0.0);
    var weight_sum = 0.0;
    for (var offset = -whole - 1; offset <= whole + 1; offset = offset + 1) {
        let weight = smart_blur_weight(f32(offset), radius);
        if (weight <= 0.0) {
            continue;
        }
        let sample_at = pixel + select(vec2<i32>(0, offset), vec2<i32>(offset, 0), !vertical);
        let gate_luma = deband_luma(premultiply(load_clamped_from(original_texture, sample_at)).rgb);
        let gated = select(0.0, 1.0, abs(gate_luma - centre_luma) <= threshold);
        let combined = weight * gated;
        // Draw A averages the source, which is straight alpha and has to be
        // premultiplied here. Draw B averages the horizontal plane, which is
        // already premultiplied, so premultiplying again would square the
        // coverage and pull the edge estimate far away from the CPU kernel.
        let sample = load_clamped_from(auxiliary_texture, sample_at);
        sum = sum + select(premultiply(sample), sample, vertical) * combined;
        weight_sum = weight_sum + combined;
    }
    let blurred = sum / weight_sum;
    // Mode zero is reserved for the horizontal pass: it has to leave the plain
    // premultiplied mean in the intermediate plane, so the vertical pass can
    // finish the separable average before any composite runs.
    if (mode == 0) {
        return blurred;
    }
    let edge = clamp(4.0 * abs(centre_luma - deband_luma(blurred.rgb)), 0.0, 1.0);
    if (mode == 2) {
        if (source_alpha <= 0.0) {
            return vec4<f32>(0.0);
        }
        return vec4<f32>(edge, edge, edge, source_alpha);
    }
    let alpha = clamp(blurred.a, 0.0, 1.0);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    let colour = blurred.rgb / alpha;
    if (mode == 3) {
        return vec4<f32>(
            colour.r + edge * (1.0 - colour.r),
            colour.g + edge * (1.0 - colour.g),
            colour.b + edge * (1.0 - colour.b),
            alpha,
        );
    }
    return vec4<f32>(colour, alpha);
}

// The matte refine composite writes the (optionally cleaned) source colour with
// the refined matte as alpha. `params` carries
// `[view_map, view_edge_region, contrast, read_frame_alpha]`; the map view wins
// over the edge-region view, exactly like `MatteRefineGrade::composite`.
fn matte_alpha_from(texture: texture_2d<f32>, p: vec2<i32>, read_frame: bool) -> f32 {
    let sample = load_clamped_from(texture, p);
    return select(sample.x, sample.a, read_frame);
}

fn matte_edge_strength_from(texture: texture_2d<f32>, pixel: vec2<i32>, read_frame: bool) -> f32 {
    let left = matte_alpha_from(texture, pixel + vec2<i32>(-1, 0), read_frame);
    let right = matte_alpha_from(texture, pixel + vec2<i32>(1, 0), read_frame);
    let up = matte_alpha_from(texture, pixel + vec2<i32>(0, -1), read_frame);
    let down = matte_alpha_from(texture, pixel + vec2<i32>(0, 1), read_frame);
    let horizontal = 0.5 * (right - left);
    let vertical = 0.5 * (down - up);
    return clamp(2.0 * (abs(horizontal) + abs(vertical)), 0.0, 1.0);
}

@fragment
fn fs_matte_refine_composite(
    @builtin(position) position: vec4<f32>,
) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped_from(original_texture, pixel);
    let read_frame = uniforms.params.w > 0.5;
    let matte = matte_alpha_from(auxiliary_texture, pixel, read_frame);
    let view_map = uniforms.params.x > 0.5;
    let view_edge_region = uniforms.params.y > 0.5;
    let contrast = uniforms.params.z;
    var alpha = matte;
    if (contrast > 0.0) {
        let factor = 1.0 + contrast / 50.0;
        alpha = clamp((alpha - 0.5) * factor + 0.5, 0.0, 1.0);
    }
    if (view_map) {
        return vec4<f32>(alpha, alpha, alpha, 1.0);
    }
    if (view_edge_region) {
        let strength = matte_edge_strength_from(auxiliary_texture, pixel, read_frame);
        return vec4<f32>(source.rgb, strength);
    }
    return vec4<f32>(source.rgb, alpha);
}
"#;

// The fractal noise generator needs twenty-six scalars, more than the shared
// `Uniforms` payload carries, so it gets its own shader module, bind group
// layout, and eight-vec4 uniform block instead of another `extra` cell.
//
// Every helper below is a transcription of
// `crates/mondrian-effects/src/fractal_noise.rs`, the CPU kernel that the parity
// test compares against. `fractal_hash`/`fractal_sample` restate `noise_hash`/
// `noise_sample` from `crates/mondrian-effects/src/noise.rs`, and the blend
// helpers restate the ones in `SPATIAL_COMPOSITE_SHADER`; WGSL has no imports,
// so all three copies have to be kept in sync by hand.
const SPATIAL_FRACTAL_NOISE_SHADER: &str = r#"
struct FractalUniforms {
    size: vec4<f32>,
    params: vec4<f32>,
    extra0: vec4<f32>,
    extra1: vec4<f32>,
    extra2: vec4<f32>,
    extra3: vec4<f32>,
    extra4: vec4<f32>,
    extra5: vec4<f32>,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> uniforms: FractalUniforms;

struct VsOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

const FEATURE_PIXELS: f32 = 100.0;
const DEGREES_TO_RADIANS: f32 = 0.017453292;
const OCTAVE_SALT_STEP: u32 = 0x9E3779B9u;
const SEED_BASE: u32 = 0x85EBCA6Bu;
const SWIRLY_STEP_COS: f32 = 0.8660254;
const SWIRLY_STEP_SIN: f32 = 0.5;

fn fractal_hash(x: u32, y: u32, salt: u32) -> u32 {
    var hash = x * 0x9E3779B9u ^ y * 0x85EBCA6Bu ^ salt;
    hash = hash ^ (hash >> 16u);
    hash = hash * 0x7FEB352Du;
    hash = hash ^ (hash >> 15u);
    hash = hash * 0x846CA68Bu;
    return hash ^ (hash >> 16u);
}

fn fractal_sample(x: u32, y: u32, salt: u32) -> f32 {
    let bits = (fractal_hash(x, y, salt) >> 8u) & 0xFFFFu;
    return f32(bits) / 65535.0 * 2.0 - 1.0;
}

fn fractal_rotate(x: f32, y: f32, rotation_cos: f32, rotation_sin: f32) -> vec2<f32> {
    return vec2<f32>(x * rotation_cos - y * rotation_sin, x * rotation_sin + y * rotation_cos);
}

fn fractal_advance_rotation(rotation_cos: f32, rotation_sin: f32, step_cos: f32, step_sin: f32) -> vec2<f32> {
    return vec2<f32>(
        rotation_cos * step_cos - rotation_sin * step_sin,
        rotation_cos * step_sin + rotation_sin * step_cos,
    );
}

fn fractal_smooth_step(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = clamp((value - edge0) / (edge1 - edge0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn fractal_terraced(value: f32) -> f32 {
    let scaled = value * 4.0;
    let floored = floor(scaled);
    let fraction = scaled - floored;
    return clamp(floored / 4.0 + fractal_smooth_step(0.0, 0.25, fraction) * 0.125, 0.0, 1.0);
}

fn fractal_soft_clamp(value: f32) -> f32 {
    let distance = value - 0.5;
    var sign_value = 1.0;
    if (distance < 0.0) {
        sign_value = -1.0;
    }
    let denominator = 1.0 - exp(-2.0);
    let saturation = 1.0 - exp(-2.0 * abs(distance));
    return 0.5 + 0.5 * sign_value * saturation / denominator;
}

// Returns the floor of `value` converted numerically (not by reinterpreting the
// float bit pattern) and the fractional part, matching the CPU `cell_of`.
fn fractal_cell(value: f32) -> vec2<f32> {
    let floored = floor(value);
    return vec2<f32>(f32(i32(floored)), value - floored);
}

fn fractal_neighbour(cell: u32, offset: i32) -> u32 {
    return bitcast<u32>(i32(cell) + offset);
}

fn fractal_interpolation_weight(noise_type: u32, fraction: f32) -> f32 {
    if (noise_type == 0u) {
        return fraction * fraction * (3.0 - 2.0 * fraction);
    }
    return fraction;
}

fn fractal_catmull_rom_weights(fraction: f32) -> vec4<f32> {
    let squared = fraction * fraction;
    let cubed = squared * fraction;
    return vec4<f32>(
        -0.5 * cubed + squared - 0.5 * fraction,
        1.5 * cubed - 2.5 * squared + 1.0,
        -1.5 * cubed + 2.0 * squared + 0.5 * fraction,
        0.5 * cubed - 0.5 * squared,
    );
}

fn fractal_sample_noise(x: f32, y: f32, noise_type: u32, salt: u32) -> f32 {
    let cell_x = fractal_cell(x);
    let cell_y = fractal_cell(y);
    let base_x = bitcast<u32>(i32(cell_x.x));
    let base_y = bitcast<u32>(i32(cell_y.x));
    if (noise_type == 2u) {
        let weights_x = fractal_catmull_rom_weights(cell_x.y);
        let weights_y = fractal_catmull_rom_weights(cell_y.y);
        var sum = 0.0;
        for (var row = 0; row < 4; row = row + 1) {
            for (var column = 0; column < 4; column = column + 1) {
                sum = sum + weights_x[column] * weights_y[row]
                    * fractal_sample(fractal_neighbour(base_x, column - 1), fractal_neighbour(base_y, row - 1), salt);
            }
        }
        return sum;
    }
    let weight_x = fractal_interpolation_weight(noise_type, cell_x.y);
    let weight_y = fractal_interpolation_weight(noise_type, cell_y.y);
    let top_left = fractal_sample(base_x, base_y, salt);
    let top_right = fractal_sample(fractal_neighbour(base_x, 1), base_y, salt);
    let bottom_left = fractal_sample(base_x, fractal_neighbour(base_y, 1), salt);
    let bottom_right = fractal_sample(fractal_neighbour(base_x, 1), fractal_neighbour(base_y, 1), salt);
    let top = top_left + (top_right - top_left) * weight_x;
    let bottom = bottom_left + (bottom_right - bottom_left) * weight_x;
    return top + (bottom - top) * weight_y;
}

fn fractal_field(position: vec2<f32>, fractal_type: u32, noise_type: u32) -> f32 {
    let complexity = u32(round(uniforms.extra1.y));
    let seed_hash = SEED_BASE ^ u32(round(uniforms.extra4.z));
    let sub_influence = uniforms.extra1.z;
    let lacunarity = uniforms.extra1.w;
    let sub_rotation = vec2<f32>(uniforms.extra2.x, uniforms.extra2.y);
    let sub_offset = vec2<f32>(uniforms.extra2.z, uniforms.extra2.w);
    let evolution_radians = uniforms.extra1.x * 360.0 * DEGREES_TO_RADIANS;
    let evolution = vec2<f32>(cos(evolution_radians), sin(evolution_radians));
    var amplitude = 1.0;
    var total = 0.0;
    var frequency = 1.0;
    var offset = vec2<f32>(0.0, 0.0);
    var rotation = vec2<f32>(1.0, 0.0);
    var field = 0.0;
    if (fractal_type == 8u) {
        field = 1.0;
    }
    for (var octave = 0u; octave < complexity; octave = octave + 1u) {
        let local = fractal_rotate(
            (position * frequency + offset).x,
            (position * frequency + offset).y,
            rotation.x,
            rotation.y,
        );
        let index = f32(octave);
        let salt = seed_hash ^ (octave * OCTAVE_SALT_STEP);
        let sample = fractal_sample_noise(
            local.x + evolution.x * 0.5 * index,
            local.y + evolution.y * 0.5 * index,
            noise_type,
            salt,
        );
        if (fractal_type == 0u || fractal_type == 5u) {
            field = field + amplitude * sample;
        } else if (fractal_type == 1u || fractal_type == 3u) {
            field = field + amplitude * abs(sample * 2.0 - 1.0);
        } else if (fractal_type == 2u) {
            field = max(field, amplitude * abs(sample * 2.0 - 1.0));
        } else if (fractal_type == 4u) {
            field = max(field, amplitude * sample);
        } else if (fractal_type == 6u) {
            field = field + amplitude * fractal_terraced(sample);
        } else if (fractal_type == 7u) {
            field = field + amplitude * (1.0 - abs(sample * 2.0 - 1.0));
        } else {
            field = field * (0.5 + 0.5 * sample);
        }
        total = total + amplitude;
        if (fractal_type == 3u) {
            amplitude = amplitude * (sub_influence * sub_influence);
        } else {
            amplitude = amplitude * sub_influence;
        }
        frequency = frequency * lacunarity;
        rotation = fractal_advance_rotation(rotation.x, rotation.y, sub_rotation.x, sub_rotation.y);
        if (fractal_type == 5u) {
            rotation = fractal_advance_rotation(rotation.x, rotation.y, SWIRLY_STEP_COS, SWIRLY_STEP_SIN);
        }
        offset = offset + sub_offset;
    }
    if (fractal_type == 8u) {
        return field;
    }
    return field / max(total, 0.000001);
}

fn fractal_grade(value: f32, overflow: u32, invert: bool, contrast_gain: f32, brightness_offset: f32) -> f32 {
    var graded = value;
    if (overflow == 0u) {
        graded = clamp(graded, 0.0, 1.0);
    } else if (overflow == 1u) {
        graded = fractal_soft_clamp(graded);
    } else {
        graded = graded - floor(graded);
    }
    if (invert) {
        graded = 1.0 - graded;
    }
    return (graded - 0.5) * contrast_gain + 0.5 + brightness_offset;
}

fn blend_lum(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.299, 0.587, 0.114));
}

fn clip_blend_color(color: vec3<f32>) -> vec3<f32> {
    let luminance = blend_lum(color);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (minimum < 0.0) {
        let factor = luminance / (luminance - minimum);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    if (maximum > 1.0) {
        let factor = (1.0 - luminance) / (maximum - luminance);
        return vec3<f32>(luminance) + (color - vec3<f32>(luminance)) * factor;
    }
    return color;
}

fn set_blend_lum(color: vec3<f32>, target_lum: f32) -> vec3<f32> {
    return clip_blend_color(color + vec3<f32>(target_lum - blend_lum(color)));
}

fn blend_sat(color: vec3<f32>) -> f32 {
    return max(color.r, max(color.g, color.b)) - min(color.r, min(color.g, color.b));
}

fn set_blend_sat(color: vec3<f32>, target_sat: f32) -> vec3<f32> {
    var result = vec3<f32>(0.0);
    let minimum = min(color.r, min(color.g, color.b));
    let maximum = max(color.r, max(color.g, color.b));
    if (maximum <= minimum) {
        return result;
    }
    let scale = target_sat / (maximum - minimum);
    if (color.r <= color.g && color.r <= color.b) {
        if (color.g <= color.b) {
            result = vec3<f32>(0.0, (color.g - minimum) * scale, target_sat);
        } else {
            result = vec3<f32>(0.0, target_sat, (color.b - minimum) * scale);
        }
    } else if (color.g <= color.r && color.g <= color.b) {
        if (color.r <= color.b) {
            result = vec3<f32>((color.r - minimum) * scale, 0.0, target_sat);
        } else {
            result = vec3<f32>(target_sat, 0.0, (color.b - minimum) * scale);
        }
    } else if (color.r <= color.g) {
        result = vec3<f32>((color.r - minimum) * scale, target_sat, 0.0);
    } else {
        result = vec3<f32>(target_sat, (color.g - minimum) * scale, 0.0);
    }
    return result;
}

fn blend_channel(mode: u32, base: f32, blend: f32) -> f32 {
    if (mode == 2u) { return base * blend; }
    if (mode == 3u) { return 1.0 - (1.0 - base) * (1.0 - blend); }
    if (mode == 4u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, base <= 0.5);
    }
    if (mode == 5u) { return min(base, blend); }
    if (mode == 6u) { return max(base, blend); }
    if (mode == 7u) {
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (1.0 - blend), 0.0, 1.0);
    }
    if (mode == 8u) {
        if (blend <= 0.001) { return 0.0; }
        return clamp(1.0 - (1.0 - base) / blend, 0.0, 1.0);
    }
    if (mode == 9u) {
        return select(1.0 - 2.0 * (1.0 - base) * (1.0 - blend), 2.0 * base * blend, blend <= 0.5);
    }
    if (mode == 10u) {
        if (blend <= 0.5) { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
        var curved = ((16.0 * base - 12.0) * base + 4.0) * base;
        if (base > 0.25) { curved = sqrt(base); }
        return base + (2.0 * blend - 1.0) * (curved - base);
    }
    if (mode == 11u) { return abs(base - blend); }
    if (mode == 12u) { return base + blend - 2.0 * base * blend; }
    if (mode == 13u) { return clamp(base - blend, 0.0, 1.0); }
    if (mode == 16u) { return clamp(base + blend - 1.0, 0.0, 1.0); }
    if (mode == 17u) { return clamp(base + blend, 0.0, 1.0); }
    if (mode == 18u) {
        if (blend <= 0.5) {
            if (blend <= 0.001) { return 0.0; }
            return clamp(1.0 - (1.0 - base) / (2.0 * blend), 0.0, 1.0);
        }
        if (blend >= 0.999) { return 1.0; }
        return clamp(base / (2.0 * (1.0 - blend)), 0.0, 1.0);
    }
    if (mode == 19u) { return clamp(base + 2.0 * blend - 1.0, 0.0, 1.0); }
    if (mode == 20u) { return select(max(base, 2.0 * (blend - 0.5)), min(base, 2.0 * blend), blend <= 0.5); }
    if (mode == 21u) { return select(1.0, 0.0, base + blend < 1.0); }
    if (mode == 22u) {
        if (blend <= 0.001) { return 1.0; }
        return clamp(base / blend, 0.0, 1.0);
    }
    return blend;
}

fn blend_rgb(mode: u32, base: vec3<f32>, blend: vec3<f32>) -> vec3<f32> {
    if (mode == 14u) { return select(base, blend, blend_lum(blend) < blend_lum(base)); }
    if (mode == 15u) { return select(base, blend, blend_lum(blend) > blend_lum(base)); }
    if (mode == 23u) { return set_blend_lum(set_blend_sat(blend, blend_sat(base)), blend_lum(base)); }
    if (mode == 24u) { return set_blend_lum(set_blend_sat(base, blend_sat(blend)), blend_lum(base)); }
    if (mode == 25u) { return set_blend_lum(blend, blend_lum(base)); }
    if (mode == 26u) { return set_blend_lum(base, blend_lum(blend)); }
    return vec3<f32>(
        blend_channel(mode, base.r, blend.r),
        blend_channel(mode, base.g, blend.g),
        blend_channel(mode, base.b, blend.b),
    );
}

// Mirrors `FractalSetup::value_at` followed by its grade and composite steps: the
// field is sampled in noise space, graded, blended over the source, and the
// source alpha is always preserved. A fully transparent source pixel is returned
// untouched, exactly like the CPU path.
@fragment
fn fs_fractal_noise(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let dims = vec2<i32>(uniforms.size.xy);
    let clamped = clamp(pixel, vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
    let source = textureLoad(src, clamped, 0);
    let centre_x = (uniforms.size.x - 1.0) * 0.5;
    let centre_y = (uniforms.size.y - 1.0) * 0.5;
    var x = (f32(clamped.x) - centre_x) / (FEATURE_PIXELS * uniforms.params.x);
    var y = (f32(clamped.y) - centre_y) / (FEATURE_PIXELS * uniforms.params.y);
    let centre_shift = uniforms.extra0.z / 10.0 * 0.5;
    x = x + centre_shift;
    y = y + centre_shift;
    let rotated = fractal_rotate(x, y, uniforms.params.z, uniforms.params.w);
    x = rotated.x + uniforms.extra0.x;
    y = rotated.y + uniforms.extra0.y;
    if (uniforms.extra0.w != 0.0) {
        x = x + y * 0.5;
    }
    let fractal_type = u32(round(uniforms.extra3.z));
    let noise_type = u32(round(uniforms.extra3.w));
    let flags = u32(round(uniforms.extra4.y));
    let graded = fractal_grade(
        fractal_field(vec2<f32>(x, y), fractal_type, noise_type),
        u32(round(uniforms.extra4.x)),
        (flags & 1u) != 0u,
        uniforms.extra3.x,
        uniforms.extra3.y,
    );
    if (source.a <= 0.0) {
        return source;
    }
    let blended = blend_rgb(u32(round(uniforms.extra5.x)), source.rgb, vec3<f32>(graded));
    let rgb = source.rgb + (blended - source.rgb) * uniforms.extra4.w;
    return vec4<f32>(rgb, source.a);
}
"#;

/// The selective colour pass.
///
/// The stage needs one hundred and sixty bytes of parameters, more than the
/// shared spatial uniform carries, so it ships its own uniform struct, its own
/// bind group layout, and its own entry point. The arithmetic mirrors
/// `mondrian_effects::selective_color` statement for statement, in the same
/// order, so the CPU and the GPU resolve the same ink shift.
const SPATIAL_SELECTIVE_COLOR_SHADER: &str = r#"
struct SelectiveColorUniform {
    control: vec4<u32>,
    deltas: array<vec4<f32>, 9>,
};

@group(0) @binding(0) var sc_src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> sc: SelectiveColorUniform;

struct VsOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn sc_smoothstep01(value: f32) -> f32 {
    let clamped = clamp(value, 0.0, 1.0);
    return clamped * clamped * (3.0 - 2.0 * clamped);
}

fn sc_hue(rgb: vec3<f32>, max_channel: f32, chroma: f32) -> f32 {
    if (chroma <= 0.0) {
        return 0.0;
    }
    var hue: f32;
    if (max_channel == rgb.r) {
        hue = 60.0 * ((rgb.g - rgb.b) / chroma);
    } else if (max_channel == rgb.g) {
        hue = 60.0 * ((rgb.b - rgb.r) / chroma + 2.0);
    } else {
        hue = 60.0 * ((rgb.r - rgb.g) / chroma + 4.0);
    }
    if (hue < 0.0) {
        hue = hue + 360.0;
    }
    return hue;
}

fn sc_tent(hue: f32, centre: f32) -> f32 {
    var distance = hue - centre;
    distance = distance - 360.0 * round(distance / 360.0);
    return max(0.0, 1.0 - abs(distance) / 60.0);
}

fn sc_relative(value: f32, delta: f32) -> f32 {
    if (delta >= 0.0) {
        return value + delta * (1.0 - value);
    }
    return value * (1.0 + delta);
}

fn sc_load(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(sc_src));
    let clamped = clamp(pixel, vec2<i32>(0, 0), size - vec2<i32>(1, 1));
    return textureLoad(sc_src, clamped, 0);
}

@fragment
fn fs_selective_color(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let source = sc_load(vec2<i32>(position.xy));
    if (source.a <= 0.0) {
        return source;
    }
    let rgb = source.rgb;
    let max_channel = max(rgb.r, max(rgb.g, rgb.b));
    let min_channel = min(rgb.r, min(rgb.g, rgb.b));
    let chroma = max_channel - min_channel;
    let luma = 0.2126 * rgb.r + 0.7152 * rgb.g + 0.0722 * rgb.b;
    let gate = sc_smoothstep01((chroma - 0.02) / 0.08);
    let hue = sc_hue(rgb, max_channel, chroma);
    let white = sc_smoothstep01((luma - 0.5) / 0.5);
    let black = sc_smoothstep01((0.5 - luma) / 0.5);

    var weights: array<f32, 9>;
    weights[0] = gate * sc_tent(hue, 0.0);
    weights[1] = gate * sc_tent(hue, 60.0);
    weights[2] = gate * sc_tent(hue, 120.0);
    weights[3] = gate * sc_tent(hue, 180.0);
    weights[4] = gate * sc_tent(hue, 240.0);
    weights[5] = gate * sc_tent(hue, 300.0);
    weights[6] = (1.0 - gate) * white;
    weights[7] = (1.0 - gate) * (1.0 - white - black);
    weights[8] = (1.0 - gate) * black;

    var delta = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    for (var index: i32 = 0; index < 9; index = index + 1) {
        delta = delta + weights[index] * sc.deltas[index];
    }

    let safe = max(max_channel, 0.000001);
    let cmyk = vec4<f32>(
        (safe - rgb.r) / safe,
        (safe - rgb.g) / safe,
        (safe - rgb.b) / safe,
        1.0 - safe,
    );
    var ink: vec4<f32>;
    if (sc.control.x == 1u) {
        ink = vec4<f32>(
            sc_relative(cmyk.x, delta.x),
            sc_relative(cmyk.y, delta.y),
            sc_relative(cmyk.z, delta.z),
            sc_relative(cmyk.w, delta.w),
        );
    } else {
        ink = cmyk + delta;
    }
    let shade = 1.0 - ink.w;
    let graded = vec3<f32>(
        clamp(shade * (1.0 - ink.x), 0.0, 1.0),
        clamp(shade * (1.0 - ink.y), 0.0, 1.0),
        clamp(shade * (1.0 - ink.z), 0.0, 1.0),
    );
    return vec4<f32>(graded, source.a);
}
"#;

/// The stroke pass.
///
/// The band is centred on the source alpha half-coverage contour, so the
/// fragment scans eight rays for the crossing, bisects each bracket six times,
/// and keeps the nearest hit. The arithmetic mirrors `mondrian_effects::stroke`
/// statement for statement, in the same order, so the CPU and the GPU resolve
/// the same band.
const SPATIAL_STROKE_SHADER: &str = r#"
struct StrokeUniform {
    control: vec4<u32>,
    color: vec4<f32>,
    geometry: vec4<f32>,
};

@group(0) @binding(0) var st_src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> st: StrokeUniform;

struct VsOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn st_direction(index: i32) -> vec2<f32> {
    var directions = array<vec2<f32>, 8>(
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.70710677, 0.70710677),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(-0.70710677, 0.70710677),
        vec2<f32>(-1.0, 0.0),
        vec2<f32>(-0.70710677, -0.70710677),
        vec2<f32>(0.0, -1.0),
        vec2<f32>(0.70710677, -0.70710677),
    );
    return directions[index];
}

fn st_offset(direction: vec2<f32>, step: f32) -> vec2<i32> {
    let offset = floor(direction * step + vec2<f32>(0.5, 0.5));
    return vec2<i32>(offset);
}

fn st_load(pixel: vec2<i32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(st_src));
    let clamped = clamp(pixel, vec2<i32>(0, 0), size - vec2<i32>(1, 1));
    return textureLoad(st_src, clamped, 0);
}

fn st_edge_distance(base: vec2<i32>, centre: f32, limit: f32) -> f32 {
    var best = limit * 4.0;
    for (var index: i32 = 0; index < 8; index = index + 1) {
        let direction = st_direction(index);
        var step = 1.0;
        var low = 0.0;
        var high = 0.0;
        var found = false;
        loop {
            let sample = st_load(base + st_offset(direction, step));
            if ((sample.a - 0.5) * (centre - 0.5) <= 0.0) {
                high = step;
                found = true;
                break;
            }
            low = step;
            if (step > limit + 1.0) {
                break;
            }
            step = step * 2.0;
        }
        if (!found) {
            continue;
        }
        for (var bisection: i32 = 0; bisection < 6; bisection = bisection + 1) {
            let middle = (low + high) * 0.5;
            let sample = st_load(base + st_offset(direction, middle));
            if ((sample.a - 0.5) * (centre - 0.5) <= 0.0) {
                high = middle;
            } else {
                low = middle;
            }
        }
        best = min(best, high);
    }
    return best;
}

@fragment
fn fs_stroke(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let base = vec2<i32>(position.xy);
    let source = st_load(base);
    let brush = max(st.geometry.x, 0.0);
    let opacity = clamp(st.color.w, 0.0, 1.0);
    if (brush <= 0.0 || opacity <= 0.0) {
        return source;
    }
    let radius = brush * 0.5;
    let distance = st_edge_distance(base, source.a, radius + 1.0);
    let hardness = clamp(st.geometry.y, 0.0, 0.95);
    let soft = max(radius * (1.0 - hardness), 0.5);
    let solid = max(radius - soft, 0.0);
    var coverage = 0.0;
    if (distance <= radius) {
        coverage = 1.0 - clamp((distance - solid) / soft, 0.0, 1.0);
    }
    let stroke_alpha = coverage * opacity;
    if (stroke_alpha <= 0.0) {
        return source;
    }
    let colour = st.color.xyz;
    if (st.control.x == 2u) {
        return vec4<f32>(colour, stroke_alpha);
    }
    if (st.control.x == 3u) {
        return vec4<f32>(source.rgb, source.a * stroke_alpha);
    }
    let source_weight = source.a * (1.0 - stroke_alpha);
    let out_alpha = stroke_alpha + source_weight;
    let premultiplied = colour * stroke_alpha + source.rgb * source_weight;
    return vec4<f32>(premultiplied / out_alpha, out_alpha);
}
"#;

/// The bezier warp pass.
///
/// Each fragment inverts a Coons patch with a fixed-count Newton iteration and
/// samples the premultiplied source at the solved parameters. The arithmetic
/// mirrors `mondrian_effects::bezier_warp` statement for statement, in the same
/// order (in f32 instead of f64); a fragment whose solve fails or runs away
/// copies its own source pixel, exactly like the kernel.
const SPATIAL_BEZIER_WARP_SHADER: &str = r#"
struct BezierWarpUniform {
    size: vec4<f32>,
    points: array<vec4<f32>, 6>,
    control: vec4<f32>,
};

@group(0) @binding(0) var bw_src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> bw: BezierWarpUniform;

struct VsOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    return out;
}

fn bw_point(index: i32) -> vec2<f32> {
    let cell = bw.points[index / 2];
    if ((index % 2) == 0) {
        return cell.xy;
    }
    return cell.zw;
}

fn bw_cubic(p0: vec2<f32>, p1: vec2<f32>, p2: vec2<f32>, p3: vec2<f32>, t: f32) -> vec2<f32> {
    let inverse = 1.0 - t;
    let a = inverse * inverse * inverse;
    let b = 3.0 * inverse * inverse * t;
    let c = 3.0 * inverse * t * t;
    let d = t * t * t;
    return a * p0 + b * p1 + c * p2 + d * p3;
}

fn bw_derivative(
    p0: vec2<f32>,
    p1: vec2<f32>,
    p2: vec2<f32>,
    p3: vec2<f32>,
    t: f32,
) -> vec2<f32> {
    let inverse = 1.0 - t;
    let a = 3.0 * inverse * inverse;
    let b = 6.0 * inverse * t;
    let c = 3.0 * t * t;
    return a * (p1 - p0) + b * (p2 - p1) + c * (p3 - p2);
}

fn bw_top(parameter: f32) -> vec2<f32> {
    return bw_cubic(bw_point(0), bw_point(1), bw_point(2), bw_point(3), parameter);
}

fn bw_bottom(parameter: f32) -> vec2<f32> {
    return bw_cubic(bw_point(9), bw_point(8), bw_point(7), bw_point(6), parameter);
}

fn bw_left_side(parameter: f32) -> vec2<f32> {
    return bw_cubic(bw_point(0), bw_point(11), bw_point(10), bw_point(9), parameter);
}

fn bw_right_side(parameter: f32) -> vec2<f32> {
    return bw_cubic(bw_point(3), bw_point(4), bw_point(5), bw_point(6), parameter);
}

fn bw_patch(u: f32, v: f32) -> vec2<f32> {
    let top = bw_top(u);
    let bottom = bw_bottom(u);
    let left = bw_left_side(v);
    let right = bw_right_side(v);
    let top_left = bw_point(0);
    let top_right = bw_point(3);
    let bottom_right = bw_point(6);
    let bottom_left = bw_point(9);
    let across = 1.0 - u;
    let down = 1.0 - v;
    return down * top + v * bottom + across * left + u * right
        - (across * down * top_left
            + u * down * top_right
            + across * v * bottom_left
            + u * v * bottom_right);
}

fn bw_patch_du(u: f32, v: f32) -> vec2<f32> {
    let top = bw_derivative(bw_point(0), bw_point(1), bw_point(2), bw_point(3), u);
    let bottom = bw_derivative(bw_point(9), bw_point(8), bw_point(7), bw_point(6), u);
    let left = bw_left_side(v);
    let right = bw_right_side(v);
    let top_left = bw_point(0);
    let top_right = bw_point(3);
    let bottom_right = bw_point(6);
    let bottom_left = bw_point(9);
    let down = 1.0 - v;
    return down * top + v * bottom - left + right
        + (down * (top_left - top_right) + v * (bottom_left - bottom_right));
}

fn bw_patch_dv(u: f32, v: f32) -> vec2<f32> {
    let top = bw_top(u);
    let bottom = bw_bottom(u);
    let left = bw_derivative(bw_point(0), bw_point(11), bw_point(10), bw_point(9), v);
    let right = bw_derivative(bw_point(3), bw_point(4), bw_point(5), bw_point(6), v);
    let top_left = bw_point(0);
    let top_right = bw_point(3);
    let bottom_right = bw_point(6);
    let bottom_left = bw_point(9);
    let across = 1.0 - u;
    return across * left + u * right - top + bottom
        + (across * (top_left - bottom_left) + u * (top_right - bottom_right));
}

fn bw_solve(goal: vec2<f32>, steps: u32) -> vec2<f32> {
    var parameters = goal;
    for (var iteration: u32 = 0u; iteration < steps; iteration = iteration + 1u) {
        let mapped = bw_patch(parameters.x, parameters.y);
        let residual = mapped - goal;
        let du = bw_patch_du(parameters.x, parameters.y);
        let dv = bw_patch_dv(parameters.x, parameters.y);
        let determinant = du.x * dv.y - dv.x * du.y;
        if (!(determinant > 0.0)) {
            return vec2<f32>(-1.0, -1.0);
        }
        let step_u = (-residual.x * dv.y + residual.y * dv.x) / determinant;
        let step_v = (-residual.y * du.x + residual.x * du.y) / determinant;
        parameters = parameters + vec2<f32>(step_u, step_v);
        if (!(parameters.x == parameters.x) || !(parameters.y == parameters.y)) {
            return vec2<f32>(-1.0, -1.0);
        }
        if (abs(parameters.x) > 4.0 || abs(parameters.y) > 4.0) {
            return vec2<f32>(-1.0, -1.0);
        }
    }
    return clamp(parameters, vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0));
}

fn bw_load(pixel: vec2<i32>) -> vec4<f32> {
    let limit = vec2<i32>(textureDimensions(bw_src)) - vec2<i32>(1, 1);
    return textureLoad(bw_src, clamp(pixel, vec2<i32>(0, 0), limit), 0);
}

fn bw_premultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    return vec4<f32>(sample.rgb * alpha, alpha);
}

fn bw_unpremultiply(sample: vec4<f32>) -> vec4<f32> {
    let alpha = clamp(sample.a, 0.0, 1.0);
    if (alpha <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(sample.rgb / alpha, alpha);
}

fn bw_sample(x: f32, y: f32) -> vec4<f32> {
    let limit = vec2<i32>(textureDimensions(bw_src)) - vec2<i32>(1, 1);
    let limit_f = vec2<f32>(f32(limit.x), f32(limit.y));
    let p = clamp(vec2<f32>(x, y), vec2<f32>(0.0, 0.0), limit_f);
    let base = floor(p);
    let fraction = p - base;
    let x0 = i32(base.x);
    let y0 = i32(base.y);
    let x1 = min(x0 + 1, limit.x);
    let y1 = min(y0 + 1, limit.y);
    let top_left = bw_premultiply(textureLoad(bw_src, vec2<i32>(x0, y0), 0));
    let top_right = bw_premultiply(textureLoad(bw_src, vec2<i32>(x1, y0), 0));
    let bottom_left = bw_premultiply(textureLoad(bw_src, vec2<i32>(x0, y1), 0));
    let bottom_right = bw_premultiply(textureLoad(bw_src, vec2<i32>(x1, y1), 0));
    let top = top_left + (top_right - top_left) * fraction.x;
    let bottom = bottom_left + (bottom_right - bottom_left) * fraction.x;
    return top + (bottom - top) * fraction.y;
}

@fragment
fn fs_bezier_warp(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = bw_load(pixel);
    let frame = bw.size.xy;
    let goal = vec2<f32>(f32(pixel.x) / frame.x, f32(pixel.y) / frame.y);
    let steps = min(u32(bw.control.x), 13u);
    let parameters = bw_solve(goal, steps);
    if (!(parameters.x >= 0.0) || !(parameters.y >= 0.0)) {
        return source;
    }
    let sample = bw_sample(parameters.x * frame.x, parameters.y * frame.y);
    return bw_unpremultiply(sample);
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::find_edges::{find_edges_frame_f32, FindEdgesGrade};
    use mondrian_effects::{box_blur, BoxBlurDirection};
    use pollster::block_on;

    #[test]
    fn spatial_effect_shader_validates() {
        let module = naga::front::wgsl::parse_str(SPATIAL_EFFECT_SHADER)
            .expect("spatial effect shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spatial effect shader validates");
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_median"),
            "the median fragment entry point must survive validation"
        );
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_dust_scratches"),
            "the dust & scratches fragment entry point must survive validation"
        );
    }

    #[test]
    fn spatial_bezier_warp_shader_validates() {
        let module = naga::front::wgsl::parse_str(SPATIAL_BEZIER_WARP_SHADER)
            .expect("spatial bezier warp shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spatial bezier warp shader validates");
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_bezier_warp"),
            "the bezier warp fragment entry point must survive validation"
        );
    }

    #[test]
    fn spatial_stroke_shader_validates() {
        let module = naga::front::wgsl::parse_str(SPATIAL_STROKE_SHADER)
            .expect("spatial stroke shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spatial stroke shader validates");
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_stroke"),
            "the stroke fragment entry point must survive validation"
        );
    }

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

    /// Compares one median result against the CPU kernel.
    ///
    /// The selected bin is the kernel contract and must be identical. The decoded
    /// value may sit one ulp away from the CPU quotient because the D3D12 shader
    /// compiler lowers `/ 255.0` into a multiply by a rounded reciprocal, which is
    /// not correctly rounded; the visible difference stays below 1.0e-6 (one
    /// 8-bit step is 1/255).
    fn assert_median_parity(cpu: &[[f32; 4]], gpu: &[[f32; 4]], label: &str) {
        assert_eq!(cpu.len(), gpu.len(), "{label} frame length");
        for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
            for channel in 0..4 {
                let cpu_value = cpu_pixel[channel];
                let gpu_value = gpu_pixel[channel];
                let cpu_bin = (cpu_value * 255.0).round() as i32;
                let gpu_bin = (gpu_value * 255.0).round() as i32;
                assert_eq!(
                    cpu_bin, gpu_bin,
                    "{label} pixel {pixel_index} channel {channel} rank: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
                assert!(
                    (cpu_value - gpu_value).abs() <= 1.0e-6,
                    "{label} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }
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
    fn gpu_compute_workgroup_execution_probe() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping workgroup probe: no GPU adapter available");
            return;
        };
        println!(
            "adapter max_compute_workgroup_size_x={} max_invocations_per_workgroup={}",
            context.device.limits().max_compute_workgroup_size_x,
            context.device.limits().max_compute_invocations_per_workgroup
        );
        let shader = context.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mondrian.probe.shader"),
            source: wgpu::ShaderSource::Wgsl(
                r#"
@group(0) @binding(0) var<storage, read_write> output: atomic<u32>;

@compute @workgroup_size(64)
fn cs_main(@builtin(local_invocation_id) lid: vec3<u32>) {
    atomicOr(&output, 1u << (lid.x % 32u));
}
"#
                .into(),
            ),
        });
        let pipeline = context.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("mondrian.probe.pipeline"),
            layout: None,
            module: &shader,
            entry_point: Some("cs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian.probe.buffer"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        context.queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&0_u32));
        let readback = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mondrian.probe.readback"),
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.probe.encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mondrian.probe.pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(
                0,
                &context.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("mondrian.probe.bind-group"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                }),
                &[],
            );
            pass.dispatch_workgroups(1, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&buffer, 0, &readback, 0, 4);
        context.queue.submit(Some(encoder.finish()));
        readback.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("probe readback");
        });
        context
            .device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .expect("poll");
        let value = {
            let range = readback.slice(..).get_mapped_range().expect("mapped probe range");
            u32::from_le_bytes([range[0], range[1], range[2], range[3]])
        };
        assert!(
            value.count_ones() > 1,
            "compute workgroup executed {value:?} (single-lane masks mean the toolchain collapsed the workgroup)"
        );
    }

    #[test]
    fn gpu_emboss_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU emboss parity test: no GPU adapter available");
            return;
        };
        let width = 6_u32;
        let height = 4_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, value * 0.5, 0.75 - value * 0.1, 1.0];
            }
        }
        pixels[5] = [0.0, 0.0, 0.0, 0.0];
        let grade = mondrian_effects::emboss::EmbossGrade::for_space(
            45.0,
            4.0,
            200.0,
            0.25,
            mondrian_core::WorkingColorSpace::LinearRec709,
        )
        .expect("grade");
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::emboss::emboss_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade
        ));
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.emboss-encoder"),
        });
        let output = executor
            .record_emboss(
                &context.device,
                &mut encoder,
                grade.gpu_controls(),
                grade.luminance_coefficients(),
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded emboss");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
            for channel in 0..4 {
                let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                assert!(
                    difference <= 1.0e-4,
                    "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }
    }

    #[test]
    fn gpu_color_emboss_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU colour emboss parity test: no GPU adapter available");
            return;
        };
        let width = 6_u32;
        let height = 4_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, value * 0.5, 0.75 - value * 0.1, 1.0];
            }
        }
        pixels[5] = [0.0, 0.0, 0.0, 0.0];
        let grade = mondrian_effects::ColorEmbossGrade::new(45.0, 4.0, 200.0, 0.25).expect("grade");
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::color_emboss::color_emboss_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade
        ));
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.color-emboss-encoder"),
        });
        let output = executor
            .record_color_emboss(
                &context.device,
                &mut encoder,
                grade.gpu_controls(),
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded colour emboss");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
            for channel in 0..4 {
                let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                assert!(
                    difference <= 1.0e-4,
                    "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }
    }

    #[test]
    fn gpu_find_edges_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU find edges parity test: no GPU adapter available");
            return;
        };
        let width = 6_u32;
        let height = 5_u32;
        let pixel_count = (width * height) as usize;
        let mut pixels = vec![[0.1_f32, 0.2, 0.3, 1.0]; pixel_count];
        for y in 0..height as usize {
            for x in 0..width as usize {
                if x >= 3 {
                    pixels[y * width as usize + x] = [1.5, 0.5, 0.25, 1.0];
                }
            }
        }
        pixels[2 * width as usize + 1] = [0.0, 0.0, 0.0, 0.0];
        let grade = FindEdgesGrade::new(0.25, true).expect("grade");
        let mut cpu = pixels.clone();
        assert!(find_edges_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade
        ));
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.find-edges-encoder"),
        });
        let output = executor
            .record_find_edges(
                &context.device,
                &mut encoder,
                grade.blend_with_original(),
                grade.invert(),
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded find edges");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
            for channel in 0..4 {
                let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                assert!(
                    difference <= 1.0e-4,
                    "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }
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
    fn a_lowered_spatial_chain_executes_every_stage_in_graph_order() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU spatial chain test: no GPU adapter available");
            return;
        };
        let width = 15_u32;
        let height = 13_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.03 * (2 * x + 3 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.4 - value * 0.5, value * 1.4, 1.0];
            }
        }
        pixels[24] = [0.0, 0.0, 0.0, 0.0];
        pixels[97] = [2.0, -0.6, 1.5, 0.5];

        let twirl =
            mondrian_effects::TwirlGrade::new(180.0, 60.0, [7.5, 6.0]).expect("valid twirl grade");
        let spherize =
            mondrian_effects::SpherizeGrade::new(4.0, [6.0, 5.5]).expect("valid spherize grade");
        let magnify = mondrian_effects::MagnifyGrade::new(
            [7.0, 6.0],
            8.0,
            1.5,
            2.0,
            0.8,
            mondrian_effects::MagnifyShape::Circle,
            mondrian_effects::MagnifyScaling::Bilinear,
            mondrian_core::BlendMode::Normal,
        )
        .expect("valid magnify grade");
        let dent = mondrian_effects::DentGrade::new(
            0.6,
            0.5,
            [6.5, 6.0],
            mondrian_effects::DentKind::Dent,
            mondrian_effects::DentEdge::Smooth,
        )
        .expect("valid dent grade");
        let flip =
            mondrian_effects::FlipGrade::new(mondrian_effects::FlipAxis::Both, [7.5, 6.25], 70.0)
                .expect("valid flip grade");
        let ripple = mondrian_effects::RippleGrade::new(
            70.0,
            [7.0, 6.0],
            mondrian_effects::RippleType::Asymmetric,
            5.5,
            2.0,
            0.15,
        )
        .expect("valid ripple grade");
        let wave_warp = mondrian_effects::WaveWarpGrade::new(
            mondrian_effects::WaveType::Triangle,
            4.5,
            26.0,
            30.0,
            0.2,
            mondrian_effects::WavePin::AllEdges,
            mondrian_effects::WaveQuality::Normal,
        )
        .expect("valid wave warp grade");
        let transform = mondrian_effects::TransformGrade::new(
            [0.0, 0.0],
            [0.75, -0.5],
            105.0,
            100.0,
            100.0,
            6.0,
            20.0,
            15.0,
            100.0,
            false,
            180.0,
            mondrian_effects::TransformSampling::Normal,
        )
        .expect("valid transform grade");
        let corner_pin = mondrian_effects::CornerPinGrade::new(
            [0.08, 0.05],
            [0.95, -0.02],
            [0.02, 0.92],
            [0.9, 1.03],
        )
        .expect("valid corner pin grade");
        let turbulent_displace = mondrian_effects::TurbulentDisplaceGrade::new(
            mondrian_effects::DisplacementType::Bulge,
            3.5,
            24.0,
            [4.0, 3.5],
            2,
            0.25,
            true,
            3,
            7,
            mondrian_effects::DisplacePin::HorizontalEdges,
            0.5,
            mondrian_effects::DisplaceAntialias::Low,
        )
        .expect("valid turbulent displace grade");
        let deband =
            mondrian_effects::DebandGrade::new(0.05, 2.0, 0.1).expect("valid deband grade");
        let glow = mondrian_effects::GlowGrade::new(
            mondrian_effects::GlowBasedOn::Color,
            120.0,
            4.0,
            80.0,
            mondrian_effects::GlowComposite::OnTop,
            mondrian_effects::GlowOperation::Screen,
            mondrian_effects::GlowColors::Original,
            1.0,
            0.0,
            0.5,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            mondrian_effects::GlowDimensions::Both,
            [0.2126, 0.7152, 0.0722],
        )
        .expect("valid glow grade");
        let fractal_noise = mondrian_effects::FractalNoiseGrade::new(
            mondrian_effects::FractalType::Rock,
            mondrian_effects::FractalNoiseType::Linear,
            false,
            130.0,
            5.0,
            mondrian_effects::FractalOverflow::SoftClamp,
            15.0,
            true,
            220.0,
            100.0,
            100.0,
            [6.0, -4.0],
            true,
            5,
            45.0,
            40.0,
            10.0,
            [1.5, -0.5],
            0.4,
            360.0,
            false,
            1,
            9,
            85.0,
            mondrian_core::BlendMode::Overlay,
        )
        .expect("valid fractal noise grade");
        let keylight = mondrian_effects::KeylightGrade::new(
            [0.0, 1.0, 0.0],
            1.0,
            0.5,
            1.0,
            [0.5, 0.5, 0.5],
            0.0,
            100.0,
            2.0,
            1.0,
            0.0,
            mondrian_effects::KeylightDisplay::Final,
        )
        .expect("valid keylight grade");
        // Grow, smooth, and composite: five passes, which keeps the stage inside
        // the six scratch textures the chain leaves free beside its source.
        let matte_refine = mondrian_effects::MatteRefineGrade::new(
            1.0,   // edge detail
            10.0,  // other edge radius
            0.0,   // view edge region
            6.0,   // smooth
            0.0,   // feather
            0.0,   // contrast
            4.0,   // shift edge
            false, // better quality
            false, // clean edge colour
            100.0, // clean amount
            true,  // extend smooth
            0.0,   // expand radius
            0.0,   // view map
            1.0,   // tremor reduction
            50.0,  // tremor amount
            0.0,   // more motion blur
            8.0,   // per frame samples
            180.0, // shutter angle degrees
        )
        .expect("valid matte refine grade");
        // Erode the chain alpha by 2.5 px: two morphology passes plus the
        // composite, which fits the two scratch textures the stage is sized for.
        let choker = mondrian_effects::ChokerGrade::new(2.5).expect("valid choker grade");
        // Protect the alpha channel, then run a four-pass minimum-then-maximum
        // chain: the two minimum passes leave the matte alone and the two
        // maximum passes dilate the holes, so the stage also checks that the
        // chain table and the scratch geometry agree.
        let minimax = mondrian_effects::MinimaxGrade::new(
            2.0,
            mondrian_effects::MinimaxOperation::MinimumThenMaximum,
            mondrian_effects::MinimaxChannel::Alpha,
            mondrian_effects::MinimaxDirection::Both,
            1.0,
        )
        .expect("valid minimax grade");
        // Blur every channel by a different amount with transparent edges: the
        // stage runs a horizontal and a vertical pass, so the two-pass scratch
        // ping-pong and the per-channel radii are both exercised in the chain.
        let channel_blur = mondrian_effects::ChannelBlurGrade::new(
            1.0,
            2.0,
            0.0,
            3.0,
            0.0,
            mondrian_effects::ChannelBlurDirection::Both,
        )
        .expect("valid channel blur grade");
        // Bend the frame along an arc before the dent pulls it towards its
        // centre, so the chain also covers the warp remap and the scratch reuse
        // between two adjacent full-frame spatial passes.
        let warp = mondrian_effects::WarpGrade::new(
            60.0,
            30.0,
            -25.0,
            mondrian_effects::WarpStyle::Arc,
            mondrian_effects::WarpAxis::Horizontal,
        )
        .expect("valid warp grade");
        let grid_warp =
            mondrian_effects::GridWarpGrade::new(6, 5, 8).expect("valid grid warp grade");
        let wire_removal = mondrian_effects::WireRemovalGrade::new(
            [7.5, 6.5],
            [7.5, 2.5],
            mondrian_effects::RemovalStyle::Both,
            2.0,
            0.5,
            0.25,
            0,
        )
        .expect("valid wire removal grade");
        let roughen_edges = mondrian_effects::RoughenEdgesGrade::new(
            2,
            [0.6, 0.2, 0.0],
            3.0,
            1.5,
            0.5,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            90.0,
            false,
            1,
            0,
        )
        .expect("valid roughen edges grade");
        // Take the exact window median with the alpha toggle off: the pass runs
        // first so every later stage consumes its quantised output, and radius 2
        // stays inside the GPU cap.
        let median = mondrian_effects::MedianGrade::new(2.0, 0.0).expect("valid median grade");
        // Route a reds tint through the stage's own 160-byte uniform and private
        // shader module, so the chain also proves the dedicated bind group layout
        // records beside the shared one.
        let selective_color = mondrian_effects::SelectiveColorGrade::new(
            1,
            1,
            [0.0, 0.0, 0.0, 25.0],
            [[40.0, 0.0, 0.0, 0.0]; 9],
        )
        .expect("valid selective colour grade");
        // Follow it with a stroke band so the chain also proves the second
        // dedicated uniform payload records beside the selective colour one.
        let stroke = mondrian_effects::StrokeGrade::new(
            mondrian_effects::StrokePaintStyle::OnOriginal,
            [1.0, 0.5, 0.25],
            3.0,
            0.5,
            0.8,
        )
        .expect("valid stroke grade");
        let dust_scratches = mondrian_effects::DustScratchesGrade::new(1.0, 0.1, 0.0)
            .expect("valid dust & scratches grade");
        // Bend the top edge of the patch towards the frame centre: the stage runs
        // the Newton solve of the Coons patch, so the chain also proves the
        // dedicated eight-cell uniform records through the shared pass recorder.
        let third = 256.0_f32 / 3.0;
        let mut bezier_points = [[0.0_f32, 0.0_f32]; 12];
        bezier_points[0] = [0.0, 0.0];
        bezier_points[1] = [third, 24.0];
        bezier_points[2] = [2.0 * third, 24.0];
        bezier_points[3] = [256.0, 0.0];
        bezier_points[4] = [256.0, third];
        bezier_points[5] = [256.0, 2.0 * third];
        bezier_points[6] = [256.0, 256.0];
        bezier_points[7] = [2.0 * third, 256.0];
        bezier_points[8] = [third, 256.0];
        bezier_points[9] = [0.0, 256.0];
        bezier_points[10] = [0.0, 2.0 * third];
        bezier_points[11] = [0.0, third];
        let bezier_warp =
            mondrian_effects::BezierWarpGrade::from_reference_points(bezier_points, 4)
                .expect("valid bezier warp grade");
        // Rotate the panorama sphere: the stage is the one spatial pass whose
        // uniform carries the eye sub-frame size, so the chain also proves the
        // integer-divided eye rectangle records through the shared pass recorder.
        let vr_rotate_sphere = mondrian_effects::VrRotateSphereGrade::new(
            360.0,
            180.0,
            12.0,
            -20.0,
            5.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
            false,
        )
        .expect("valid vr rotate sphere grade");
        // Project the panorama onto a perspective plane: the stage always
        // records a lens, so the chain also covers the pass that has no
        // identity to skip.
        let vr_sphere_to_plane =
            mondrian_effects::VrSphereToPlaneGrade::new(120.0, 8.0, -12.0, 4.0, false)
                .expect("valid vr sphere to plane grade");
        // Project a perspective card onto the panorama sphere: the pass has no
        // identity, and it is the one projection that feathers on the
        // destination patch, so the chain also covers the band composite.
        let vr_project_2d = mondrian_effects::VrProject2DGrade::new(
            240.0,
            120.0,
            50.0,
            4.0,
            20.0,
            6.0,
            -8.0,
            2.0,
            5.0,
            -10.0,
            3.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
        )
        .expect("valid vr project 2d grade");
        let passes = [
            mondrian_effects::EffectGpuSpatialPass::Median { grade: median },
            mondrian_effects::EffectGpuSpatialPass::SelectiveColor { grade: selective_color },
            mondrian_effects::EffectGpuSpatialPass::Stroke { grade: stroke },
            mondrian_effects::EffectGpuSpatialPass::BoxBlur {
                radius: 1.5,
                iterations: 1,
                direction: BoxBlurDirection::HorizontalAndVertical,
                repeat_edge_pixels: true,
            },
            mondrian_effects::EffectGpuSpatialPass::Twirl { grade: twirl },
            mondrian_effects::EffectGpuSpatialPass::Spherize { grade: spherize },
            mondrian_effects::EffectGpuSpatialPass::Magnify { grade: magnify },
            mondrian_effects::EffectGpuSpatialPass::Warp { grade: warp },
            mondrian_effects::EffectGpuSpatialPass::GridWarp { grade: grid_warp },
            mondrian_effects::EffectGpuSpatialPass::WireRemoval { grade: wire_removal },
            mondrian_effects::EffectGpuSpatialPass::RoughenEdges { grade: roughen_edges },
            mondrian_effects::EffectGpuSpatialPass::Dent { grade: dent },
            mondrian_effects::EffectGpuSpatialPass::Flip { grade: flip },
            mondrian_effects::EffectGpuSpatialPass::Ripple { grade: ripple },
            mondrian_effects::EffectGpuSpatialPass::WaveWarp { grade: wave_warp },
            mondrian_effects::EffectGpuSpatialPass::Transform { grade: transform },
            mondrian_effects::EffectGpuSpatialPass::CornerPin { grade: corner_pin },
            mondrian_effects::EffectGpuSpatialPass::TurbulentDisplace { grade: turbulent_displace },
            mondrian_effects::EffectGpuSpatialPass::Deband { grade: deband },
            mondrian_effects::EffectGpuSpatialPass::Glow { grade: glow },
            mondrian_effects::EffectGpuSpatialPass::FractalNoise { grade: fractal_noise },
            mondrian_effects::EffectGpuSpatialPass::Keylight { grade: keylight },
            mondrian_effects::EffectGpuSpatialPass::MatteRefine { grade: matte_refine },
            mondrian_effects::EffectGpuSpatialPass::Choker { grade: choker },
            mondrian_effects::EffectGpuSpatialPass::Minimax { grade: minimax },
            mondrian_effects::EffectGpuSpatialPass::ChannelBlur { grade: channel_blur },
            mondrian_effects::EffectGpuSpatialPass::DustScratches { grade: dust_scratches },
            mondrian_effects::EffectGpuSpatialPass::BezierWarp { grade: bezier_warp },
            mondrian_effects::EffectGpuSpatialPass::VrRotateSphere { grade: vr_rotate_sphere },
            mondrian_effects::EffectGpuSpatialPass::VrSphereToPlane { grade: vr_sphere_to_plane },
            mondrian_effects::EffectGpuSpatialPass::VrProject2D { grade: vr_project_2d },
        ];
        // One stage per spatial pass the renderer can lower, so a new pass has to
        // extend this chain as well as the dispatch arms.
        assert_eq!(passes.len(), 31, "the spatial chain must cover every pass");

        let mut cpu = pixels.clone();
        assert!(mondrian_effects::median_frame_f32(
            &mut cpu, width, height, &median
        ));
        assert!(mondrian_effects::selective_color_frame_f32(
            &mut cpu,
            &selective_color,
        ));
        assert!(mondrian_effects::stroke_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &stroke,
        ));
        assert!(mondrian_effects::box_blur_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            1.5,
            1,
            BoxBlurDirection::HorizontalAndVertical,
            true,
        ));
        assert!(mondrian_effects::twirl_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            twirl,
        ));
        assert!(mondrian_effects::spherize_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            spherize,
        ));
        assert!(mondrian_effects::magnify_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            magnify,
        ));
        assert!(mondrian_effects::warp_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            warp,
        ));
        assert!(mondrian_effects::grid_warp_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grid_warp,
        ));
        assert!(mondrian_effects::wire_removal_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            wire_removal,
        ));
        assert!(mondrian_effects::roughen_edges_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &roughen_edges,
        ));
        assert!(mondrian_effects::dent_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            dent,
        ));
        assert!(mondrian_effects::flip_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &flip,
        ));
        assert!(mondrian_effects::ripple_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            ripple,
        ));
        assert!(mondrian_effects::wave_warp_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            wave_warp,
        ));
        assert!(mondrian_effects::transform_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &transform,
        ));
        assert!(mondrian_effects::corner_pin_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &corner_pin,
        ));
        assert!(mondrian_effects::turbulent_displace_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &turbulent_displace,
        ));
        assert!(mondrian_effects::deband_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &deband,
        ));
        assert!(mondrian_effects::glow_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &glow,
        ));
        assert!(mondrian_effects::fractal_noise_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &fractal_noise,
        ));
        assert!(mondrian_effects::keylight_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &keylight,
        ));
        assert!(mondrian_effects::matte_refine_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &matte_refine,
        ));
        assert!(mondrian_effects::choker_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &choker,
        ));
        assert!(mondrian_effects::minimax_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &minimax,
        ));
        assert!(mondrian_effects::channel_blur_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            &channel_blur,
        ));
        assert!(mondrian_effects::dust_scratches_frame_f32(
            &mut cpu,
            width,
            height,
            &dust_scratches,
        ));
        assert!(mondrian_effects::bezier_warp_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            bezier_warp,
        ));
        assert!(mondrian_effects::vr_rotate_sphere_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            vr_rotate_sphere,
        ));
        assert!(mondrian_effects::vr_sphere_to_plane_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            vr_sphere_to_plane,
        ));
        assert!(mondrian_effects::vr_project_2d_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            vr_project_2d,
        ));

        let executor = EffectSpatialGpu::new(&context.device);
        let original = upload(&context, &pixels, width, height);
        // Seven rotating scratch textures keep every stage's destination out of
        // its own source, which wgpu rejects as conflicting texture usage, and
        // give the three-pass deband chain, the six-pass glow chain, the
        // four-pass minimax chain, and the two-pass channel blur chain their
        // intermediates.
        let pool: Vec<wgpu::Texture> =
            (0..7).map(|_| scratch(&context, width, height, 1).remove(0)).collect();
        let mut current: Option<usize> = None;
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.chain-encoder"),
        });
        for pass in &passes {
            let needed =
                EffectSpatialGpu::texture_pass_count(pass).expect("supported pass") as usize;
            let available: Vec<usize> = (0..pool.len())
                .filter(|index| Some(*index) != current)
                .take(needed.max(1))
                .collect();
            assert_eq!(
                available.len(),
                needed.max(1),
                "spatial chain needs more scratch"
            );
            let write_pool: Vec<wgpu::Texture> =
                available.iter().map(|index| pool[*index].clone()).collect();
            let source = current.map_or_else(|| original.clone(), |index| pool[index].clone());
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    pass,
                    &source,
                    &write_pool,
                    width,
                    height,
                )
                .expect("recorded spatial stage");
            // A multi-pass stage may hand back any texture it wrote, so locate
            // the destination instead of assuming the last candidate.
            let written = available
                .iter()
                .copied()
                .find(|index| pool[*index] == output)
                .expect("the stage must write one of its pool textures");
            current = Some(written);
        }
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(
            &context,
            &pool[current.expect("a written stage")],
            width,
            height,
        );
        for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
            for channel in 0..4 {
                let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                assert!(
                    difference <= 1.0e-4,
                    "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }
    }
    #[test]
    fn gpu_color_offset_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU colour offset parity test: no GPU adapter available");
            return;
        };
        let width = 11_u32;
        let height = 5_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.09 * (x as f32) + 0.03 * (y as f32);
                pixels[y * width as usize + x] = [value, 1.0 - value, 0.2 + value * 0.5, 1.0];
            }
        }
        pixels[16] = [0.0, 0.0, 0.0, 0.0];
        pixels[39] = [2.5, -0.75, 1.25, 0.5];
        let grades = [
            mondrian_effects::ColorOffsetGrade::new(
                [2.0, 0.0, -1.0],
                mondrian_effects::ColorOffsetOverflow::Clamp,
            )
            .expect("clamp grade"),
            mondrian_effects::ColorOffsetGrade::new(
                [0.5, -1.25, 3.0],
                mondrian_effects::ColorOffsetOverflow::Wrap,
            )
            .expect("wrap grade"),
            mondrian_effects::ColorOffsetGrade::new(
                [-2.0, 1.0, 0.5],
                mondrian_effects::ColorOffsetOverflow::Mirror,
            )
            .expect("mirror grade"),
        ];
        for grade in grades {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::color_offset_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.color-offset-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_color_offset(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("colour offset pass should record");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_linear_color_key_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU linear colour key parity test: no GPU adapter available");
            return;
        };
        let width = 9_u32;
        let height = 7_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.11 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, 0.9 - value, 0.4 + value * 0.5, 1.0];
            }
        }
        pixels[12] = [0.0, 1.0, 0.0, 1.0];
        pixels[20] = [0.0, 0.0, 0.0, 0.0];
        pixels[33] = [2.5, -0.5, 1.25, 0.5];
        let grades = [
            mondrian_effects::LinearColorKeyGrade::new(
                [0.0, 1.0, 0.0],
                mondrian_effects::MatchColours::Rgb,
                0.1,
                0.2,
                mondrian_effects::KeyOperation::KeyColours,
            )
            .expect("rgb key grade"),
            mondrian_effects::LinearColorKeyGrade::new(
                [0.3, 0.4, 0.6],
                mondrian_effects::MatchColours::Chroma,
                0.05,
                0.1,
                mondrian_effects::KeyOperation::KeepColours,
            )
            .expect("chroma key grade"),
            mondrian_effects::LinearColorKeyGrade::new(
                [0.2, 0.8, 0.2],
                mondrian_effects::MatchColours::Green,
                0.15,
                0.0,
                mondrian_effects::KeyOperation::KeyColours,
            )
            .expect("green key grade"),
        ];
        for grade in grades {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::linear_color_key_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.linear-color-key-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_linear_color_key(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded linear colour key");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "linear colour key pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_split_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU split parity test: no GPU adapter available");
            return;
        };
        let width = 14_u32;
        let height = 12_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.03 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.35 + value * 0.5, 0.7 - value, 1.0];
            }
        }
        pixels[19] = [0.0, 0.0, 0.0, 0.0];
        pixels[92] = [2.5, -0.75, 1.25, 0.5];
        for (point_a, point_b, split) in [
            ([0.0_f32, 0.0], [13.0_f32, 0.0], 6.0_f32),
            ([0.0, 6.0], [0.0, 11.0], 2.5),
            ([0.3, 0.0], [13.3, 11.0], 3.5),
        ] {
            let grade =
                mondrian_effects::SplitGrade::new(point_a, point_b, split).expect("split grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::split_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.split-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_split(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded split");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "split {point_a:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_ripple_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU ripple parity test: no GPU adapter available");
            return;
        };
        let width = 23_u32;
        let height = 19_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.021 * (x + 3 * y) as f32;
                pixels[y * width as usize + x] = [
                    value,
                    0.62 - value * 0.35,
                    0.15 + value * 0.8,
                    1.0 - 0.02 * (x % 4) as f32,
                ];
            }
        }
        // Exact zero coverage must canonicalise hidden colour on both paths, and
        // every pixel outside the ripple passes through the premultiplied
        // round trip rather than the raw texture value.
        pixels[16] = [0.0, 0.0, 0.0, 0.0];
        pixels[100] = [3.0, 0.5, -1.0, 0.0];
        pixels[240] = [1.75, -0.4, 0.9, 0.35];
        for (radius, centre, wave_type, wave_width, wave_height, phase) in [
            (
                80.0_f32,
                [11.0_f32, 9.5_f32],
                mondrian_effects::RippleType::Asymmetric,
                7.0_f32,
                3.0_f32,
                0.1_f32,
            ),
            (
                45.0,
                [10.25, 8.75],
                mondrian_effects::RippleType::Symmetric,
                11.0,
                5.5,
                0.35,
            ),
            (
                100.0,
                [0.0, 0.0],
                mondrian_effects::RippleType::Asymmetric,
                5.0,
                2.5,
                0.7,
            ),
        ] {
            let grade = mondrian_effects::RippleGrade::new(
                radius,
                centre,
                wave_type,
                wave_width,
                wave_height,
                phase,
            )
            .expect("ripple grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::ripple_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.ripple-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_ripple(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded ripple");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "ripple {wave_type:?} pixel {index} channel {channel}: \
                         cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_flip_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU flip parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.03 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [
                    value,
                    0.55 - value * 0.4,
                    0.2 + value * 0.9,
                    1.0 - 0.01 * (x % 5) as f32,
                ];
            }
        }
        pixels[16] = [0.0, 0.0, 0.0, 0.0];
        pixels[81] = [2.5, -0.75, 1.25, 0.5];
        for (axis, centre, blend) in [
            (
                mondrian_effects::FlipAxis::Horizontal,
                [6.0_f32, 5.0_f32],
                100.0_f32,
            ),
            (mondrian_effects::FlipAxis::Vertical, [5.5, 4.5], 60.0),
            (mondrian_effects::FlipAxis::Both, [5.25, 4.75], 35.0),
        ] {
            let grade = mondrian_effects::FlipGrade::new(axis, centre, blend).expect("flip grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::flip_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                &grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.flip-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_flip(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded flip");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "flip {} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}",
                        axis.as_key()
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_wave_warp_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU wave warp parity test: no GPU adapter available");
            return;
        };
        let width = 19_u32;
        let height = 17_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.027 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [
                    value,
                    0.5 - value * 0.35,
                    0.15 + value * 0.8,
                    1.0 - 0.015 * (y % 4) as f32,
                ];
            }
        }
        pixels[33] = [0.0, 0.0, 0.0, 0.0];
        pixels[150] = [1.75, -0.4, 0.9, 0.35];
        for (wave_type, crest, wavelength, direction, phase, pin, quality) in [
            (
                mondrian_effects::WaveType::Sine,
                6.0_f32,
                40.0_f32,
                90.0_f32,
                0.125_f32,
                mondrian_effects::WavePin::None,
                mondrian_effects::WaveQuality::Low,
            ),
            (
                mondrian_effects::WaveType::Triangle,
                5.5,
                24.0,
                0.0,
                0.3,
                mondrian_effects::WavePin::AllEdges,
                mondrian_effects::WaveQuality::Normal,
            ),
            (
                mondrian_effects::WaveType::Circle,
                4.5,
                18.0,
                45.0,
                0.6,
                mondrian_effects::WavePin::Center,
                mondrian_effects::WaveQuality::High,
            ),
            (
                mondrian_effects::WaveType::SmoothNoise,
                5.0,
                30.0,
                30.0,
                0.2,
                mondrian_effects::WavePin::VerticalEdges,
                mondrian_effects::WaveQuality::Low,
            ),
        ] {
            let grade = mondrian_effects::WaveWarpGrade::new(
                wave_type, crest, wavelength, direction, phase, pin, quality,
            )
            .expect("wave warp grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::wave_warp_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.wave-warp-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_wave_warp(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded wave warp");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "wave warp {} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}",
                        wave_type.as_key()
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_transform_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU transform parity test: no GPU adapter available");
            return;
        };
        let width = 21_u32;
        let height = 17_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.023 * (x + 3 * y) as f32;
                pixels[y * width as usize + x] = [
                    value,
                    0.5 - 0.37 * value,
                    0.15 + 0.8 * value,
                    1.0 - 0.02 * (y % 5) as f32,
                ];
            }
        }
        pixels[36] = [0.0, 0.0, 0.0, 0.0];
        pixels[301] = [1.75, -0.4, 0.9, 0.35];
        for (index, grade) in [
            mondrian_effects::TransformGrade::new(
                [0.0, 0.0],
                [1.5, -0.5],
                100.0,
                100.0,
                100.0,
                0.0,
                0.0,
                0.0,
                100.0,
                false,
                180.0,
                mondrian_effects::TransformSampling::Low,
            )
            .expect("translation transform grade"),
            mondrian_effects::TransformGrade::new(
                [-3.5, 2.0],
                [2.5, 1.5],
                85.0,
                100.0,
                100.0,
                0.0,
                0.0,
                30.0,
                100.0,
                false,
                180.0,
                mondrian_effects::TransformSampling::Normal,
            )
            .expect("rotation transform grade"),
            mondrian_effects::TransformGrade::new(
                [0.0, 0.0],
                [0.0, 0.0],
                100.0,
                100.0,
                100.0,
                18.0,
                35.0,
                -12.0,
                100.0,
                true,
                240.0,
                mondrian_effects::TransformSampling::Normal,
            )
            .expect("skew transform grade"),
            mondrian_effects::TransformGrade::new(
                [2.0, -1.0],
                [-2.0, 3.0],
                120.0,
                90.0,
                110.0,
                -9.0,
                0.0,
                200.0,
                40.0,
                false,
                180.0,
                mondrian_effects::TransformSampling::High,
            )
            .expect("opacity transform grade"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::transform_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                &grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.transform-encoder"),
                });
            let output = executor
                .record_transform(
                    &context.device,
                    &mut encoder,
                    &grade,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded transform");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "transform grade {index} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_corner_pin_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU corner pin parity test: no GPU adapter available");
            return;
        };
        let width = 23_u32;
        let height = 19_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.019 * (x + 4 * y) as f32;
                pixels[y * width as usize + x] = [
                    value,
                    0.5 - 0.4 * value,
                    0.12 + 0.85 * value,
                    1.0 - 0.02 * ((x + y) % 5) as f32,
                ];
            }
        }
        pixels[41] = [0.0, 0.0, 0.0, 0.0];
        pixels[383] = [0.0, 0.0, 0.0, 0.0];
        for (index, grade) in [
            mondrian_effects::CornerPinGrade::new([0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0])
                .expect("identity corner pin grade"),
            mondrian_effects::CornerPinGrade::new(
                [0.1, 0.05],
                [1.1, 0.05],
                [0.1, 1.05],
                [1.1, 1.05],
            )
            .expect("translated corner pin grade"),
            mondrian_effects::CornerPinGrade::new([1.0, 0.0], [0.0, 0.0], [1.0, 1.0], [0.0, 1.0])
                .expect("mirrored corner pin grade"),
            mondrian_effects::CornerPinGrade::new(
                [0.15, 0.05],
                [0.85, -0.05],
                [-0.1, 1.02],
                [1.05, 0.98],
            )
            .expect("trapezoid corner pin grade"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::corner_pin_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                &grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.corner-pin-encoder"),
                });
            let output = executor
                .record_corner_pin(
                    &context.device,
                    &mut encoder,
                    &grade,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded corner pin");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "corner pin grade {index} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_turbulent_displace_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU turbulent displace parity test: no GPU adapter available");
            return;
        };
        let width = 23_u32;
        let height = 19_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.017 * (x + 3 * y) as f32;
                pixels[y * width as usize + x] = [
                    value,
                    0.55 - 0.4 * value,
                    0.15 + 0.8 * value,
                    1.0 - 0.02 * ((x * 3 + y) % 4) as f32,
                ];
            }
        }
        pixels[41] = [0.0, 0.0, 0.0, 0.0];
        pixels[383] = [2.4, -0.7, 1.6, 0.65];
        let cases = [
            (
                "default",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Turbulent,
                    50.0,
                    100.0,
                    [128.0, 128.0],
                    1,
                    0.0,
                    false,
                    1,
                    0,
                    mondrian_effects::DisplacePin::None,
                    0.0,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("default turbulent displace grade"),
            ),
            (
                "bulge",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Bulge,
                    -120.0,
                    64.0,
                    [100.0, 90.0],
                    3,
                    0.5,
                    false,
                    2,
                    11,
                    mondrian_effects::DisplacePin::None,
                    0.0,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("bulge turbulent displace grade"),
            ),
            (
                "twist cycle",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Twist,
                    80.0,
                    32.0,
                    [60.0, 70.0],
                    3,
                    0.375,
                    true,
                    5,
                    42,
                    mondrian_effects::DisplacePin::None,
                    0.0,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("twist turbulent displace grade"),
            ),
            (
                "cross resize",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Cross,
                    12.0,
                    18.0,
                    [90.0, 80.0],
                    2,
                    0.2,
                    false,
                    1,
                    9,
                    mondrian_effects::DisplacePin::None,
                    1.0,
                    mondrian_effects::DisplaceAntialias::High,
                )
                .expect("cross turbulent displace grade"),
            ),
            (
                "pinned all edges",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Horizontal,
                    14.0,
                    20.0,
                    [110.0, 60.0],
                    4,
                    0.6,
                    true,
                    2,
                    5,
                    mondrian_effects::DisplacePin::AllEdges,
                    0.25,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("pinned turbulent displace grade"),
            ),
            (
                "turbulent gradient",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Turbulent,
                    2.0,
                    8.0,
                    [70.0, 40.0],
                    3,
                    0.15,
                    true,
                    2,
                    3,
                    mondrian_effects::DisplacePin::VerticalEdges,
                    0.3,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("gradient turbulent displace grade"),
            ),
            (
                "turbulent smoother one octave",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::TurbulentSmoother,
                    0.5,
                    16.0,
                    [80.0, 60.0],
                    1,
                    0.3,
                    false,
                    1,
                    21,
                    mondrian_effects::DisplacePin::None,
                    0.0,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("diagnostic smoother grade"),
            ),
            (
                // The turbulent gradient divides a finite difference by
                // `2 * 0.001`, so a one-ulp difference between the CPU and the
                // shader field evaluation is amplified five-hundredfold; this
                // case keeps `amount * 0.5 * size` small enough that the
                // residual stays inside the 1e-4 colour tolerance.
                "turbulent smoother",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::TurbulentSmoother,
                    0.125,
                    16.0,
                    [80.0, 60.0],
                    4,
                    0.3,
                    false,
                    1,
                    21,
                    mondrian_effects::DisplacePin::None,
                    0.0,
                    mondrian_effects::DisplaceAntialias::Low,
                )
                .expect("smoother turbulent displace grade"),
            ),
            (
                "vertical centre pin",
                mondrian_effects::TurbulentDisplaceGrade::new(
                    mondrian_effects::DisplacementType::Vertical,
                    5.0,
                    10.0,
                    [50.0, 30.0],
                    2,
                    0.4,
                    true,
                    3,
                    8,
                    mondrian_effects::DisplacePin::Center,
                    0.75,
                    mondrian_effects::DisplaceAntialias::High,
                )
                .expect("vertical turbulent displace grade"),
            ),
        ];
        for (index, (label, grade)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::turbulent_displace_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.turbulent-displace-encoder"),
                });
            let (params, extra, aces) = grade.gpu_controls();
            let output = executor
                .record_turbulent_displace(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    aces,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded turbulent displace");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "turbulent displace case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_deband_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU deband parity test: no GPU adapter available");
            return;
        };
        let width = 21_u32;
        let height = 17_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                // A smooth diagonal ramp is exactly the eight-bit banding the
                // effect exists to flatten.
                let value = 0.01 * (2 * x + 3 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.45 - value * 0.5, value * 1.4, 1.0];
            }
        }
        // Two hard edges whose two sides the merge must leave alone.
        for y in 0..height as usize {
            for x in 11..width as usize {
                pixels[y * width as usize + x] = [0.78, 0.22, 0.12, 1.0];
            }
        }
        for y in 9..height as usize {
            for x in 0..5 {
                pixels[y * width as usize + x] = [0.05, 0.05, 0.09, 1.0];
            }
        }
        // Zero coverage must survive both sides of the seam.
        pixels[37] = [0.0, 0.0, 0.0, 0.0];
        let cases = [
            (
                "default",
                mondrian_effects::DebandGrade::new(0.025, 3.0, 0.05).expect("default deband grade"),
            ),
            (
                "wide",
                mondrian_effects::DebandGrade::new(0.01, 8.0, 0.2).expect("wide deband grade"),
            ),
            (
                "narrow band",
                mondrian_effects::DebandGrade::new(0.2, 1.5, 0.0).expect("narrow deband grade"),
            ),
            (
                "dither only",
                mondrian_effects::DebandGrade::new(0.025, 0.0, 0.5)
                    .expect("dither-only deband grade"),
            ),
        ];
        for (index, (label, grade)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::deband_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let pool = scratch(&context, width, height, 3);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.deband-encoder"),
                });
            let pass = mondrian_effects::EffectGpuSpatialPass::Deband { grade: *grade };
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded deband");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "deband case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_glow_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU glow parity test: no GPU adapter available");
            return;
        };
        let width = 21_u32;
        let height = 17_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                // A diagonal ramp crosses the threshold at different columns and
                // rows, so the mask is not a single flat value.
                let value = 0.02 * (3 * x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.55 - value * 0.5, value * 1.2, 1.0];
            }
        }
        // One fully transparent pixel and one half-covered pixel: the first must
        // stay untouched, the second must bloom with its own coverage weight.
        pixels[37] = [0.0, 0.0, 0.0, 0.0];
        pixels[59] = [0.9, 0.2, 0.3, 0.5];
        let cases = [
            (
                "default",
                mondrian_effects::GlowGrade::new(
                    mondrian_effects::GlowBasedOn::Color,
                    153.0,
                    10.0,
                    1.0,
                    mondrian_effects::GlowComposite::OnTop,
                    mondrian_effects::GlowOperation::Screen,
                    mondrian_effects::GlowColors::Original,
                    1.0,
                    0.0,
                    0.5,
                    [1.0, 1.0, 1.0],
                    [0.0, 0.0, 0.0],
                    mondrian_effects::GlowDimensions::Both,
                    [0.2126, 0.7152, 0.0722],
                )
                .expect("default glow grade"),
            ),
            (
                "colour ramp behind",
                mondrian_effects::GlowGrade::new(
                    mondrian_effects::GlowBasedOn::Color,
                    90.0,
                    12.0,
                    120.0,
                    mondrian_effects::GlowComposite::Behind,
                    mondrian_effects::GlowOperation::Overlay,
                    mondrian_effects::GlowColors::AAndB,
                    2.0,
                    45.0,
                    0.4,
                    [0.8, 0.3, 0.1],
                    [0.1, 0.2, 0.9],
                    mondrian_effects::GlowDimensions::Both,
                    [0.2126, 0.7152, 0.0722],
                )
                .expect("authored glow grade"),
            ),
            (
                "cycling single axis",
                mondrian_effects::GlowGrade::new(
                    mondrian_effects::GlowBasedOn::ColorAndAlpha,
                    120.0,
                    7.0,
                    200.0,
                    mondrian_effects::GlowComposite::None,
                    mondrian_effects::GlowOperation::ColorDodge,
                    mondrian_effects::GlowColors::Cycle,
                    3.0,
                    120.0,
                    0.6,
                    [0.2, 0.9, 0.4],
                    [0.9, 0.1, 0.6],
                    mondrian_effects::GlowDimensions::Horizontal,
                    [0.2126, 0.7152, 0.0722],
                )
                .expect("cycling glow grade"),
            ),
        ];
        let alpha_cases = [
            ("alpha on top", mondrian_effects::GlowComposite::OnTop),
            ("alpha behind", mondrian_effects::GlowComposite::Behind),
            ("alpha only", mondrian_effects::GlowComposite::None),
        ]
        .map(|(label, composite)| {
            (
                label,
                mondrian_effects::GlowGrade::new(
                    mondrian_effects::GlowBasedOn::Alpha,
                    32.0,
                    2.5,
                    160.0,
                    composite,
                    mondrian_effects::GlowOperation::Overlay,
                    mondrian_effects::GlowColors::AAndB,
                    1.0,
                    0.0,
                    0.6,
                    [0.1, 0.2, 0.9],
                    [0.9, 0.4, 0.1],
                    mondrian_effects::GlowDimensions::Both,
                    [0.2126, 0.7152, 0.0722],
                )
                .expect("alpha glow grade"),
            )
        });
        for (index, (label, grade)) in cases.iter().chain(&alpha_cases).enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::glow_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let pool = scratch(&context, width, height, 6);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.glow-encoder"),
                });
            let pass = mondrian_effects::EffectGpuSpatialPass::Glow { grade: *grade };
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded glow");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            let mut max_error = 0.0_f32;
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    max_error = max_error.max(difference);
                    assert!(
                        difference <= 1.0e-4,
                        "glow case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
            eprintln!("LGT-01 raw {label} error={max_error:.9e}");
        }
    }

    #[test]
    fn gpu_keylight_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU keylight parity test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let pixel = if x <= 5 {
                    // A pure screen field in the left third.
                    [0.0, 1.0, 0.0, 1.0]
                } else if x <= 10 {
                    // A green-tinted foreground in the middle third.
                    [0.2, 0.9, 0.3, 1.0]
                } else {
                    // A neutral foreground in the right third.
                    [0.5, 0.5, 0.5, 1.0]
                };
                pixels[y * width as usize + x] = pixel;
            }
        }
        // One fully transparent pixel must stay untouched in every display mode.
        pixels[width as usize + 2] = [0.0, 0.0, 0.0, 0.0];
        let cases = [
            (
                "defaults",
                mondrian_effects::KeylightGrade::documented_defaults()
                    .expect("documented keylight defaults"),
            ),
            (
                "morphology",
                mondrian_effects::KeylightGrade::new(
                    [0.0, 1.0, 0.0],
                    1.0,
                    0.5,
                    1.0,
                    [0.5, 0.5, 0.5],
                    0.0,
                    100.0,
                    3.0,
                    2.0,
                    2.0,
                    mondrian_effects::KeylightDisplay::Final,
                )
                .expect("morphology keylight grade"),
            ),
            (
                "matte",
                mondrian_effects::KeylightGrade::new(
                    [0.0, 1.0, 0.0],
                    1.2,
                    0.4,
                    1.5,
                    [0.45, 0.6, 0.5],
                    5.0,
                    90.0,
                    0.0,
                    0.0,
                    0.0,
                    mondrian_effects::KeylightDisplay::Matte,
                )
                .expect("matte keylight grade"),
            ),
            (
                "background",
                mondrian_effects::KeylightGrade::new(
                    [0.0, 1.0, 0.0],
                    0.8,
                    0.65,
                    1.0,
                    [0.5, 0.5, 0.5],
                    0.0,
                    100.0,
                    0.0,
                    0.0,
                    0.0,
                    mondrian_effects::KeylightDisplay::Background,
                )
                .expect("background keylight grade"),
            ),
        ];
        for (index, (label, grade)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::keylight_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let pool = scratch(&context, width, height, 8);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.keylight-encoder"),
                });
            let pass = mondrian_effects::EffectGpuSpatialPass::Keylight { grade: *grade };
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded keylight");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "keylight case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn matte_refine_test_grade(
        edge_detail: f32,
        other_edge_radius: f32,
        view_edge_region: f32,
        smooth: f32,
        feather: f32,
        contrast: f32,
        shift_edge: f32,
        better_quality: bool,
        clean_edge_color: bool,
        clean_amount: f32,
        extend_smooth: bool,
        expand_radius: f32,
        view_map: f32,
    ) -> mondrian_effects::MatteRefineGrade {
        mondrian_effects::MatteRefineGrade::new(
            edge_detail,
            other_edge_radius,
            view_edge_region,
            smooth,
            feather,
            contrast,
            shift_edge,
            better_quality,
            clean_edge_color,
            clean_amount,
            extend_smooth,
            expand_radius,
            view_map,
            1.0,
            50.0,
            0.0,
            8.0,
            180.0,
        )
        .expect("valid matte refine grade")
    }

    #[test]
    fn gpu_matte_refine_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU matte refine parity test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        let centre_x = 8.0_f32;
        let centre_y = 6.0_f32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let dx = x as f32 - centre_x;
                let dy = y as f32 - centre_y;
                let distance = (dx * dx + dy * dy).sqrt();
                let pixel = if distance <= 4.0 {
                    [0.3, 0.6, 0.9, 1.0]
                } else if distance <= 6.0 {
                    // A dark fringe on the soft edge, which the cleanup pass
                    // replaces with the interior colour.
                    let alpha = (6.0 - distance) / 2.0;
                    [0.05, 0.08, 0.12, alpha]
                } else {
                    [0.0, 0.0, 0.0, 0.0]
                };
                pixels[y * width as usize + x] = pixel;
            }
        }
        // A transparent hole inside the disc, so grow and shrink have a hole to
        // close and reopen.
        pixels[(6 * width + 7) as usize] = [0.0, 0.0, 0.0, 0.0];
        let cases = [
            (
                "defaults",
                matte_refine_test_grade(
                    1.0, 10.0, 0.0, 0.0, 0.0, 0.0, 0.0, false, false, 100.0, false, 0.0, 0.0,
                ),
            ),
            (
                "feather",
                matte_refine_test_grade(
                    1.0, 10.0, 0.0, 0.0, 12.0, 0.0, 0.0, false, false, 100.0, false, 0.0, 0.0,
                ),
            ),
            (
                "shift-smooth",
                matte_refine_test_grade(
                    0.5, 10.0, 0.0, 6.0, 0.0, 0.0, -4.0, false, false, 100.0, false, 0.0, 0.0,
                ),
            ),
            (
                "expand-contrast",
                matte_refine_test_grade(
                    1.0, 10.0, 0.0, 0.0, 0.0, 60.0, 0.0, false, false, 100.0, false, 8.0, 0.0,
                ),
            ),
            (
                "grow-shrink",
                matte_refine_test_grade(
                    1.0, 10.0, 0.0, 0.0, 0.0, 0.0, -4.0, false, false, 100.0, false, 8.0, 0.0,
                ),
            ),
            (
                "cleanup",
                matte_refine_test_grade(
                    1.0, 6.0, 0.0, 0.0, 0.0, 0.0, 0.0, false, true, 100.0, false, 0.0, 0.0,
                ),
            ),
            (
                "view-map",
                matte_refine_test_grade(
                    1.0, 10.0, 0.0, 0.0, 0.0, 0.0, 0.0, false, false, 100.0, false, 0.0, 1.0,
                ),
            ),
        ];
        for (index, (label, grade)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::matte_refine_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let pool = scratch(&context, width, height, 8);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.matte-refine-encoder"),
                });
            let pass = mondrian_effects::EffectGpuSpatialPass::MatteRefine { grade: *grade };
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded matte refine");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "matte refine case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_choker_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU choker parity test: no GPU adapter available");
            return;
        };
        let width = 11_u32;
        let height = 9_u32;
        let centre_x = 5.0_f32;
        let centre_y = 4.0_f32;
        // Three rings: an opaque core, a soft semi-transparent band, and a fully
        // transparent surround. Every zero-coverage pixel is pure black, so the
        // alpha morphology never has to invent a colour for a transparent pixel.
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let dx = x as f32 - centre_x;
                let dy = y as f32 - centre_y;
                let distance = (dx * dx + dy * dy).sqrt();
                let pixel = if distance <= 2.0 {
                    [0.2, 0.5, 0.8, 1.0]
                } else if distance <= 3.5 {
                    [0.1, 0.35, 0.55, (3.5 - distance) / 1.5]
                } else {
                    [0.0, 0.0, 0.0, 0.0]
                };
                pixels[y * width as usize + x] = pixel;
            }
        }
        let cases = [
            (
                "erode",
                mondrian_effects::ChokerGrade::new(3.0).expect("valid erode grade"),
            ),
            (
                "dilate",
                mondrian_effects::ChokerGrade::new(-4.0).expect("valid dilate grade"),
            ),
        ];
        for (index, (label, grade)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::choker_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let pool = scratch(&context, width, height, 2);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.choker-encoder"),
                });
            let pass = mondrian_effects::EffectGpuSpatialPass::Choker { grade: *grade };
            assert_eq!(
                EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
                2
            );
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded choker");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "choker case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // The chain needs two rasters: the matte plane and the composite target.
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let single = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.choker-geometry-encoder"),
        });
        let grade = mondrian_effects::ChokerGrade::new(2.0).expect("valid choker grade");
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::Choker { grade },
                &source,
                &single,
                width,
                height,
            )
            .expect_err("a single scratch texture cannot run the choker chain");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("choker_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn gpu_minimax_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU minimax parity test: no GPU adapter available");
            return;
        };
        let width = 11_u32;
        let height = 9_u32;
        // A deterministic ramp with transparent corners: every channel varies,
        // so a minimum, a maximum, and the alpha-only chain all differ from the
        // source in observable places.
        let mut pixels = vec![[0.0_f32; 4]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let phase = 3 * x + 11 * y;
                pixels[y * width as usize + x] = [
                    (phase % 11) as f32 / 10.0,
                    (phase % 7) as f32 / 6.0,
                    (phase % 13) as f32 / 12.0,
                    (phase % 5) as f32 / 4.0,
                ];
            }
        }
        let build = |radius, operation, channel, direction, dont_shrink_edges| {
            mondrian_effects::MinimaxGrade::new(
                radius,
                operation,
                channel,
                direction,
                dont_shrink_edges,
            )
            .expect("valid minimax grade")
        };
        let cases = [
            (
                "minimum both",
                build(
                    3.0,
                    mondrian_effects::MinimaxOperation::Minimum,
                    mondrian_effects::MinimaxChannel::All,
                    mondrian_effects::MinimaxDirection::Both,
                    0.0,
                ),
                2_u32,
            ),
            (
                "maximum horizontal red",
                build(
                    2.0,
                    mondrian_effects::MinimaxOperation::Maximum,
                    mondrian_effects::MinimaxChannel::Red,
                    mondrian_effects::MinimaxDirection::Horizontal,
                    0.0,
                ),
                1_u32,
            ),
            (
                "minimum then maximum vertical alpha protected",
                build(
                    2.0,
                    mondrian_effects::MinimaxOperation::MinimumThenMaximum,
                    mondrian_effects::MinimaxChannel::Alpha,
                    mondrian_effects::MinimaxDirection::Vertical,
                    1.0,
                ),
                2_u32,
            ),
            (
                "minimum then maximum both colour",
                build(
                    1.0,
                    mondrian_effects::MinimaxOperation::MinimumThenMaximum,
                    mondrian_effects::MinimaxChannel::Color,
                    mondrian_effects::MinimaxDirection::Both,
                    0.0,
                ),
                4_u32,
            ),
        ];
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 4);
        for (index, (label, grade, expected_passes)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::minimax_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let pass = mondrian_effects::EffectGpuSpatialPass::Minimax { grade: *grade };
            assert_eq!(
                EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
                *expected_passes,
                "case {index} ({label}) pass count"
            );
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.minimax-encoder"),
                });
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool[..*expected_passes as usize],
                    width,
                    height,
                )
                .expect("recorded minimax");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "minimax case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // A chain of four passes needs four rasters, and three must be refused
        // with the documented geometry error instead of silently clamping.
        let grade = build(
            1.0,
            mondrian_effects::MinimaxOperation::MaximumThenMinimum,
            mondrian_effects::MinimaxChannel::All,
            mondrian_effects::MinimaxDirection::Both,
            0.0,
        );
        let short = scratch(&context, width, height, 3);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.minimax-geometry-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::Minimax { grade },
                &source,
                &short,
                width,
                height,
            )
            .expect_err("three scratch textures cannot run the four-pass chain");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("minimax_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn gpu_channel_blur_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU channel blur parity test: no GPU adapter available");
            return;
        };
        let width = 11_u32;
        let height = 9_u32;
        // A deterministic ramp with a zero-coverage corner and a semi-transparent
        // band: every channel varies, so per-channel radii and the alpha chain
        // both differ from the source in observable places.
        let mut pixels = vec![[0.0_f32; 4]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let phase = 5 * x + 7 * y;
                pixels[y * width as usize + x] = [
                    (phase % 9) as f32 / 8.0,
                    (phase % 6) as f32 / 5.0,
                    (phase % 12) as f32 / 11.0,
                    (phase % 4) as f32 / 3.0,
                ];
            }
        }
        pixels[0] = [0.0, 0.0, 0.0, 0.0];
        for x in 0..width as usize {
            pixels[3 * width as usize + x][3] = 0.25;
        }
        let build = |red, green, blue, alpha, edge_behavior, direction| {
            mondrian_effects::ChannelBlurGrade::new(
                red,
                green,
                blue,
                alpha,
                edge_behavior,
                direction,
            )
            .expect("valid channel blur grade")
        };
        let cases = [
            (
                "rgb 2/4/6 alpha 8 both repeat edge",
                build(
                    2.0,
                    4.0,
                    6.0,
                    8.0,
                    1.0,
                    mondrian_effects::ChannelBlurDirection::Both,
                ),
                2_u32,
            ),
            (
                "alpha 3 horizontal",
                build(
                    0.0,
                    0.0,
                    0.0,
                    3.0,
                    1.0,
                    mondrian_effects::ChannelBlurDirection::Horizontal,
                ),
                1_u32,
            ),
        ];
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 2);
        for (index, (label, grade, expected_passes)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::channel_blur_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let pass = mondrian_effects::EffectGpuSpatialPass::ChannelBlur { grade: *grade };
            assert_eq!(
                EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
                *expected_passes,
                "case {index} ({label}) pass count"
            );
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.channel-blur-encoder"),
                });
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool[..*expected_passes as usize],
                    width,
                    height,
                )
                .expect("recorded channel blur");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "channel blur case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // An 8x6 frame with radii past the frame extent and transparent edges:
        // the pass rounds each amount, clamps it to `max(width, height)`, and the
        // corner pixels darken because out-of-window taps still divide.
        let small_width = 8_u32;
        let small_height = 6_u32;
        let mut small = vec![[0.0_f32; 4]; (small_width * small_height) as usize];
        for y in 0..small_height as usize {
            for x in 0..small_width as usize {
                let phase = 3 * x + 5 * y;
                small[y * small_width as usize + x] = [
                    (phase % 7) as f32 / 6.0,
                    (phase % 5) as f32 / 4.0,
                    (phase % 3) as f32 / 2.0,
                    (phase % 8) as f32 / 7.0,
                ];
            }
        }
        let clamp_grade = build(
            12.0,
            0.0,
            0.0,
            5.0,
            0.0,
            mondrian_effects::ChannelBlurDirection::Both,
        );
        let mut cpu = small.clone();
        assert!(mondrian_effects::channel_blur_frame_f32(
            &mut cpu,
            small_width as usize,
            small_height as usize,
            &clamp_grade,
        ));
        let source = upload(&context, &small, small_width, small_height);
        let pool = scratch(&context, small_width, small_height, 2);
        let pass = mondrian_effects::EffectGpuSpatialPass::ChannelBlur { grade: clamp_grade };
        assert_eq!(
            EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
            2,
            "clamped case pass count"
        );
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.channel-blur-clamp-encoder"),
        });
        let output = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &pool,
                small_width,
                small_height,
            )
            .expect("recorded clamped channel blur");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, small_width, small_height);
        for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
            for channel in 0..4 {
                let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                assert!(
                    difference <= 1.0e-4,
                    "channel blur clamp pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                );
            }
        }

        // A two-axis blur needs two rasters, and one must be refused with the
        // documented geometry error instead of silently clamping.
        let source = upload(&context, &small, small_width, small_height);
        let short = scratch(&context, small_width, small_height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.channel-blur-geometry-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &short,
                small_width,
                small_height,
            )
            .expect_err("one scratch texture cannot run the two-axis blur");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("channel_blur_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn gpu_median_opaque_noise() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU median parity test: no GPU adapter available");
            return;
        };
        let width = 16_u32;
        let height = 16_u32;
        // On-grid data (multiples of 1/255) plus one isolated speck and one empty
        // pixel: the speck is erased by the window median and the empty pixel
        // keeps its colour because the alpha toggle is off.
        let mut pixels = vec![[0.0_f32; 4]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let phase = 17 * x + 29 * y;
                pixels[y * width as usize + x] = [
                    (phase % 256) as f32 / 255.0,
                    ((phase * 3) % 256) as f32 / 255.0,
                    ((phase * 7) % 256) as f32 / 255.0,
                    1.0,
                ];
            }
        }
        pixels[5 * width as usize + 5] = [1.0, 0.0, 1.0, 1.0];
        pixels[9 * width as usize + 3] = [0.0, 0.0, 0.0, 0.0];
        let grade = mondrian_effects::MedianGrade::new(1.0, 0.0).expect("valid median grade");
        let pass = mondrian_effects::EffectGpuSpatialPass::Median { grade };
        assert_eq!(
            EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
            1
        );
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::median_frame_f32(
            &mut cpu, width, height, &grade
        ));
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.median-encoder"),
        });
        let output = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &pool,
                width,
                height,
            )
            .expect("recorded median");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        assert_median_parity(&cpu, &gpu, "median");
    }

    #[test]
    fn gpu_median_transparent_neighbour() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU median parity test: no GPU adapter available");
            return;
        };
        // Odd geometry, so the rank is a true middle sample on both axes.
        let width = 9_u32;
        let height = 7_u32;
        let mut pixels = vec![[0.0_f32; 4]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let phase = 11 * x + 23 * y;
                pixels[y * width as usize + x] = [
                    (phase % 256) as f32 / 255.0,
                    ((phase * 5) % 256) as f32 / 255.0,
                    ((phase * 9) % 256) as f32 / 255.0,
                    1.0,
                ];
            }
        }
        // A semi-transparent band whose hidden RGB is black: with alpha in the
        // median the band survives, without it the RGB median still sees the
        // hidden samples.
        for x in 0..width as usize {
            pixels[3 * width as usize + x] = [0.0, 0.0, 0.0, 0.5];
        }
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 1);
        for toggle in [0.0_f32, 1.0] {
            let grade =
                mondrian_effects::MedianGrade::new(2.0, toggle).expect("valid median grade");
            let pass = mondrian_effects::EffectGpuSpatialPass::Median { grade };
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::median_frame_f32(
                &mut cpu, width, height, &grade
            ));
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.median-alpha-encoder"),
                });
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded median");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            assert_median_parity(&cpu, &gpu, &format!("median toggle {toggle}"));
        }
    }

    #[test]
    fn gpu_median_hdr_and_non_finite() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU median parity test: no GPU adapter available");
            return;
        };
        let width = 32_u32;
        let height = 24_u32;
        let mut pixels = vec![[0.0_f32; 4]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let phase = 13 * x + 31 * y;
                pixels[y * width as usize + x] = [
                    (phase % 256) as f32 / 255.0,
                    ((phase * 3) % 256) as f32 / 255.0,
                    ((phase * 5) % 256) as f32 / 255.0,
                    1.0,
                ];
            }
        }
        // HDR above one clamps to the top bin, negatives and NaN to the bottom.
        pixels[11] = [2.5, -0.25, f32::NAN, 1.0];
        pixels[12] = [4.0, f32::NEG_INFINITY, 1.0, 1.0];
        let grade = mondrian_effects::MedianGrade::new(3.0, 0.0).expect("valid median grade");
        let pass = mondrian_effects::EffectGpuSpatialPass::Median { grade };
        assert_eq!(
            EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
            1
        );
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::median_frame_f32(
            &mut cpu, width, height, &grade
        ));
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.median-hdr-encoder"),
        });
        let output = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &pool,
                width,
                height,
            )
            .expect("recorded median");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        assert_median_parity(&cpu, &gpu, "median hdr");

        // The radius gate belongs to admission, not to recording.
        let too_wide = mondrian_effects::EffectGpuSpatialPass::Median {
            grade: mondrian_effects::MedianGrade::new(4.0, 0.0).expect("valid grade"),
        };
        assert!(matches!(
            EffectSpatialGpu::texture_pass_count(&too_wide),
            Err(EffectSpatialGpuError::UnsupportedPass("median_radius"))
        ));
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.median-geometry-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &[],
                width,
                height,
            )
            .expect_err("the median needs one scratch texture");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("median_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn gpu_dust_scratches_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU dust & scratches parity test: no GPU adapter available");
            return;
        };
        // Odd geometry on both axes, so the window rank is a true middle sample.
        let width = 9_u32;
        let height = 7_u32;
        let base = [0.25_f32, 0.5, 0.75, 1.0];
        let mut pixels = vec![base; (width * height) as usize];
        // A scratch and a dust speck: both deviate from the window median by far
        // more than the threshold, so both are repaired to the median.
        pixels[3 * width as usize + 4] = [1.0, 1.0, 1.0, 1.0];
        pixels[5 * width as usize + 2] = [0.0, 0.0, 0.0, 1.0];
        // A ten-unit step that stays inside the threshold: the centre must be
        // kept bit for bit, which exercises the pass-through branch.
        pixels[0] = [0.25 + 10.0 / 255.0, 0.5, 0.75, 1.0];
        let grade = mondrian_effects::DustScratchesGrade::new(2.0, 0.1, 0.0).expect("valid grade");
        let pass = mondrian_effects::EffectGpuSpatialPass::DustScratches { grade };
        assert_eq!(
            EffectSpatialGpu::texture_pass_count(&pass).expect("supported pass"),
            1
        );
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::dust_scratches_frame_f32(
            &mut cpu, width, height, &grade
        ));
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.dust-scratches-encoder"),
        });
        let output = executor
            .record(
                &context.device,
                &mut encoder,
                &pass,
                &source,
                &pool,
                width,
                height,
            )
            .expect("recorded dust & scratches");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        assert_median_parity(&cpu, &gpu, "dust & scratches");
    }

    #[test]
    fn gpu_dust_scratches_alpha_participates() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU dust & scratches parity test: no GPU adapter available");
            return;
        };
        let width = 11_u32;
        let height = 6_u32;
        let mut pixels = vec![[0.0_f32; 4]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let phase = 19 * x + 37 * y;
                pixels[y * width as usize + x] = [
                    (phase % 256) as f32 / 255.0,
                    ((phase * 5) % 256) as f32 / 255.0,
                    ((phase * 9) % 256) as f32 / 255.0,
                    1.0,
                ];
            }
        }
        // A semi-transparent band whose hidden RGB is black: with the alpha
        // toggle on the band's alpha takes part in the gate and the repair, with
        // it off every alpha bit stays exactly where the frame put it.
        for x in 0..width as usize {
            pixels[2 * width as usize + x] = [0.0, 0.0, 0.0, 0.5];
        }
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 1);
        for toggle in [0.0_f32, 1.0] {
            let grade = mondrian_effects::DustScratchesGrade::new(1.0, 0.25, toggle)
                .expect("valid dust & scratches grade");
            let pass = mondrian_effects::EffectGpuSpatialPass::DustScratches { grade };
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::dust_scratches_frame_f32(
                &mut cpu, width, height, &grade
            ));
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.dust-scratches-alpha-encoder"),
                });
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded dust & scratches");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            assert_median_parity(&cpu, &gpu, &format!("dust & scratches toggle {toggle}"));
        }
    }

    #[test]
    fn gpu_dust_scratches_rejects_a_wide_radius() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU dust & scratches admission test: no GPU adapter available");
            return;
        };
        let too_wide = mondrian_effects::EffectGpuSpatialPass::DustScratches {
            grade: mondrian_effects::DustScratchesGrade::new(4.0, 0.0, 0.0)
                .expect("valid dust & scratches grade"),
        };
        // The radius gate belongs to admission, not to recording.
        assert!(matches!(
            EffectSpatialGpu::texture_pass_count(&too_wide),
            Err(EffectSpatialGpuError::UnsupportedPass(
                "dust_scratches_radius"
            ))
        ));
        let width = 5_u32;
        let height = 5_u32;
        let pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; (width * height) as usize];
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let pool = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.dust-scratches-radius-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &too_wide,
                &source,
                &pool,
                width,
                height,
            )
            .expect_err("a radius past the GPU cap cannot be recorded");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("dust_scratches_radius")
            ),
            "unexpected error: {error:?}"
        );
        let supported = mondrian_effects::EffectGpuSpatialPass::DustScratches {
            grade: mondrian_effects::DustScratchesGrade::new(1.0, 0.0, 0.0)
                .expect("valid dust & scratches grade"),
        };
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.dust-scratches-geometry-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &supported,
                &source,
                &[],
                width,
                height,
            )
            .expect_err("the repair needs one scratch texture");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("dust_scratches_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn gpu_fractal_noise_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU fractal noise parity test: no GPU adapter available");
            return;
        };
        let width = 21_u32;
        let height = 17_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.02 * (3 * x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.55 - value * 0.5, value * 1.2, 1.0];
            }
        }
        // One fully transparent pixel and one half-covered pixel: the first must
        // stay untouched, the second must receive the blended field.
        pixels[37] = [0.0, 0.0, 0.0, 0.0];
        pixels[59] = [0.9, 0.2, 0.3, 0.5];
        let cases = [
            (
                "default",
                mondrian_effects::FractalNoiseGrade::new(
                    mondrian_effects::FractalType::Basic,
                    mondrian_effects::FractalNoiseType::SoftLinear,
                    false,
                    100.0,
                    0.0,
                    mondrian_effects::FractalOverflow::Clip,
                    0.0,
                    true,
                    100.0,
                    100.0,
                    100.0,
                    [0.0, 0.0],
                    false,
                    6,
                    50.0,
                    50.0,
                    0.0,
                    [0.0, 0.0],
                    0.0,
                    0.0,
                    false,
                    1,
                    0,
                    100.0,
                    mondrian_core::BlendMode::Normal,
                )
                .expect("default fractal noise grade"),
            ),
            (
                "turbulent sharp spline soft clamp",
                mondrian_effects::FractalNoiseGrade::new(
                    mondrian_effects::FractalType::TurbulentSharp,
                    mondrian_effects::FractalNoiseType::Spline,
                    false,
                    175.0,
                    0.0,
                    mondrian_effects::FractalOverflow::SoftClamp,
                    30.0,
                    true,
                    100.0,
                    100.0,
                    100.0,
                    [95.0, -95.0],
                    false,
                    8,
                    65.0,
                    60.0,
                    0.0,
                    [0.0, 0.0],
                    0.0,
                    0.0,
                    false,
                    1,
                    7,
                    100.0,
                    mondrian_core::BlendMode::Normal,
                )
                .expect("authored fractal noise grade"),
            ),
            (
                "swirly wrap invert",
                mondrian_effects::FractalNoiseGrade::new(
                    mondrian_effects::FractalType::Swirly,
                    mondrian_effects::FractalNoiseType::SoftLinear,
                    true,
                    100.0,
                    10.0,
                    mondrian_effects::FractalOverflow::Wrap,
                    45.0,
                    true,
                    100.0,
                    100.0,
                    100.0,
                    [50.0, 50.0],
                    false,
                    5,
                    60.0,
                    60.0,
                    20.0,
                    [0.0, 0.0],
                    0.0,
                    60.0,
                    false,
                    1,
                    3,
                    60.0,
                    mondrian_core::BlendMode::Screen,
                )
                .expect("swirly fractal noise grade"),
            ),
            (
                "max perspective cycle",
                mondrian_effects::FractalNoiseGrade::new(
                    mondrian_effects::FractalType::Max,
                    mondrian_effects::FractalNoiseType::Linear,
                    false,
                    120.0,
                    -10.0,
                    mondrian_effects::FractalOverflow::Clip,
                    0.0,
                    false,
                    300.0,
                    250.0,
                    180.0,
                    [98.0, -49.0],
                    true,
                    4,
                    40.0,
                    30.0,
                    0.0,
                    [3.0, -2.0],
                    0.6,
                    540.0,
                    true,
                    2,
                    11,
                    100.0,
                    mondrian_core::BlendMode::Normal,
                )
                .expect("projected fractal noise grade"),
            ),
        ];
        for (index, (label, grade)) in cases.iter().enumerate() {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::fractal_noise_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let pool = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.fractal-noise-encoder"),
                });
            let pass = mondrian_effects::EffectGpuSpatialPass::FractalNoise { grade: *grade };
            let output = executor
                .record(
                    &context.device,
                    &mut encoder,
                    &pass,
                    &source,
                    &pool,
                    width,
                    height,
                )
                .expect("recorded fractal noise");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "fractal noise case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_lens_distortion_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU lens distortion parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.04 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, 0.45 - value * 0.5, value * 1.3, 1.0];
            }
        }
        pixels[40] = [0.0, 0.0, 0.0, 0.0];
        pixels[70] = [2.5, -0.75, 1.25, 0.5];
        for (fov, invert, direction, centre, best_pixels, resize) in [
            (
                90.0_f32,
                false,
                mondrian_effects::LensDistortionDirection::Diagonal,
                [6.0_f32, 5.0],
                false,
                mondrian_effects::LensDistortionResize::None,
            ),
            (
                140.0,
                true,
                mondrian_effects::LensDistortionDirection::Horizontal,
                [5.5, 4.5],
                true,
                mondrian_effects::LensDistortionResize::Expand,
            ),
            (
                35.0,
                false,
                mondrian_effects::LensDistortionDirection::Vertical,
                [3.25, 7.5],
                false,
                mondrian_effects::LensDistortionResize::Contract,
            ),
        ] {
            let grade = mondrian_effects::LensDistortionGrade::new(
                fov,
                invert,
                direction,
                centre,
                best_pixels,
                resize,
            )
            .expect("lens distortion grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::lens_distortion_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.lens-distortion-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_lens_distortion(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded lens distortion");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "lens distortion {fov} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_bulge_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU bulge parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, 0.4 - value * 0.3, value * 1.2, 1.0];
            }
        }
        pixels[17] = [0.0, 0.0, 0.0, 0.0];
        pixels[63] = [2.5, -0.75, 1.25, 0.5];
        for (radius, centre, height_value, taper, antialias, pinned) in [
            (
                [3.5_f32, 3.0],
                [6.0_f32, 5.0],
                1.0_f32,
                0.0_f32,
                false,
                false,
            ),
            ([5.0, 2.0], [3.0, 7.0], -1.5, 2.0, true, false),
            ([4.0, 4.0], [10.0, 9.0], 2.0, 0.0, false, true),
        ] {
            let grade = mondrian_effects::BulgeGrade::new(
                radius,
                centre,
                height_value,
                taper,
                antialias,
                pinned,
            )
            .expect("bulge grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::bulge_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.bulge-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_bulge(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded bulge");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "bulge {radius:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_lens_flare_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU lens flare parity test: no GPU adapter available");
            return;
        };
        let width = 17_u32;
        let height = 13_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.04 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, 0.35 - value * 0.2, value * 1.4, 1.0];
            }
        }
        pixels[42] = [0.0, 0.0, 0.0, 0.0];
        pixels[88] = [2.5, -0.75, 1.25, 0.5];
        for (brightness, blend, centre, lens) in [
            (
                180.0_f32,
                0.25_f32,
                [6.5_f32, 5.0],
                mondrian_effects::LensFlareType::Prime35,
            ),
            (
                90.0,
                0.0,
                [12.0, 9.5],
                mondrian_effects::LensFlareType::Zoom50To300,
            ),
            (
                300.0,
                0.6,
                [2.0, 1.0],
                mondrian_effects::LensFlareType::Prime105,
            ),
        ] {
            let grade = mondrian_effects::LensFlareGrade::new(brightness, blend, centre, lens)
                .expect("lens flare grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::lens_flare_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.lens-flare-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_lens_flare(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded lens flare");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "{lens:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_polar_coordinates_match_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU polar coordinates parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
            }
        }
        pixels[30] = [0.0, 0.0, 0.0, 0.0];
        pixels[61] = [2.5, -0.75, 1.25, 0.5];
        for (conversion, interpolation) in [
            (
                mondrian_effects::PolarConversion::RectToPolar,
                mondrian_effects::PolarInterpolation::Bilinear,
            ),
            (
                mondrian_effects::PolarConversion::RectToPolar,
                mondrian_effects::PolarInterpolation::Nearest,
            ),
            (
                mondrian_effects::PolarConversion::PolarToRect,
                mondrian_effects::PolarInterpolation::Bilinear,
            ),
            (
                mondrian_effects::PolarConversion::PolarToRect,
                mondrian_effects::PolarInterpolation::Nearest,
            ),
        ] {
            let grade = mondrian_effects::PolarCoordinatesGrade::new(conversion, interpolation);
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::polar_coordinates_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.polar-encoder"),
                });
            let output = executor
                .record_polar_coordinates(
                    &context.device,
                    &mut encoder,
                    grade.gpu_controls(),
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded polar coordinates");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "{conversion:?} {interpolation:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_mirror_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU mirror parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
            }
        }
        pixels[30] = [0.0, 0.0, 0.0, 0.0];
        pixels[61] = [2.5, -0.75, 1.25, 0.5];
        for (angle, centre) in [
            (0.0_f32, [6.0_f32, 5.0_f32]),
            (37.5, [3.5, 7.25]),
            (90.0, [12.0, 0.0]),
            (-125.0, [-2.0, 1.0]),
        ] {
            let grade = mondrian_effects::MirrorGrade::new(angle, centre).expect("mirror grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::mirror_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.mirror-encoder"),
                });
            let output = executor
                .record_mirror(
                    &context.device,
                    &mut encoder,
                    grade.gpu_controls(),
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded mirror");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "angle {angle} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_spherize_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU spherize parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
            }
        }
        pixels[30] = [0.0, 0.0, 0.0, 0.0];
        pixels[61] = [2.5, -0.75, 1.25, 0.5];
        for (radius, centre) in [
            (5.5_f32, [6.0_f32, 5.0_f32]),
            (4.0, [3.5, 7.25]),
            (9.0, [0.0, 0.0]),
        ] {
            let grade =
                mondrian_effects::SpherizeGrade::new(radius, centre).expect("spherize grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::spherize_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.spherize-encoder"),
                });
            let output = executor
                .record_spherize(
                    &context.device,
                    &mut encoder,
                    grade.gpu_controls(),
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded spherize");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "radius {radius} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    /// GPU coverage for the magnify lens (DST-07).
    mod gpu_magnify {
        use super::*;
        use crate::spatial_pass_gpu::{EffectSpatialGpu, SPATIAL_EFFECT_SHADER};
        use pollster::block_on;

        #[test]
        fn spatial_magnify_shader_validates() {
            let module = naga::front::wgsl::parse_str(SPATIAL_EFFECT_SHADER)
                .expect("spatial effect shader parses");
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .expect("spatial effect shader validates");
            assert!(
                module.entry_points.iter().any(|entry| entry.name == "fs_magnify"),
                "the magnify fragment entry point must survive validation"
            );
        }

        #[test]
        fn gpu_magnify_matches_the_cpu_kernel() {
            let Ok(context) = block_on(crate::GpuContext::new()) else {
                eprintln!("skipping GPU magnify parity test: no GPU adapter available");
                return;
            };
            let width = 256_u32;
            let height = 256_u32;
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let u = x as f32 / (width - 1) as f32;
                    let v = y as f32 / (height - 1) as f32;
                    pixels[y * width as usize + x] =
                        [0.15 + 0.7 * u, 0.1 + 0.65 * v, 0.2 + 0.5 * u * v, 1.0];
                }
            }
            let cases = [
                mondrian_effects::MagnifyGrade::new(
                    [128.0, 128.0],
                    120.0,
                    1.5,
                    0.0,
                    1.0,
                    mondrian_effects::MagnifyShape::Circle,
                    mondrian_effects::MagnifyScaling::Bilinear,
                    mondrian_core::BlendMode::Normal,
                )
                .expect("magnify grade"),
                mondrian_effects::MagnifyGrade::new(
                    [96.5, 140.25],
                    180.0,
                    2.0,
                    24.0,
                    0.75,
                    mondrian_effects::MagnifyShape::Circle,
                    mondrian_effects::MagnifyScaling::Supersample2,
                    mondrian_core::BlendMode::Normal,
                )
                .expect("feathered magnify grade"),
                mondrian_effects::MagnifyGrade::new(
                    [128.0, 128.0],
                    160.0,
                    1.75,
                    32.0,
                    0.5,
                    mondrian_effects::MagnifyShape::Square,
                    mondrian_effects::MagnifyScaling::Supersample2,
                    mondrian_core::BlendMode::Multiply,
                )
                .expect("square magnify grade"),
            ];
            for grade in cases {
                let mut cpu = pixels.clone();
                assert!(mondrian_effects::magnify_frame_f32(
                    &mut cpu,
                    width as usize,
                    height as usize,
                    grade,
                ));
                let executor = EffectSpatialGpu::new(&context.device);
                let source = upload(&context, &pixels, width, height);
                let destination = scratch(&context, width, height, 1);
                let (params, extra, aces, _) = grade.gpu_controls();
                let mut encoder =
                    context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("mondrian.effect-spatial.magnify-encoder"),
                    });
                let output = executor
                    .record_magnify(
                        &context.device,
                        &mut encoder,
                        params,
                        extra,
                        aces,
                        &source,
                        &destination[0],
                        width,
                        height,
                    )
                    .expect("recorded magnify");
                context.queue.submit(Some(encoder.finish()));
                let gpu = readback(&context, &output, width, height);
                for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                    for channel in 0..4 {
                        let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                        assert!(
                            difference <= 1.0e-4,
                            "pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                        );
                    }
                }
            }
        }

        #[test]
        fn magnify_lowers_to_its_own_spatial_stage() {
            let grade = mondrian_effects::MagnifyGrade::new(
                [128.0, 128.0],
                200.0,
                2.0,
                8.0,
                1.0,
                mondrian_effects::MagnifyShape::Circle,
                mondrian_effects::MagnifyScaling::Bilinear,
                mondrian_core::BlendMode::Normal,
            )
            .expect("magnify grade");
            let op = mondrian_effects::EffectRenderOp::Magnify { grade };
            let pass = mondrian_effects::gpu_pipeline::spatial_pass_for(&op)
                .expect("magnify must lower to a spatial stage");
            assert_eq!(pass.name(), "magnify");
            assert!(matches!(
                pass,
                mondrian_effects::EffectGpuSpatialPass::Magnify { .. }
            ));
            let expected = (grade.radius_pixels().ceil() as u32).saturating_add(2);
            assert_eq!(pass.horizontal_halo(), expected);
            assert_eq!(pass.vertical_halo(), expected);
            assert_ne!(pass.horizontal_halo(), u32::MAX);
            assert_ne!(pass.vertical_halo(), u32::MAX);
        }
    }
    #[test]
    fn gpu_bezier_warp_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU bezier warp parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.06 * (x + 3 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.5 - value * 0.4, value * 1.3, 1.0];
            }
        }
        pixels[12] = [0.0, 0.0, 0.0, 0.0];
        pixels[77] = [2.0, -0.5, 1.5, 0.5];
        let identity = [
            [0.0_f32, 0.0],
            [85.333_336, 0.0],
            [170.666_67, 0.0],
            [256.0, 0.0],
            [256.0, 85.333_336],
            [256.0, 170.666_67],
            [256.0, 256.0],
            [170.666_67, 256.0],
            [85.333_336, 256.0],
            [0.0, 256.0],
            [0.0, 170.666_67],
            [0.0, 85.333_336],
        ];
        let mut bent = identity;
        bent[1][1] = 48.0;
        bent[2][1] = 40.0;
        let mut skewed = identity;
        skewed[0] = [24.0, 16.0];
        skewed[6] = [240.0, 248.0];
        skewed[9] = [16.0, 240.0];
        for (points, quality) in [(bent, 3_u32), (skewed, 10_u32)] {
            let grade = mondrian_effects::BezierWarpGrade::from_reference_points(points, quality)
                .expect("bezier warp grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::bezier_warp_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.bezier-warp-encoder"),
                });
            let output = executor
                .record_bezier_warp(
                    &context.device,
                    &mut encoder,
                    grade,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded bezier warp");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                // Straight-alpha RGB is ill-conditioned once coverage is
                // negligible: the kernel and the shader solve the patch in f64
                // and f32, so a fragment landing one float step away from a
                // transparent texel edge divides a neighbouring colour by a
                // near-zero alpha. Both sides must agree that the pixel is
                // transparent, and the alpha itself must stay put.
                if cpu_pixel[3] < 1.0e-4 && gpu_pixel[3] < 1.0e-4 {
                    let difference = (cpu_pixel[3] - gpu_pixel[3]).abs();
                    assert!(
                        difference <= 1.0e-6,
                        "quality {quality} pixel {index} alpha: \
                         cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                    continue;
                }
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "quality {quality} pixel {index} channel {channel}: \
                         cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn bezier_warp_lowers_to_its_own_spatial_stage() {
        let points = [
            [0.0_f32, 0.0],
            [85.333_336, 48.0],
            [170.666_67, 40.0],
            [256.0, 0.0],
            [256.0, 85.333_336],
            [256.0, 170.666_67],
            [256.0, 256.0],
            [170.666_67, 256.0],
            [85.333_336, 256.0],
            [0.0, 256.0],
            [0.0, 170.666_67],
            [0.0, 85.333_336],
        ];
        let grade = mondrian_effects::BezierWarpGrade::from_reference_points(points, 5)
            .expect("bezier warp grade");
        let op = mondrian_effects::EffectRenderOp::BezierWarp { grade };
        let pass = mondrian_effects::gpu_pipeline::spatial_pass_for(&op)
            .expect("bezier warp must lower to a spatial stage");
        assert_eq!(pass.name(), "bezier_warp");
        assert!(matches!(
            pass,
            mondrian_effects::EffectGpuSpatialPass::BezierWarp { .. }
        ));
        // The patch can map any target anywhere, so the halo covers the frame.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn vr_rotate_sphere_halo_is_full_frame() {
        let grade = mondrian_effects::VrRotateSphereGrade::new(
            360.0,
            180.0,
            10.0,
            -20.0,
            5.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
            false,
        )
        .expect("valid vr rotate sphere grade");
        let op = mondrian_effects::EffectRenderOp::VrRotateSphere { grade };
        let pass = mondrian_effects::gpu_pipeline::spatial_pass_for(&op)
            .expect("vr rotate sphere must lower to a spatial stage");
        assert_eq!(pass.name(), "vr_rotate_sphere");
        assert!(matches!(
            pass,
            mondrian_effects::EffectGpuSpatialPass::VrRotateSphere { .. }
        ));
        // One row's longitude sweep already covers the full circle, so every
        // destination pixel may read anywhere in its eye sub-frame.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn vr_rotate_sphere_takes_a_single_pass() {
        let grade = mondrian_effects::VrRotateSphereGrade::new(
            360.0,
            180.0,
            10.0,
            -20.0,
            5.0,
            mondrian_effects::VrFrameLayout::OverUnder,
            false,
        )
        .expect("valid vr rotate sphere grade");
        let pass = mondrian_effects::EffectGpuSpatialPass::VrRotateSphere { grade };
        // A single trigonometric remap per pixel: no window, iteration or
        // separable direction to keep in scratch textures.
        assert_eq!(EffectSpatialGpu::texture_pass_count(&pass), Ok(1));
    }

    #[test]
    fn spatial_vr_rotate_sphere_shader_validates() {
        let module = naga::front::wgsl::parse_str(SPATIAL_EFFECT_SHADER)
            .expect("spatial effect shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spatial effect shader validates");
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_vr_rotate_sphere"),
            "the vr rotate sphere fragment entry point must survive validation"
        );
    }

    /// Independent f64 evaluation of the inverse equirectangular mapping.
    ///
    /// The rotation matrix is built from the authored angles directly instead of
    /// reusing the kernel's private matrix, so the two implementations can only
    /// agree if the published mapping is right.
    fn reference_vr_position(
        grade: mondrian_effects::VrRotateSphereGrade,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> (f64, f64) {
        let (eye_width, eye_height) = grade.eye_size(width, height);
        let (origin_x, origin_y) = grade.eye_origin(column as usize, row as usize, width, height);
        let u = (column - origin_x as f64 + 0.5) / eye_width as f64;
        let v = (row - origin_y as f64 + 0.5) / eye_height as f64;
        let h_fov = grade.horizontal_fov_degrees().to_radians();
        let v_fov = grade.vertical_fov_degrees().to_radians();
        let longitude = (u - 0.5) * h_fov;
        let latitude = (0.5 - v) * v_fov;
        let (sin_lon, cos_lon) = (longitude.sin(), longitude.cos());
        let (sin_lat, cos_lat) = (latitude.sin(), latitude.cos());
        let direction = [cos_lat * sin_lon, sin_lat, cos_lat * cos_lon];
        let pitch = grade.tilt_x_degrees().to_radians();
        let yaw = grade.pan_y_degrees().to_radians();
        let roll = grade.roll_z_degrees().to_radians();
        let (sp, cp) = (pitch.sin(), pitch.cos());
        let (sy, cy) = (yaw.sin(), yaw.cos());
        let (sr, cr) = (roll.sin(), roll.cos());
        let rotation = [
            [cr * cy, cr * sy * sp - sr * cp, cr * sy * cp + sr * sp],
            [sr * cy, sr * sy * sp + cr * cp, sr * sy * cp - cr * sp],
            [-sy, cy * sp, cy * cp],
        ];
        let sample = if grade.invert() {
            [
                rotation[0][0] * direction[0]
                    + rotation[1][0] * direction[1]
                    + rotation[2][0] * direction[2],
                rotation[0][1] * direction[0]
                    + rotation[1][1] * direction[1]
                    + rotation[2][1] * direction[2],
                rotation[0][2] * direction[0]
                    + rotation[1][2] * direction[1]
                    + rotation[2][2] * direction[2],
            ]
        } else {
            [
                rotation[0][0] * direction[0]
                    + rotation[0][1] * direction[1]
                    + rotation[0][2] * direction[2],
                rotation[1][0] * direction[0]
                    + rotation[1][1] * direction[1]
                    + rotation[1][2] * direction[2],
                rotation[2][0] * direction[0]
                    + rotation[2][1] * direction[1]
                    + rotation[2][2] * direction[2],
            ]
        };
        let longitude_src = sample[0].atan2(sample[2]);
        let latitude_src = sample[1].clamp(-1.0, 1.0).asin();
        let mut u_source = longitude_src / h_fov + 0.5;
        if grade.wrap_longitude() {
            u_source -= u_source.floor();
        }
        let v_source = 0.5 - latitude_src / v_fov;
        (
            u_source * eye_width as f64 - 0.5 + origin_x as f64,
            v_source * eye_height as f64 - 0.5 + origin_y as f64,
        )
    }

    #[test]
    fn cpu_and_gpu_agree_to_one_eight_thousandth_on_a_smooth_gradient() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU vr rotate sphere parity test: no GPU adapter available");
            return;
        };
        let width = 128_u32;
        let height = 64_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        // A periodic analytic gradient: the panorama seam does not introduce a
        // colour step, so the only differences left are numeric.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                pixels[y * width as usize + x] = [
                    0.5 + 0.4 * (std::f32::consts::TAU * u).sin(),
                    0.5 + 0.4 * (std::f32::consts::TAU * v).cos(),
                    0.5 + 0.3 * (std::f32::consts::TAU * (u + v)).sin(),
                    1.0,
                ];
            }
        }
        let grade = mondrian_effects::VrRotateSphereGrade::new(
            360.0,
            180.0,
            10.0,
            -20.0,
            5.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
            false,
        )
        .expect("valid vr rotate sphere grade");
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::vr_rotate_sphere_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade,
        ));
        // Poles and the seam are excluded on purpose: next to |lat| = 90 the
        // asin derivative diverges, and the two seam columns tap the clamped
        // frame edge from opposite sides.
        for row in 2..height as usize - 2 {
            for column in 2..width as usize - 2 {
                let (sample_x, sample_y) = grade.source_position(
                    column as f64,
                    row as f64,
                    width as usize,
                    height as usize,
                );
                let (reference_x, reference_y) = reference_vr_position(
                    grade,
                    column as f64,
                    row as f64,
                    width as usize,
                    height as usize,
                );
                assert!(
                    (sample_x - reference_x).abs() <= 1.0e-4,
                    "column {column} row {row}: sample x {sample_x} vs reference {reference_x}"
                );
                assert!(
                    (sample_y - reference_y).abs() <= 1.0e-4,
                    "column {column} row {row}: sample y {sample_y} vs reference {reference_y}"
                );
            }
        }
        let (params, extra, aces, _) = grade.gpu_controls();
        let (eye_width, eye_height) = grade.eye_size(width as usize, height as usize);
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.vr-rotate-sphere-encoder"),
        });
        let output = executor
            .record_vr_rotate_sphere(
                &context.device,
                &mut encoder,
                params,
                extra,
                aces,
                [0.0, 0.0, eye_width as f32, eye_height as f32],
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded vr rotate sphere");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        let mut compared = 0_usize;
        for row in 2..height as usize - 2 {
            for column in 2..width as usize - 2 {
                let index = row * width as usize + column;
                for channel in 0..4 {
                    let difference = (cpu[index][channel] - gpu[index][channel]).abs();
                    assert!(
                        difference <= 2.0 / 255.0,
                        "column {column} row {row} channel {channel}: cpu {:?} gpu {:?}",
                        cpu[index],
                        gpu[index]
                    );
                }
                compared += 1;
            }
        }
        assert!(
            compared > (width as usize * height as usize) / 2,
            "the gradient comparison must cover most of the frame"
        );
    }

    #[test]
    fn gpu_output_has_no_nan_or_inf_at_the_poles() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU vr rotate sphere pole test: no GPU adapter available");
            return;
        };
        let width = 64_u32;
        let height = 64_u32;
        let mut pixels = vec![[0.25_f32, 0.5, 0.75, 1.0]; (width * height) as usize];
        pixels[0] = [0.0, 0.0, 0.0, 0.0];
        pixels[63] = [1.0, 1.0, 1.0, 0.5];
        let grade = mondrian_effects::VrRotateSphereGrade::new(
            360.0,
            180.0,
            90.0,
            0.0,
            0.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
            false,
        )
        .expect("valid vr rotate sphere grade");
        let (params, extra, aces, _) = grade.gpu_controls();
        let (eye_width, eye_height) = grade.eye_size(width as usize, height as usize);
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.vr-rotate-sphere-poles-encoder"),
        });
        let output = executor
            .record_vr_rotate_sphere(
                &context.device,
                &mut encoder,
                params,
                extra,
                aces,
                [0.0, 0.0, eye_width as f32, eye_height as f32],
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded vr rotate sphere");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        // A 90 degree pitch puts the whole top and bottom row on the poles, where
        // the longitude is undefined. The value there is allowed to be anything;
        // it must just never be NaN or infinite.
        for (index, pixel) in gpu.iter().enumerate() {
            for (channel, value) in pixel.iter().enumerate() {
                assert!(
                    value.is_finite(),
                    "pole pixel {index} channel {channel} is {value}"
                );
            }
        }
    }

    #[test]
    fn vr_sphere_to_plane_halo_is_full_frame() {
        let grade = mondrian_effects::VrSphereToPlaneGrade::new(120.0, 10.0, -20.0, 5.0, false)
            .expect("valid vr sphere to plane grade");
        let op = mondrian_effects::EffectRenderOp::VrSphereToPlane { grade };
        let pass = mondrian_effects::gpu_pipeline::spatial_pass_for(&op)
            .expect("vr sphere to plane must lower to a spatial stage");
        assert_eq!(pass.name(), "vr_sphere_to_plane");
        assert!(matches!(
            pass,
            mondrian_effects::EffectGpuSpatialPass::VrSphereToPlane { .. }
        ));
        // A perspective view with a 120 degree lens covers two thirds of the
        // sphere, so a destination pixel may read almost anywhere.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn vr_sphere_to_plane_takes_a_single_pass() {
        let grade = mondrian_effects::VrSphereToPlaneGrade::new(90.0, 0.0, 0.0, 0.0, false)
            .expect("valid vr sphere to plane grade");
        let pass = mondrian_effects::EffectGpuSpatialPass::VrSphereToPlane { grade };
        // One trigonometric remap per pixel: no window, iteration or separable
        // direction to keep in scratch textures.
        assert_eq!(EffectSpatialGpu::texture_pass_count(&pass), Ok(1));
    }

    #[test]
    fn spatial_vr_sphere_to_plane_shader_validates() {
        let module = naga::front::wgsl::parse_str(SPATIAL_EFFECT_SHADER)
            .expect("spatial effect shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spatial effect shader validates");
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_vr_sphere_to_plane"),
            "the vr sphere to plane fragment entry point must survive validation"
        );
    }

    /// Independent f64 evaluation of the sphere to plane inverse mapping.
    ///
    /// The projection ray and the rotation matrix are rebuilt from the authored
    /// angles instead of reusing the kernel's private matrix, so the two
    /// implementations can only agree if the published mapping is right.
    fn reference_vr_sphere_to_plane_position(
        grade: mondrian_effects::VrSphereToPlaneGrade,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> (f64, f64) {
        let h_fov = grade.horizontal_fov_degrees().to_radians();
        let focal = (h_fov * 0.5).tan();
        let u = (column + 0.5) / width as f64;
        let v = (row + 0.5) / height as f64;
        let ax = (2.0 * u - 1.0) * focal;
        let ay = (1.0 - 2.0 * v) * focal * (height as f64 / width as f64);
        let ray_length = (ax * ax + ay * ay + 1.0).sqrt();
        let direction = [ax / ray_length, ay / ray_length, 1.0 / ray_length];
        let pitch = grade.projection_tilt_x_degrees().to_radians();
        let yaw = grade.projection_tilt_y_degrees().to_radians();
        let roll = grade.projection_tilt_z_degrees().to_radians();
        let (sp, cp) = (pitch.sin(), pitch.cos());
        let (sy, cy) = (yaw.sin(), yaw.cos());
        let (sr, cr) = (roll.sin(), roll.cos());
        let rotation = [
            [cr * cy, cr * sy * sp - sr * cp, cr * sy * cp + sr * sp],
            [sr * cy, sr * sy * sp + cr * cp, sr * sy * cp - cr * sp],
            [-sy, cy * sp, cy * cp],
        ];
        let sample = if grade.invert() {
            [
                rotation[0][0] * direction[0]
                    + rotation[1][0] * direction[1]
                    + rotation[2][0] * direction[2],
                rotation[0][1] * direction[0]
                    + rotation[1][1] * direction[1]
                    + rotation[2][1] * direction[2],
                rotation[0][2] * direction[0]
                    + rotation[1][2] * direction[1]
                    + rotation[2][2] * direction[2],
            ]
        } else {
            [
                rotation[0][0] * direction[0]
                    + rotation[0][1] * direction[1]
                    + rotation[0][2] * direction[2],
                rotation[1][0] * direction[0]
                    + rotation[1][1] * direction[1]
                    + rotation[1][2] * direction[2],
                rotation[2][0] * direction[0]
                    + rotation[2][1] * direction[1]
                    + rotation[2][2] * direction[2],
            ]
        };
        let longitude = sample[0].atan2(sample[2]);
        let latitude = sample[1].clamp(-1.0, 1.0).asin();
        (
            (longitude / std::f64::consts::TAU + 0.5) * width as f64 - 0.5,
            (0.5 - latitude / std::f64::consts::PI) * height as f64 - 0.5,
        )
    }

    #[test]
    fn gpu_vr_sphere_to_plane_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU vr sphere to plane parity test: no GPU adapter available");
            return;
        };
        let width = 128_u32;
        let height = 64_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        // A periodic analytic gradient: the source seam does not introduce a
        // colour step, so the only differences left are numeric.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                pixels[y * width as usize + x] = [
                    0.5 + 0.4 * (std::f32::consts::TAU * u).sin(),
                    0.5 + 0.4 * (std::f32::consts::TAU * v).cos(),
                    0.5 + 0.3 * (std::f32::consts::TAU * (u + v)).sin(),
                    1.0,
                ];
            }
        }
        let grade = mondrian_effects::VrSphereToPlaneGrade::new(120.0, 8.0, -12.0, 4.0, false)
            .expect("valid vr sphere to plane grade");
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::vr_sphere_to_plane_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade,
        ));
        let (params, extra, aces, _) =
            grade.gpu_controls_for_frame(width as usize, height as usize);
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.vr-sphere-to-plane-encoder"),
        });
        let output = executor
            .record_vr_sphere_to_plane(
                &context.device,
                &mut encoder,
                params,
                extra,
                aces,
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded vr sphere to plane");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        let mut compared = 0_usize;
        for row in 2..height as usize - 2 {
            for column in 2..width as usize - 2 {
                let (sample_x, sample_y) = grade.source_position(
                    column as f64,
                    row as f64,
                    width as usize,
                    height as usize,
                );
                let (reference_x, reference_y) = reference_vr_sphere_to_plane_position(
                    grade,
                    column as f64,
                    row as f64,
                    width as usize,
                    height as usize,
                );
                assert!(
                    (sample_x - reference_x).abs() <= 1.0e-4,
                    "column {column} row {row}: sample x {sample_x} vs reference {reference_x}"
                );
                assert!(
                    (sample_y - reference_y).abs() <= 1.0e-4,
                    "column {column} row {row}: sample y {sample_y} vs reference {reference_y}"
                );
                // The seam clamps to the last source column, where the two
                // implementations can tap opposite sides of the wrap.
                if !(1.0..=width as f64 - 2.0).contains(&sample_x) {
                    continue;
                }
                let index = row * width as usize + column;
                for channel in 0..4 {
                    let difference = (cpu[index][channel] - gpu[index][channel]).abs();
                    assert!(
                        difference <= 2.0 / 255.0,
                        "column {column} row {row} channel {channel}: cpu {:?} gpu {:?}",
                        cpu[index],
                        gpu[index]
                    );
                }
                compared += 1;
            }
        }
        assert!(
            compared > (width as usize * height as usize) / 2,
            "the gradient comparison must cover most of the frame"
        );
    }

    #[test]
    fn gpu_vr_sphere_to_plane_is_finite_at_the_poles() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU vr sphere to plane pole test: no GPU adapter available");
            return;
        };
        let width = 64_u32;
        let height = 64_u32;
        let mut pixels = vec![[0.25_f32, 0.5, 0.75, 1.0]; (width * height) as usize];
        pixels[0] = [0.0, 0.0, 0.0, 0.0];
        pixels[63] = [1.0, 1.0, 1.0, 0.5];
        // A 90 degree pitch points the view straight up: the top and bottom rows
        // land on the poles, where the longitude is undefined.
        let grade = mondrian_effects::VrSphereToPlaneGrade::new(180.0, 90.0, 0.0, 0.0, false)
            .expect("valid vr sphere to plane grade");
        let (params, extra, aces, _) =
            grade.gpu_controls_for_frame(width as usize, height as usize);
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.vr-sphere-to-plane-poles-encoder"),
        });
        let output = executor
            .record_vr_sphere_to_plane(
                &context.device,
                &mut encoder,
                params,
                extra,
                aces,
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded vr sphere to plane");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        for (index, pixel) in gpu.iter().enumerate() {
            for (channel, value) in pixel.iter().enumerate() {
                assert!(
                    value.is_finite(),
                    "pole pixel {index} channel {channel} is {value}"
                );
            }
        }
    }

    #[test]
    fn vr_project_2d_halo_is_full_frame() {
        let grade = mondrian_effects::VrProject2DGrade::new(
            120.0,
            90.0,
            45.0,
            0.0,
            10.0,
            5.0,
            -8.0,
            3.0,
            2.0,
            0.0,
            -4.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
        )
        .expect("valid vr project 2d grade");
        let op = mondrian_effects::EffectRenderOp::VrProject2D { grade };
        let pass = mondrian_effects::gpu_pipeline::spatial_pass_for(&op)
            .expect("vr project 2d must lower to a spatial stage");
        assert_eq!(pass.name(), "vr_project_2d");
        assert!(matches!(
            pass,
            mondrian_effects::EffectGpuSpatialPass::VrProject2D { .. }
        ));
        // The card can sit anywhere on the destination sphere, so a destination
        // pixel may read anywhere in the source frame.
        assert_eq!(pass.horizontal_halo(), u32::MAX);
        assert_eq!(pass.vertical_halo(), u32::MAX);
    }

    #[test]
    fn vr_project_2d_takes_a_single_pass() {
        let grade = mondrian_effects::VrProject2DGrade::new(
            360.0,
            180.0,
            60.0,
            0.0,
            10.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
        )
        .expect("valid vr project 2d grade");
        let pass = mondrian_effects::EffectGpuSpatialPass::VrProject2D { grade };
        // One trigonometric remap per pixel: no window, iteration or separable
        // direction to keep in scratch textures.
        assert_eq!(EffectSpatialGpu::texture_pass_count(&pass), Ok(1));
    }

    #[test]
    fn spatial_vr_project_2d_shader_validates() {
        let module = naga::front::wgsl::parse_str(SPATIAL_EFFECT_SHADER)
            .expect("spatial effect shader parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("spatial effect shader validates");
        assert!(
            module.entry_points.iter().any(|entry| entry.name == "fs_vr_project_2d"),
            "the vr project 2d fragment entry point must survive validation"
        );
    }

    /// Independent f64 evaluation of the plane to sphere inverse mapping.
    ///
    /// The destination ray, both poses and the patch test are rebuilt from the
    /// authored parameters instead of reusing the kernel's private geometry, so
    /// the two implementations can only agree if the published mapping is right.
    fn reference_vr_project_2d_position(
        grade: mondrian_effects::VrProject2DGrade,
        column: f64,
        row: f64,
        width: usize,
        height: usize,
    ) -> Option<(f64, f64)> {
        let u = (column + 0.5) / width as f64;
        let v = (row + 0.5) / height as f64;
        let longitude = (u - 0.5) * grade.horizontal_fov_degrees().to_radians();
        let latitude = (0.5 - v) * grade.vertical_fov_degrees().to_radians();
        let direction = [
            latitude.cos() * longitude.sin(),
            latitude.sin(),
            latitude.cos() * longitude.cos(),
        ];
        let projection = reference_rotation(
            grade.projection_tilt_x_degrees().to_radians(),
            grade.projection_pan_y_degrees().to_radians(),
            grade.projection_roll_z_degrees().to_radians(),
        );
        let q = reference_transpose_transform(projection, direction);
        if q[2] <= 0.0 {
            return None;
        }
        let patch_x = q[0] / q[2];
        let patch_y = q[1] / q[2];
        let half_extent = grade.patch_half_extent();
        if patch_x.abs() > half_extent || patch_y.abs() > half_extent {
            return None;
        }
        let source = reference_rotation(
            grade.source_tilt_x_degrees().to_radians(),
            grade.source_pan_y_degrees().to_radians(),
            grade.source_roll_z_degrees().to_radians(),
        );
        let s = reference_transpose_transform(source, [patch_x, patch_y, 1.0]);
        if s[2] <= 0.0 {
            return None;
        }
        let source_x = s[0] / s[2];
        let source_y = s[1] / s[2];
        let denominator = 2.0 * half_extent;
        let tangent = grade.stereo_disparity_degrees().to_radians().tan();
        let u_source = 0.5 + (source_x - tangent) / denominator;
        let v_source = 0.5 - source_y / denominator;
        Some((
            u_source * width as f64 - 0.5,
            v_source * height as f64 - 0.5,
        ))
    }

    fn reference_rotation(pitch: f64, yaw: f64, roll: f64) -> [[f64; 3]; 3] {
        let (sp, cp) = (pitch.sin(), pitch.cos());
        let (sy, cy) = (yaw.sin(), yaw.cos());
        let (sr, cr) = (roll.sin(), roll.cos());
        [
            [cr * cy, cr * sy * sp - sr * cp, cr * sy * cp + sr * sp],
            [sr * cy, sr * sy * sp + cr * cp, sr * sy * cp - cr * sp],
            [-sy, cy * sp, cy * cp],
        ]
    }

    fn reference_transpose_transform(matrix: [[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
        [
            matrix[0][0] * vector[0] + matrix[1][0] * vector[1] + matrix[2][0] * vector[2],
            matrix[0][1] * vector[0] + matrix[1][1] * vector[1] + matrix[2][1] * vector[2],
            matrix[0][2] * vector[0] + matrix[1][2] * vector[1] + matrix[2][2] * vector[2],
        ]
    }

    #[test]
    fn gpu_vr_project_2d_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU vr project 2d parity test: no GPU adapter available");
            return;
        };
        let width = 128_u32;
        let height = 64_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        // A periodic analytic gradient: the source seam does not introduce a
        // colour step, so the only differences left are numeric.
        for y in 0..height as usize {
            for x in 0..width as usize {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                pixels[y * width as usize + x] = [
                    0.5 + 0.4 * (std::f32::consts::TAU * u).sin(),
                    0.5 + 0.4 * (std::f32::consts::TAU * v).cos(),
                    0.5 + 0.3 * (std::f32::consts::TAU * (u + v)).sin(),
                    1.0,
                ];
            }
        }
        // A 179 degree card covers almost the whole visible hemisphere, so the
        // comparison exercises the mapping instead of a few centre pixels.
        let grade = mondrian_effects::VrProject2DGrade::new(
            360.0,
            180.0,
            179.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
        )
        .expect("valid vr project 2d grade");
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::vr_project_2d_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade,
        ));
        let (params, extra, aces, extra1) = grade.gpu_controls();
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.vr-project-2d-encoder"),
        });
        let output = executor
            .record_vr_project_2d(
                &context.device,
                &mut encoder,
                params,
                extra,
                aces,
                extra1,
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded vr project 2d");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        let mut compared = 0_usize;
        for row in 0..height as usize {
            for column in 0..width as usize {
                let column_f64 = column as f64;
                let row_f64 = row as f64;
                let Some((sample_x, sample_y)) =
                    grade.source_position(column_f64, row_f64, width as usize, height as usize)
                else {
                    // A skipped pixel must stay bit-identical on both sides.
                    assert_eq!(
                        cpu[row * width as usize + column].map(f32::to_bits),
                        pixels[row * width as usize + column].map(f32::to_bits),
                        "cpu skip at column {column} row {row}"
                    );
                    assert_eq!(
                        gpu[row * width as usize + column].map(f32::to_bits),
                        pixels[row * width as usize + column].map(f32::to_bits),
                        "gpu skip at column {column} row {row}"
                    );
                    continue;
                };
                let (reference_x, reference_y) = reference_vr_project_2d_position(
                    grade,
                    column_f64,
                    row_f64,
                    width as usize,
                    height as usize,
                )
                .expect("reference agrees the pixel hits the patch");
                assert!(
                    (sample_x - reference_x).abs() <= 1.0e-4,
                    "column {column} row {row}: sample x {sample_x} vs reference {reference_x}"
                );
                assert!(
                    (sample_y - reference_y).abs() <= 1.0e-4,
                    "column {column} row {row}: sample y {sample_y} vs reference {reference_y}"
                );
                // The source clamps at the frame border, where the two
                // implementations can tap opposite sides of the border, and the
                // patch edge can flip between f32 and f64.
                if !(2.0..=width as f64 - 3.0).contains(&sample_x)
                    || !(2.0..=height as f64 - 3.0).contains(&sample_y)
                {
                    continue;
                }
                let mut near_boundary = false;
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        if grade
                            .patch_coordinates(
                                column_f64 + f64::from(dx),
                                row_f64 + f64::from(dy),
                                width as usize,
                                height as usize,
                            )
                            .is_none()
                        {
                            near_boundary = true;
                        }
                    }
                }
                if near_boundary {
                    continue;
                }
                let index = row * width as usize + column;
                for channel in 0..4 {
                    let difference = (cpu[index][channel] - gpu[index][channel]).abs();
                    assert!(
                        difference <= 2.0 / 255.0,
                        "column {column} row {row} channel {channel}: cpu {:?} gpu {:?}",
                        cpu[index],
                        gpu[index]
                    );
                }
                compared += 1;
            }
        }
        assert!(
            compared > (width as usize * height as usize) / 3,
            "the gradient comparison must cover most of the visible hemisphere, compared {compared}"
        );
    }

    #[test]
    fn gpu_vr_project_2d_eye_split_matches_the_cpu_integer_division() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU vr project 2d eye test: no GPU adapter available");
            return;
        };
        // An odd frame height is the case where a floating point half would put
        // the second eye one row below the CPU's integer division.
        let width = 64_u32;
        let height = 181_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let u = (x as f32 + 0.5) / width as f32;
                let v = (y as f32 + 0.5) / height as f32;
                pixels[y * width as usize + x] = [
                    0.5 + 0.35 * (std::f32::consts::TAU * u).sin(),
                    0.4 + 0.3 * v,
                    0.5 + 0.3 * (std::f32::consts::TAU * (u + v)).cos(),
                    1.0,
                ];
            }
        }
        let layout = mondrian_effects::VrFrameLayout::OverUnder;
        let grade = mondrian_effects::VrProject2DGrade::new(
            360.0, 180.0, 120.0, 0.0, 10.0, 6.0, -4.0, 3.0, 2.0, -3.0, 1.0, layout,
        )
        .expect("valid vr project 2d grade");
        // The layout does not affect the mapping, so the same geometry without
        // the stereo split gives the eye-local patch coordinates.
        let mono = mondrian_effects::VrProject2DGrade::new(
            360.0,
            180.0,
            120.0,
            0.0,
            10.0,
            6.0,
            -4.0,
            3.0,
            2.0,
            -3.0,
            1.0,
            mondrian_effects::VrFrameLayout::Monoscopic,
        )
        .expect("valid monoscopic vr project 2d grade");
        let mut cpu = pixels.clone();
        assert!(mondrian_effects::vr_project_2d_frame_f32(
            &mut cpu,
            width as usize,
            height as usize,
            grade,
        ));
        let (params, extra, aces, extra1) = grade.gpu_controls();
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &pixels, width, height);
        let destination = scratch(&context, width, height, 1);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.vr-project-2d-eyes-encoder"),
        });
        let output = executor
            .record_vr_project_2d(
                &context.device,
                &mut encoder,
                params,
                extra,
                aces,
                extra1,
                &source,
                &destination[0],
                width,
                height,
            )
            .expect("recorded vr project 2d");
        context.queue.submit(Some(encoder.finish()));
        let gpu = readback(&context, &output, width, height);
        let (eye_width, eye_height) = layout.eye_size(width as usize, height as usize);
        assert_eq!((eye_width, eye_height), (64, 90));
        let mut compared = 0_usize;
        for eye in 0..2_usize {
            for local_row in 0..eye_height {
                for local_column in 0..eye_width {
                    let row = eye * eye_height + local_row;
                    let index = row * width as usize + local_column;
                    let Some((patch_x, patch_y)) = mono.patch_coordinates(
                        local_column as f64,
                        local_row as f64,
                        eye_width,
                        eye_height,
                    ) else {
                        // Outside the patch both kernels leave the pixel alone.
                        assert_eq!(
                            gpu[index].map(f32::to_bits),
                            pixels[index].map(f32::to_bits),
                            "gpu skip at eye {eye} column {local_column} row {local_row}"
                        );
                        continue;
                    };
                    if mono.feather_weight(patch_x, patch_y) < 0.05 {
                        continue;
                    }
                    for channel in 0..4 {
                        let difference = (cpu[index][channel] - gpu[index][channel]).abs();
                        assert!(
                            difference <= 2.0 / 255.0,
                            "eye {eye} column {local_column} row {local_row} channel {channel}: \
                             cpu {:?} gpu {:?}",
                            cpu[index],
                            gpu[index]
                        );
                    }
                    compared += 1;
                }
            }
        }
        // The odd trailing row belongs to no eye: neither kernel may touch it.
        for column in 0..width as usize {
            let index = 180 * width as usize + column;
            assert_eq!(
                cpu[index].map(f32::to_bits),
                pixels[index].map(f32::to_bits),
                "cpu touched the odd trailing row"
            );
            assert_eq!(
                gpu[index].map(f32::to_bits),
                pixels[index].map(f32::to_bits),
                "gpu touched the odd trailing row"
            );
        }
        assert!(
            compared > 2_000,
            "the two eyes must both project a real patch, compared {compared}"
        );
    }

    #[test]
    fn gpu_dent_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU dent parity test: no GPU adapter available");
            return;
        };
        let cases = [
            (
                13_u32,
                11_u32,
                0_usize,
                0.4_f32,
                0.5_f32,
                [6.0_f32, 5.0_f32],
                mondrian_effects::DentKind::Dent,
                mondrian_effects::DentEdge::Smooth,
            ),
            (
                13_u32,
                11_u32,
                142_usize,
                1.0_f32,
                1.0_f32,
                [0.0_f32, 0.0_f32],
                mondrian_effects::DentKind::Bulge,
                mondrian_effects::DentEdge::Smooth,
            ),
            (
                16_u32,
                9_u32,
                // Pixel (0, 8): outside the disk and only ever sampled exactly on
                // its texel. Pixel (0, 0) would also be outside, but pixel (2, 2)
                // clamps its sample onto that texel with a fractional y near one
                // epsilon, where the GPU's FMA contraction and the CPU's two-step
                // rounding disagree about zero coverage.
                128_usize,
                0.75_f32,
                0.8_f32,
                [7.5_f32, 4.5_f32],
                mondrian_effects::DentKind::Dent,
                mondrian_effects::DentEdge::Hard,
            ),
        ];
        for (width, height, zero_coverage, size, strength, centre, kind, edge) in cases {
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let value = 0.05 * (x + 2 * y) as f32;
                    pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
                }
            }
            pixels[30] = [0.0, 0.0, 0.0, 0.0];
            pixels[61] = [2.5, -0.75, 1.25, 0.5];
            pixels[zero_coverage] = [0.0, 0.0, 0.0, 0.0];
            let grade = mondrian_effects::DentGrade::new(size, strength, centre, kind, edge)
                .expect("dent grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::dent_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            assert_eq!(
                cpu[zero_coverage],
                [0.0, 0.0, 0.0, 0.0],
                "cpu zero-coverage pixel {zero_coverage} must stay transparent",
            );
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.dent-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_dent(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded dent");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            assert_eq!(
                gpu[zero_coverage],
                [0.0, 0.0, 0.0, 0.0],
                "gpu zero-coverage pixel {zero_coverage} must stay transparent",
            );
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "size {size} strength {strength} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_warp_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU warp parity test: no GPU adapter available");
            return;
        };
        let cases = [
            (
                13_u32,
                11_u32,
                0_usize,
                100.0_f32,
                0.0_f32,
                0.0_f32,
                mondrian_effects::WarpStyle::Arc,
                mondrian_effects::WarpAxis::Horizontal,
            ),
            (
                13_u32,
                11_u32,
                65_usize,
                -50.0_f32,
                100.0_f32,
                0.0_f32,
                mondrian_effects::WarpStyle::Flag,
                mondrian_effects::WarpAxis::Vertical,
            ),
            (
                16_u32,
                9_u32,
                0_usize,
                100.0_f32,
                0.0_f32,
                100.0_f32,
                mondrian_effects::WarpStyle::ShellUpper,
                mondrian_effects::WarpAxis::Horizontal,
            ),
        ];
        for (
            width,
            height,
            zero_coverage,
            bend,
            horizontal_distortion,
            vertical_distortion,
            style,
            axis,
        ) in cases
        {
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let value = 0.05 * (x + 2 * y) as f32;
                    pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
                }
            }
            // The rim pixel is fixed by the mapping and samples its own texel
            // with a zero fraction, so both sides return the texel unchanged.
            pixels[zero_coverage] = [0.0, 0.0, 0.0, 0.0];
            let grade = mondrian_effects::WarpGrade::new(
                bend,
                horizontal_distortion,
                vertical_distortion,
                style,
                axis,
            )
            .expect("warp grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::warp_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            assert_eq!(
                cpu[zero_coverage],
                [0.0, 0.0, 0.0, 0.0],
                "cpu zero-coverage pixel {zero_coverage} must stay transparent",
            );
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.warp-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_warp(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded warp");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            assert_eq!(
                gpu[zero_coverage],
                [0.0, 0.0, 0.0, 0.0],
                "gpu zero-coverage pixel {zero_coverage} must stay transparent",
            );
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "style {style:?} axis {axis:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_grid_warp_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU grid warp parity test: no GPU adapter available");
            return;
        };
        let cases = [
            (13_u32, 11_u32, 7_u32, 7_u32, 1_u32, "default"),
            (13_u32, 11_u32, 31_u32, 31_u32, 10_u32, "extreme"),
            (16_u32, 9_u32, 3_u32, 9_u32, 5_u32, "asymmetric"),
        ];
        for (width, height, columns, rows, quality, label) in cases {
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let value = 0.05 * (x + 2 * y) as f32;
                    pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
                }
            }
            pixels[24] = [0.0, 0.0, 0.0, 0.0];
            let grade = mondrian_effects::GridWarpGrade::new(columns, rows, quality)
                .expect("grid warp grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::grid_warp_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.grid-warp-encoder"),
                });
            let (params, extra) = grade.gpu_controls(width as usize, height as usize);
            let output = executor
                .record_grid_warp(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded grid warp");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "grid warp {label} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // One pass needs exactly one scratch raster.
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &vec![[0.0_f32; 4]; 13 * 11], 13, 11);
        let none = scratch(&context, 13, 11, 0);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.grid-warp-geometry-encoder"),
        });
        let grade = mondrian_effects::GridWarpGrade::new(7, 7, 1).expect("valid grid warp grade");
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::GridWarp { grade },
                &source,
                &none,
                13,
                11,
            )
            .expect_err("a missing scratch texture cannot run the grid warp");
        assert!(
            matches!(
                error,
                EffectSpatialGpuError::UnsupportedPass("grid_warp_scratch_geometry")
            ),
            "unexpected error: {error:?}"
        );
    }
    #[test]
    fn gpu_wire_removal_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU wire removal parity test: no GPU adapter available");
            return;
        };
        let width = 15_u32;
        let height = 11_u32;
        let cases = [
            (
                "both",
                [7.5_f32, 5.5],
                [7.5, 1.5],
                mondrian_effects::RemovalStyle::Both,
                2.0_f32,
                0.5_f32,
                0.25_f32,
            ),
            (
                "left",
                [4.5, 5.5],
                [10.5, 5.5],
                mondrian_effects::RemovalStyle::Left,
                2.0,
                0.0,
                0.0,
            ),
            (
                "right",
                [4.5, 5.5],
                [10.5, 5.5],
                mondrian_effects::RemovalStyle::Right,
                3.0,
                1.0,
                0.5,
            ),
            (
                "cross",
                [7.5, 2.5],
                [7.5, 8.5],
                mondrian_effects::RemovalStyle::Cross,
                3.0,
                0.3,
                0.75,
            ),
        ];
        for (label, point_a, point_b, style, thickness, slope, mirror_blend) in cases {
            let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let value = 0.04 * (x + 2 * y) as f32;
                    pixels[y * width as usize + x] = [value, 0.5 - value * 0.4, value * 1.5, 1.0];
                }
            }
            pixels[24] = [0.0, 0.0, 0.0, 0.0];
            let grade = mondrian_effects::WireRemovalGrade::new(
                point_a,
                point_b,
                style,
                thickness,
                slope,
                mirror_blend,
                0,
            )
            .expect("wire removal grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::wire_removal_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.wire-removal-encoder"),
                });
            let (params, extra) = grade.gpu_controls();
            let output = executor
                .record_wire_removal(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded wire removal");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "wire removal {label} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // One pass needs exactly one scratch raster.
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(
            &context,
            &vec![[0.0_f32; 4]; (width * height) as usize],
            width,
            height,
        );
        let none = scratch(&context, width, height, 0);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.wire-removal-geometry-encoder"),
        });
        let grade = mondrian_effects::WireRemovalGrade::new(
            [7.5, 5.5],
            [7.5, 1.5],
            mondrian_effects::RemovalStyle::Both,
            2.0,
            0.5,
            0.25,
            0,
        )
        .expect("valid wire removal grade");
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::WireRemoval { grade },
                &source,
                &none,
                width,
                height,
            )
            .expect_err("a missing scratch texture cannot run wire removal");
        assert!(
            matches!(error, EffectSpatialGpuError::EmptyFrame),
            "unexpected error: {error:?}"
        );
    }
    #[test]
    fn gpu_selective_color_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU selective colour parity test: no GPU adapter available");
            return;
        };
        // Frame one: one block per colour family plus a transparent pixel, so the
        // hue tents, the chroma gate, and the alpha passthrough all run.
        let blocks = [
            [1.0_f32, 0.0, 0.0, 1.0],
            [1.0, 0.5, 0.0, 1.0],
            [0.5, 0.5, 0.5, 1.0],
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.4, 0.2, 0.9, 0.0],
        ];
        let block_frame: Vec<[f32; 4]> = blocks.into_iter().cycle().take(20).collect();
        let block_grade =
            mondrian_effects::SelectiveColorGrade::new(1, 1, [0.0; 4], [[50.0, 0.0, 0.0, 0.0]; 9])
                .expect("valid reds relative selective colour grade");

        // Frame two: a gradient with a fully transparent row and a yellow shift
        // authored in absolute mode, so the additive ink path runs.
        let mut gradient_frame = Vec::new();
        for y in 0..3 {
            for x in 0..7 {
                let coverage = if y == 2 { 0.0 } else { 1.0 };
                gradient_frame.push([
                    x as f32 / 6.0,
                    y as f32 / 2.0,
                    1.0 - x as f32 / 6.0,
                    coverage,
                ]);
            }
        }
        let mut blue_deltas = [[0.0_f32; 4]; 9];
        blue_deltas[4] = [0.0, 0.0, -40.0, 0.0];
        let gradient_grade =
            mondrian_effects::SelectiveColorGrade::new(2, 5, [0.0; 4], blue_deltas)
                .expect("valid blues absolute selective colour grade");

        // Frame three: out-of-range samples plus a near-neutral highlight and a
        // near-neutral clip, so the tone ranges and the unclamped CMYK round trip
        // are both compared. The single output clamp is what keeps them finite.
        let mut hdr_frame = Vec::new();
        for index in 0..16 {
            let x = index % 4;
            let y = index / 4;
            hdr_frame.push([1.5 + 0.1 * x as f32, -0.2 - 0.05 * y as f32, 0.5, 1.0]);
        }
        hdr_frame[14] = [1.55, 1.552, 1.548, 1.0];
        hdr_frame[15] = [-0.20, -0.19, -0.21, 1.0];
        let mut neutral_deltas = [[0.0_f32; 4]; 9];
        neutral_deltas[7] = [0.0, 0.0, 0.0, 100.0];
        let hdr_grade = mondrian_effects::SelectiveColorGrade::new(1, 8, [0.0; 4], neutral_deltas)
            .expect("valid neutrals relative selective colour grade");

        let cases = [
            ("blocks", 5_u32, 4_u32, block_frame, block_grade),
            ("gradient", 7, 3, gradient_frame, gradient_grade),
            ("hdr", 4, 4, hdr_frame, hdr_grade),
        ];
        for (label, width, height, pixels, grade) in cases {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::selective_color_frame_f32(
                &mut cpu, &grade
            ));

            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.selective-color-encoder"),
                });
            let output = executor
                .record_selective_color(
                    &context.device,
                    &mut encoder,
                    grade,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded selective colour");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    // The shader mirrors the kernel statement for statement; the
                    // only divergence left is fused multiply-add selection inside
                    // the adapter's float units, so 1e-4 absolute covers the ink
                    // round trip without hiding a formula change.
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "selective colour {label} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // One pass needs exactly one scratch raster.
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &vec![[0.0_f32; 4]; 20], 5, 4);
        let none = scratch(&context, 5, 4, 0);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.selective-color-geometry-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::SelectiveColor { grade: block_grade },
                &source,
                &none,
                5,
                4,
            )
            .expect_err("a missing scratch texture cannot run selective colour");
        assert!(
            matches!(error, EffectSpatialGpuError::EmptyFrame),
            "unexpected error: {error:?}"
        );
    }
    #[test]
    fn gpu_stroke_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU stroke parity test: no GPU adapter available");
            return;
        };
        // Frame one: a hard-edged rectangle matte, so every ray direction finds a
        // crossing and the band wraps the whole contour.
        let mut rectangle = Vec::new();
        for y in 0..6_u32 {
            for x in 0..8_u32 {
                let inside = (1..=6).contains(&x) && (1..=4).contains(&y);
                rectangle.push([0.3, 0.6, 0.9, if inside { 1.0 } else { 0.0 }]);
            }
        }
        let rectangle_grade = mondrian_effects::StrokeGrade::new(
            mondrian_effects::StrokePaintStyle::OnOriginal,
            [1.0, 0.25, 0.0],
            2.0,
            0.75,
            0.8,
        )
        .expect("valid rectangle stroke grade");

        // Frame two: an analytic half plane with a four pixel linear ramp, the
        // fixture the kernel distance tests use, painted on transparent.
        let mut ramp = Vec::new();
        for _ in 0..13 {
            for x in 0..17_u32 {
                let alpha = (0.5 + (8.0 - x as f32) / 4.0).clamp(0.0, 1.0);
                ramp.push([0.9, 0.2, 0.1, alpha]);
            }
        }
        let ramp_grade = mondrian_effects::StrokeGrade::new(
            mondrian_effects::StrokePaintStyle::OnTransparent,
            [0.2, 0.9, 0.3],
            5.0,
            0.0,
            1.0,
        )
        .expect("valid ramp stroke grade");

        // Frame three: an oblique triangle, so the diagonal rays and the
        // eight-percent direction quantisation both run, revealing the source.
        let mut triangle = Vec::new();
        for y in 0..9_u32 {
            for x in 0..32_u32 {
                let inside = x as i32 >= 2 * y as i32 + 2 && x as i32 <= 22 - y as i32;
                triangle.push([
                    0.1 + 0.02 * x as f32,
                    0.5,
                    0.8 - 0.02 * y as f32,
                    if inside { 1.0 } else { 0.0 },
                ]);
            }
        }
        let triangle_grade = mondrian_effects::StrokeGrade::new(
            mondrian_effects::StrokePaintStyle::RevealOriginal,
            [1.0, 1.0, 1.0],
            3.0,
            0.5,
            0.9,
        )
        .expect("valid triangle stroke grade");

        let cases = [
            ("rectangle", 8_u32, 6_u32, rectangle, rectangle_grade),
            ("ramp", 17, 13, ramp, ramp_grade),
            ("triangle", 32, 9, triangle, triangle_grade),
        ];
        for (label, width, height, pixels, grade) in cases {
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::stroke_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                &grade,
            ));

            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.stroke-encoder"),
                });
            let output = executor
                .record_stroke(
                    &context.device,
                    &mut encoder,
                    grade,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded stroke");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    // The shader mirrors the kernel statement for statement; the
                    // only divergence left is fused multiply-add selection inside
                    // the adapter's float units, so 1e-4 absolute covers the band
                    // composite without hiding a formula change.
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "stroke {label} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }

        // A fully transparent frame has no half-coverage contour, so all three
        // paint styles must return it bit for bit.
        let transparent = vec![[0.0_f32; 4]; 9 * 7];
        for style in mondrian_effects::StrokePaintStyle::ALL {
            let grade = mondrian_effects::StrokeGrade::new(style, [1.0, 0.5, 0.25], 4.0, 0.75, 1.0)
                .expect("valid transparent stroke grade");
            let mut cpu = transparent.clone();
            assert!(mondrian_effects::stroke_frame_f32(&mut cpu, 9, 7, &grade));
            assert_eq!(
                cpu, transparent,
                "the CPU must leave a transparent frame alone"
            );
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &transparent, 9, 7);
            let destination = scratch(&context, 9, 7, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.stroke-transparent-encoder"),
                });
            let output = executor
                .record_stroke(
                    &context.device,
                    &mut encoder,
                    grade,
                    &source,
                    &destination[0],
                    9,
                    7,
                )
                .expect("recorded transparent stroke");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, 9, 7);
            assert_eq!(
                gpu, transparent,
                "the GPU must leave a transparent frame alone"
            );
        }

        // One pass needs exactly one scratch raster.
        let executor = EffectSpatialGpu::new(&context.device);
        let source = upload(&context, &vec![[0.0_f32; 4]; 20], 5, 4);
        let none = scratch(&context, 5, 4, 0);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.stroke-geometry-encoder"),
        });
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::Stroke { grade: rectangle_grade },
                &source,
                &none,
                5,
                4,
            )
            .expect_err("a missing scratch texture cannot run a stroke");
        assert!(
            matches!(error, EffectSpatialGpuError::EmptyFrame),
            "unexpected error: {error:?}"
        );
    }
    #[test]
    fn gpu_roughen_edges_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU roughen edges parity test: no GPU adapter available");
            return;
        };
        let check = |label: &str,
                     pixels: &[[f32; 4]],
                     width: u32,
                     height: u32,
                     grade: mondrian_effects::RoughenEdgesGrade| {
            let mut cpu = pixels.to_vec();
            assert!(mondrian_effects::roughen_edges_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                &grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.roughen-edges-encoder"),
                });
            let (params, extra, extra1) = grade.gpu_controls();
            let output = executor
                .record_roughen_edges(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    grade.aces_controls(),
                    extra1,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded roughen edges");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "roughen edges {label} pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        };

        // A hard edged block grows outward by the authored border.
        let width = 6_u32;
        let height = 5_u32;
        let mut block = vec![[0.0_f32, 0.0, 0.0, 0.0]; (width * height) as usize];
        for y in 1..4 {
            for x in 1..4 {
                block[(y * width + x) as usize] = [0.25, 0.5, 0.75, 1.0];
            }
        }
        check(
            "block",
            &block,
            width,
            height,
            mondrian_effects::RoughenEdgesGrade::new(
                1,
                [0.6, 0.2, 0.0],
                2.0,
                1.0,
                0.0,
                100.0,
                0.0,
                [0.0, 0.0],
                2,
                0.0,
                false,
                1,
                0,
            )
            .expect("block roughen edges grade"),
        );

        // A soft alpha ramp exercises the fractal influence and the rusty tint.
        let width = 7_u32;
        let height = 4_u32;
        let mut ramp = vec![[0.0_f32, 0.0, 0.0, 0.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let alpha = ((x + y) as f32 / 9.0).clamp(0.0, 1.0);
                ramp[y * width as usize + x] = [0.4 * alpha, 0.2 * alpha, 0.1 * alpha, alpha];
            }
        }
        check(
            "ramp",
            &ramp,
            width,
            height,
            mondrian_effects::RoughenEdgesGrade::new(
                4,
                [0.6, 0.2, 0.0],
                3.0,
                1.4,
                0.5,
                100.0,
                20.0,
                [1.5, -2.0],
                3,
                1080.0,
                true,
                2,
                7,
            )
            .expect("ramp roughen edges grade"),
        );

        // A lone opaque pixel exercises the cutout threshold and the evolution.
        let width = 5_u32;
        let height = 5_u32;
        let mut dot = vec![[0.0_f32, 0.0, 0.0, 0.0]; (width * height) as usize];
        dot[(2 * width + 2) as usize] = [0.3, 0.4, 0.5, 1.0];
        check(
            "dot",
            &dot,
            width,
            height,
            mondrian_effects::RoughenEdgesGrade::new(
                7,
                [0.6, 0.2, 0.0],
                2.0,
                1.0,
                1.0,
                100.0,
                0.0,
                [0.0, 0.0],
                4,
                90.0,
                false,
                1,
                0,
            )
            .expect("dot roughen edges grade"),
        );

        // One pass needs exactly one scratch raster.
        let executor = EffectSpatialGpu::new(&context.device);
        let width = 6_u32;
        let height = 5_u32;
        let source = upload(
            &context,
            &vec![[0.0_f32; 4]; (width * height) as usize],
            width,
            height,
        );
        let none = scratch(&context, width, height, 0);
        let mut encoder = context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mondrian.effect-spatial.roughen-edges-geometry-encoder"),
        });
        let grade = mondrian_effects::RoughenEdgesGrade::new(
            1,
            [0.6, 0.2, 0.0],
            2.0,
            1.0,
            0.0,
            100.0,
            0.0,
            [0.0, 0.0],
            2,
            0.0,
            false,
            1,
            0,
        )
        .expect("valid roughen edges grade");
        let error = executor
            .record(
                &context.device,
                &mut encoder,
                &mondrian_effects::EffectGpuSpatialPass::RoughenEdges { grade },
                &source,
                &none,
                width,
                height,
            )
            .expect_err("a missing scratch texture cannot run roughen edges");
        assert!(
            matches!(error, EffectSpatialGpuError::EmptyFrame),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn gpu_twirl_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU twirl parity test: no GPU adapter available");
            return;
        };
        let width = 13_u32;
        let height = 11_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.05 * (x + 2 * y) as f32;
                pixels[y * width as usize + x] = [value, 0.45 - value * 0.3, value * 1.6, 1.0];
            }
        }
        pixels[30] = [0.0, 0.0, 0.0, 0.0];
        pixels[61] = [2.5, -0.75, 1.25, 0.5];
        for (angle, radius, centre) in [
            (90.0_f32, 100.0_f32, [6.0_f32, 5.0_f32]),
            (-125.0, 60.0, [3.5, 7.25]),
            (30.0, 20.0, [-2.0, 1.0]),
        ] {
            let grade =
                mondrian_effects::TwirlGrade::new(angle, radius, centre).expect("twirl grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::twirl_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.twirl-encoder"),
                });
            let params = grade.gpu_controls();
            let output = executor
                .record_twirl(
                    &context.device,
                    &mut encoder,
                    params,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded twirl");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "angle {angle} radius {radius} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_offset_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU offset parity test: no GPU adapter available");
            return;
        };
        let width = 9_u32;
        let height = 7_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.07 * (x * 2 + y) as f32;
                pixels[y * width as usize + x] = [value, 0.35 - value * 0.4, value * 1.4, 1.0];
            }
        }
        pixels[20] = [0.0, 0.0, 0.0, 0.0];
        pixels[41] = [2.5, -0.75, 1.25, 0.5];
        for (shift, blend) in [
            ([3.0_f32, -2.0_f32], 0.0_f32),
            ([0.5, 1.5], 0.35),
            ([-11.25, 4.75], 0.8),
        ] {
            let grade = mondrian_effects::OffsetGrade::new(shift, blend).expect("offset grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::offset_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.offset-encoder"),
                });
            let params = grade.gpu_controls();
            let output = executor
                .record_offset(
                    &context.device,
                    &mut encoder,
                    params,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded offset");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "shift {shift:?} blend {blend} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }
    #[test]
    fn gpu_convolution_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU convolution parity test: no GPU adapter available");
            return;
        };
        let width = 8_u32;
        let height = 6_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.06 * (x * 2 + y) as f32;
                pixels[y * width as usize + x] = [value, 0.4 - value * 0.5, value * 1.5, 1.0];
            }
        }
        pixels[17] = [0.0, 0.0, 0.0, 0.0];
        pixels[30] = [2.5, -0.75, 1.25, 0.5];
        for (kernel, absolute_values, blend) in [
            (
                [0.0_f32, -1.0, 0.0, -1.0, 5.0, -1.0, 0.0, -1.0, 0.0],
                false,
                0.0_f32,
            ),
            ([1.0_f32 / 9.0; 9], true, 0.35),
            (
                [0.0_f32, 1.0, -1.0, 2.0, -2.0, 1.0, -1.0, 0.5, 0.25],
                true,
                0.75,
            ),
        ] {
            let grade = mondrian_effects::ConvolutionGrade::new(kernel, absolute_values, blend)
                .expect("convolution grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::convolution_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.convolution-encoder"),
                });
            let (params, extra, aces) = grade.gpu_controls();
            let output = executor
                .record_convolution(
                    &context.device,
                    &mut encoder,
                    params,
                    extra,
                    aces,
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded convolution");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "kernel {kernel:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gpu_radial_blur_matches_the_cpu_kernel() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU radial blur parity test: no GPU adapter available");
            return;
        };
        let width = 9_u32;
        let height = 7_u32;
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; (width * height) as usize];
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = 0.08 * (x + y) as f32;
                pixels[y * width as usize + x] = [value, value * 0.5 + 0.1, 0.6 - value, 1.0];
            }
        }
        pixels[20] = [0.0, 0.0, 0.0, 0.0];
        for mode in [
            mondrian_effects::RadialBlurMode::Spin,
            mondrian_effects::RadialBlurMode::Zoom,
        ] {
            let grade = mondrian_effects::RadialBlurGrade::new(250.0, [3.5, 3.0], mode, true, 11)
                .expect("radial blur grade");
            let mut cpu = pixels.clone();
            assert!(mondrian_effects::radial_blur_frame_f32(
                &mut cpu,
                width as usize,
                height as usize,
                grade,
            ));
            let executor = EffectSpatialGpu::new(&context.device);
            let source = upload(&context, &pixels, width, height);
            let destination = scratch(&context, width, height, 1);
            let mut encoder =
                context.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("mondrian.effect-spatial.radial-blur-encoder"),
                });
            let output = executor
                .record_radial_blur(
                    &context.device,
                    &mut encoder,
                    [
                        grade.centre()[0],
                        grade.centre()[1],
                        grade.taps() as f32,
                        f32::from(grade.mode().as_u8()),
                    ],
                    [grade.amount(), grade.seed() as f32, 0.0, 0.0],
                    &source,
                    &destination[0],
                    width,
                    height,
                )
                .expect("recorded radial blur");
            context.queue.submit(Some(encoder.finish()));
            let gpu = readback(&context, &output, width, height);
            for (index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "mode {mode:?} pixel {index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
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

    /// Compare the recorded GPU box blur against the CPU kernel at the sizes
    /// and radii that decide whether the GPU path is worth routing to.
    ///
    /// Run with `cargo test -p mondrian-renderer --lib report_spatial_gpu_throughput
    /// -- --nocapture`. Numbers are wall-clock including upload and readback for
    /// the GPU case, so they are an upper bound for a production loop that keeps
    /// frames resident.
    #[test]
    fn report_spatial_gpu_throughput() {
        let Ok(context) = block_on(crate::GpuContext::new()) else {
            eprintln!("skipping GPU spatial throughput report: no GPU adapter available");
            return;
        };
        for (width, height) in [(1280_u32, 720_u32), (1920, 1080), (3840, 2160)] {
            let pixels = vec![[0.2_f32, 0.4, 0.6, 1.0]; (width * height) as usize];
            for radius in [8.0_f32, 50.0] {
                let mut cpu_pixels = pixels.clone();
                let cpu_start = std::time::Instant::now();
                assert!(box_blur::box_blur_frame_f32(
                    &mut cpu_pixels,
                    width as usize,
                    height as usize,
                    radius,
                    1,
                    BoxBlurDirection::HorizontalAndVertical,
                    true,
                ));
                let cpu_elapsed = cpu_start.elapsed();
                let gpu_start = std::time::Instant::now();
                let _ = run_box_blur_on_gpu(
                    &context,
                    &pixels,
                    width,
                    height,
                    radius,
                    1,
                    BoxBlurDirection::HorizontalAndVertical,
                    true,
                );
                let gpu_elapsed = gpu_start.elapsed();
                eprintln!(
                    "box blur {width}x{height} r={radius}: cpu(1 thread) {:.1} ms, gpu(incl. upload+readback) {:.1} ms",
                    cpu_elapsed.as_secs_f64() * 1000.0,
                    gpu_elapsed.as_secs_f64() * 1000.0,
                );
            }
        }
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
