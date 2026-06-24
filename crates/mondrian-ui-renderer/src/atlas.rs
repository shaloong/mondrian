//! 纹理图集
//!
//! 将多个小纹理打包到一个大纹理中，减少 bind group 切换。
//! 由 glyph atlas 和通用 raster image atlas 共享。

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

/// Runtime diagnostics for a [`TextureAtlas`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureAtlasStats {
    /// Atlas width in pixels.
    pub width: u32,
    /// Atlas height in pixels.
    pub height: u32,
    /// Number of cached entries.
    pub entries: usize,
    /// Number of free rectangles tracked by the allocator.
    pub free_rects: usize,
    /// Total pixels covered by successful allocations.
    pub used_pixels: u64,
    /// Total atlas pixels.
    pub total_pixels: u64,
    /// `used_pixels / total_pixels`, or `0.0` for a zero-sized atlas.
    pub occupancy: f32,
    /// Area of the largest currently tracked free rectangle.
    pub largest_free_rect_pixels: u64,
    /// Number of allocation requests rejected because the item could not fit.
    pub failed_allocations: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FreeRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl FreeRect {
    fn area(self) -> u64 {
        self.width as u64 * self.height as u64
    }

    fn can_fit(self, width: u32, height: u32) -> bool {
        width <= self.width && height <= self.height
    }

    fn contains(self, other: FreeRect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x.saturating_add(other.width) <= self.x.saturating_add(self.width)
            && other.y.saturating_add(other.height) <= self.y.saturating_add(self.height)
    }
}

/// Fixed-size texture atlas with guillotine-style free rectangle packing.
///
/// The atlas does not own GPU memory and does not evict entries. It is a deterministic
/// allocator used by glyph and raster image caches. Higher-level renderer code is
/// responsible for page/generation policies when a single page is exhausted.
#[derive(Debug, Clone)]
pub struct TextureAtlas {
    pub width: u32,
    pub height: u32,
    entries: HashMap<String, AtlasAllocation>,
    free_rects: Vec<FreeRect>,
    used_pixels: u64,
    failed_allocations: u64,
}

impl TextureAtlas {
    /// Create an empty fixed-size atlas.
    pub fn new(width: u32, height: u32) -> Self {
        let free_rects = if width == 0 || height == 0 {
            Vec::new()
        } else {
            vec![FreeRect { x: 0, y: 0, width, height }]
        };
        Self {
            width,
            height,
            entries: HashMap::new(),
            free_rects,
            used_pixels: 0,
            failed_allocations: 0,
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

        if item_width == 0
            || item_height == 0
            || item_width > self.width
            || item_height > self.height
        {
            self.failed_allocations = self.failed_allocations.saturating_add(1);
            return None;
        }

        let rect_index = match self.best_free_rect_index(item_width, item_height) {
            Some(index) => index,
            None => {
                self.failed_allocations = self.failed_allocations.saturating_add(1);
                return None;
            }
        };
        let free = self.free_rects.swap_remove(rect_index);

        let allocation = AtlasAllocation {
            x: free.x,
            y: free.y,
            width: item_width,
            height: item_height,
        };

        self.split_free_rect(free, item_width, item_height);
        self.merge_adjacent_free_rects();
        self.prune_contained_free_rects();

        self.entries.insert(key.to_string(), allocation);
        self.used_pixels = self.used_pixels.saturating_add(item_width as u64 * item_height as u64);

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

    /// Return allocator diagnostics useful for renderer telemetry.
    pub fn stats(&self) -> TextureAtlasStats {
        let total_pixels = self.width as u64 * self.height as u64;
        let largest_free_rect_pixels =
            self.free_rects.iter().map(|rect| rect.area()).max().unwrap_or(0);
        TextureAtlasStats {
            width: self.width,
            height: self.height,
            entries: self.entries.len(),
            free_rects: self.free_rects.len(),
            used_pixels: self.used_pixels,
            total_pixels,
            occupancy: if total_pixels == 0 {
                0.0
            } else {
                self.used_pixels as f32 / total_pixels as f32
            },
            largest_free_rect_pixels,
            failed_allocations: self.failed_allocations,
        }
    }

    fn best_free_rect_index(&self, width: u32, height: u32) -> Option<usize> {
        let mut best: Option<(usize, u64, u32)> = None;
        for (index, rect) in self.free_rects.iter().copied().enumerate() {
            if !rect.can_fit(width, height) {
                continue;
            }
            let waste = rect.area().saturating_sub(width as u64 * height as u64);
            let short_side = (rect.width - width).min(rect.height - height);
            match best {
                Some((_, best_waste, best_short_side))
                    if waste > best_waste
                        || (waste == best_waste && short_side >= best_short_side) => {}
                _ => best = Some((index, waste, short_side)),
            }
        }
        best.map(|(index, _, _)| index)
    }

    fn split_free_rect(&mut self, free: FreeRect, width: u32, height: u32) {
        let remaining_w = free.width - width;
        let remaining_h = free.height - height;

        if remaining_w == 0 && remaining_h == 0 {
            return;
        }

        // Guillotine split. Pick the split that gives the larger remainder a full strip,
        // which tends to preserve useful rectangular space for later allocations.
        if remaining_w >= remaining_h {
            self.push_free_rect(FreeRect {
                x: free.x + width,
                y: free.y,
                width: remaining_w,
                height: free.height,
            });
            self.push_free_rect(FreeRect {
                x: free.x,
                y: free.y + height,
                width,
                height: remaining_h,
            });
        } else {
            self.push_free_rect(FreeRect {
                x: free.x + width,
                y: free.y,
                width: remaining_w,
                height,
            });
            self.push_free_rect(FreeRect {
                x: free.x,
                y: free.y + height,
                width: free.width,
                height: remaining_h,
            });
        }
    }

    fn push_free_rect(&mut self, rect: FreeRect) {
        if rect.width > 0 && rect.height > 0 {
            self.free_rects.push(rect);
        }
    }

    fn merge_adjacent_free_rects(&mut self) {
        loop {
            let mut merged = false;
            'outer: for i in 0..self.free_rects.len() {
                for j in i + 1..self.free_rects.len() {
                    let a = self.free_rects[i];
                    let b = self.free_rects[j];
                    if a.y == b.y && a.height == b.height && a.x.saturating_add(a.width) == b.x {
                        self.free_rects[i] = FreeRect {
                            x: a.x,
                            y: a.y,
                            width: a.width + b.width,
                            height: a.height,
                        };
                        self.free_rects.swap_remove(j);
                        merged = true;
                        break 'outer;
                    }
                    if b.y == a.y && b.height == a.height && b.x.saturating_add(b.width) == a.x {
                        self.free_rects[i] = FreeRect {
                            x: b.x,
                            y: b.y,
                            width: b.width + a.width,
                            height: b.height,
                        };
                        self.free_rects.swap_remove(j);
                        merged = true;
                        break 'outer;
                    }
                    if a.x == b.x && a.width == b.width && a.y.saturating_add(a.height) == b.y {
                        self.free_rects[i] = FreeRect {
                            x: a.x,
                            y: a.y,
                            width: a.width,
                            height: a.height + b.height,
                        };
                        self.free_rects.swap_remove(j);
                        merged = true;
                        break 'outer;
                    }
                    if b.x == a.x && b.width == a.width && b.y.saturating_add(b.height) == a.y {
                        self.free_rects[i] = FreeRect {
                            x: b.x,
                            y: b.y,
                            width: b.width,
                            height: b.height + a.height,
                        };
                        self.free_rects.swap_remove(j);
                        merged = true;
                        break 'outer;
                    }
                }
            }
            if !merged {
                break;
            }
        }
    }

    fn prune_contained_free_rects(&mut self) {
        let mut i = 0;
        while i < self.free_rects.len() {
            let rect = self.free_rects[i];
            let contained = self
                .free_rects
                .iter()
                .enumerate()
                .any(|(other_index, other)| other_index != i && other.contains(rect));
            if contained {
                self.free_rects.swap_remove(i);
            } else {
                i += 1;
            }
        }
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
        atlas.allocate_pixels("a", 200, 50).unwrap();
        let allocation = atlas.allocate_pixels("b", 100, 50);
        assert!(allocation.is_some(), "Should reuse tracked free space");
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

    #[test]
    fn atlas_reuses_two_dimensional_remainder_space() {
        let mut atlas = TextureAtlas::new(100, 100);

        assert_eq!(
            atlas.allocate_pixels("a", 60, 60),
            Some(AtlasAllocation { x: 0, y: 0, width: 60, height: 60 })
        );
        let second = atlas.allocate_pixels("b", 40, 40);
        assert!(second.is_some());

        let allocation = atlas.allocate_pixels("c", 40, 60);
        assert!(allocation.is_some());
        assert_eq!(atlas.entry_count(), 3);
    }

    #[test]
    fn atlas_stats_report_occupancy_and_failures() {
        let mut atlas = TextureAtlas::new(100, 100);
        atlas.allocate_pixels("a", 25, 20).unwrap();
        assert!(atlas.allocate_pixels("zero", 0, 20).is_none());
        assert!(atlas.allocate_pixels("oversized", 101, 1).is_none());

        let stats = atlas.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.used_pixels, 500);
        assert_eq!(stats.total_pixels, 10_000);
        assert!((stats.occupancy - 0.05).abs() < f32::EPSILON);
        assert_eq!(stats.failed_allocations, 2);
        assert!(stats.largest_free_rect_pixels > 0);
    }

    #[test]
    fn atlas_zero_sized_never_allocates_and_reports_zero_occupancy() {
        let mut atlas = TextureAtlas::new(0, 0);
        assert!(atlas.allocate_pixels("a", 1, 1).is_none());

        let stats = atlas.stats();
        assert_eq!(stats.total_pixels, 0);
        assert_eq!(stats.occupancy, 0.0);
        assert_eq!(stats.failed_allocations, 1);
    }
}
