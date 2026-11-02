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
    find_edges: wgpu::RenderPipeline,
    emboss: wgpu::RenderPipeline,
    color_emboss: wgpu::RenderPipeline,
    radial_blur: wgpu::RenderPipeline,
    offset: wgpu::RenderPipeline,
    twirl: wgpu::RenderPipeline,
    spherize: wgpu::RenderPipeline,
    mirror: wgpu::RenderPipeline,
    polar_coordinates: wgpu::RenderPipeline,
    lens_distortion: wgpu::RenderPipeline,
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
    minimax: wgpu::RenderPipeline,
    bindings: wgpu::BindGroupLayout,
    composite_bindings: wgpu::BindGroupLayout,
    fractal_noise_bindings: wgpu::BindGroupLayout,
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
            source: wgpu::ShaderSource::Wgsl(SPATIAL_COMPOSITE_SHADER.into()),
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
        Self {
            box_blur,
            emboss,
            color_emboss,
            radial_blur,
            offset,
            twirl,
            spherize,
            mirror,
            polar_coordinates,
            lens_distortion,
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
            minimax,
            bindings,
            composite_bindings,
            fractal_noise_bindings,
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
            // The chain is one pass per operation step and axis: 1, 2 or 4.
            // `record_minimax` executes exactly the same table.
            EffectGpuSpatialPass::Minimax { grade } => Ok(grade.pass_count()),
            EffectGpuSpatialPass::LensDistortion { .. } => Ok(1),
            EffectGpuSpatialPass::LensFlare { .. } => Ok(1),
            EffectGpuSpatialPass::Bulge { .. } => Ok(1),
            EffectGpuSpatialPass::Split { .. } => Ok(1),
            EffectGpuSpatialPass::LinearColorKey { .. } => Ok(1),
            EffectGpuSpatialPass::ColorOffset { .. } => Ok(1),
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

    /// Records one composite pass over the original frame and an auxiliary layer.
    #[allow(clippy::too_many_arguments)]
    fn record_composite_pass(
        &self,
        device: &wgpu::Device,
        encoder: &mut dyn crate::gpu_command::GpuCommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        original: &wgpu::TextureView,
        auxiliary: &wgpu::TextureView,
        output: &wgpu::TextureView,
        uniforms: &SpatialPassUniform,
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
    let threshold = clamp(uniforms.params.x, 0.0, 1.0);
    var colour_weight = 0.0;
    if (threshold < 1.0) {
        colour_weight = clamp((luminance - threshold) / (1.0 - threshold), 0.0, 1.0);
    }
    let based_on = u32(round(uniforms.extra.x));
    var weight = colour_weight;
    var colour = sample.rgb;
    if (based_on == 1u) {
        weight = alpha;
        colour = vec3<f32>(1.0, 1.0, 1.0);
    } else if (based_on == 2u) {
        weight = max(colour_weight, alpha);
    }
    let colours = u32(round(uniforms.extra.y));
    let midpoint = uniforms.extra.z;
    var tint = colour;
    if (colours == 1u) {
        let ramp_low = 0.5 * luminance / midpoint;
        let ramp_high = 0.5 + 0.5 * (luminance - midpoint) / (1.0 - midpoint);
        let ramp = select(ramp_high, ramp_low, luminance < midpoint);
        tint = mix(uniforms.aces.rgb, uniforms.extra1.rgb, ramp);
    } else if (colours == 2u) {
        let phase = uniforms.extra.w + luminance * uniforms.aces.a;
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
}"#;

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

// The glow merge mirrors `GlowGrade::composite`: the contribution is the
// extracted-and-blurred layer times its own coverage and the authored
// intensity, and the merge always keeps the source alpha.
@fragment
fn fs_glow_composite(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(floor(position.xy));
    let source = load_clamped_from(original_texture, pixel);
    let glow = load_clamped_from(auxiliary_texture, pixel);
    if (source.a <= 0.0) {
        return source;
    }
    let intensity = uniforms.params.x;
    let mode = u32(round(uniforms.params.y));
    let composite = u32(round(uniforms.params.z));
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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_effects::find_edges::{find_edges_frame_f32, FindEdgesGrade};
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
        let passes = [
            mondrian_effects::EffectGpuSpatialPass::BoxBlur {
                radius: 1.5,
                iterations: 1,
                direction: BoxBlurDirection::HorizontalAndVertical,
                repeat_edge_pixels: true,
            },
            mondrian_effects::EffectGpuSpatialPass::Twirl { grade: twirl },
            mondrian_effects::EffectGpuSpatialPass::Spherize { grade: spherize },
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
        ];

        let mut cpu = pixels.clone();
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

        let executor = EffectSpatialGpu::new(&context.device);
        let original = upload(&context, &pixels, width, height);
        // Seven rotating scratch textures keep every stage's destination out of
        // its own source, which wgpu rejects as conflicting texture usage, and
        // give the three-pass deband chain, the six-pass glow chain, and the
        // four-pass minimax chain their intermediates.
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
        for (index, (label, grade)) in cases.iter().enumerate() {
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
            for (pixel_index, (cpu_pixel, gpu_pixel)) in cpu.iter().zip(gpu.iter()).enumerate() {
                for channel in 0..4 {
                    let difference = (cpu_pixel[channel] - gpu_pixel[channel]).abs();
                    assert!(
                        difference <= 1.0e-4,
                        "glow case {index} ({label}) pixel {pixel_index} channel {channel}: cpu {cpu_pixel:?} gpu {gpu_pixel:?}"
                    );
                }
            }
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
