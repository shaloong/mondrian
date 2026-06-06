//! 文本排版
//!
//! 基于 cosmic-text 0.19 Buffer 的单行/多行排版。

use cosmic_text::{Align, Attrs, Buffer, FontSystem, Metrics, Shaping, Wrap};

pub struct TextLayout {
    buffer: Buffer,
}

impl TextLayout {
    pub fn new_single_line(
        font_system: &mut FontSystem,
        text: &str,
        attrs: Attrs<'_>,
        font_size: f32,
    ) -> Self {
        let line_height = font_size * 1.3;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(Align::Left));
        buffer.shape_until_scroll(font_system, false);

        Self { buffer }
    }

    pub fn new_multiline(
        font_system: &mut FontSystem,
        text: &str,
        attrs: Attrs<'_>,
        font_size: f32,
        max_width: f32,
    ) -> Self {
        let line_height = font_size * 1.3;
        let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
        buffer.set_size(Some(max_width), None);
        buffer.set_wrap(Wrap::Word);
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(Align::Left));
        buffer.shape_until_scroll(font_system, false);

        Self { buffer }
    }

    pub fn size(&self) -> (f32, f32) {
        let mut w = 0.0f32;
        let mut h = 0.0f32;
        for run in self.buffer.layout_runs() {
            w = w.max(run.line_w);
            h += run.line_height;
        }
        (w, h)
    }

    pub fn line_height(&self) -> f32 {
        self.buffer.metrics().line_height
    }

    /// 收集所有已排版的 LayoutGlyph 副本
    pub fn glyphs(&self) -> Vec<cosmic_text::LayoutGlyph> {
        self.buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.to_vec())
            .collect()
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::FontManager;

    fn test_attrs() -> Attrs<'static> {
        Attrs::new()
    }

    #[test]
    fn single_line_has_non_zero_size() {
        let mut mgr = FontManager::new();
        let attrs = test_attrs();
        let layout =
            TextLayout::new_single_line(&mut mgr.font_system, "Hello World", attrs, 16.0);
        let (w, h) = layout.size();
        assert!(w > 0.0, "width should be > 0");
        assert!(h > 0.0, "height should be > 0");
    }

    #[test]
    fn multiline_wraps_long_text() {
        let mut mgr = FontManager::new();
        let attrs = test_attrs();
        let layout = TextLayout::new_multiline(
            &mut mgr.font_system,
            "Hello World This Is Long Text",
            attrs,
            16.0,
            100.0,
        );
        let (w, h) = layout.size();
        assert!(w > 0.0);
        assert!(h > 0.0);
    }

    #[test]
    fn line_height_matches_font_size() {
        let mut mgr = FontManager::new();
        let attrs = test_attrs();
        let layout =
            TextLayout::new_single_line(&mut mgr.font_system, "Test", attrs, 20.0);
        let lh = layout.line_height();
        // line_height should be approximately font_size * 1.3
        assert!(lh > 20.0);
    }

    #[test]
    fn empty_text_has_no_glyphs() {
        let mut mgr = FontManager::new();
        let attrs = test_attrs();
        let layout = TextLayout::new_single_line(&mut mgr.font_system, "", attrs, 16.0);
        let glyphs = layout.glyphs();
        assert!(glyphs.is_empty());
    }

    #[test]
    fn different_font_sizes_produce_different_line_heights() {
        let mut mgr = FontManager::new();
        let attrs = test_attrs();
        let small = TextLayout::new_single_line(&mut mgr.font_system, "Test", attrs.clone(), 12.0);
        let large = TextLayout::new_single_line(&mut mgr.font_system, "Test", attrs, 24.0);
        assert!(large.line_height() > small.line_height());
    }
}
