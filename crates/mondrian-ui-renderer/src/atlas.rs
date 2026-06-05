//! 纹理图集
//!
//! 将多个小纹理打包到一个大纹理中，减少 bind group 切换。
//! Stage B 提供最小实现，后续扩展 glyph atlas 支持。

use std::collections::HashMap;

use glam::Vec2;
use mondrian_ui_core::types::Rect;

/// 图集中的一个条目
#[derive(Debug, Clone)]
pub struct AtlasEntry {
    pub rect: Rect,
    pub allocated: bool,
}

/// 简易纹理图集
///
/// 采用固定尺寸、固定槽位的简单方案。
/// 后续可以用 guillotine 或 skyline 算法替换。
#[derive(Debug, Clone)]
pub struct TextureAtlas {
    pub width: u32,
    pub height: u32,
    entries: HashMap<String, AtlasEntry>,
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

    /// 分配一个图集槽位，返回其 UV 坐标
    pub fn allocate(&mut self, key: &str, item_width: u32, item_height: u32) -> Option<Rect> {
        if self.entries.contains_key(key) {
            return self.entries.get(key).map(|e| e.rect);
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

        let x = self.next_x as f32 / self.width as f32;
        let y = self.next_y as f32 / self.height as f32;
        let w = item_width as f32 / self.width as f32;
        let h = item_height as f32 / self.height as f32;

        let entry = AtlasEntry {
            rect: Rect::new(x, y, w, h),
            allocated: true,
        };

        self.entries.insert(key.to_string(), entry);
        self.next_x += item_width;
        self.row_height = self.row_height.max(item_height);

        Some(Rect::new(x, y, w, h))
    }

    /// 查询某个条目
    pub fn get(&self, key: &str) -> Option<Rect> {
        self.entries.get(key).map(|e| e.rect)
    }

    /// 图集总尺寸
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}
