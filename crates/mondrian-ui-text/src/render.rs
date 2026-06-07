//! 文本渲染器
//!
//! 将文字排版并生成 DrawCommand::Image 列表（每个字形指向 GlyphAtlas UV）。

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_renderer::command::DrawCommand;

use crate::atlas::GlyphAtlas;
use crate::font::FontManager;
use crate::layout::TextLayout;

pub struct TextRenderer {
    pub font_manager: FontManager,
    pub atlas: GlyphAtlas,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            font_manager: FontManager::new(),
            atlas: GlyphAtlas::new(2048),
        }
    }

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
                commands.push(DrawCommand::Image {
                    bounds: Rect::new(x, y, glyph.w, glyph.font_size),
                    uv_rect,
                    tint: color,
                });
            }
        }
        commands
    }

    pub fn measure_text(&mut self, text: &str, font_size: f32) -> (f32, f32) {
        let attrs = cosmic_text::Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout = TextLayout::new_single_line(
            &mut self.font_manager.font_system, text, attrs, font_size,
        );
        layout.size()
    }

    /// Take pending uploads for GPU texture upload
    pub fn take_pending_uploads(&mut self) -> Vec<crate::atlas::GlyphUpload> {
        std::mem::take(&mut self.atlas.pending_uploads)
    }
}

/// 后处理：将 DrawCommand::Text 替换为字形 DrawCommand::Image
///
/// 在每个渲染帧调用 `TreeWalker::paint()` 之后、`UiRenderer::render()` 之前使用。
/// 首次出现的字形本帧不显示（下帧 atlas 上传 GPU 后可见）。
pub fn resolve_text_commands(
    commands: Vec<DrawCommand>,
    text_renderer: &mut TextRenderer,
) -> Vec<DrawCommand> {
    let mut resolved = Vec::with_capacity(commands.len());
    for cmd in commands {
        match cmd {
            DrawCommand::Text { text, style, position, color } => {
                let glyph_cmds = text_renderer.layout_and_render(
                    &text, style.font_size, position, color, None,
                );
                resolved.extend(glyph_cmds);
            }
            _ => resolved.push(cmd),
        }
    }
    resolved
}

impl Default for TextRenderer {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_renderer_creates() { let _r = TextRenderer::new(); }

    #[test]
    fn text_renderer_default_creates() { let _r = TextRenderer::default(); }

    #[test]
    fn measure_text_returns_positive() {
        let mut r = TextRenderer::new();
        let (w, h) = r.measure_text("Hello", 16.0);
        assert!(w > 0.0); assert!(h > 0.0);
    }

    #[test]
    fn measure_empty_text() {
        let mut r = TextRenderer::new();
        let (w, _h) = r.measure_text("", 16.0);
        assert_eq!(w, 0.0);
    }

    #[test]
    fn layout_and_render_empty_text() {
        let mut r = TextRenderer::new();
        let cmds = r.layout_and_render("", 16.0, Point::ZERO, Color::WHITE, None);
        assert!(cmds.is_empty());
    }
}
