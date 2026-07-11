//! UI 渲染器 —— 将 DrawCommand 提交到 GPU
//!
//! [`UiRenderer`] 持有 wgpu 渲染管线 + glyph 纹理图集，每帧接收绘制命令。

use bytemuck::Pod;
use mondrian_core::Color;
use mondrian_ui_core::types::RasterImageColorSpace;
use std::collections::HashMap;
use std::time::Instant;
use wgpu::util::DeviceExt;

use crate::atlas::{TextureAtlas, TextureAtlasStats};
use crate::batch::{build_batches, EXTERNAL_TEXTURE_KEY_PREFIX, IMAGE_TEXTURE_KEY};
use crate::command::{
    diagnose_draw_commands, raster_image_payload_len, DrawCommand, ExternalTextureKey,
};
use crate::pipeline::UiPipeline;
use crate::shape::RectVertex;
use crate::CornerRadii;

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

/// Renderer-owned binding for a GPU texture registered outside the image atlas.
pub struct ExternalTextureRegistration {
    bind_group: wgpu::BindGroup,
    transfer: ExternalTextureTransfer,
}

/// Transfer contract applied while compositing an external GPU texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTextureTransfer {
    /// Texture samples already represent linear attachment values.
    Linear,
    /// Preserve opaque encoded/device code values through an sRGB attachment.
    SrgbSurfaceCodeValuesOpaque,
}

/// External texture registration failed before any renderer state changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTextureRegistrationError {
    /// Code-value preservation requires an sRGB attachment OETF.
    CodeValuesRequireSrgbSurface { surface_format: wgpu::TextureFormat },
}

impl std::fmt::Display for ExternalTextureRegistrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CodeValuesRequireSrgbSurface { surface_format } => write!(
                formatter,
                "encoded code-value preservation requires an sRGB surface, got {surface_format:?}"
            ),
        }
    }
}

impl std::error::Error for ExternalTextureRegistrationError {}

#[derive(Debug, Clone, Copy, PartialEq)]
enum RasterImageResolve {
    Resolved {
        uv_rect: mondrian_ui_core::types::Rect,
        uploaded: bool,
        upload_bytes: u64,
    },
    Failed,
    UnsupportedColorSpace,
    PageReset,
}

/// 字形上传数据
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Diagnostics produced while uploading glyph atlas data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GlyphUploadStats {
    /// Glyph uploads submitted to the GPU queue.
    pub submitted_glyphs: u32,
    /// Glyph uploads skipped because dimensions or payload length were invalid.
    pub skipped_glyphs: u32,
    /// Bytes written to the GPU glyph atlas after alpha-to-RGBA expansion.
    pub upload_bytes: u64,
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
    /// Bytes written to the renderer-owned raster image atlas this frame.
    pub raster_image_upload_bytes: u64,
    /// Bytes written to per-frame uniform buffers.
    pub uniform_upload_bytes: u64,
    /// Bytes written to per-batch vertex buffers.
    pub vertex_buffer_upload_bytes: u64,
    /// Total bytes uploaded by this renderer entry point.
    ///
    /// This excludes glyph uploads, because glyphs are uploaded through
    /// [`UiRenderer::upload_glyphs`] before rendering and have their own stats.
    pub gpu_upload_bytes: u64,
    /// CPU time spent inside `render_resolved_commands`, in microseconds.
    pub frame_cpu_time_micros: u64,
    /// The frame uploaded new raster images into the renderer-owned image atlas.
    pub uploaded_raster_images: bool,
    /// Raster images that could not be uploaded or allocated in the image atlas.
    ///
    /// Failed images are replaced with a low-alpha diagnostic rectangle instead
    /// of disappearing silently, so the app can surface resource pressure while
    /// the frame remains visibly debuggable.
    pub failed_raster_images: u32,
    /// Raster images rejected because their declared color space is not
    /// supported by the current atlas texture contract.
    pub unsupported_raster_color_spaces: u32,
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
    /// Current renderer-owned raster image atlas generation.
    pub image_atlas_generation: u64,
    /// Raster image atlas page resets since renderer creation.
    pub image_atlas_page_resets: u64,
    /// Raster image atlas page resets triggered while resolving this frame.
    pub image_atlas_page_resets_this_frame: u32,
    /// External GPU textures currently registered with the renderer.
    pub external_texture_entries: usize,
    /// External texture draw commands whose key was missing from the renderer registry.
    pub failed_external_textures: u32,
    /// Submitted batches that sampled an external GPU texture.
    pub submitted_external_texture_batches: usize,
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
    external_texture_sampler: wgpu::Sampler,
    external_textures: HashMap<String, ExternalTextureRegistration>,
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
    DrawCommand::Rect {
        bounds,
        color,
        corner_radii: CornerRadii::all(radius),
    }
}

fn external_texture_failure_fallback(
    bounds: mondrian_ui_core::types::Rect,
    tint: Color,
) -> DrawCommand {
    let mut color = tint;
    color.a = (color.a * 0.28).clamp(0.12, 0.34);
    DrawCommand::Rect {
        bounds,
        color,
        corner_radii: CornerRadii::all(bounds.width.min(bounds.height).min(10.0) * 0.2),
    }
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
        let external_texture_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("ui_external_texture_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            pipeline,
            glyph_texture,
            glyph_bind_group,
            image_texture,
            image_bind_group,
            image_atlas: TextureAtlas::new(ATLAS_SIZE, ATLAS_SIZE),
            image_cache: HashMap::new(),
            external_texture_sampler,
            external_textures: HashMap::new(),
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
    pub fn upload_glyphs(&self, queue: &wgpu::Queue, uploads: &[GlyphUpload]) -> GlyphUploadStats {
        let mut stats = GlyphUploadStats::default();
        for upload in uploads {
            if upload.width == 0 || upload.height == 0 {
                stats.skipped_glyphs = stats.skipped_glyphs.saturating_add(1);
                continue;
            }
            let pixel_count = (upload.width * upload.height) as usize;
            if upload.data.len() != pixel_count {
                stats.skipped_glyphs = stats.skipped_glyphs.saturating_add(1);
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
            stats.submitted_glyphs = stats.submitted_glyphs.saturating_add(1);
            stats.upload_bytes = stats.upload_bytes.saturating_add(rgba.len() as u64);
        }
        stats
    }

    /// Return diagnostics for the renderer-owned raster image atlas.
    pub fn image_atlas_stats(&self) -> TextureAtlasStats {
        self.image_atlas.stats()
    }

    /// Register or replace a GPU texture view for later [`DrawCommand::ExternalTexture`] draws.
    pub fn register_external_texture_view(
        &mut self,
        device: &wgpu::Device,
        key: ExternalTextureKey,
        texture_view: &wgpu::TextureView,
        transfer: ExternalTextureTransfer,
    ) -> Result<(), ExternalTextureRegistrationError> {
        if transfer == ExternalTextureTransfer::SrgbSurfaceCodeValuesOpaque
            && !self.surface_format.is_srgb()
        {
            return Err(
                ExternalTextureRegistrationError::CodeValuesRequireSrgbSurface {
                    surface_format: self.surface_format,
                },
            );
        }
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ui_external_texture_bg"),
            layout: &self.pipeline.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.external_texture_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(texture_view),
                },
            ],
        });
        self.external_textures.insert(
            key.into(),
            ExternalTextureRegistration { bind_group, transfer },
        );
        Ok(())
    }

    /// Remove one external GPU texture binding from the renderer registry.
    pub fn unregister_external_texture(&mut self, key: &ExternalTextureKey) -> bool {
        self.external_textures.remove(key.as_str()).is_some()
    }

    /// Number of external GPU texture bindings currently registered.
    pub fn external_texture_count(&self) -> usize {
        self.external_textures.len()
    }

    fn resolve_raster_images(
        &mut self,
        queue: &wgpu::Queue,
        commands: &[DrawCommand],
    ) -> (Vec<DrawCommand>, UiRenderFrameStats) {
        let mut aggregate_stats = UiRenderFrameStats::default();
        let mut allow_page_reset = true;
        loop {
            let mut pass_stats = UiRenderFrameStats::default();
            let mut resolved = Vec::with_capacity(commands.len());
            let mut restart_after_page_reset = false;

            for command in commands {
                match command {
                    DrawCommand::RasterImage {
                        key,
                        bounds,
                        width,
                        height,
                        color_space,
                        rgba,
                        tint,
                    } => {
                        match self.resolve_raster_image(
                            queue,
                            key,
                            *width,
                            *height,
                            *color_space,
                            rgba,
                            allow_page_reset,
                        ) {
                            RasterImageResolve::Resolved { uv_rect, uploaded, upload_bytes } => {
                                pass_stats.uploaded_raster_images |= uploaded;
                                pass_stats.raster_image_upload_bytes = pass_stats
                                    .raster_image_upload_bytes
                                    .saturating_add(upload_bytes);
                                resolved.push(DrawCommand::RasterAtlasImage {
                                    bounds: *bounds,
                                    uv_rect,
                                    tint: *tint,
                                });
                            }
                            RasterImageResolve::PageReset => {
                                pass_stats.image_atlas_page_resets_this_frame =
                                    pass_stats.image_atlas_page_resets_this_frame.saturating_add(1);
                                restart_after_page_reset = true;
                                break;
                            }
                            RasterImageResolve::Failed => {
                                pass_stats.failed_raster_images =
                                    pass_stats.failed_raster_images.saturating_add(1);
                                resolved.push(raster_image_failure_fallback(*bounds, *tint));
                            }
                            RasterImageResolve::UnsupportedColorSpace => {
                                pass_stats.failed_raster_images =
                                    pass_stats.failed_raster_images.saturating_add(1);
                                pass_stats.unsupported_raster_color_spaces =
                                    pass_stats.unsupported_raster_color_spaces.saturating_add(1);
                                resolved.push(raster_image_failure_fallback(*bounds, *tint));
                            }
                        }
                    }
                    other => resolved.push(other.clone()),
                };
            }

            aggregate_stats.raster_image_upload_bytes = aggregate_stats
                .raster_image_upload_bytes
                .saturating_add(pass_stats.raster_image_upload_bytes);
            aggregate_stats.uploaded_raster_images |= pass_stats.uploaded_raster_images;
            aggregate_stats.image_atlas_page_resets_this_frame = aggregate_stats
                .image_atlas_page_resets_this_frame
                .saturating_add(pass_stats.image_atlas_page_resets_this_frame);

            if restart_after_page_reset && allow_page_reset {
                allow_page_reset = false;
                continue;
            }

            aggregate_stats.failed_raster_images = pass_stats.failed_raster_images;
            aggregate_stats.unsupported_raster_color_spaces =
                pass_stats.unsupported_raster_color_spaces;
            return (resolved, aggregate_stats);
        }
    }

    fn resolve_raster_image(
        &mut self,
        queue: &wgpu::Queue,
        key: &str,
        width: u32,
        height: u32,
        color_space: RasterImageColorSpace,
        rgba: &[u8],
        allow_page_reset: bool,
    ) -> RasterImageResolve {
        if color_space != RasterImageColorSpace::Srgb {
            return RasterImageResolve::UnsupportedColorSpace;
        }
        let Some(expected_len) = raster_image_payload_len(width, height) else {
            return RasterImageResolve::Failed;
        };
        if width == 0 || height == 0 || rgba.len() != expected_len {
            return RasterImageResolve::Failed;
        }

        let cache_key = format!("{key}@{width}x{height}@{color_space:?}");
        if let Some(entry) = self.image_cache.get(&cache_key) {
            return RasterImageResolve::Resolved {
                uv_rect: entry.uv_rect,
                uploaded: false,
                upload_bytes: 0,
            };
        }

        let Some(pad_twice) = IMAGE_ATLAS_PAD.checked_mul(2) else {
            return RasterImageResolve::Failed;
        };
        let Some(alloc_w) = width.checked_add(pad_twice) else {
            return RasterImageResolve::Failed;
        };
        let Some(alloc_h) = height.checked_add(pad_twice) else {
            return RasterImageResolve::Failed;
        };
        if alloc_w > ATLAS_SIZE || alloc_h > ATLAS_SIZE {
            return RasterImageResolve::Failed;
        }
        let Some(allocated) = self.image_atlas.allocate_pixels(&cache_key, alloc_w, alloc_h) else {
            if allow_page_reset {
                self.reset_image_atlas_page();
                return RasterImageResolve::PageReset;
            }
            return RasterImageResolve::Failed;
        };
        let px = allocated.x + IMAGE_ATLAS_PAD;
        let py = allocated.y + IMAGE_ATLAS_PAD;
        let uv_rect = mondrian_ui_core::types::Rect::new(
            px as f32 / ATLAS_SIZE as f32,
            py as f32 / ATLAS_SIZE as f32,
            width as f32 / ATLAS_SIZE as f32,
            height as f32 / ATLAS_SIZE as f32,
        );
        let Some((upload_width, upload_height, padded_rgba)) =
            rgba_with_edge_padding(rgba, width, height, IMAGE_ATLAS_PAD)
        else {
            return RasterImageResolve::Failed;
        };

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
        RasterImageResolve::Resolved {
            uv_rect,
            uploaded: true,
            upload_bytes: padded_rgba.len() as u64,
        }
    }

    fn reset_image_atlas_page(&mut self) {
        self.image_atlas.reset_page();
        self.image_cache.clear();
    }

    fn resolve_external_textures(
        &self,
        commands: &[DrawCommand],
        stats: &mut UiRenderFrameStats,
    ) -> Vec<DrawCommand> {
        let mut resolved = Vec::with_capacity(commands.len());
        for command in commands {
            match command {
                DrawCommand::ExternalTexture { key, bounds, tint, .. }
                    if !self.external_textures.contains_key(key.as_str()) =>
                {
                    stats.failed_external_textures =
                        stats.failed_external_textures.saturating_add(1);
                    resolved.push(external_texture_failure_fallback(*bounds, *tint));
                }
                other => resolved.push(other.clone()),
            }
        }
        resolved
    }

    fn texture_bind_group_for_batch_key(
        &self,
        texture_key: Option<&str>,
    ) -> Option<&wgpu::BindGroup> {
        match texture_key {
            Some(IMAGE_TEXTURE_KEY) => Some(&self.image_bind_group),
            Some(key) if key.starts_with(EXTERNAL_TEXTURE_KEY_PREFIX) => {
                let external_key = &key[EXTERNAL_TEXTURE_KEY_PREFIX.len()..];
                self.external_textures.get(external_key).map(|entry| &entry.bind_group)
            }
            Some(_) => None,
            None => Some(&self.glyph_bind_group),
        }
    }

    fn pipeline_for_batch_key(&self, texture_key: Option<&str>) -> &wgpu::RenderPipeline {
        let transfer = texture_key
            .and_then(|key| key.strip_prefix(EXTERNAL_TEXTURE_KEY_PREFIX))
            .and_then(|key| self.external_textures.get(key))
            .map(|registration| registration.transfer)
            .unwrap_or(ExternalTextureTransfer::Linear);
        match transfer {
            ExternalTextureTransfer::Linear => &self.pipeline.render_pipeline,
            ExternalTextureTransfer::SrgbSurfaceCodeValuesOpaque => {
                &self.pipeline.encoded_code_value_pipeline
            }
        }
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
        let frame_started = Instant::now();
        let command_diagnostics = diagnose_draw_commands(commands);
        debug_assert_eq!(
            command_diagnostics.unresolved_text_commands, 0,
            "UiRenderer::render_resolved_commands received unresolved text commands"
        );
        let (commands, mut stats) = self.resolve_raster_images(queue, commands);
        let commands = self.resolve_external_textures(&commands, &mut stats);
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
        stats.image_atlas_generation = image_atlas_stats.generation;
        stats.image_atlas_page_resets = image_atlas_stats.page_resets;
        stats.external_texture_entries = self.external_textures.len();
        let batches = build_batches(&commands, screen_size);
        stats.batch_count = batches.len();
        stats.vertex_count = batches.iter().map(|batch| batch.vertices.len()).sum();
        self.ensure_msaa_target(device, screen_size);
        let msaa_view = self.msaa_target.as_ref().map(|target| &target.view);

        let uniform_data = Uniforms {
            screen_size: [screen_size.0 as f32, screen_size.1 as f32],
            _pad: [0.0; 2],
        };
        stats.uniform_upload_bytes = std::mem::size_of::<Uniforms>() as u64;
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
                rpass.set_pipeline(self.pipeline_for_batch_key(batch.texture_key.as_deref()));
                let Some(texture_bind_group) =
                    self.texture_bind_group_for_batch_key(batch.texture_key.as_deref())
                else {
                    stats.failed_external_textures =
                        stats.failed_external_textures.saturating_add(1);
                    continue;
                };
                if batch
                    .texture_key
                    .as_deref()
                    .is_some_and(|key| key.starts_with(EXTERNAL_TEXTURE_KEY_PREFIX))
                {
                    stats.submitted_external_texture_batches =
                        stats.submitted_external_texture_batches.saturating_add(1);
                }
                rpass.set_bind_group(1, texture_bind_group, &[]);

                let vertex_data: &[RectVertex] = &batch.vertices;
                let vertex_bytes = std::mem::size_of_val(vertex_data) as u64;
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
                stats.vertex_buffer_upload_bytes =
                    stats.vertex_buffer_upload_bytes.saturating_add(vertex_bytes);
            }
        }

        queue.submit(std::iter::once(encoder.finish()));
        stats.gpu_upload_bytes = stats
            .raster_image_upload_bytes
            .saturating_add(stats.uniform_upload_bytes)
            .saturating_add(stats.vertex_buffer_upload_bytes);
        stats.frame_cpu_time_micros = elapsed_micros(frame_started);
        stats
    }
}

fn elapsed_micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
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
            DrawCommand::Rect { bounds, color, corner_radii } => {
                assert_eq!(bounds, Rect::new(10.0, 20.0, 30.0, 40.0));
                assert_eq!((color.r, color.g, color.b), (0.4, 0.5, 0.6));
                assert!((0.08..=0.24).contains(&color.a));
                assert!(corner_radii.max_radius() > 0.0);
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
        assert_eq!(
            stats.uniform_upload_bytes,
            std::mem::size_of::<Uniforms>() as u64
        );
        assert_eq!(
            stats.vertex_buffer_upload_bytes,
            (6 * std::mem::size_of::<RectVertex>()) as u64
        );
        assert_eq!(
            stats.gpu_upload_bytes,
            stats.uniform_upload_bytes + stats.vertex_buffer_upload_bytes
        );
        assert!(stats.frame_cpu_time_micros > 0);
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
    fn offscreen_renderer_keeps_subpixel_45_degree_hairlines_connected_at_dpi_scales() {
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let physical_size = scaled_u32(96.0, scale);
            let Some(mut harness) = OffscreenHarness::new(physical_size, physical_size) else {
                return;
            };
            let mut encoder = DrawEncoder::new();
            let cases = [
                ((8.25, 10.75), (36.25, 38.75), "positive-a"),
                ((48.75, 12.25), (76.75, 40.25), "positive-b"),
                ((10.50, 80.25), (38.50, 52.25), "negative-a"),
                ((52.25, 82.75), (80.25, 54.75), "negative-b"),
            ];

            for ((sx, sy), (ex, ey), _) in cases {
                encoder.draw_line(
                    scaled_point(sx, sy, scale),
                    scaled_point(ex, ey, scale),
                    scale,
                    Color::WHITE,
                );
            }

            let pixels = harness.render(encoder.finish());
            for ((sx, sy), (ex, ey), label) in cases {
                assert_readback_line_coverage_connects_caps(
                    &pixels,
                    physical_size,
                    scaled_point(sx, sy, scale),
                    scaled_point(ex, ey, scale),
                    24,
                    &format!("scale {scale} {label}"),
                );
            }
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
    fn offscreen_renderer_clips_line_circle_and_triangle_primitives() {
        let Some(mut harness) = OffscreenHarness::new(96, 96) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.push_clip(Rect::new(16.0, 16.0, 64.0, 64.0));
        encoder.push_clip(Rect::new(28.0, 28.0, 40.0, 40.0));
        encoder.draw_line(
            Point::new(12.0, 48.0),
            Point::new(84.0, 48.0),
            2.0,
            Color::WHITE,
        );
        encoder.draw_rect(Rect::new(38.0, 38.0, 20.0, 20.0), Color::WHITE, 10.0);
        encoder.draw_triangles(
            &[
                Point::new(44.0, 20.0),
                Point::new(76.0, 76.0),
                Point::new(12.0, 76.0),
            ],
            Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
        );
        encoder.pop_clip();
        encoder.pop_clip();

        let pixels = harness.render(encoder.finish());

        for (x, y, label) in [
            (48, 48, "line/circle center"),
            (44, 44, "triangle interior"),
        ] {
            let sample = pixel(&pixels, 96, x, y);
            assert!(
                sample[3] >= 180,
                "{label} inside nested clip should render, got {sample:?}"
            );
        }

        for (x, y, label) in [
            (24, 48, "left of nested clip"),
            (72, 48, "right of nested clip"),
            (48, 24, "above nested clip"),
            (48, 72, "below nested clip"),
            (14, 48, "outside parent clip"),
        ] {
            let sample = pixel(&pixels, 96, x, y);
            assert_eq!(
                sample[3], 0,
                "{label} should be clipped out for all primitive types, got {sample:?}"
            );
        }
    }

    #[test]
    fn offscreen_renderer_handles_near_zero_primitives_without_background_pollution() {
        let Some(mut harness) = OffscreenHarness::new(32, 32) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_rect(Rect::new(8.0, 8.0, 0.0, 16.0), Color::WHITE, 0.0);
        encoder.draw_rect(Rect::new(10.0, 8.0, 0.001, 0.001), Color::WHITE, 0.001);
        encoder.draw_line(
            Point::new(12.0, 12.0),
            Point::new(12.0, 12.0),
            0.0,
            Color::WHITE,
        );
        encoder.draw_triangles(
            &[
                Point::new(16.0, 16.0),
                Point::new(16.0, 16.0),
                Point::new(16.0, 16.0),
            ],
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        let visible_pixels = pixels.chunks_exact(4).filter(|pixel| pixel[3] > 0).count();
        assert!(
            visible_pixels <= 16,
            "near-zero primitives should not pollute the frame, visible pixel count={visible_pixels}"
        );

        let stats = harness.last_stats.expect("render should record stats");
        assert!(
            stats.submitted_vertices <= 12,
            "zero-width rects and degenerate triangles should be skipped before GPU submission, stats={stats:?}"
        );
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
    fn offscreen_renderer_covers_primitives_at_representative_dpi_scales() {
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let physical_size = scaled_u32(96.0, scale);
            let Some(mut harness) = OffscreenHarness::new(physical_size, physical_size) else {
                return;
            };
            let mut encoder = DrawEncoder::new();

            encoder.draw_line(
                scaled_point(8.0, 8.0, scale),
                scaled_point(40.0, 40.0, scale),
                scale,
                Color::WHITE,
            );
            encoder.draw_rect(
                scaled_rect(52.0, 12.0, 28.0, 28.0, scale),
                Color::WHITE,
                14.0 * scale,
            );
            encoder.draw_triangles(
                &[
                    scaled_point(10.0, 86.0, scale),
                    scaled_point(48.0, 86.0, scale),
                    scaled_point(10.0, 48.0, scale),
                ],
                Color { r: 0.0, g: 1.0, b: 0.0, a: 1.0 },
            );

            let pixels = harness.render(encoder.finish());
            let sample_radius = scale.ceil() as u32;

            for logical in [10.0_f32, 16.0, 22.0, 28.0, 34.0, 38.0] {
                let x = scaled_u32(logical, scale);
                let y = scaled_u32(logical, scale);
                let alpha = max_alpha_in_square(&pixels, physical_size, x, y, sample_radius);
                assert!(
                    alpha >= 32,
                    "scale {scale} diagonal hairline lost coverage near logical {logical}: alpha={alpha}"
                );
            }

            let circle_center = pixel(
                &pixels,
                physical_size,
                scaled_u32(66.0, scale),
                scaled_u32(26.0, scale),
            );
            assert!(
                circle_center[3] >= 220,
                "scale {scale} circle center should be opaque, got {circle_center:?}"
            );
            for (x, y) in [(52.0, 12.0), (79.0, 12.0), (52.0, 39.0), (79.0, 39.0)] {
                let alpha = pixel(
                    &pixels,
                    physical_size,
                    scaled_u32(x, scale),
                    scaled_u32(y, scale),
                )[3];
                assert!(
                    alpha <= 48,
                    "scale {scale} circle corner ({x},{y}) should stay transparent, got {alpha}"
                );
            }
            let circle_edge_alpha = max_alpha_in_square(
                &pixels,
                physical_size,
                scaled_u32(66.0, scale),
                scaled_u32(12.0, scale),
                sample_radius,
            );
            assert!(
                circle_edge_alpha >= 64,
                "scale {scale} circle top edge should have AA coverage, got {circle_edge_alpha}"
            );

            let triangle_inside = pixel(
                &pixels,
                physical_size,
                scaled_u32(20.0, scale),
                scaled_u32(76.0, scale),
            );
            assert!(
                triangle_inside[1] >= 180 && triangle_inside[3] >= 220,
                "scale {scale} triangle interior should be green and opaque, got {triangle_inside:?}"
            );
            let triangle_outside = pixel(
                &pixels,
                physical_size,
                scaled_u32(54.0, scale),
                scaled_u32(54.0, scale),
            );
            assert_eq!(
                triangle_outside[3], 0,
                "scale {scale} triangle outside should stay transparent, got {triangle_outside:?}"
            );
        }
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
            RasterImageColorSpace::Srgb,
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
        assert_eq!(stats.raster_image_upload_bytes, 6 * 6 * 4);
        assert!(stats.gpu_upload_bytes >= stats.raster_image_upload_bytes);
    }

    #[test]
    fn offscreen_renderer_rejects_unsupported_raster_color_space_with_diagnostics() {
        let Some(mut harness) = OffscreenHarness::new(16, 16) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_raster_image(
            "test.raster.display-p3",
            Rect::new(0.0, 0.0, 16.0, 16.0),
            1,
            1,
            RasterImageColorSpace::DisplayP3,
            std::sync::Arc::from(vec![255, 0, 0, 255]),
            Color::WHITE,
        );

        let _ = harness.render(encoder.finish());
        let stats = harness.last_stats.expect("render should record stats");

        assert_eq!(stats.unsupported_raster_color_spaces, 1);
        assert_eq!(stats.failed_raster_images, 1);
        assert_eq!(stats.raster_image_upload_bytes, 0);
        assert!(!stats.uploaded_raster_images);
        assert_eq!(stats.image_atlas_entries, 0);
    }

    #[test]
    fn offscreen_renderer_resets_stale_raster_image_page_and_retries_frame() {
        let Some(mut harness) = OffscreenHarness::new(32, 32) else {
            return;
        };
        harness
            .renderer
            .image_atlas
            .allocate_pixels("test.stale.full.page", ATLAS_SIZE, ATLAS_SIZE)
            .expect("stale allocation should fill the atlas");
        harness.renderer.image_cache.insert(
            "test.stale.full.page@1x1@Srgb".to_string(),
            ImageCacheEntry { uv_rect: Rect::new(0.0, 0.0, 1.0, 1.0) },
        );

        let rgba = vec![
            255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        let mut encoder = DrawEncoder::new();
        encoder.draw_raster_image(
            "new.image.after.reset",
            Rect::new(8.0, 8.0, 16.0, 16.0),
            2,
            2,
            RasterImageColorSpace::Srgb,
            std::sync::Arc::from(rgba),
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        let center = pixel(&pixels, 32, 16, 16);
        assert!(
            center[0] >= 240 && center[3] >= 240,
            "raster image should render after atlas page reset, got {center:?}"
        );

        let stats = harness.last_stats.expect("render should record stats");
        assert_eq!(stats.image_atlas_page_resets_this_frame, 1);
        assert_eq!(stats.image_atlas_generation, 1);
        assert_eq!(stats.image_atlas_page_resets, 1);
        assert_eq!(stats.failed_raster_images, 0);
        assert!(stats.uploaded_raster_images);
        assert!(!harness.renderer.image_cache.contains_key("test.stale.full.page@1x1"));
    }

    #[test]
    fn offscreen_renderer_samples_registered_external_texture_without_atlas_upload() {
        let Some(mut harness) = OffscreenHarness::new(32, 32) else {
            return;
        };
        let key = ExternalTextureKey::new("viewer.preview.gpu").expect("external texture key");
        let texture = harness.external_texture(2, 2, &[255, 0, 0, 255]);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        harness
            .renderer
            .register_external_texture_view(
                &harness.device,
                key.clone(),
                &view,
                ExternalTextureTransfer::Linear,
            )
            .expect("linear external texture");

        let mut encoder = DrawEncoder::new();
        encoder.draw_external_texture(
            key,
            Rect::new(8.0, 8.0, 16.0, 16.0),
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        let center = pixel(&pixels, 32, 16, 16);

        assert!(
            center[0] >= 220 && center[1] <= 32 && center[2] <= 32 && center[3] >= 220,
            "external texture center should sample red, got {center:?}"
        );
        let stats = harness.last_stats.expect("render should record stats");
        assert_eq!(stats.external_texture_entries, 1);
        assert_eq!(stats.failed_external_textures, 0);
        assert_eq!(stats.submitted_external_texture_batches, 1);
        assert_eq!(stats.raster_image_upload_bytes, 0);
        assert!(!stats.uploaded_raster_images);
    }

    #[test]
    fn encoded_code_value_registration_rejects_non_srgb_surface() {
        let Some(mut harness) = OffscreenHarness::new(16, 16) else {
            return;
        };
        let key = ExternalTextureKey::new("viewer.encoded.invalid").expect("external texture key");
        let texture = harness.external_texture_with_format(
            1,
            1,
            &[128, 64, 192, 17],
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let error = harness
            .renderer
            .register_external_texture_view(
                &harness.device,
                key,
                &view,
                ExternalTextureTransfer::SrgbSurfaceCodeValuesOpaque,
            )
            .expect_err("non-sRGB target must fail closed");

        assert_eq!(
            error,
            ExternalTextureRegistrationError::CodeValuesRequireSrgbSurface {
                surface_format: wgpu::TextureFormat::Rgba8Unorm
            }
        );
        assert_eq!(harness.renderer.external_texture_count(), 0);
    }

    #[test]
    fn encoded_code_values_survive_srgb_attachment_round_trip() {
        let Some(mut harness) =
            OffscreenHarness::with_format(16, 16, wgpu::TextureFormat::Rgba8UnormSrgb)
        else {
            return;
        };
        let key = ExternalTextureKey::new("viewer.encoded.codes").expect("external texture key");
        let expected = [128, 64, 192, 17];
        let texture =
            harness.external_texture_with_format(1, 1, &expected, wgpu::TextureFormat::Rgba8Unorm);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        harness
            .renderer
            .register_external_texture_view(
                &harness.device,
                key.clone(),
                &view,
                ExternalTextureTransfer::SrgbSurfaceCodeValuesOpaque,
            )
            .expect("sRGB surface code-value registration");
        let mut encoder = DrawEncoder::new();
        encoder.draw_external_texture(
            key,
            Rect::new(0.0, 0.0, 16.0, 16.0),
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        let actual = pixel(&pixels, 16, 8, 8);
        for channel in 0..3 {
            assert!(
                actual[channel].abs_diff(expected[channel]) <= 1,
                "encoded channel {channel} changed across the sRGB carrier: expected {expected:?}, got {actual:?}"
            );
        }
        assert_eq!(actual[3], 255, "code-value presentation must be opaque");
    }

    #[test]
    fn offscreen_renderer_reports_missing_external_texture() {
        let Some(mut harness) = OffscreenHarness::new(32, 32) else {
            return;
        };
        let key = ExternalTextureKey::new("missing.viewer.texture").expect("external texture key");
        let mut encoder = DrawEncoder::new();
        encoder.draw_external_texture(
            key,
            Rect::new(8.0, 8.0, 16.0, 16.0),
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Color::WHITE,
        );

        let pixels = harness.render(encoder.finish());
        let center = pixel(&pixels, 32, 16, 16);

        assert!(
            center[3] > 0,
            "missing external texture should render visible diagnostic fallback"
        );
        let stats = harness.last_stats.expect("render should record stats");
        assert_eq!(stats.external_texture_entries, 0);
        assert_eq!(stats.failed_external_textures, 1);
        assert_eq!(stats.submitted_external_texture_batches, 0);
    }

    #[test]
    fn upload_glyphs_reports_submitted_skipped_and_bytes() {
        let Some(harness) = OffscreenHarness::new(16, 16) else {
            return;
        };
        let stats = harness.renderer.upload_glyphs(
            &harness.queue,
            &[
                GlyphUpload {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 2,
                    data: vec![64, 255],
                },
                GlyphUpload { x: 2, y: 0, width: 2, height: 2, data: vec![255] },
                GlyphUpload { x: 4, y: 0, width: 0, height: 1, data: Vec::new() },
            ],
        );

        assert_eq!(stats.submitted_glyphs, 1);
        assert_eq!(stats.skipped_glyphs, 2);
        assert_eq!(stats.upload_bytes, 8);
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
            Self::with_format(width, height, wgpu::TextureFormat::Rgba8Unorm)
        }

        fn with_format(width: u32, height: u32, format: wgpu::TextureFormat) -> Option<Self> {
            let instance = wgpu::Instance::new(
                wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
            );
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    compatible_surface: None,
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    apply_limit_buckets: false,
                }))
                .ok()?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                    .ok()?;
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

        fn external_texture(&self, width: u32, height: u32, rgba: &[u8; 4]) -> wgpu::Texture {
            self.external_texture_with_format(
                width,
                height,
                rgba,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            )
        }

        fn external_texture_with_format(
            &self,
            width: u32,
            height: u32,
            rgba: &[u8; 4],
            format: wgpu::TextureFormat,
        ) -> wgpu::Texture {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("ui_offscreen_external_texture"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for _ in 0..width.saturating_mul(height) {
                pixels.extend_from_slice(rgba);
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            );
            texture
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

            let mapped = slice.get_mapped_range().expect("ui readback mapped range");
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

    fn assert_readback_line_coverage_connects_caps(
        pixels: &[u8],
        width: u32,
        start: Point,
        end: Point,
        min_alpha: u8,
        label: &str,
    ) {
        let height = pixels.len() as u32 / (width * 4);
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let len = dx.hypot(dy);
        let (ux, uy) = if len > 0.001 {
            (dx / len, dy / len)
        } else {
            (1.0, 0.0)
        };

        let min_x = (start.x.min(end.x).floor() as i32 - 4).max(0);
        let max_x = (start.x.max(end.x).ceil() as i32 + 4).min(width as i32 - 1);
        let min_y = (start.y.min(end.y).floor() as i32 - 4).max(0);
        let max_y = (start.y.max(end.y).ceil() as i32 + 4).min(height as i32 - 1);
        assert!(
            min_x <= max_x && min_y <= max_y,
            "{label} line bounds should intersect the render target"
        );

        let grid_width = (max_x - min_x + 1) as usize;
        let grid_height = (max_y - min_y + 1) as usize;
        let mut visible = vec![false; grid_width * grid_height];
        let mut start_seed = None;
        let mut end_pixels = vec![false; grid_width * grid_height];

        for gy in 0..grid_height {
            for gx in 0..grid_width {
                let x = min_x + gx as i32;
                let y = min_y + gy as i32;
                let alpha = pixel(pixels, width, x as u32, y as u32)[3];
                if alpha < min_alpha {
                    continue;
                }

                let idx = gy * grid_width + gx;
                visible[idx] = true;
                let center_x = x as f32 + 0.5;
                let center_y = y as f32 + 0.5;
                let projected = (center_x - start.x) * ux + (center_y - start.y) * uy;
                if projected <= 2.0 {
                    start_seed.get_or_insert(idx);
                }
                if projected >= len - 2.0 {
                    end_pixels[idx] = true;
                }
            }
        }

        let seed = start_seed.unwrap_or_else(|| {
            panic!(
                "{label} line coverage should include a visible start cap above alpha {min_alpha}"
            )
        });
        assert!(
            end_pixels.iter().any(|is_end| *is_end),
            "{label} line coverage should include a visible end cap above alpha {min_alpha}"
        );

        let mut visited = vec![false; grid_width * grid_height];
        let mut queue = std::collections::VecDeque::from([seed]);
        visited[seed] = true;

        while let Some(idx) = queue.pop_front() {
            if end_pixels[idx] {
                return;
            }

            let gx = idx % grid_width;
            let gy = idx / grid_width;
            for oy in -1_i32..=1 {
                for ox in -1_i32..=1 {
                    if ox == 0 && oy == 0 {
                        continue;
                    }
                    let nx = gx as i32 + ox;
                    let ny = gy as i32 + oy;
                    if nx < 0 || ny < 0 || nx >= grid_width as i32 || ny >= grid_height as i32 {
                        continue;
                    }
                    let next = ny as usize * grid_width + nx as usize;
                    if visible[next] && !visited[next] {
                        visited[next] = true;
                        queue.push_back(next);
                    }
                }
            }
        }

        panic!("{label} line coverage should form an 8-connected visible path from start cap to end cap above alpha {min_alpha}");
    }

    fn scaled_u32(logical: f32, scale: f32) -> u32 {
        (logical * scale).round().max(0.0) as u32
    }

    fn scaled_point(x: f32, y: f32, scale: f32) -> Point {
        Point::new(x * scale, y * scale)
    }

    fn scaled_rect(x: f32, y: f32, width: f32, height: f32, scale: f32) -> Rect {
        Rect::new(x * scale, y * scale, width * scale, height * scale)
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

    // ── Edge-case primitive harness ─────────────────────────────────────────

    #[test]
    fn offscreen_renderer_handles_subpixel_rect_positions() {
        let Some(mut harness) = OffscreenHarness::new(32, 32) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        // Rect at subpixel position; must still produce visible pixels
        encoder.draw_rect(
            Rect::new(8.3, 8.7, 16.0, 16.0),
            Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
            0.0,
        );
        let pixels = harness.render(encoder.finish());
        let center = pixel(&pixels, 32, 16, 16);
        assert!(
            center[3] >= 120,
            "subpixel rect center should have coverage, got {center:?}"
        );
        let stats = harness.last_stats.expect("render should record stats");
        assert!(
            stats.submitted_vertices > 0,
            "subpixel rect must produce GPU vertices"
        );
    }

    #[test]
    fn offscreen_renderer_handles_deeply_nested_clips() {
        let Some(mut harness) = OffscreenHarness::new(64, 64) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        // 4 levels of nested clips
        encoder.push_clip(Rect::new(4.0, 4.0, 56.0, 56.0));
        encoder.push_clip(Rect::new(12.0, 12.0, 40.0, 40.0));
        encoder.push_clip(Rect::new(20.0, 20.0, 24.0, 24.0));
        encoder.push_clip(Rect::new(26.0, 26.0, 12.0, 12.0));
        encoder.draw_rect(
            Rect::new(0.0, 0.0, 64.0, 64.0),
            Color { r: 1.0, g: 0.0, b: 1.0, a: 1.0 },
            0.0,
        );
        encoder.pop_clip();
        encoder.pop_clip();
        encoder.pop_clip();
        encoder.pop_clip();

        let pixels = harness.render(encoder.finish());
        let inside = pixel(&pixels, 64, 32, 32);
        assert!(
            inside[3] >= 180,
            "4-level nested clip interior should render, got {inside:?}"
        );
        let outside = pixel(&pixels, 64, 18, 32);
        assert_eq!(
            outside[3], 0,
            "outside 3rd clip level should reject, got {outside:?}"
        );
    }

    #[test]
    fn offscreen_renderer_survives_one_by_one_viewport_rendering() {
        let Some(mut harness) = OffscreenHarness::new(1, 1) else {
            return;
        };
        let mut encoder = DrawEncoder::new();
        encoder.draw_rect(
            Rect::new(0.0, 0.0, 1.0, 1.0),
            Color { r: 1.0, g: 0.0, b: 0.0, a: 1.0 },
            0.0,
        );
        let pixels = harness.render(encoder.finish());
        assert_eq!(pixels.len(), 4);
        let stats = harness.last_stats.expect("render should record stats");
        assert!(
            stats.submitted_vertices <= 6,
            "1x1 viewport should produce at most one rect"
        );
    }
}
