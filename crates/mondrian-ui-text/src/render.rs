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
    /// 首次渲染的字形本帧不显示（返回空），下帧图集上传后可见。
    pub fn layout_and_render(
        &mut self,
        text: &str,
        font_size: f32,
        position: Point,
        color: Color,
        max_width: Option<f32>,
    ) -> Vec<DrawCommand> {
        // 先获取 attrs 的副本（不持有 font_system 的引用）
        let attrs = {
            let mgr = &self.font_manager;
            Attrs::new()
                .family(cosmic_text::Family::SansSerif)
                .weight(cosmic_text::Weight::NORMAL)
        };

        let font_system = &mut self.font_manager.font_system;
        let layout = if let Some(w) = max_width {
            TextLayout::new_multiline(font_system, text, attrs, w)
        } else {
            TextLayout::new_single_line(font_system, text, attrs)
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
        let attrs = Attrs::new()
            .family(cosmic_text::Family::SansSerif)
            .weight(cosmic_text::Weight::NORMAL);
        let layout = TextLayout::new_single_line(
            &mut self.font_manager.font_system,
            text,
            attrs,
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

use cosmic_text::Attrs;

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}
