//! Zero-copy GPU texture display for composited preview frames.
//!
//! Wraps a composited wgpu texture in an `egui_wgpu::CallbackTrait`
//! so it can be rendered directly in egui's wgpu render pass,
//! bypassing the GPU→CPU→GPU roundtrip.

use std::sync::OnceLock;

// egui_wgpu re-exports wgpu as `egui_wgpu::wgpu`.
use egui_wgpu::wgpu;

// ── Per-frame bind group (stored in CallbackResources) ────────────────

struct TextureBindGroup {
    bind_group: wgpu::BindGroup,
    #[allow(dead_code)]
    texture_view: wgpu::TextureView,
}

// ── Shared pipeline resources ─────────────────────────────────────────

/// One-time-created render pipeline and sampler for the full-screen quad.
struct GpuTexturePipeline {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    bind_group_layout: wgpu::BindGroupLayout,
}

fn global_pipeline(device: &wgpu::Device) -> &'static GpuTexturePipeline {
    static PIPELINE: OnceLock<GpuTexturePipeline> = OnceLock::new();
    PIPELINE.get_or_init(|| {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gpu_texture_shader"),
            source: wgpu::ShaderSource::Wgsl(
                include_str!("../../../../mondrian-renderer/shaders/gpu_texture.wgsl").into(),
            ),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("gpu_texture_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("gpu_texture_pl"),
            immediate_size: 0,
            bind_group_layouts: &[Some(&bind_group_layout)],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("gpu_texture_pipeline"),
            layout: Some(&pipeline_layout),
            cache: None,
            multiview_mask: None,
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
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("gpu_texture_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        GpuTexturePipeline { pipeline, sampler, bind_group_layout }
    })
}

// ── Public API ─────────────────────────────────────────────────────────

/// A composited frame ready for zero-copy GPU display via egui callback.
///
/// Implements `egui_wgpu::CallbackTrait` — use with `egui::PaintCallback`
/// to render the composited texture directly in egui's render pass.
#[derive(Clone)]
pub struct CompositedFrame {
    texture: wgpu::Texture,
    pipeline: &'static GpuTexturePipeline,
    #[allow(dead_code)]
    width: u32,
    #[allow(dead_code)]
    height: u32,
}

impl CompositedFrame {
    /// Wrap an already-composited wgpu texture for zero-copy display.
    /// The shared pipeline is lazily initialized on first call.
    pub fn new(device: &wgpu::Device, texture: wgpu::Texture, width: u32, height: u32) -> Self {
        Self {
            texture,
            pipeline: global_pipeline(device),
            width,
            height,
        }
    }

    /// Size of the composited frame in pixels.
    pub fn size_pixels(&self) -> [u32; 2] {
        [self.width, self.height]
    }
}

impl egui_wgpu::CallbackTrait for CompositedFrame {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let texture_view = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("gpu_texture_bg"),
            layout: &self.pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.pipeline.sampler),
                },
            ],
        });
        callback_resources.insert(TextureBindGroup { bind_group, texture_view });
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(data) = callback_resources.get::<TextureBindGroup>() {
            render_pass.set_pipeline(&self.pipeline.pipeline);
            render_pass.set_bind_group(0, &data.bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}
