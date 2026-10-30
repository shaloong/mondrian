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
    lens_flare: wgpu::RenderPipeline,
    bulge: wgpu::RenderPipeline,
    split: wgpu::RenderPipeline,
    linear_color_key: wgpu::RenderPipeline,
    color_offset: wgpu::RenderPipeline,
    convolution: wgpu::RenderPipeline,
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
            lens_flare,
            bulge,
            split,
            linear_color_key,
            color_offset,
            convolution,
            find_edges,
            bindings,
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
        let passes = [
            mondrian_effects::EffectGpuSpatialPass::BoxBlur {
                radius: 1.5,
                iterations: 1,
                direction: BoxBlurDirection::HorizontalAndVertical,
                repeat_edge_pixels: true,
            },
            mondrian_effects::EffectGpuSpatialPass::Twirl { grade: twirl },
            mondrian_effects::EffectGpuSpatialPass::Spherize { grade: spherize },
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

        let executor = EffectSpatialGpu::new(&context.device);
        let original = upload(&context, &pixels, width, height);
        // Three rotating scratch textures keep every stage's destination out of
        // its own source, which wgpu rejects as conflicting texture usage.
        let pool: Vec<wgpu::Texture> =
            (0..3).map(|_| scratch(&context, width, height, 1).remove(0)).collect();
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
            let written = *available.last().expect("a write target");
            assert_eq!(
                output, pool[written],
                "the stage must write its last pool texture"
            );
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
