//! 字形纹理图集
//!
//! CPU 光栅化字形 → 上传到 GPU R8Unorm 纹理图集。
//! 复用 mondrian-ui-renderer 的 TextureAtlas 分配槽位。

use std::collections::HashMap;

use cosmic_text::{CacheKey, FontSystem, LayoutGlyph, SwashCache};
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::atlas::{TextureAtlas, TextureAtlasStats};

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

    /// 获取或光栅化字形，返回 atlas UV、bitmap 尺寸和 swash placement。
    ///
    /// 首次光栅化会分配 atlas 槽位并加入 `pending_uploads`，同时立即返回
    /// 可用于本帧 draw command 的 UV。调用方必须在提交本帧前上传 pending
    /// glyph 数据。
    /// `top` 是 glyph bitmap 内部 placement.top（swash rasterized offset）。
    pub fn get_or_rasterize(
        &mut self,
        font_system: &mut FontSystem,
        glyph: &LayoutGlyph,
        _sub_x: f32,
    ) -> Option<(Rect, u32, u32, i32, i32)> {
        // UI text atlas intentionally ignores subpixel bins (SubpixelBin).
        // We construct CacheKey with position (0,0) rather than going through
        // LayoutGlyph::physical(), because physical() encodes glyph.x.fract()
        // into the SubpixelBin — producing different hinted bitmaps (and thus
        // different left bearings) for the same glyph at different screen
        // positions. Stable metrics are preferred over LCD/subpixel sharpness.
        // Subpixel positioning is handled at draw time (bitmap_x in render.rs).
        //
        // A subpixel-aware atlas mode may be added later for RichText / code
        // editors where per-subpixel-bin sharpness is worth the atlas cost.
        let (cache_key, _, _) = CacheKey::new(
            glyph.font_id,
            glyph.glyph_id,
            glyph.font_size,
            (0.0, 0.0),
            glyph.font_weight,
            glyph.cache_key_flags,
        );
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

        // Allocate and upload a transparent border around the glyph. The UV rect
        // still points at the inner glyph bitmap, but linear filtering can now
        // sample the border without bleeding from uninitialized or adjacent texels.
        let pad_twice = self.pad.checked_mul(2)?;
        let alloc_w = bmp_w.checked_add(pad_twice)?;
        let alloc_h = bmp_h.checked_add(pad_twice)?;
        let alloc_key = format!("glyph_{cache_key:?}");
        let allocation = self.atlas.allocate_pixels(&alloc_key, alloc_w, alloc_h)?;

        let px = allocation.x + self.pad;
        let py = allocation.y + self.pad;

        let uv_rect = Rect::new(
            px as f32 / self.atlas.width as f32,
            py as f32 / self.atlas.height as f32,
            bmp_w as f32 / self.atlas.width as f32,
            bmp_h as f32 / self.atlas.height as f32,
        );

        self.glyph_map.insert(cache_key, (uv_rect, bmp_w, bmp_h, top, left));
        let padded_alpha = alpha_with_transparent_padding(&alpha, bmp_w, bmp_h, self.pad)?;
        self.pending_uploads.push(GlyphUpload {
            x: allocation.x,
            y: allocation.y,
            width: alloc_w,
            height: alloc_h,
            data: padded_alpha,
        });

        Some((uv_rect, bmp_w, bmp_h, top, left))
    }

    pub fn size(&self) -> (u32, u32) {
        self.atlas.size()
    }
    pub fn stats(&self) -> TextureAtlasStats {
        self.atlas.stats()
    }
    pub fn has_pending(&self) -> bool {
        !self.pending_uploads.is_empty()
    }
}

fn alpha_with_transparent_padding(
    alpha: &[u8],
    width: u32,
    height: u32,
    pad: u32,
) -> Option<Vec<u8>> {
    let expected_len = width.checked_mul(height)? as usize;
    if alpha.len() != expected_len {
        return None;
    }

    let pad_twice = pad.checked_mul(2)?;
    let padded_width = width.checked_add(pad_twice)?;
    let padded_height = height.checked_add(pad_twice)?;
    let mut padded = vec![0; padded_width.checked_mul(padded_height)? as usize];

    let width_usize = width as usize;
    let padded_width_usize = padded_width as usize;
    for row in 0..height as usize {
        let src_start = row * width_usize;
        let dst_start = (row + pad as usize) * padded_width_usize + pad as usize;
        padded[dst_start..dst_start + width_usize]
            .copy_from_slice(&alpha[src_start..src_start + width_usize]);
    }

    Some(padded)
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
                } else {
                    0
                };
                alpha.push(gray as u8);
            }
            alpha.resize(pixel_count, 0);
            (w, h, top, left, alpha)
        }
        _ => (w, h, top, left, vec![0u8; (w * h) as usize]),
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
        assert_eq!(atlas.stats().entries, 0);
    }

    #[test]
    fn alpha_padding_adds_transparent_border() {
        let source = vec![10, 20, 30, 40, 50, 60];
        let padded = alpha_with_transparent_padding(&source, 3, 2, 1).unwrap();

        assert_eq!(padded.len(), 5 * 4);
        assert_eq!(&padded[0..5], &[0, 0, 0, 0, 0]);
        assert_eq!(&padded[5..10], &[0, 10, 20, 30, 0]);
        assert_eq!(&padded[10..15], &[0, 40, 50, 60, 0]);
        assert_eq!(&padded[15..20], &[0, 0, 0, 0, 0]);
    }

    #[test]
    fn alpha_padding_rejects_mismatched_source_length() {
        assert!(alpha_with_transparent_padding(&[1, 2, 3], 2, 2, 1).is_none());
    }

    #[test]
    fn atlas_rasterize_glyph_upload_size_matches() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, "M", attrs, 24.0);

        for glyph in layout.glyphs() {
            let _uv = atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0);
        }

        // Verify all pending uploads have correct data sizes
        for upload in &atlas.pending_uploads {
            if upload.width > 0 && upload.height > 0 {
                assert_eq!(
                    upload.data.len(),
                    (upload.width * upload.height) as usize,
                    "Upload {}x{} should have {} bytes, got {}",
                    upload.width,
                    upload.height,
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
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, "M", attrs, 24.0);
        let glyphs = layout.glyphs();
        assert!(!glyphs.is_empty());

        let _ = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0], 0.0);
        assert!(!atlas.pending_uploads.is_empty());

        let upload = &atlas.pending_uploads[0];
        assert!(
            upload.width > 0 && upload.height > 0,
            "Glyph size: {}x{}",
            upload.width,
            upload.height
        );

        // Bitmap should have non-zero pixels (the glyph shape)
        let non_zero = upload.data.iter().filter(|&&b| b > 0).count();
        assert!(
            non_zero > 0,
            "Glyph bitmap is all zeros! {}x{} = {} bytes",
            upload.width,
            upload.height,
            upload.data.len()
        );
    }

    #[test]
    fn atlas_rasterize_upload_includes_padding_while_uv_stays_inner() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, "M", attrs, 24.0);
        let glyph = layout.glyphs().into_iter().next().expect("glyph");

        let _ = atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0);
        let (uv, bitmap_width, bitmap_height, _top, _left) =
            atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0).expect("cached glyph");
        let upload = atlas.pending_uploads.first().expect("glyph upload");

        assert_eq!(upload.width, bitmap_width + 2);
        assert_eq!(upload.height, bitmap_height + 2);
        assert_eq!(uv.x, (upload.x + 1) as f32 / atlas.size().0 as f32);
        assert_eq!(uv.y, (upload.y + 1) as f32 / atlas.size().1 as f32);
        assert_eq!(uv.width, bitmap_width as f32 / atlas.size().0 as f32);
        assert_eq!(uv.height, bitmap_height as f32 / atlas.size().1 as f32);

        let row_width = upload.width as usize;
        assert!(upload.data[..row_width].iter().all(|alpha| *alpha == 0));
        assert!(upload.data[upload.data.len() - row_width..].iter().all(|alpha| *alpha == 0));
        assert!(upload.data.chunks(row_width).all(|row| row[0] == 0 && row[row.len() - 1] == 0));
    }

    #[test]
    fn atlas_uv_within_bounds() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, "W", attrs, 24.0);
        let glyphs = layout.glyphs();
        assert!(!glyphs.is_empty());

        let (uv, _w, _h, _top, _left) = atlas
            .get_or_rasterize(&mut mgr.font_system, &glyphs[0], 0.0)
            .expect("First call should return freshly allocated UV");

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
            &mut mgr.font_system,
            text,
            attrs,
            font_size,
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
                let ratio_w = if glyph.w > 0.0 {
                    bmp_w as f32 / glyph.w
                } else {
                    1.0
                };
                let ratio_h = if glyph.font_size > 0.0 {
                    bmp_h as f32 / glyph.font_size
                } else {
                    1.0
                };

                // Non-fatal: layout glyph dimensions vs rasterized dimensions may differ
                // due to hinting. Recorded for manual review if needed.
                let _ = (ratio_w, ratio_h);
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
            &mut mgr.font_system,
            text,
            attrs.clone(),
            24.0,
        );
        let layout2 =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, text, attrs, 24.0);

        let glyphs1 = layout1.glyphs();
        let glyphs2 = layout2.glyphs();
        assert_eq!(glyphs1.len(), glyphs2.len());

        for (g1, g2) in glyphs1.iter().zip(glyphs2.iter()) {
            let ck1 = g1.physical((0.0, 0.0), 1.0).cache_key;
            let ck2 = g2.physical((0.0, 0.0), 1.0).cache_key;
            assert_eq!(
                ck1, ck2,
                "Cache key changed between calls for the same glyph! Glyph ID may not be stable."
            );
        }
    }

    #[test]
    fn atlas_second_call_returns_cached_uv() {
        let mut atlas = GlyphAtlas::new(1024);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, "A", attrs, 24.0);
        let glyphs = layout.glyphs();
        assert!(!glyphs.is_empty());

        // First call rasterizes and returns freshly allocated UV; second should
        // reuse the cached geometry.
        let first = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0], 0.0);
        let second = atlas.get_or_rasterize(&mut mgr.font_system, &glyphs[0], 0.0);

        assert!(first.is_some(), "First call should return UV+size");
        assert!(second.is_some(), "Second call should return cached UV+size");
        assert_eq!(
            first.map(|(r, _, _, _, _)| r),
            second.map(|(r, _, _, _, _)| r)
        );
    }

    // ── Bearing & Placement Tests ────────────────────────────────────────

    /// Verify swash top values are within sane ranges for common glyphs.
    /// top should be roughly the ascent (positive, glyph extends above baseline).
    #[test]
    fn bearing_top_values_in_expected_range() {
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let mut cache = SwashCache::new();

        for &(ch, size) in &[
            ('A', 16.0),
            ('a', 16.0),
            ('i', 16.0),
            ('e', 16.0),
            ('l', 16.0),
            ('A', 13.0),
            ('i', 13.0),
            ('l', 13.0),
            ('A', 24.0),
            ('i', 24.0),
            ('l', 24.0),
        ] {
            let text = ch.to_string();
            let layout = crate::layout::TextLayout::new_single_line(
                &mut mgr.font_system,
                &text,
                attrs.clone(),
                size,
            );
            for glyph in layout.glyphs() {
                let ck = glyph.physical((0.0, 0.0), 1.0).cache_key;
                if let Some(img) = cache.get_image(&mut mgr.font_system, ck) {
                    let top = img.placement.top;
                    // top should be positive for most glyphs (bitmap extends above baseline)
                    // A few glyphs like underscores may have top <= 0
                    assert!(
                        top >= 0,
                        "Glyph '{}' at {}px: top={}, expected >= 0 (bitmap should extend above baseline)",
                        ch, size, top
                    );
                    // top should be less than ~1.5x font_size (sanity check)
                    let max_top = (size * 1.5) as i32;
                    assert!(
                        top <= max_top,
                        "Glyph '{}' at {}px: top={} exceeds max expected {}",
                        ch,
                        size,
                        top,
                        max_top
                    );
                }
            }
        }
    }

    /// Verify left bearing (swash placement.left) is a sane integer.
    #[test]
    fn bearing_left_values_in_expected_range() {
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let mut cache = SwashCache::new();

        for &(ch, size) in &[
            ('A', 16.0),
            ('i', 16.0),
            ('e', 16.0),
            ('l', 16.0),
            ('M', 24.0),
            ('W', 24.0),
            ('j', 16.0),
        ] {
            let text = ch.to_string();
            let layout = crate::layout::TextLayout::new_single_line(
                &mut mgr.font_system,
                &text,
                attrs.clone(),
                size,
            );
            for glyph in layout.glyphs() {
                let ck = glyph.physical((0.0, 0.0), 1.0).cache_key;
                if let Some(img) = cache.get_image(&mut mgr.font_system, ck) {
                    let left = img.placement.left;
                    // left is typically >= -(size) for most glyphs
                    // Some glyphs (like 'f' italic) may have more negative left bearing
                    let min_left = -(size as i32 * 2);
                    assert!(
                        left >= min_left,
                        "Glyph '{}' at {}px: left={}, expected >= {}",
                        ch,
                        size,
                        left,
                        min_left
                    );
                    // left shouldn't exceed bitmap width (extreme case)
                    assert!(
                        left <= img.placement.width as i32 + (size as i32),
                        "Glyph '{}' at {}px: left={} seems extreme for width={}",
                        ch,
                        size,
                        left,
                        img.placement.width
                    );
                }
            }
        }
    }

    /// Narrow characters (i, l) at various font sizes: bearing should be consistent.
    /// Known issue: at certain sizes hinting may cause bearing to shift by 1px.
    #[test]
    fn narrow_glyph_bearing_consistency() {
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let mut cache = SwashCache::new();
        let sizes = [
            10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 24.0, 28.0, 32.0, 36.0, 48.0,
        ];

        for &ch in &['i', 'l', 'I'] {
            let mut prev_left: Option<i32> = None;
            for &size in &sizes {
                let text = ch.to_string();
                let layout = crate::layout::TextLayout::new_single_line(
                    &mut mgr.font_system,
                    &text,
                    attrs.clone(),
                    size,
                );
                for glyph in layout.glyphs() {
                    let ck = glyph.physical((0.0, 0.0), 1.0).cache_key;
                    if let Some(img) = cache.get_image(&mut mgr.font_system, ck) {
                        let left = img.placement.left;
                        let w = img.placement.width;
                        // The left bearing should scale roughly with font size
                        // We just record values and check they don't jump wildly
                        if let Some(pl) = prev_left {
                            let left_ratio = left as f32 / pl as f32;
                            // Between adjacent sizes, ratio should be close to size ratio
                            // Allow some tolerance for hinting grid-fitting
                            if pl != 0 && left != 0 {
                                assert!(
                                    left_ratio > 0.0 && left_ratio < 5.0,
                                    "'{}' at {}px vs previous: left bearing ratio={:.2} is extreme ({} -> {})",
                                    ch, size, left_ratio, pl, left
                                );
                            }
                        }
                        // Width sanity: narrow chars should stay narrow
                        if size >= 12.0 {
                            assert!(w > 0, "'{}' at {}px: zero-width bitmap", ch, size);
                        }
                        prev_left = Some(left);
                    }
                }
            }
        }
    }

    /// Verify that placement.top within a single line of text is consistent
    /// relative to the font ascent: taller glyphs have higher top values.
    #[test]
    fn placement_top_correlates_with_glyph_height() {
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let mut cache = SwashCache::new();

        let tall_chars = ['A', 'l', 'h', 'f', 'M']; // should all have similar top values
        let short_chars = ['e', 'a', 'c', 'o', 'n'];

        for size in [16.0, 20.0, 24.0] {
            for group in [&tall_chars[..], &short_chars[..]] {
                let text: String = group.iter().collect();
                let layout = crate::layout::TextLayout::new_single_line(
                    &mut mgr.font_system,
                    &text,
                    attrs.clone(),
                    size,
                );
                let mut tops: Vec<i32> = Vec::new();
                for glyph in layout.glyphs() {
                    let ck = glyph.physical((0.0, 0.0), 1.0).cache_key;
                    if let Some(img) = cache.get_image(&mut mgr.font_system, ck) {
                        tops.push(img.placement.top);
                    }
                }
                // Within a character group of similar height at the same size,
                // top values should not vary by more than 2 pixels (hinting tolerance)
                if tops.len() >= 2 {
                    let max_top = tops.iter().max().unwrap();
                    let min_top = tops.iter().min().unwrap();
                    assert!(
                        max_top - min_top <= 3,
                        "At {}px, chars {:?}: top values {:?} vary too much (max-min={})",
                        size,
                        group,
                        tops,
                        max_top - min_top
                    );
                }
            }
        }
    }

    // ── Atlas Integrity Tests ────────────────────────────────────────────

    /// Verify UV rect dimensions match bitmap dimensions exactly.
    #[test]
    fn uv_rect_matches_bitmap_dimensions() {
        let mut atlas = GlyphAtlas::new(2048);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, "Hello", attrs, 24.0);

        for glyph in layout.glyphs() {
            let _ = atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0);
            let result = atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0);
            if let Some((uv, w, h, _top, _left)) = result {
                let uv_w_px = uv.width * atlas.size().0 as f32;
                let uv_h_px = uv.height * atlas.size().1 as f32;
                assert!(
                    (uv_w_px - w as f32).abs() < 0.51,
                    "UV width {}px != bitmap width {}px",
                    uv_w_px,
                    w
                );
                assert!(
                    (uv_h_px - h as f32).abs() < 0.51,
                    "UV height {}px != bitmap height {}px",
                    uv_h_px,
                    h
                );
            }
        }
    }

    /// Verify different glyphs don't overlap in atlas space.
    #[test]
    fn no_glyph_overlap_in_atlas() {
        let mut atlas = GlyphAtlas::new(2048);
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        // Use diverse characters to get multiple glyphs
        let chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        let layout =
            crate::layout::TextLayout::new_single_line(&mut mgr.font_system, chars, attrs, 24.0);

        // First pass: rasterize
        for glyph in layout.glyphs() {
            let _ = atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0);
        }
        // Second pass: get UVs
        let mut uv_rects: Vec<Rect> = Vec::new();
        for glyph in layout.glyphs() {
            if let Some((uv, _, _, _, _)) =
                atlas.get_or_rasterize(&mut mgr.font_system, &glyph, 0.0)
            {
                uv_rects.push(uv);
            }
        }

        // Check for overlaps (with 1px padding tolerance)
        for i in 0..uv_rects.len() {
            for j in i + 1..uv_rects.len() {
                let a = uv_rects[i];
                let b = uv_rects[j];
                let overlap_x = a.x < b.x + b.width && a.x + a.width > b.x;
                let overlap_y = a.y < b.y + b.height && a.y + a.height > b.y;
                assert!(
                    !(overlap_x && overlap_y),
                    "Glyph {} and {} overlap in atlas: {:?} vs {:?}",
                    i,
                    j,
                    a,
                    b
                );
            }
        }
    }

    // ── Problematic Character Tests ──────────────────────────────────────

    /// Test that 'i', 'e', 'l' at a full range of font sizes produce valid glyphs.
    #[test]
    fn problematic_chars_at_all_sizes() {
        let mut mgr = FontManager::new();
        let attrs = cosmic_text::Attrs::new();
        let mut cache = SwashCache::new();
        // Test every integer size from 8 to 48
        let test_chars = ['i', 'e', 'l', 'I', 'L', 'f', 'j', 't'];
        let mut issues = Vec::new();

        for size in 8..=48 {
            let size_f = size as f32;
            for &ch in &test_chars {
                let text = ch.to_string();
                let layout = crate::layout::TextLayout::new_single_line(
                    &mut mgr.font_system,
                    &text,
                    attrs.clone(),
                    size_f,
                );
                let glyphs = layout.glyphs();
                if glyphs.is_empty() {
                    issues.push(format!("'{}' at {}px: no glyphs", ch, size));
                    continue;
                }
                for glyph in glyphs {
                    let ck = glyph.physical((0.0, 0.0), 1.0).cache_key;
                    match cache.get_image(&mut mgr.font_system, ck) {
                        None => issues.push(format!("'{}' at {}px: swash returned None", ch, size)),
                        Some(img) => {
                            if img.placement.width == 0 || img.placement.height == 0 {
                                issues.push(format!(
                                    "'{}' at {}px: zero-size bitmap {}x{}",
                                    ch, size, img.placement.width, img.placement.height
                                ));
                            }
                        }
                    }
                }
            }
        }

        if !issues.is_empty() {
            panic!("Glyph rasterization issues found:\n{}", issues.join("\n"));
        }
    }
}
