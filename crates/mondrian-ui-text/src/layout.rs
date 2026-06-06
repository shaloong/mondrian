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
    ) -> Self {
        let mut buffer = Buffer::new(font_system, Metrics::new(16.0, 20.0));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, Some(Align::Left));
        buffer.shape_until_scroll(font_system, false);

        Self { buffer }
    }

    pub fn new_multiline(
        font_system: &mut FontSystem,
        text: &str,
        attrs: Attrs<'_>,
        max_width: f32,
    ) -> Self {
        let mut buffer = Buffer::new(font_system, Metrics::new(16.0, 20.0));
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
