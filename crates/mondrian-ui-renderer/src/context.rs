//! UI 渲染器 —— 将 DrawCommand 提交到 GPU
//!
//! [`UiRenderer`] 持有 wgpu 渲染管线 + glyph 纹理图集，每帧接收绘制命令。

use bytemuck::Pod;
use wgpu::util::DeviceExt;

use crate::batch::build_batches;
use crate::command::DrawCommand;
use crate::pipeline::UiPipeline;
use crate::shape::RectVertex;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen_size: [f32; 2],
    aa_floor: f32,
    _pad: f32,
}

/// 字形上传数据
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// GPU 2D UI 渲染器
pub struct UiRenderer {
    pipeline: UiPipeline,
    glyph_texture: wgpu::Texture,
    glyph_bind_group: wgpu::BindGroup,
}

impl UiRenderer {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let pipeline = UiPipeline::new(device, surface_format);

        let atlas_size: u32 = 2048;
        let glyph_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph_atlas"),
            size: wgpu::Extent3d {
                width: atlas_size,
                height: atlas_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let glyph_view = glyph_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let glyph_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("glyph_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let glyph_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyph_bg"),
            layout: &pipeline.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&glyph_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&glyph_view),
                },
            ],
        });

        Self { pipeline, glyph_texture, glyph_bind_group }
    }

    /// 上传字形 bitmap 到 GPU 图集纹理（alpha→Rgba8 格式转换）
    pub fn upload_glyphs(&self, queue: &wgpu::Queue, uploads: &[GlyphUpload]) {
        for upload in uploads {
            if upload.width == 0 || upload.height == 0 {
                continue;
            }
            let pixel_count = (upload.width * upload.height) as usize;
            if upload.data.len() != pixel_count {
                continue;
            }
            // Convert alpha-only to RGBA: each pixel becomes [255, 255, 255, alpha]
            let rgba: Vec<u8> =
                upload.data.iter().flat_map(|&a| vec![255u8, 255, 255, a]).collect();
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.glyph_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: upload.x, y: upload.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.width * 4),
                    rows_per_image: Some(upload.height),
                },
                wgpu::Extent3d {
                    width: upload.width,
                    height: upload.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    pub fn render(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        commands: &[DrawCommand],
        screen_size: (u32, u32),
    ) {
        let batches = build_batches(commands, screen_size);

                let ref_height = 1080.0_f32;
        let aa_floor = (ref_height / (screen_size.1.max(1) as f32)).clamp(0.5, 1.5);
        let uniform_data = Uniforms {
            screen_size: [screen_size.0 as f32, screen_size.1 as f32],
            aa_floor,
            _pad: 0.0,
        };
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ui_uniform"),
            contents: bytemuck::bytes_of(&uniform_data),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_bg"),
            layout: &self.pipeline.bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let mut encoder = device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ui_encoder") });

        {
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            rpass.set_pipeline(&self.pipeline.render_pipeline);
            rpass.set_bind_group(0, &bind_group, &[]);
            rpass.set_bind_group(1, &self.glyph_bind_group, &[]);

            for batch in &batches {
                if batch.vertices.is_empty() {
                    continue;
                }
                let vertex_data: &[RectVertex] = &batch.vertices;
                let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ui_vb"),
                    contents: bytemuck::cast_slice(vertex_data),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                rpass.set_vertex_buffer(0, vertex_buffer.slice(..));
                rpass.draw(0..vertex_data.len() as u32, 0..1);
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
    }
}
