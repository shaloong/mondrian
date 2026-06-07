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
            if let Some((uv_rect, bmp_w, bmp_h)) = self.atlas.get_or_rasterize(font_system, &glyph) {
                let x = position.x + glyph.x;
                let y = position.y + glyph.y;
                commands.push(DrawCommand::Image {
                    bounds: Rect::new(x, y, bmp_w as f32, bmp_h as f32),
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

    #[test]
    fn different_chars_have_different_uvs() {
        let mut r = TextRenderer::new();
        // First frame: rasterizes all glyphs (returns empty)
        let _ = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        // Upload happens between frames (simulated by pending_uploads being present)
        assert!(!r.atlas.pending_uploads.is_empty());
        // Second frame: should return cached UVs
        let cmds = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);

        // Should have at least 2 Image commands (one per character)
        assert!(cmds.len() >= 2, "Expected 2+ glyphs for 'AB', got {}", cmds.len());

        // Extract UV rects from the Image commands
        let mut uvs = Vec::new();
        for cmd in &cmds {
            if let DrawCommand::Image { uv_rect, .. } = cmd {
                uvs.push(*uv_rect);
            }
        }

        // Different characters MUST have different UV rects
        // (if they're the same, all chars are mapping to the same glyph = bug)
        if uvs.len() >= 2 {
            let (r1, r2) = (uvs[0], uvs[1]);
            let same = (r1.x - r2.x).abs() < 0.0001
                && (r1.y - r2.y).abs() < 0.0001
                && (r1.width - r2.width).abs() < 0.0001
                && (r1.height - r2.height).abs() < 0.0001;
            assert!(!same,
                "Characters 'A' and 'B' mapped to the SAME UV rect {:?} — glyph differentiation broken", r1);
        }
    }

    #[test]
    fn upload_position_matches_uv_rect() {
        let mut r = TextRenderer::new();
        // First frame: rasterize
        let _ = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        let uploads = r.take_pending_uploads();
        assert!(!uploads.is_empty());

        // Second frame: get cached UVs
        let cmds = r.layout_and_render("AB", 24.0, Point::ZERO, Color::WHITE, None);
        let mut uv_rects = Vec::new();
        for cmd in &cmds {
            if let DrawCommand::Image { uv_rect, .. } = cmd {
                uv_rects.push(*uv_rect);
            }
        }
        assert!(!uv_rects.is_empty(), "Second frame should produce Image commands");

        // Each upload's position should match a UV rect
        // Upload pixel position → UV = px/2048, py/2048
        // But note: UVs from the atlas include padding offset,
        // and uploads write to the padded position
        for upload in &uploads {
            let expected_u = upload.x as f32 / 2048.0;
            let expected_v = upload.y as f32 / 2048.0;
            let found = uv_rects.iter().any(|uv| {
                (uv.x - expected_u).abs() < 0.001 && (uv.y - expected_v).abs() < 0.001
            });
            assert!(found,
                "Upload at ({},{}) has no matching UV rect among {:?}",
                upload.x, upload.y, uv_rects);
        }
    }

    #[test]
    fn consecutive_chars_have_consecutive_uvs() {
        let mut r = TextRenderer::new();
        let _ = r.layout_and_render("ABC", 24.0, Point::ZERO, Color::WHITE, None);
        let _ = r.take_pending_uploads();
        let cmds = r.layout_and_render("ABC", 24.0, Point::ZERO, Color::WHITE, None);

        let mut uvs = Vec::new();
        for cmd in &cmds {
            if let DrawCommand::Image { uv_rect, .. } = cmd {
                uvs.push(*uv_rect);
            }
        }
        assert!(uvs.len() >= 3, "Expected 3 glyphs, got {}", uvs.len());

        // UVs for consecutive chars should be distinct
        assert_ne!(uvs[0], uvs[1], "A and B should have different UVs");
        assert_ne!(uvs[1], uvs[2], "B and C should have different UVs");
        assert_ne!(uvs[0], uvs[2], "A and C should have different UVs");
    }

    #[test]
    fn text_commands_produce_non_empty_bitmaps() {
        let mut r = TextRenderer::new();
        // Trigger rasterization
        let _ = r.layout_and_render("Test", 16.0, Point::ZERO, Color::WHITE, None);
        let uploads = r.take_pending_uploads();
        assert!(!uploads.is_empty(), "Should have pending uploads after rasterization");

        for u in &uploads {
            let non_zero = u.data.iter().filter(|&&b| b > 0).count();
            assert!(non_zero > 0,
                "Glyph {}x{} bitmap is all zeros!", u.width, u.height);
        }
    }
}
