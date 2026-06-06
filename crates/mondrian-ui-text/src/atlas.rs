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

        let mut bitmap = vec![0u8; (width * height) as usize];
        let _ = self.cache.get_image(font_system, cache_key);

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
}
