//! Shared widget painting helpers.

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;
use mondrian_ui_theme::spacing::ShadowToken;

const CHECKERBOARD_MAX_CELL: f32 = 7.0;
const FOCUS_RING_INSET: f32 = -2.0;
const FOCUS_RING_RADIUS_OUTSET: f32 = 2.0;
const FOCUS_RING_ALPHA: f32 = 0.38;

pub(crate) fn paint_shadow(ctx: &mut PaintContext, bounds: Rect, radius: f32) {
    let shadow = &ctx.theme.spacing.shadow_md;
    ctx.encoder.draw_rect(shadow_rect(bounds, shadow), shadow_color(shadow), radius);
}

pub(crate) fn paint_popover_shadow(ctx: &mut PaintContext, bounds: Rect, radius: f32) {
    let spacing = &ctx.theme.spacing;
    let layers = [
        (&spacing.shadow_xl, 0.34, 0.38, radius + 4.0),
        (&spacing.shadow_md, 0.52, 0.20, radius + 2.0),
        (&spacing.shadow_sm, 0.82, 0.04, radius),
    ];

    for (shadow, alpha_scale, blur_scale, layer_radius) in layers {
        let mut color = shadow_color(shadow);
        color.a *= alpha_scale;
        ctx.encoder.draw_rect(
            soft_shadow_rect(bounds, shadow, blur_scale),
            color,
            layer_radius,
        );
    }
}

pub(crate) fn paint_focus_ring(ctx: &mut PaintContext, bounds: Rect, radius: f32) {
    ctx.encoder.draw_rect(
        focus_ring_rect(bounds),
        focus_ring_color(ctx.theme.colors.ring),
        focus_ring_radius(radius),
    );
}

pub(crate) fn paint_checkerboard(ctx: &mut PaintContext, rect: Rect, cell_size: f32, radius: f32) {
    let colors = &ctx.theme.colors;
    let vertices = checkerboard_vertices(
        rect,
        cell_size,
        colors.checkerboard_light,
        colors.checkerboard_dark,
    );
    ctx.encoder.draw_colored_triangles_in_rect(&vertices, rect, radius);
}

pub(crate) fn checkerboard_vertices(
    rect: Rect,
    cell_size: f32,
    light: Color,
    dark: Color,
) -> Vec<(Point, Color)> {
    let checker = checkerboard_cell_size(cell_size);
    let cols = (rect.width / checker).ceil() as i32;
    let rows = (rect.height / checker).ceil() as i32;
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

pub(crate) fn snap_stroke_center(coord: f32, stroke_width: f32) -> f32 {
    if !coord.is_finite() {
        return coord;
    }
    let stroke_width = normalized_stroke_width(stroke_width);
    (coord - stroke_width * 0.5).round() + stroke_width * 0.5
}

pub(crate) fn vertical_stroke_rect(x: f32, y: f32, height: f32, stroke_width: f32) -> Rect {
    let stroke_width = normalized_stroke_width(stroke_width);
    let center = snap_stroke_center(x, stroke_width);
    Rect::new(
        center - stroke_width * 0.5,
        y,
        stroke_width,
        height.max(0.0),
    )
}

pub(crate) fn horizontal_stroke_rect(y: f32, x: f32, width: f32, stroke_width: f32) -> Rect {
    let stroke_width = normalized_stroke_width(stroke_width);
    let center = snap_stroke_center(y, stroke_width);
    Rect::new(x, center - stroke_width * 0.5, width.max(0.0), stroke_width)
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

pub(crate) fn soft_shadow_rect(bounds: Rect, shadow: &ShadowToken, blur_scale: f32) -> Rect {
    let blur_spread = shadow.blur.max(0.0) * blur_scale.max(0.0);
    let spread = shadow.spread.max(0.0) + blur_spread;
    Rect::new(
        bounds.x + shadow.offset_x - spread,
        bounds.y + shadow.offset_y - spread,
        bounds.width + spread * 2.0,
        bounds.height + spread * 2.0,
    )
}

pub(crate) fn focus_ring_rect(bounds: Rect) -> Rect {
    bounds.inset(FOCUS_RING_INSET, FOCUS_RING_INSET)
}

pub(crate) fn focus_ring_radius(radius: f32) -> f32 {
    radius + FOCUS_RING_RADIUS_OUTSET
}

pub(crate) fn focus_ring_color(color: Color) -> Color {
    Color { a: FOCUS_RING_ALPHA, ..color }
}

fn checkerboard_cell_size(cell_size: f32) -> f32 {
    if cell_size.is_finite() {
        cell_size.clamp(1.0, CHECKERBOARD_MAX_CELL)
    } else {
        1.0
    }
}

fn normalized_stroke_width(stroke_width: f32) -> f32 {
    if stroke_width.is_finite() && stroke_width > 0.0 {
        stroke_width
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
        triangle_colors: Vec<Color>,
    }

    impl RecordingEncoder {
        fn new() -> Self {
            Self {
                triangles_in_rect: Vec::new(),
                triangle_colors: Vec::new(),
            }
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
            self.triangle_colors.extend(vertices.iter().map(|(_, color)| *color));
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
    fn soft_shadow_rect_uses_blur_as_visual_spread() {
        let shadow = ShadowToken {
            offset_x: 1.0,
            offset_y: 2.0,
            blur: 20.0,
            spread: 3.0,
            color: [0.0, 0.0, 0.0, 0.2],
        };

        assert_eq!(
            soft_shadow_rect(Rect::new(10.0, 20.0, 100.0, 40.0), &shadow, 0.25),
            Rect::new(3.0, 14.0, 116.0, 56.0)
        );
    }

    #[test]
    fn focus_ring_geometry_expands_bounds_and_radius() {
        assert_eq!(
            focus_ring_rect(Rect::new(10.0, 12.0, 20.0, 8.0)),
            Rect::new(8.0, 10.0, 24.0, 12.0)
        );
        assert_eq!(focus_ring_radius(4.0), 6.0);
    }

    #[test]
    fn focus_ring_color_uses_shared_alpha_without_changing_rgb() {
        let color = Color { r: 0.2, g: 0.4, b: 0.8, a: 0.7 };

        assert_eq!(
            focus_ring_color(color),
            Color { r: 0.2, g: 0.4, b: 0.8, a: 0.38 }
        );
    }

    #[test]
    fn checkerboard_vertices_cover_partial_edge_cells() {
        let light = Color::from_hex(0xFFFFFF);
        let dark = Color::from_hex(0x000000);
        let vertices = checkerboard_vertices(Rect::new(10.0, 20.0, 10.0, 8.0), 6.0, light, dark);

        assert_eq!(vertices.len(), 24);
        assert_eq!(vertices[18].0, Point::new(16.0, 26.0));
        assert_eq!(vertices[19].0, Point::new(20.0, 26.0));
        assert_eq!(vertices[20].0, Point::new(16.0, 28.0));
    }

    #[test]
    fn checkerboard_cell_size_is_clamped_to_shared_maximum() {
        let vertices = checkerboard_vertices(
            Rect::new(0.0, 0.0, 14.0, 7.0),
            99.0,
            Color::WHITE,
            Color::BLACK,
        );

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
    fn paint_checkerboard_uses_theme_tokens() {
        let mut encoder = RecordingEncoder::new();
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let rect = Rect::new(4.0, 5.0, 12.0, 6.0);

        {
            let mut ctx = PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: Rect::new(0.0, 0.0, 1920.0, 1080.0),
            };
            paint_checkerboard(&mut ctx, rect, 6.0, 3.0);
        }

        assert_eq!(encoder.triangle_colors[0], theme.colors.checkerboard_light);
        assert_eq!(encoder.triangle_colors[6], theme.colors.checkerboard_dark);
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

    #[test]
    fn snap_stroke_center_aligns_edges_to_device_pixels() {
        assert_eq!(snap_stroke_center(10.2, 1.0), 10.5);
        assert_eq!(snap_stroke_center(10.8, 1.0), 10.5);
        assert_eq!(snap_stroke_center(10.2, 2.0), 10.0);
        assert_eq!(snap_stroke_center(10.8, 2.0), 11.0);
    }

    #[test]
    fn stroke_rect_helpers_snap_axis_aligned_lines() {
        assert_eq!(
            vertical_stroke_rect(10.2, 3.0, 20.0, 1.0),
            Rect::new(10.0, 3.0, 1.0, 20.0)
        );
        assert_eq!(
            horizontal_stroke_rect(10.8, 4.0, 30.0, 2.0),
            Rect::new(4.0, 10.0, 30.0, 2.0)
        );
    }
}
