//! UI 渲染器 —— 将 DrawCommand 提交到 GPU
//!
//! [`UiRenderer`] 持有 wgpu 渲染管线 + glyph 纹理图集，每帧接收绘制命令。

use bytemuck::Pod;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

use crate::atlas::TextureAtlas;
use crate::batch::build_batches;
use crate::command::DrawCommand;
use crate::pipeline::UiPipeline;
use crate::shape::RectVertex;

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, bytemuck::Zeroable)]
struct Uniforms {
    screen_size: [f32; 2],
    _pad: [f32; 2],
}

const UI_SAMPLE_COUNT: u32 = 4;
const ATLAS_SIZE: u32 = 2048;
const IMAGE_ATLAS_PAD: u32 = 1;

struct MsaaTarget {
    size: (u32, u32),
    view: wgpu::TextureView,
}

#[derive(Debug, Clone, Copy)]
struct ImageCacheEntry {
    uv_rect: mondrian_ui_core::types::Rect,
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
    image_texture: wgpu::Texture,
    image_bind_group: wgpu::BindGroup,
    image_atlas: TextureAtlas,
    image_cache: HashMap<String, ImageCacheEntry>,
    surface_format: wgpu::TextureFormat,
    msaa_target: Option<MsaaTarget>,
}

fn scissor_rect_for_clip(
    clip_rect: Option<mondrian_ui_core::types::Rect>,
    screen_size: (u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let (screen_w, screen_h) = screen_size;
    if screen_w == 0 || screen_h == 0 {
        return None;
    }

    let Some(clip) = clip_rect else {
        return Some((0, 0, screen_w, screen_h));
    };

    let max_x = screen_w as f32;
    let max_y = screen_h as f32;
    let left = clip.x.floor().clamp(0.0, max_x) as u32;
    let top = clip.y.floor().clamp(0.0, max_y) as u32;
    let right = (clip.x + clip.width).ceil().clamp(0.0, max_x) as u32;
    let bottom = (clip.y + clip.height).ceil().clamp(0.0, max_y) as u32;

    let width = right.saturating_sub(left);
    let height = bottom.saturating_sub(top);
    if width == 0 || height == 0 {
        None
    } else {
        Some((left, top, width, height))
    }
}

fn rgba_with_transparent_padding(
    rgba: &[u8],
    width: u32,
    height: u32,
    pad: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let pixel_count = width.checked_mul(height)?;
    let expected_len = pixel_count.checked_mul(4)? as usize;
    if rgba.len() != expected_len {
        return None;
    }

    let pad_twice = pad.checked_mul(2)?;
    let padded_width = width.checked_add(pad_twice)?;
    let padded_height = height.checked_add(pad_twice)?;
    let padded_len = padded_width.checked_mul(padded_height)?.checked_mul(4)? as usize;
    let mut padded = vec![0; padded_len];

    let src_row_bytes = width as usize * 4;
    let dst_row_bytes = padded_width as usize * 4;
    let pad_bytes = pad as usize * 4;
    for row in 0..height as usize {
        let src_start = row * src_row_bytes;
        let dst_start = (row + pad as usize) * dst_row_bytes + pad_bytes;
        padded[dst_start..dst_start + src_row_bytes]
            .copy_from_slice(&rgba[src_start..src_start + src_row_bytes]);
    }

    Some((padded_width, padded_height, padded))
}

impl UiRenderer {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let pipeline = UiPipeline::new(device, surface_format, UI_SAMPLE_COUNT);

        let glyph_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph_atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
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
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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

        let image_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui_image_atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let image_view = image_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui_image_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let image_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_image_bg"),
            layout: &pipeline.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&image_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&image_view),
                },
            ],
        });

        Self {
            pipeline,
            glyph_texture,
            glyph_bind_group,
            image_texture,
            image_bind_group,
            image_atlas: TextureAtlas::new(ATLAS_SIZE, ATLAS_SIZE),
            image_cache: HashMap::new(),
            surface_format,
            msaa_target: None,
        }
    }

    fn ensure_msaa_target(&mut self, device: &wgpu::Device, screen_size: (u32, u32)) {
        if UI_SAMPLE_COUNT <= 1 || screen_size.0 == 0 || screen_size.1 == 0 {
            self.msaa_target = None;
            return;
        }

        let needs_recreate = match self.msaa_target.as_ref() {
            Some(target) => target.size != screen_size,
            None => true,
        };
        if needs_recreate {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui_msaa_target"),
                size: wgpu::Extent3d {
                    width: screen_size.0,
                    height: screen_size.1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: UI_SAMPLE_COUNT,
                dimension: wgpu::TextureDimension::D2,
                format: self.surface_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.msaa_target = Some(MsaaTarget {
                size: screen_size,
                view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            });
        }
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
            // Convert alpha-only to RGBA: each pixel becomes [255, 255, 255, alpha].
            let mut rgba = Vec::with_capacity(pixel_count * 4);
            for &alpha in &upload.data {
                rgba.extend_from_slice(&[255, 255, 255, alpha]);
            }
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

    fn resolve_raster_images(
        &mut self,
        queue: &wgpu::Queue,
        commands: &[DrawCommand],
    ) -> Vec<DrawCommand> {
        commands
            .iter()
            .filter_map(|command| match command {
                DrawCommand::RasterImage { key, bounds, width, height, rgba, tint } => {
                    self.resolve_raster_image(queue, key, *width, *height, rgba).map(|uv_rect| {
                        DrawCommand::RasterAtlasImage { bounds: *bounds, uv_rect, tint: *tint }
                    })
                }
                other => Some(other.clone()),
            })
            .collect()
    }

    fn resolve_raster_image(
        &mut self,
        queue: &wgpu::Queue,
        key: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<mondrian_ui_core::types::Rect> {
        let expected_len = width as usize * height as usize * 4;
        if width == 0 || height == 0 || rgba.len() != expected_len {
            return None;
        }

        let cache_key = format!("{key}@{width}x{height}");
        if let Some(entry) = self.image_cache.get(&cache_key) {
            return Some(entry.uv_rect);
        }

        let pad_twice = IMAGE_ATLAS_PAD.checked_mul(2)?;
        let alloc_w = width.checked_add(pad_twice)?;
        let alloc_h = height.checked_add(pad_twice)?;
        let allocated = self.image_atlas.allocate_pixels(&cache_key, alloc_w, alloc_h)?;
        let px = allocated.x + IMAGE_ATLAS_PAD;
        let py = allocated.y + IMAGE_ATLAS_PAD;
        let uv_rect = mondrian_ui_core::types::Rect::new(
            px as f32 / ATLAS_SIZE as f32,
            py as f32 / ATLAS_SIZE as f32,
            width as f32 / ATLAS_SIZE as f32,
            height as f32 / ATLAS_SIZE as f32,
        );
        let (upload_width, upload_height, padded_rgba) =
            rgba_with_transparent_padding(rgba, width, height, IMAGE_ATLAS_PAD)?;

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.image_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: allocated.x, y: allocated.y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &padded_rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(upload_width * 4),
                rows_per_image: Some(upload_height),
            },
            wgpu::Extent3d {
                width: upload_width,
                height: upload_height,
                depth_or_array_layers: 1,
            },
        );

        self.image_cache.insert(cache_key, ImageCacheEntry { uv_rect });
        Some(uv_rect)
    }

    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        commands: &[DrawCommand],
        screen_size: (u32, u32),
    ) {
        let commands = self.resolve_raster_images(queue, commands);
        let batches = build_batches(&commands, screen_size);
        self.ensure_msaa_target(device, screen_size);
        let msaa_view = self.msaa_target.as_ref().map(|target| &target.view);

        let uniform_data = Uniforms {
            screen_size: [screen_size.0 as f32, screen_size.1 as f32],
            _pad: [0.0; 2],
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
            let (attachment_view, resolve_target) = if let Some(msaa_view) = msaa_view {
                (msaa_view, Some(view))
            } else {
                (view, None)
            };
            let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: attachment_view,
                    resolve_target,
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

            for batch in &batches {
                if batch.vertices.is_empty() {
                    continue;
                }
                let Some((x, y, width, height)) =
                    scissor_rect_for_clip(batch.clip_rect, screen_size)
                else {
                    continue;
                };
                rpass.set_scissor_rect(x, y, width, height);
                let texture_bind_group = if batch.texture_key.as_deref() == Some("image") {
                    &self.image_bind_group
                } else {
                    &self.glyph_bind_group
                };
                rpass.set_bind_group(1, texture_bind_group, &[]);

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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_core::types::Rect;

    #[test]
    fn scissor_none_uses_full_screen() {
        assert_eq!(
            scissor_rect_for_clip(None, (1920, 1080)),
            Some((0, 0, 1920, 1080))
        );
    }

    #[test]
    fn scissor_clamps_to_screen_and_uses_conservative_bounds() {
        assert_eq!(
            scissor_rect_for_clip(Some(Rect::new(-2.4, 10.2, 20.3, 8.1)), (100, 50)),
            Some((0, 10, 18, 9))
        );
    }

    #[test]
    fn scissor_empty_clip_skips_batch() {
        assert_eq!(
            scissor_rect_for_clip(Some(Rect::new(120.0, 10.0, 20.0, 20.0)), (100, 50)),
            None
        );
    }

    #[test]
    fn rgba_padding_adds_transparent_border_and_preserves_inner_pixels() {
        let source = vec![
            10, 11, 12, 13, //
            20, 21, 22, 23, //
            30, 31, 32, 33, //
            40, 41, 42, 43,
        ];
        let (width, height, padded) = rgba_with_transparent_padding(&source, 2, 2, 1).unwrap();

        assert_eq!((width, height), (4, 4));
        assert_eq!(padded.len(), 4 * 4 * 4);

        let transparent = [0, 0, 0, 0];
        for x in 0..4 {
            let top = x * 4;
            let bottom = (3 * 4 + x) * 4;
            assert_eq!(&padded[top..top + 4], &transparent);
            assert_eq!(&padded[bottom..bottom + 4], &transparent);
        }
        for y in 0..4 {
            let left = y * 4 * 4;
            let right = left + 3 * 4;
            assert_eq!(&padded[left..left + 4], &transparent);
            assert_eq!(&padded[right..right + 4], &transparent);
        }

        let row_stride = 4 * 4;
        assert_eq!(&padded[row_stride + 4..row_stride + 12], &source[0..8]);
        assert_eq!(
            &padded[row_stride * 2 + 4..row_stride * 2 + 12],
            &source[8..16]
        );
    }

    #[test]
    fn rgba_padding_rejects_mismatched_payload() {
        assert!(rgba_with_transparent_padding(&[1, 2, 3], 1, 1, 1).is_none());
    }
}
