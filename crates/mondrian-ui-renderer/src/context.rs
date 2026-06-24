//! UI 渲染器 —— 将 DrawCommand 提交到 GPU
//!
//! [`UiRenderer`] 持有 wgpu 渲染管线 + glyph 纹理图集，每帧接收绘制命令。

use bytemuck::Pod;
use mondrian_core::Color;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

use crate::atlas::{TextureAtlas, TextureAtlasStats};
use crate::batch::build_batches;
use crate::command::{diagnose_draw_commands, raster_image_payload_len, DrawCommand};
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

/// Diagnostics produced while submitting one resolved UI frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiRenderFrameStats {
    /// Commands inspected before low-level rendering.
    pub command_count: usize,
    /// Deepest clip stack depth reached by the submitted command stream.
    pub max_clip_depth: u32,
    /// Deepest transform stack depth reached by the submitted command stream.
    pub max_transform_depth: u32,
    /// `PopClip` commands without a matching preceding `PushClip`.
    pub unmatched_clip_pops: u32,
    /// `PopTransform` commands without a matching preceding `PushTranslate`.
    pub unmatched_transform_pops: u32,
    /// Clip pushes still open at the end of the submitted stream.
    pub unclosed_clip_depth: u32,
    /// Transform pushes still open at the end of the submitted stream.
    pub unclosed_transform_depth: u32,
    /// Text commands that reached this low-level renderer entry point unresolved.
    pub unresolved_text_commands: u32,
    /// Clip commands with non-finite coordinates or non-positive size.
    pub invalid_clip_bounds: u32,
    /// Translate commands with non-finite offsets.
    pub invalid_translate_offsets: u32,
    /// Draw batches produced by command batching before render-pass filtering.
    pub batch_count: usize,
    /// Vertices produced by command batching before render-pass filtering.
    pub vertex_count: usize,
    /// Batches submitted to the GPU render pass.
    pub submitted_batches: usize,
    /// Vertices submitted to the GPU render pass.
    pub submitted_vertices: usize,
    /// Batches skipped because they contained no vertices.
    pub skipped_empty_batches: usize,
    /// Batches skipped because their clip/scissor rectangle was empty or invalid.
    pub skipped_scissor_batches: usize,
    /// The frame uploaded new raster images into the renderer-owned image atlas.
    pub uploaded_raster_images: bool,
    /// Raster images that could not be uploaded or allocated in the image atlas.
    ///
    /// Failed images are replaced with a low-alpha diagnostic rectangle instead
    /// of disappearing silently, so the app can surface resource pressure while
    /// the frame remains visibly debuggable.
    pub failed_raster_images: u32,
    /// Entries currently cached in the renderer-owned raster image atlas.
    pub image_atlas_entries: usize,
    /// Pixels currently occupied by raster image atlas allocations, including padding.
    pub image_atlas_used_pixels: u64,
    /// Total pixels available in the renderer-owned raster image atlas.
    pub image_atlas_total_pixels: u64,
    /// Area of the largest currently reusable free rectangle in the raster image atlas.
    pub image_atlas_largest_free_rect_pixels: u64,
    /// Allocation requests rejected by the raster image atlas since renderer creation.
    pub image_atlas_failed_allocations: u64,
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
    if !clip.x.is_finite()
        || !clip.y.is_finite()
        || !clip.width.is_finite()
        || !clip.height.is_finite()
        || clip.width <= 0.0
        || clip.height <= 0.0
    {
        return None;
    }

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

fn rgba_with_edge_padding(
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

    let dst_row_bytes = padded_width as usize * 4;
    for dst_y in 0..padded_height as usize {
        let src_y = dst_y.saturating_sub(pad as usize).min(height as usize - 1);
        for dst_x in 0..padded_width as usize {
            let src_x = dst_x.saturating_sub(pad as usize).min(width as usize - 1);
            let src_start = (src_y * width as usize + src_x) * 4;
            let dst_start = dst_y * dst_row_bytes + dst_x * 4;
            padded[dst_start..dst_start + 4].copy_from_slice(&rgba[src_start..src_start + 4]);
        }
    }

    Some((padded_width, padded_height, padded))
}

fn raster_image_failure_fallback(
    bounds: mondrian_ui_core::types::Rect,
    tint: Color,
) -> DrawCommand {
    let mut color = tint;
    color.a = (color.a * 0.18).clamp(0.08, 0.24);
    let radius = bounds.width.min(bounds.height).min(8.0) * 0.25;
    DrawCommand::Rect { bounds, color, corner_radius: radius }
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
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
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

    /// Return diagnostics for the renderer-owned raster image atlas.
    pub fn image_atlas_stats(&self) -> TextureAtlasStats {
        self.image_atlas.stats()
    }

    fn resolve_raster_images(
        &mut self,
        queue: &wgpu::Queue,
        commands: &[DrawCommand],
    ) -> (Vec<DrawCommand>, UiRenderFrameStats) {
        let mut stats = UiRenderFrameStats::default();
        let mut resolved = Vec::with_capacity(commands.len());

        for command in commands {
            match command {
                DrawCommand::RasterImage { key, bounds, width, height, rgba, tint } => {
                    if let Some((uv_rect, uploaded)) =
                        self.resolve_raster_image(queue, key, *width, *height, rgba)
                    {
                        stats.uploaded_raster_images |= uploaded;
                        resolved.push(DrawCommand::RasterAtlasImage {
                            bounds: *bounds,
                            uv_rect,
                            tint: *tint,
                        });
                    } else {
                        stats.failed_raster_images = stats.failed_raster_images.saturating_add(1);
                        resolved.push(raster_image_failure_fallback(*bounds, *tint));
                    }
                }
                other => resolved.push(other.clone()),
            }
        }

        (resolved, stats)
    }

    fn resolve_raster_image(
        &mut self,
        queue: &wgpu::Queue,
        key: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Option<(mondrian_ui_core::types::Rect, bool)> {
        let expected_len = raster_image_payload_len(width, height)?;
        if width == 0 || height == 0 || rgba.len() != expected_len {
            return None;
        }

        let cache_key = format!("{key}@{width}x{height}");
        if let Some(entry) = self.image_cache.get(&cache_key) {
            return Some((entry.uv_rect, false));
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
            rgba_with_edge_padding(rgba, width, height, IMAGE_ATLAS_PAD)?;

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
        Some((uv_rect, true))
    }

    /// Render draw commands that have already had text commands resolved to
    /// glyph atlas image draws.
    ///
    /// Product and widget windows should normally call their app-level frame
    /// renderer, which runs `mondrian-ui-text::resolve_text_commands` and
    /// uploads glyphs before calling this low-level submission path.
    pub fn render_resolved_commands(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        commands: &[DrawCommand],
        screen_size: (u32, u32),
    ) -> UiRenderFrameStats {
        let command_diagnostics = diagnose_draw_commands(commands);
        debug_assert_eq!(
            command_diagnostics.unresolved_text_commands, 0,
            "UiRenderer::render_resolved_commands received unresolved text commands"
        );
        let (commands, mut stats) = self.resolve_raster_images(queue, commands);
        stats.command_count = command_diagnostics.command_count;
        stats.max_clip_depth = command_diagnostics.max_clip_depth;
        stats.max_transform_depth = command_diagnostics.max_transform_depth;
        stats.unmatched_clip_pops = command_diagnostics.unmatched_clip_pops;
        stats.unmatched_transform_pops = command_diagnostics.unmatched_transform_pops;
        stats.unclosed_clip_depth = command_diagnostics.unclosed_clip_depth;
        stats.unclosed_transform_depth = command_diagnostics.unclosed_transform_depth;
        stats.unresolved_text_commands = command_diagnostics.unresolved_text_commands;
        stats.invalid_clip_bounds = command_diagnostics.invalid_clip_bounds;
        stats.invalid_translate_offsets = command_diagnostics.invalid_translate_offsets;
        let image_atlas_stats = self.image_atlas.stats();
        stats.image_atlas_entries = image_atlas_stats.entries;
        stats.image_atlas_used_pixels = image_atlas_stats.used_pixels;
        stats.image_atlas_total_pixels = image_atlas_stats.total_pixels;
        stats.image_atlas_largest_free_rect_pixels = image_atlas_stats.largest_free_rect_pixels;
        stats.image_atlas_failed_allocations = image_atlas_stats.failed_allocations;
        let batches = build_batches(&commands, screen_size);
        stats.batch_count = batches.len();
        stats.vertex_count = batches.iter().map(|batch| batch.vertices.len()).sum();
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
                    stats.skipped_empty_batches = stats.skipped_empty_batches.saturating_add(1);
                    continue;
                }
                let Some((x, y, width, height)) =
                    scissor_rect_for_clip(batch.clip_rect, screen_size)
                else {
                    stats.skipped_scissor_batches = stats.skipped_scissor_batches.saturating_add(1);
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
                stats.submitted_batches = stats.submitted_batches.saturating_add(1);
                stats.submitted_vertices =
                    stats.submitted_vertices.saturating_add(vertex_data.len());
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::DrawEncoder;
    use mondrian_ui_core::types::{Point, Rect};

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
    fn scissor_rejects_invalid_clip_bounds() {
        for clip in [
            Rect::new(f32::NAN, 10.0, 20.0, 20.0),
            Rect::new(10.0, f32::INFINITY, 20.0, 20.0),
            Rect::new(10.0, 10.0, f32::NAN, 20.0),
            Rect::new(10.0, 10.0, 20.0, f32::INFINITY),
            Rect::new(10.0, 10.0, 0.0, 20.0),
            Rect::new(10.0, 10.0, 20.0, -1.0),
        ] {
            assert_eq!(scissor_rect_for_clip(Some(clip), (100, 50)), None);
        }
    }

    #[test]
    fn rgba_padding_dilates_edge_pixels_and_preserves_inner_pixels() {
        let source = vec![
            10, 11, 12, 13, //
            20, 21, 22, 23, //
            30, 31, 32, 33, //
            40, 41, 42, 43,
        ];
        let (width, height, padded) = rgba_with_edge_padding(&source, 2, 2, 1).unwrap();

        assert_eq!((width, height), (4, 4));
        assert_eq!(padded.len(), 4 * 4 * 4);

        assert_eq!(&padded[0..4], &source[0..4]);
        assert_eq!(&padded[3 * 4..4 * 4], &source[4..8]);
        assert_eq!(&padded[3 * 4 * 4..3 * 4 * 4 + 4], &source[8..12]);
        assert_eq!(&padded[3 * 4 * 4 + 3 * 4..4 * 4 * 4], &source[12..16]);

        let row_stride = 4 * 4;
        assert_eq!(&padded[row_stride + 4..row_stride + 12], &source[0..8]);
        assert_eq!(
            &padded[row_stride * 2 + 4..row_stride * 2 + 12],
            &source[8..16]
        );
    }

    #[test]
    fn rgba_padding_rejects_mismatched_payload() {
        assert!(rgba_with_edge_padding(&[1, 2, 3], 1, 1, 1).is_none());
    }

    #[test]
    fn raster_image_payload_len_rejects_overflow() {
        assert_eq!(raster_image_payload_len(2, 3), Some(24));
        assert_eq!(raster_image_payload_len(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn raster_image_failure_fallback_is_visible_but_low_alpha() {
        let fallback = raster_image_failure_fallback(
            Rect::new(10.0, 20.0, 30.0, 40.0),
            Color { r: 0.4, g: 0.5, b: 0.6, a: 1.0 },
        );

        match fallback {
            DrawCommand::Rect { bounds, color, corner_radius } => {
                assert_eq!(bounds, Rect::new(10.0, 20.0, 30.0, 40.0));
                assert_eq!((color.r, color.g, color.b), (0.4, 0.5, 0.6));
                assert!((0.08..=0.24).contains(&color.a));
                assert!(corner_radius > 0.0);
            }
            other => panic!("expected rect fallback, got {other:?}"),
        }
    }

    #[test]
    fn offscreen_renderer_draws_opaque_rect_with_readback() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_rect(
            Rect::new(16.0, 16.0, 32.0, 32.0),
            Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
            0.0,
        );

        let pixels = harness.render(encoder.finish());
        let center = pixel(&pixels, 64, 32, 32);

        assert!(
            center[0] >= 240,
            "red channel should be saturated, got {center:?}"
        );
        assert!(
            center[1] <= 8,
            "green channel should remain near zero, got {center:?}"
        );
        assert!(
            center[2] <= 8,
            "blue channel should remain near zero, got {center:?}"
        );
        assert!(center[3] >= 240, "alpha should be opaque, got {center:?}");

        let stats = harness.last_stats.expect("render should record stats");
        assert_eq!(stats.batch_count, 1);
        assert_eq!(stats.vertex_count, 6);
        assert_eq!(stats.submitted_batches, 1);
        assert_eq!(stats.submitted_vertices, 6);
        assert_eq!(stats.skipped_empty_batches, 0);
        assert_eq!(stats.skipped_scissor_batches, 0);
    }

    #[test]
    fn offscreen_renderer_keeps_45_degree_hairline_visible() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_line(
            mondrian_ui_core::types::Point::new(8.0, 8.0),
            mondrian_ui_core::types::Point::new(56.0, 56.0),
            1.0,
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        for i in 10..=54 {
            let alpha = max_alpha_in_square(&pixels, 64, i, i, 1);
            assert!(
                alpha >= 32,
                "45-degree hairline lost visible coverage around ({i},{i}); max alpha={alpha}"
            );
        }
    }

    #[test]
    fn offscreen_renderer_draws_square_rounded_rect_as_circle() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_rect(Rect::new(16.0, 16.0, 32.0, 32.0), Color::WHITE, 16.0);

        let pixels = harness.render(encoder.finish());

        assert!(
            pixel(&pixels, 64, 32, 32)[3] >= 240,
            "circle center should be opaque"
        );
        for (x, y) in [(16, 16), (47, 16), (16, 47), (47, 47)] {
            let alpha = pixel(&pixels, 64, x, y)[3];
            assert!(
                alpha <= 24,
                "circle corner ({x},{y}) should stay transparent, got {alpha}"
            );
        }

        let top = max_alpha_in_square(&pixels, 64, 32, 16, 1);
        let right = max_alpha_in_square(&pixels, 64, 47, 32, 1);
        let bottom = max_alpha_in_square(&pixels, 64, 32, 47, 1);
        let left = max_alpha_in_square(&pixels, 64, 16, 32, 1);
        for (label, alpha) in [
            ("top", top),
            ("right", right),
            ("bottom", bottom),
            ("left", left),
        ] {
            assert!(
                alpha >= 80,
                "circle {label} edge should have visible AA coverage, got {alpha}"
            );
        }
    }

    #[test]
    fn offscreen_renderer_applies_clip_scissor_to_rects() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.push_clip(Rect::new(24.0, 24.0, 16.0, 16.0));
        encoder.draw_rect(
            Rect::new(0.0, 0.0, 64.0, 64.0),
            Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
            0.0,
        );
        encoder.pop_clip();

        let pixels = harness.render(encoder.finish());

        assert!(
            pixel(&pixels, 64, 32, 32)[3] >= 240,
            "clip interior should render"
        );
        for (x, y) in [(20, 32), (43, 32), (32, 20), (32, 43)] {
            let outside = pixel(&pixels, 64, x, y);
            assert_eq!(
                outside[3], 0,
                "clip should reject ({x},{y}), got {outside:?}"
            );
        }
    }

    #[test]
    fn offscreen_renderer_reports_scissor_skipped_batches() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.push_clip(Rect::new(96.0, 96.0, 16.0, 16.0));
        encoder.draw_rect(Rect::new(0.0, 0.0, 64.0, 64.0), Color::WHITE, 0.0);
        encoder.pop_clip();

        let pixels = harness.render(encoder.finish());

        assert!(
            pixels.chunks_exact(4).all(|pixel| pixel[3] == 0),
            "fully clipped frame should remain transparent"
        );
        let stats = harness.last_stats.expect("render should record stats");
        assert_eq!(stats.batch_count, 1);
        assert_eq!(stats.vertex_count, 6);
        assert_eq!(stats.submitted_batches, 0);
        assert_eq!(stats.submitted_vertices, 0);
        assert_eq!(stats.skipped_scissor_batches, 1);
    }

    #[test]
    fn offscreen_renderer_preserves_gradient_corner_direction() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_gradient_rect(
            Rect::new(8.0, 8.0, 48.0, 48.0),
            [
                Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
                Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
                Color { r: 0.0, g: 0.0, b: 1.0, a: 1.0 },
                Color::WHITE,
            ],
            0.0,
        );

        let pixels = harness.render(encoder.finish());
        assert_dominant_channel(pixel(&pixels, 64, 10, 10), 0, "top-left");
        assert_dominant_channel(pixel(&pixels, 64, 53, 10), 1, "top-right");
        assert_dominant_channel(pixel(&pixels, 64, 10, 53), 2, "bottom-left");

        let bottom_right = pixel(&pixels, 64, 53, 53);
        assert!(
            bottom_right[0] >= 180
                && bottom_right[1] >= 180
                && bottom_right[2] >= 180
                && bottom_right[3] >= 240,
            "bottom-right should remain close to white, got {bottom_right:?}"
        );
    }

    #[test]
    fn offscreen_renderer_draws_filled_triangles_with_stable_winding() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_triangles(
            &[
                Point::new(8.0, 8.0),
                Point::new(56.0, 8.0),
                Point::new(8.0, 56.0),
            ],
            Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
        );

        let pixels = harness.render(encoder.finish());

        let inside = pixel(&pixels, 64, 20, 20);
        assert!(
            inside[1] >= 240 && inside[3] >= 240,
            "triangle interior got {inside:?}"
        );

        let outside = pixel(&pixels, 64, 54, 54);
        assert_eq!(
            outside[3], 0,
            "triangle outside should stay transparent, got {outside:?}"
        );
    }

    #[test]
    fn offscreen_renderer_uploads_and_samples_raster_images() {
        let Some(mut harness) = OffscreenHarness::new(32, 32) else {
            return;
        };
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for y in 0..4 {
            for x in 0..4 {
                let index = (y * 4 + x) * 4;
                rgba[index] = if x < 2 { 255 } else { 0 };
                rgba[index + 1] = if x >= 2 { 255 } else { 0 };
                rgba[index + 2] = if y >= 2 { 255 } else { 0 };
                rgba[index + 3] = 255;
            }
        }

        let mut encoder = DrawEncoder::new();
        encoder.draw_raster_image(
            "test.raster.quadrants",
            Rect::new(8.0, 8.0, 16.0, 16.0),
            4,
            4,
            std::sync::Arc::from(rgba),
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        let top_left = pixel(&pixels, 32, 10, 10);
        let top_right = pixel(&pixels, 32, 21, 10);
        let bottom_left = pixel(&pixels, 32, 10, 21);
        let bottom_right = pixel(&pixels, 32, 21, 21);

        assert!(
            top_left[0] > top_left[1].saturating_add(80) && top_left[3] >= 240,
            "top-left raster sample should be red, got {top_left:?}"
        );
        assert!(
            top_right[1] > top_right[0].saturating_add(80) && top_right[3] >= 240,
            "top-right raster sample should be green, got {top_right:?}"
        );
        assert!(
            bottom_left[0] >= 180 && bottom_left[2] >= 180 && bottom_left[3] >= 240,
            "bottom-left raster sample should be magenta-ish, got {bottom_left:?}"
        );
        assert!(
            bottom_right[1] >= 180 && bottom_right[2] >= 180 && bottom_right[3] >= 240,
            "bottom-right raster sample should be cyan-ish, got {bottom_right:?}"
        );

        let stats = harness.last_stats.expect("render should record stats");
        assert!(stats.uploaded_raster_images);
        assert_eq!(stats.failed_raster_images, 0);
        assert!(stats.image_atlas_entries >= 1);
    }

    struct OffscreenHarness {
        device: wgpu::Device,
        queue: wgpu::Queue,
        renderer: UiRenderer,
        texture: wgpu::Texture,
        size: (u32, u32),
        last_stats: Option<UiRenderFrameStats>,
    }

    impl OffscreenHarness {
        fn new(width: u32, height: u32) -> Option<Self> {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: None,
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                }))
                .ok()?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                    .ok()?;
            let format = wgpu::TextureFormat::Rgba8Unorm;
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui_offscreen_test_target"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let renderer = UiRenderer::new(&device, format);

            Some(Self {
                device,
                queue,
                renderer,
                texture,
                size: (width, height),
                last_stats: None,
            })
        }

        fn render(&mut self, commands: Vec<DrawCommand>) -> Vec<u8> {
            let view = self.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let stats = self.renderer.render_resolved_commands(
                &self.device,
                &self.queue,
                &view,
                &commands,
                self.size,
            );
            self.last_stats = Some(stats);
            self.readback()
        }

        fn readback(&self) -> Vec<u8> {
            let (width, height) = self.size;
            let bytes_per_pixel = 4u32;
            let unpadded_bytes_per_row = width * bytes_per_pixel;
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;
            let buffer_size = padded_bytes_per_row as u64 * height as u64;

            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui_offscreen_test_readback"),
                size: buffer_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ui_offscreen_test_readback_encoder"),
            });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded_bytes_per_row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            self.queue.submit([encoder.finish()]);

            let (tx, rx) = std::sync::mpsc::channel();
            let slice = readback.slice(..);
            slice.map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
            let _ =
                self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None });
            rx.recv()
                .expect("readback map callback should run")
                .expect("readback map should succeed");

            let mapped = slice.get_mapped_range();
            let mut out = vec![0u8; width as usize * height as usize * 4];
            for row in 0..height as usize {
                let src_start = row * padded_bytes_per_row as usize;
                let src_end = src_start + unpadded_bytes_per_row as usize;
                let dst_start = row * unpadded_bytes_per_row as usize;
                let dst_end = dst_start + unpadded_bytes_per_row as usize;
                out[dst_start..dst_end].copy_from_slice(&mapped[src_start..src_end]);
            }
            drop(mapped);
            readback.unmap();
            out
        }
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let index = ((y * width + x) * 4) as usize;
        [
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ]
    }

    fn max_alpha_in_square(pixels: &[u8], width: u32, x: u32, y: u32, radius: u32) -> u8 {
        let height = pixels.len() as u32 / (width * 4);
        let left = x.saturating_sub(radius);
        let right = (x + radius).min(width - 1);
        let top = y.saturating_sub(radius);
        let bottom = (y + radius).min(height - 1);
        let mut max_alpha = 0u8;
        for sample_y in top..=bottom {
            for sample_x in left..=right {
                max_alpha = max_alpha.max(pixel(pixels, width, sample_x, sample_y)[3]);
            }
        }
        max_alpha
    }

    fn assert_dominant_channel(pixel: [u8; 4], channel: usize, label: &str) {
        assert!(
            pixel[3] >= 240,
            "{label} alpha should be opaque, got {pixel:?}"
        );
        for candidate in 0..3 {
            if candidate == channel {
                assert!(
                    pixel[candidate] >= 150,
                    "{label} expected channel {channel} to dominate, got {pixel:?}"
                );
            } else {
                assert!(
                    pixel[channel] > pixel[candidate].saturating_add(48),
                    "{label} expected channel {channel} to dominate channel {candidate}, got {pixel:?}"
                );
            }
        }
    }
}
