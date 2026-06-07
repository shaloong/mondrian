//! 文本排版
//!
//! 基于 cosmic-text 0.19 Buffer 的单行/多行排版。

use cosmic_text::{Align, Attrs, Buffer, FontSystem, LayoutGlyph, LayoutRun, Metrics, Shaping, Wrap};

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
        buffer.shape_until_scroll(font_system, true);
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
        buffer.shape_until_scroll(font_system, true);
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

    /// Iterate layout runs with their line_y offset.
    /// Each run contains glyphs positioned relative to the run's line.
    pub fn runs(&self) -> Vec<LayoutRun<'_>> {
        self.buffer.layout_runs().collect()
    }

    /// Collect glyphs with their run context for proper baseline positioning.
    /// Returns (line_y, glyph) pairs.
    pub fn positioned_glyphs(&self) -> Vec<(f32, &LayoutGlyph)> {
        let mut result = Vec::new();
        for run in self.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                result.push((run.line_y, glyph));
            }
        }
        result
    }

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
