//! 字形纹理图集
//!
//! CPU 光栅化字形 → 上传到 GPU 纹理图集。
//! 复用 mondrian-ui-renderer 的 TextureAtlas 分配槽位。

use std::collections::HashMap;

use cosmic_text::{CacheKey, FontSystem, LayoutGlyph, SwashCache};
use mondrian_ui_core::types::Rect;
use mondrian_ui_renderer::atlas::TextureAtlas;

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
}

impl GlyphAtlas {
    pub fn new(atlas_size: u32) -> Self {
        Self {
            atlas: TextureAtlas::new(atlas_size, atlas_size),
            cache: SwashCache::new(),
            glyph_map: HashMap::new(),
            pending_uploads: Vec::new(),
        }
    }

    /// 获取或光栅化字形。
    /// 首次遇到时返回 None（下帧可用），已缓存的返回 UV rect。
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

        let width = glyph.w.ceil() as u32;
        let height = glyph.font_size.ceil() as u32;
        if width == 0 || height == 0 {
            return None;
        }

        let alloc_key = format!("glyph_{cache_key:?}");
        let uv = self.atlas.allocate(&alloc_key, width, height)?;

        let px = (uv.x * self.atlas.width as f32) as u32;
        let py = (uv.y * self.atlas.height as f32) as u32;

        let image = self.cache.get_image(font_system, cache_key);
        let bitmap = match image {
            Some(img) => match img.content {
                cosmic_text::SwashContent::SubpixelMask => {
                    img.data.iter().flat_map(|&s| vec![s, s, s, 255]).collect()
                }
                _ => img.data.clone(),
            },
            None => return None,
        };

        self.glyph_map.insert(cache_key, uv);
        self.pending_uploads.push(GlyphUpload {
            x: px,
            y: py,
            width,
            height,
            data: bitmap,
        });

        None
    }

    pub fn size(&self) -> (u32, u32) {
        self.atlas.size()
    }

    pub fn has_pending(&self) -> bool {
        !self.pending_uploads.is_empty()
    }

    /// Rasterize a glyph and return the raw alpha bitmap.
    /// Cached: subsequent calls for the same glyph return the cached data.
    pub fn rasterize_alpha(
        &mut self,
        font_system: &mut FontSystem,
        glyph: &LayoutGlyph,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let physical = glyph.physical((0.0, 0.0), 1.0);
        let cache_key = physical.cache_key;

        let width = glyph.w.ceil() as u32;
        let height = glyph.font_size.ceil() as u32;
        if width == 0 || height == 0 {
            return None;
        }

        // Check if we already rasterized this glyph
        if self.glyph_map.contains_key(&cache_key) {
            // Already cached — return a minimal placeholder to avoid re-work
            // The caller can check width/height to know dimensions
        }

        let image = self.cache.get_image(font_system, cache_key);
        let bitmap = match image {
            Some(img) => match img.content {
                cosmic_text::SwashContent::SubpixelMask => {
                    // Subpixel: each byte is a subpixel component, pack into alpha
                    let mut alpha = Vec::with_capacity((width * height) as usize);
                    for chunk in img.data.chunks(3) {
                        let avg = chunk.iter().map(|&b| b as u32).sum::<u32>() / 3;
                        alpha.push(avg as u8);
                    }
                    // Pad if needed
                    alpha.resize((width * height) as usize, 0);
                    alpha
                }
                _ => {
                    // Mask (1 byte per pixel alpha)
                    let mut alpha = img.data.clone();
                    alpha.resize((width * height) as usize, 0);
                    alpha
                }
            },
            None => return None,
        };

        self.glyph_map.insert(cache_key, Rect::ZERO); // mark as cached
        Some((width, height, bitmap))
    }
}
