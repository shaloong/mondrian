//! 文本渲染器
//!
//! 将文字排版并生成 DrawCommand::Image 列表（每个字形一个 quad，指向 GlyphAtlas UV）。

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::command::DrawCommand;

use crate::atlas::GlyphAtlas;
use crate::font::FontManager;
use crate::layout::TextLayout;

pub struct TextRenderer {
    font_manager: FontManager,
    atlas: GlyphAtlas,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            font_manager: FontManager::new(),
            atlas: GlyphAtlas::new(2048),
        }
    }

    /// 排版文字并生成 DrawCommand 列表。
    /// 首次渲染的字形本帧不显示（返回无命令），下帧图集上传后可见。
    pub fn layout_and_render(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        color: Color,
        max_width: Option<f32>,
    ) -> Vec<DrawCommand> {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);

        let font_system = &mut self.font_manager.font_system;
        let layout = if let Some(w) = max_width {
            TextLayout::new_multiline(font_system, text, attrs, font_size, w)
        } else {
            TextLayout::new_single_line(font_system, text, attrs, font_size)
        };

        let mut commands = Vec::new();

        for glyph in layout.glyphs() {
            let uv = self.atlas.get_or_rasterize(font_system, &glyph);

            if let Some(uv_rect) = uv {
                let x = position.x + glyph.x;
                let y = position.y + glyph.y;
                let bounds = Rect::new(x, y, glyph.w, glyph.font_size);

                commands.push(DrawCommand::Image {
                    bounds,
                    uv_rect,
                    tint: color,
                });
            }
        }

        commands
    }

    /// 测量单行文本的像素尺寸（宽度, 高度）
    pub fn measure_text(&mut self, text: &str, font_size: f32) -> (f32, f32) {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout = TextLayout::new_single_line(
            &mut self.font_manager.font_system,
            text,
            attrs,
            font_size,
        );
        layout.size()
    }

    pub fn atlas(&self) -> &GlyphAtlas {
        &self.atlas
    }

    pub fn atlas_mut(&mut self) -> &mut GlyphAtlas {
        &mut self.atlas
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renderer_creates() {
        let _r = TextRenderer::new();
    }

    #[test]
    fn text_renderer_default_creates() {
        let _r = TextRenderer::default();
    }

    #[test]
    fn measure_text_returns_positive() {
        let mut r = TextRenderer::new();
        let (w, h) = r.measure_text("Hello", 16.0);
        assert!(w > 0.0);
        assert!(h > 0.0);
    }

    #[test]
    fn measure_empty_text() {
        let mut r = TextRenderer::new();
        let (w, _h) = r.measure_text("", 16.0);
        assert_eq!(w, 0.0);
        // height may still be > 0 due to line metrics
    }

    #[test]
    fn layout_and_render_empty_text() {
        let mut r = TextRenderer::new();
        let cmds = r.layout_and_render("", 16.0, Point::ZERO, Color::WHITE, None);
        assert!(cmds.is_empty());
    }

    #[test]
    fn layout_and_render_non_empty_text() {
        let mut r = TextRenderer::new();
        let cmds = r.layout_and_render("Hello", 16.0, Point::new(0.0, 0.0), Color::WHITE, None);
        // First frame: glyphs not yet rasterized, so they won't have UV coords
        // This is expected behavior — get_or_rasterize returns None on first encounter
        // So we just verify no panic occurs
        let _ = cmds;
    }

    #[test]
    fn atlas_accessible() {
        let r = TextRenderer::new();
        let size = r.atlas().size();
        assert_eq!(size, (2048, 2048));
    }

    #[test]
    fn atlas_mut_accessible() {
        let mut r = TextRenderer::new();
        let size = r.atlas_mut().size();
        assert_eq!(size, (2048, 2048));
    }
}
