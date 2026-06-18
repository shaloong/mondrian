//! 纹理图集
//!
//! 将多个小纹理打包到一个大纹理中，减少 bind group 切换。
//! Stage B 提供最小实现，后续扩展 glyph atlas 支持。

use std::collections::HashMap;

use mondrian_ui_core::types::Rect;

/// Pixel-space allocation returned by [`TextureAtlas`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasAllocation {
    /// Left pixel coordinate in the atlas texture.
    pub x: u32,
    /// Top pixel coordinate in the atlas texture.
    pub y: u32,
    /// Allocated width in pixels.
    pub width: u32,
    /// Allocated height in pixels.
    pub height: u32,
}

impl AtlasAllocation {
    /// Convert this allocation to normalized atlas UV coordinates.
    pub fn uv_rect(self, atlas_width: u32, atlas_height: u32) -> Rect {
        Rect::new(
            self.x as f32 / atlas_width as f32,
            self.y as f32 / atlas_height as f32,
            self.width as f32 / atlas_width as f32,
            self.height as f32 / atlas_height as f32,
        )
    }
}

/// 简易纹理图集
///
/// 采用固定尺寸、固定槽位的简单方案。
/// 后续可以用 guillotine 或 skyline 算法替换。
#[derive(Debug, Clone)]
pub struct TextureAtlas {
    pub width: u32,
    pub height: u32,
    entries: HashMap<String, AtlasAllocation>,
    next_x: u32,
    next_y: u32,
    row_height: u32,
}

impl TextureAtlas {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            entries: HashMap::new(),
            next_x: 0,
            next_y: 0,
            row_height: 0,
        }
    }

    /// 分配一个图集槽位，返回其像素坐标。
    pub fn allocate_pixels(
        &mut self,
        key: &str,
        item_width: u32,
        item_height: u32,
    ) -> Option<AtlasAllocation> {
        if self.entries.contains_key(key) {
            return self.entries.get(key).copied();
        }

        // 简单行式打包
        if self.next_x + item_width > self.width {
            self.next_x = 0;
            self.next_y += self.row_height;
            self.row_height = 0;
        }

        if self.next_y + item_height > self.height {
            return None; // 图集已满
        }

        let allocation = AtlasAllocation {
            x: self.next_x,
            y: self.next_y,
            width: item_width,
            height: item_height,
        };

        self.entries.insert(key.to_string(), allocation);
        self.next_x += item_width;
        self.row_height = self.row_height.max(item_height);

        Some(allocation)
    }

    /// 查询某个条目
    pub fn get_pixels(&self, key: &str) -> Option<AtlasAllocation> {
        self.entries.get(key).copied()
    }

    /// 图集总尺寸
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_new_is_empty() {
        let atlas = TextureAtlas::new(1024, 1024);
        assert_eq!(atlas.size(), (1024, 1024));
        assert_eq!(atlas.entry_count(), 0);
    }

    #[test]
    fn atlas_allocation_can_convert_to_uv_coords() {
        let mut atlas = TextureAtlas::new(1024, 1024);
        let uv = atlas
            .allocate_pixels("test", 256, 256)
            .unwrap()
            .uv_rect(atlas.width, atlas.height);
        // UV should be in [0, 1] range
        assert!(uv.x >= 0.0 && uv.x <= 1.0);
        assert!(uv.y >= 0.0 && uv.y <= 1.0);
        assert!(uv.width > 0.0 && uv.width <= 1.0);
        assert!(uv.height > 0.0 && uv.height <= 1.0);
    }

    #[test]
    fn atlas_allocate_pixels_returns_exact_pixel_coords() {
        let mut atlas = TextureAtlas::new(1024, 1024);
        let first = atlas.allocate_pixels("a", 17, 19).unwrap();
        let second = atlas.allocate_pixels("b", 23, 29).unwrap();

        assert_eq!(first, AtlasAllocation { x: 0, y: 0, width: 17, height: 19 });
        assert_eq!(
            second,
            AtlasAllocation { x: 17, y: 0, width: 23, height: 29 }
        );
        assert_eq!(Some(first), atlas.get_pixels("a"));
    }

    #[test]
    fn atlas_allocate_multiple_items() {
        let mut atlas = TextureAtlas::new(1024, 1024);
        for i in 0..4 {
            let allocation = atlas.allocate_pixels(&format!("item_{i}"), 256, 256);
            assert!(allocation.is_some());
        }
        assert_eq!(atlas.entry_count(), 4);
    }

    #[test]
    fn atlas_allocate_same_key_returns_cached() {
        let mut atlas = TextureAtlas::new(1024, 1024);
        let first = atlas.allocate_pixels("same", 128, 128).unwrap();
        let second = atlas.allocate_pixels("same", 256, 256).unwrap(); // different size, same key
        assert_eq!(first, second);
        assert_eq!(atlas.entry_count(), 1);
    }

    #[test]
    fn atlas_get_returns_pixels_for_existing() {
        let mut atlas = TextureAtlas::new(1024, 1024);
        let allocation = atlas.allocate_pixels("glyph", 64, 64).unwrap();
        assert_eq!(Some(allocation), atlas.get_pixels("glyph"));
    }

    #[test]
    fn atlas_get_returns_none_for_missing() {
        let atlas = TextureAtlas::new(1024, 1024);
        assert!(atlas.get_pixels("missing").is_none());
    }

    #[test]
    fn atlas_row_wraps_when_full_width() {
        let mut atlas = TextureAtlas::new(256, 512);
        // First item takes 200px → next_x = 200
        atlas.allocate_pixels("a", 200, 50).unwrap();
        // Second item (100px) won't fit in remaining 56px → wraps to next row
        let allocation = atlas.allocate_pixels("b", 100, 50);
        assert!(allocation.is_some(), "Should wrap to next row");
        assert_eq!(atlas.entry_count(), 2);
    }

    #[test]
    fn atlas_returns_none_when_full() {
        let mut atlas = TextureAtlas::new(64, 64);
        // Fill the atlas with one item
        let allocation = atlas.allocate_pixels("big", 64, 64);
        assert!(allocation.is_some());
        // Next item should fail
        let overflow = atlas.allocate_pixels("overflow", 1, 1);
        assert!(overflow.is_none());
    }
}
