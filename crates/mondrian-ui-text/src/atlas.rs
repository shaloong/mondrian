//! 字形纹理图集
//!
//! CPU 光栅化字形 → 上传到 GPU R8Unorm 纹理图集。
//! 复用 mondrian-ui-renderer 的 TextureAtlas 分配槽位。

use std::collections::HashMap;

use cosmic_text::{CacheKey, FontSystem, LayoutGlyph, SwashCache};
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::atlas::TextureAtlas;

/// 待上传到 GPU 的字形数据（单通道 alpha）
#[derive(Debug, Clone)]
pub struct GlyphUpload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

pub struct GlyphAtlas {
    atlas: TextureAtlas,
    cache: SwashCache,
    glyph_map: HashMap<CacheKey, Rect>,
    pub pending_uploads: Vec<GlyphUpload>,
    /// 图集中每个字形周围保留的内边距（像素），防止采样渗色
    pad: u32,
}

impl GlyphAtlas {
    pub fn new(atlas_size: u32) -> Self {
        Self {
            atlas: TextureAtlas::new(atlas_size, atlas_size),
            cache: SwashCache::new(),
            glyph_map: HashMap::new(),
            pending_uploads: Vec::new(),
            pad: 1,
        }
    }

    /// 获取或光栅化字形。首次返回 None（本帧上传，下帧可见）。
    pub fn get_or_rasterize(
        &mut self,
        font_system: &mut FontSystem,
        glyph: &LayoutGlyph,
    ) -> Option<Rect> {
        let physical = glyph.physical((0.0, 0.0), 1.0);
        let cache_key = physical.cache_key;
        if let Some(uv) = self.glyph_map.get(&cache_key) {
            return Some(*uv);
        }

        let image = self.cache.get_image(font_system, cache_key);
        let (bmp_w, bmp_h, alpha) = match image {
            Some(img) => swash_to_alpha(img),
            None => return None,
        };
        if bmp_w == 0 || bmp_h == 0 {
            return None;
        }

        // Allocate atlas space with padding
        let alloc_w = bmp_w + self.pad * 2;
        let alloc_h = bmp_h + self.pad * 2;
        let alloc_key = format!("glyph_{cache_key:?}");
        let uv = self.atlas.allocate(&alloc_key, alloc_w, alloc_h)?;

        let px = (uv.x * self.atlas.width as f32) as u32 + self.pad;
        let py = (uv.y * self.atlas.height as f32) as u32 + self.pad;

        let uv_rect = Rect::new(
            px as f32 / self.atlas.width as f32,
            py as f32 / self.atlas.height as f32,
            bmp_w as f32 / self.atlas.width as f32,
            bmp_h as f32 / self.atlas.height as f32,
        );

        self.glyph_map.insert(cache_key, uv_rect);
        self.pending_uploads.push(GlyphUpload {
            x: px, y: py,
            width: bmp_w, height: bmp_h,
            data: alpha,
        });

        None
    }

    pub fn size(&self) -> (u32, u32) { self.atlas.size() }
    pub fn has_pending(&self) -> bool { !self.pending_uploads.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontManager;

    #[test]
    fn atlas_creates_empty() {
        let atlas = GlyphAtlas::new(1024);
        assert!(!atlas.has_pending());
        assert_eq!(atlas.size(), (1024, 1024));
    }

    #[test]
    fn atlas_rasterize_glyph_upload_size_matches() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, "M", attrs, 24.0,
        );

        for glyph in layout.glyphs() {
            let _uv = atlas.get_or_rasterize(&mut mgr.font_system, &glyph);
        }

        // Verify all pending uploads have correct data sizes
        for upload in &atlas.pending_uploads {
            if upload.width > 0 && upload.height > 0 {
                assert_eq!(
                    upload.data.len(),
                    (upload.width * upload.height) as usize,
                    "Upload {}x{} should have {} bytes, got {}",
                    upload.width, upload.height,
                    upload.width * upload.height,
                    upload.data.len(),
                );
            }
        }
    }

    #[test]
    fn atlas_second_call_returns_cached_uv() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, "A", attrs, 24.0,
        );
        let glyphs = layout.glyphs();
        assert!(!glyphs.is_empty());

        // First call: rasterizes, returns None (or Some if cached)
        let first = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0]);
        // Second call: should return cached UV
        let second = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0]);

        if first.is_some() {
            // Was already cached somehow
            assert_eq!(first, second);
        } else {
            // First rasterized, second should be cached
            assert!(second.is_some());
        }
    }
}

/// Convert swash Image to R8 alpha bitmap.
/// Mask: 1 byte/pixel alpha → pass through.
/// SubpixelMask: average 3 bytes/pixel → 1 byte grayscale.
fn swash_to_alpha(image: &cosmic_text::SwashImage) -> (u32, u32, Vec<u8>) {
    match image.content {
        cosmic_text::SwashContent::Mask => {
            // Mask data is exactly w*h bytes of alpha
            let w = image.placement.width;
            let h = image.placement.height;
            let mut alpha = image.data.clone();
            alpha.resize((w * h) as usize, 0);
            (w, h, alpha)
        }
        cosmic_text::SwashContent::SubpixelMask => {
            let w = image.placement.width;
            let h = image.placement.height;
            let pixel_count = (w * h) as usize;
            let mut alpha = Vec::with_capacity(pixel_count);
            for chunk in image.data.chunks(3) {
                let gray = if chunk.len() >= 3 {
                    (chunk[0] as u32 + chunk[1] as u32 + chunk[2] as u32) / 3
                } else {
                    0
                };
                alpha.push(gray as u8);
            }
            alpha.resize(pixel_count, 0);
            (w, h, alpha)
        }
        _ => {
            let w = image.placement.width;
            let h = image.placement.height;
            (w, h, vec![0u8; (w * h) as usize])
        }
    }
}
