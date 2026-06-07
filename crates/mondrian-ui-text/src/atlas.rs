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
    /// Map from cache_key to (uv_rect, bmp_w, bmp_h, top, left)
    glyph_map: HashMap<CacheKey, (Rect, u32, u32, i32, i32)>,
    pub pending_uploads: Vec<GlyphUpload>,
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

        /// 获取或光栅化字形。首次返回 None，下一次返回 (UV rect, bmp_w, bmp_h, top)。
    /// `top` 是 glyph bitmap 内部 placement.top（swash rasterized offset）。
    pub fn get_or_rasterize(
        &mut self,
        font_system: &mut FontSystem,
        glyph: &LayoutGlyph,
    ) -> Option<(Rect, u32, u32, i32, i32)> {
        let physical = glyph.physical((0.0, 0.0), 1.0);
        let cache_key = physical.cache_key;
        if let Some(&(uv, w, h, top, left)) = self.glyph_map.get(&cache_key) {
            return Some((uv, w, h, top, left));
        }

        let image = self.cache.get_image(font_system, cache_key);
        let (bmp_w, bmp_h, top, left, alpha) = match image {
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

        self.glyph_map.insert(cache_key, (uv_rect, bmp_w, bmp_h, top, left));
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

/// Convert swash Image to R8 alpha bitmap. Returns (width, height, top, left, data).
fn swash_to_alpha(image: &cosmic_text::SwashImage) -> (u32, u32, i32, i32, Vec<u8>) {
    let w = image.placement.width;
    let h = image.placement.height;
    let top = image.placement.top;
    let left = image.placement.left;
    match image.content {
        cosmic_text::SwashContent::Mask => {
            let mut alpha = image.data.clone();
            alpha.resize((w * h) as usize, 0);
            (w, h, top, left, alpha)
        }
        cosmic_text::SwashContent::SubpixelMask => {
            let pixel_count = (w * h) as usize;
            let mut alpha = Vec::with_capacity(pixel_count);
            for chunk in image.data.chunks(3) {
                let gray = if chunk.len() >= 3 {
                    (chunk[0] as u32 + chunk[1] as u32 + chunk[2] as u32) / 3
                } else { 0 };
                alpha.push(gray as u8);
            }
            alpha.resize(pixel_count, 0);
            (w, h, top, left, alpha)
        }
        _ => {
            (w, h, top, left, vec![0u8; (w * h) as usize])
        }
    }
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
    fn atlas_rasterize_produces_non_empty_bitmap() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, "M", attrs, 24.0,
        );
        let glyphs = layout.glyphs();
        assert!(!glyphs.is_empty());

        let _ = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0]);
        assert!(!atlas.pending_uploads.is_empty());

        let upload = &atlas.pending_uploads[0];
        assert!(upload.width > 0 && upload.height > 0,
            "Glyph size: {}x{}", upload.width, upload.height);

        // Bitmap should have non-zero pixels (the glyph shape)
        let non_zero = upload.data.iter().filter(|&&b| b > 0).count();
        assert!(non_zero > 0,
            "Glyph bitmap is all zeros! {}x{} = {} bytes",
            upload.width, upload.height, upload.data.len());
    }

    #[test]
    fn atlas_uv_within_bounds() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, "W", attrs, 24.0,
        );
        let glyphs = layout.glyphs();
        assert!(!glyphs.is_empty());

        // First call rasterizes (returns None), second returns cached UV
        let _ = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0]);
        let (uv, _w, _h, _top, _left) = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0])
            .expect("Second call should return cached UV");

        assert!(uv.x >= 0.0 && uv.x <= 1.0, "UV x={} out of [0,1]", uv.x);
        assert!(uv.y >= 0.0 && uv.y <= 1.0, "UV y={} out of [0,1]", uv.y);
        assert!(uv.width > 0.0, "UV width={} should be > 0", uv.width);
        assert!(uv.height > 0.0, "UV height={} should be > 0", uv.height);
        assert!(uv.x + uv.width <= 1.01, "UV right edge out of bounds");
        assert!(uv.y + uv.height <= 1.01, "UV bottom edge out of bounds");
    }

    #[test]
    fn rasterized_dimensions_vs_layout_dimensions() {
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let text = "Hello";
        let font_size = 24.0;
        let layout = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, text, attrs, font_size,
        );

        for glyph in layout.glyphs() {
            let physical = glyph.physical((0.0, 0.0), 1.0);
            // Rasterize via SwashCache to get actual bitmap dimensions
            let mut cache = SwashCache::new();
            if let Some(image) = cache.get_image(&mut mgr.font_system, physical.cache_key) {
                let bmp_w = image.placement.width;
                let bmp_h = image.placement.height;

                // Layout glyph dimensions vs rasterized dimensions
                // They should be reasonably close
                let ratio_w = if glyph.w > 0.0 { bmp_w as f32 / glyph.w } else { 1.0 };
                let ratio_h = if glyph.font_size > 0.0 { bmp_h as f32 / glyph.font_size } else { 1.0 };

                // Log diagnostics if there's a significant mismatch
                if (ratio_w - 1.0).abs() > 0.5 || (ratio_h - 1.0).abs() > 0.5 {
                    // This is just diagnostic; the test passes even with a mismatch
                    eprintln!("Glyph dim mismatch: layout=({:.1},{:.1}) raster=({},{}) ratio=({:.2},{:.2})",
                        glyph.w, glyph.font_size, bmp_w, bmp_h, ratio_w, ratio_h);
                }
            }
        }
    }

    #[test]
    fn cache_key_stable_across_calls() {
        // Verify that the same glyph produces the same cache_key on repeated calls
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let text = "A";
        let layout1 = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, text, attrs.clone(), 24.0,
        );
        let layout2 = crate::layout::TextLayout::new_single_line(
            &mut mgr.font_system, text, attrs, 24.0,
        );

        let glyphs1 = layout1.glyphs();
        let glyphs2 = layout2.glyphs();
        assert_eq!(glyphs1.len(), glyphs2.len());

        for (g1, g2) in glyphs1.iter().zip(glyphs2.iter()) {
            let ck1 = g1.physical((0.0, 0.0), 1.0).cache_key;
            let ck2 = g2.physical((0.0, 0.0), 1.0).cache_key;
            assert_eq!(ck1, ck2,
                "Cache key changed between calls for the same glyph! Glyph ID may not be stable.");
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

        // First call: rasterizes, returns None
        let first = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0]);
        // Second call: should return cached UV
        let second = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0]);

        if first.is_none() {
            // First rasterized, bitmap in pending_uploads, second should be cached
            assert!(second.is_some(), "Second call should return cached UV+size");
        } else {
            // Already cached somehow — should be the same
            assert_eq!(first.map(|(r, _, _, _, _)| r), second.map(|(r, _, _, _, _)| r));
        }
    }
}
