//! Shared widget painting helpers.

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;
use mondrian_ui_theme::spacing::ShadowToken;

const CHECKERBOARD_MAX_CELL: f32 = 7.0;

pub(crate) fn paint_shadow(ctx: &mut PaintContext, bounds: Rect, radius: f32) {
    let shadow = &ctx.theme.spacing.shadow_md;
    ctx.encoder.draw_rect(shadow_rect(bounds, shadow), shadow_color(shadow), radius);
}

pub(crate) fn paint_checkerboard(ctx: &mut PaintContext, rect: Rect, cell_size: f32, radius: f32) {
    let vertices = checkerboard_vertices(rect, cell_size);
    ctx.encoder.draw_colored_triangles_in_rect(&vertices, rect, radius);
}

pub(crate) fn checkerboard_colors() -> (Color, Color) {
    (
        Color { r: 0.75, g: 0.75, b: 0.78, a: 1.0 },
        Color { r: 0.48, g: 0.48, b: 0.52, a: 1.0 },
    )
}

pub(crate) fn checkerboard_vertices(rect: Rect, cell_size: f32) -> Vec<(Point, Color)> {
    let checker = checkerboard_cell_size(cell_size);
    let cols = (rect.width / checker).ceil() as i32;
    let rows = (rect.height / checker).ceil() as i32;
    let (light, dark) = checkerboard_colors();
    let mut vertices = Vec::with_capacity((cols * rows).max(0) as usize * 6);
    for row in 0..rows {
        for col in 0..cols {
            let x = rect.x + col as f32 * checker;
            let y = rect.y + row as f32 * checker;
            let cell = Rect::new(
                x,
                y,
                (rect.x + rect.width - x).min(checker),
                (rect.y + rect.height - y).min(checker),
            );
            let color = if (row + col) % 2 == 0 { light } else { dark };
            push_rect_triangles(&mut vertices, cell, color);
        }
    }
    vertices
}

pub(crate) fn color_with_alpha(mut color: Color, alpha: f32) -> Color {
    color.a = (color.a * alpha).clamp(0.0, 1.0);
    color
}

pub(crate) fn mix_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

pub(crate) fn soft_border(color: Color) -> Color {
    color_with_alpha(color, 0.72)
}

pub(crate) fn shadow_rect(bounds: Rect, shadow: &ShadowToken) -> Rect {
    Rect::new(
        bounds.x + shadow.offset_x - shadow.spread,
        bounds.y + shadow.offset_y - shadow.spread,
        bounds.width + shadow.spread * 2.0,
        bounds.height + shadow.spread * 2.0,
    )
}

pub(crate) fn shadow_color(shadow: &ShadowToken) -> Color {
    Color {
        r: shadow.color[0],
        g: shadow.color[1],
        b: shadow.color[2],
        a: shadow.color[3],
    }
}

fn checkerboard_cell_size(cell_size: f32) -> f32 {
    if cell_size.is_finite() {
        cell_size.clamp(1.0, CHECKERBOARD_MAX_CELL)
    } else {
        1.0
    }
}

fn push_rect_triangles(vertices: &mut Vec<(Point, Color)>, rect: Rect, color: Color) {
    let p0 = Point::new(rect.x, rect.y);
    let p1 = Point::new(rect.x + rect.width, rect.y);
    let p2 = Point::new(rect.x, rect.y + rect.height);
    let p3 = Point::new(rect.x + rect.width, rect.y + rect.height);
    vertices.extend_from_slice(&[
        (p0, color),
        (p1, color),
        (p2, color),
        (p2, color),
        (p1, color),
        (p3, color),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use mondrian_ui_core::widget::DrawCommandEncoder;

    struct RecordingEncoder {
        triangles_in_rect: Vec<(usize, Rect, f32)>,
    }

    impl RecordingEncoder {
        fn new() -> Self {
            Self { triangles_in_rect: Vec::new() }
        }
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}
        fn pop_clip(&mut self) {}
        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {}
        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}
        fn draw_colored_triangles_in_rect(
            &mut self,
            vertices: &[(Point, Color)],
            mask_bounds: Rect,
            corner_radius: f32,
        ) {
            self.triangles_in_rect.push((vertices.len(), mask_bounds, corner_radius));
        }
        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}
        fn push_translate(&mut self, _offset: Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn paint_ctx<'a>(encoder: &'a mut RecordingEncoder) -> PaintContext<'a> {
        let theme: &'static mondrian_ui_theme::Theme =
            Box::leak(Box::new(mondrian_ui_theme::ThemePreset::Dark.build()));
        PaintContext {
            encoder,
            theme,
            clip_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
        }
    }

    #[test]
    fn shadow_rect_applies_offset_and_spread() {
        let shadow = ShadowToken {
            offset_x: 2.0,
            offset_y: 4.0,
            blur: 12.0,
            spread: 3.0,
            color: [0.0, 0.0, 0.0, 0.5],
        };

        assert_eq!(
            shadow_rect(Rect::new(10.0, 20.0, 100.0, 40.0), &shadow),
            Rect::new(9.0, 21.0, 106.0, 46.0)
        );
    }

    #[test]
    fn shadow_color_reads_rgba_channels() {
        let shadow = ShadowToken {
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 0.0,
            spread: 0.0,
            color: [0.1, 0.2, 0.3, 0.4],
        };

        assert_eq!(
            shadow_color(&shadow),
            Color { r: 0.1, g: 0.2, b: 0.3, a: 0.4 }
        );
    }

    #[test]
    fn checkerboard_vertices_cover_partial_edge_cells() {
        let vertices = checkerboard_vertices(Rect::new(10.0, 20.0, 10.0, 8.0), 6.0);

        assert_eq!(vertices.len(), 24);
        assert_eq!(vertices[18].0, Point::new(16.0, 26.0));
        assert_eq!(vertices[19].0, Point::new(20.0, 26.0));
        assert_eq!(vertices[20].0, Point::new(16.0, 28.0));
    }

    #[test]
    fn checkerboard_cell_size_is_clamped_to_shared_maximum() {
        let vertices = checkerboard_vertices(Rect::new(0.0, 0.0, 14.0, 7.0), 99.0);

        assert_eq!(vertices.len(), 12);
        assert_eq!(vertices[6].0, Point::new(7.0, 0.0));
    }

    #[test]
    fn checkerboard_cell_size_recovers_from_non_finite_input() {
        assert_eq!(checkerboard_cell_size(f32::NAN), 1.0);
        assert_eq!(checkerboard_cell_size(f32::INFINITY), 1.0);
    }

    #[test]
    fn paint_checkerboard_uses_mask_bounds_and_radius() {
        let mut encoder = RecordingEncoder::new();
        let rect = Rect::new(4.0, 5.0, 12.0, 6.0);

        {
            let mut ctx = paint_ctx(&mut encoder);
            paint_checkerboard(&mut ctx, rect, 6.0, 3.0);
        }

        assert_eq!(encoder.triangles_in_rect, vec![(12, rect, 3.0)]);
    }

    #[test]
    fn color_with_alpha_multiplies_existing_alpha() {
        let color = Color { r: 0.1, g: 0.2, b: 0.3, a: 0.5 };

        assert_eq!(
            color_with_alpha(color, 0.4),
            Color { r: 0.1, g: 0.2, b: 0.3, a: 0.2 }
        );
    }

    #[test]
    fn mix_color_clamps_ratio() {
        let a = Color { r: 0.0, g: 0.2, b: 0.4, a: 0.6 };
        let b = Color { r: 1.0, g: 0.8, b: 0.6, a: 0.4 };

        assert_eq!(mix_color(a, b, -1.0), a);
        assert_eq!(mix_color(a, b, 2.0), b);
    }

    #[test]
    fn soft_border_preserves_rgb_and_reduces_alpha() {
        let color = Color { r: 0.2, g: 0.4, b: 0.6, a: 0.5 };

        assert_eq!(
            soft_border(color),
            Color { r: 0.2, g: 0.4, b: 0.6, a: 0.36 }
        );
    }
}
