//! Color picker paint: panel chrome, swatch, color area, bars, crosshair, trigger.

use std::f32::consts::TAU;

use mondrian_core::{Color, HsvColor};
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use crate::paint::{color_with_alpha, mix_color, paint_checkerboard, soft_border};

// ── Constants ────────────────────────────────────────────────────────────────────

const HUE_SEGMENTS: usize = 6;
pub(crate) const WHEEL_SEGMENTS: usize = 120;
const WHEEL_EDGE_OVERDRAW: f32 = 1.5;

// ── Panel chrome ─────────────────────────────────────────────────────────────────

pub(super) fn paint_panel_chrome(ctx: &mut PaintContext, bounds: Rect, eyedropper_active: bool) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    paint_shadow(ctx, bounds, spacing.radius_lg);
    ctx.encoder.draw_rect(bounds, soft_border(tokens.border), spacing.radius_lg);
    ctx.encoder.draw_rect(
        bounds.inset(1.0, 1.0),
        mix_color(tokens.popover, tokens.foreground, 0.018),
        spacing.radius_lg - 1.0,
    );
    if eyedropper_active {
        ctx.encoder.draw_rect(
            bounds.inset(2.0, 2.0),
            color_with_alpha(tokens.primary, 0.26),
            spacing.radius_lg - 2.0,
        );
    }
}

// ── Swatch ───────────────────────────────────────────────────────────────────────

pub(super) fn paint_swatch(ctx: &mut PaintContext, color: Color, swatch_rect: Rect) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let border = swatch_rect;
    let swatch = border.inset(2.0, 2.0);
    ctx.encoder.draw_rect(border, soft_border(tokens.border), spacing.radius_lg);
    ctx.encoder.draw_rect(
        border.inset(1.0, 1.0),
        tokens.popover,
        spacing.radius_lg - 1.0,
    );
    paint_checkerboard(ctx, swatch, 5.0, spacing.radius_md);
    ctx.encoder.draw_rect(swatch, color, spacing.radius_md);
}

// ── Eyedropper ───────────────────────────────────────────────────────────────────

pub(super) fn paint_eyedropper_button(
    ctx: &mut PaintContext,
    rect: Rect,
    enabled: bool,
    active: bool,
    pressed: bool,
    hovered: bool,
    icon: Option<&crate::vector_icon::VectorIcon>,
) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let fill = if !enabled {
        mix_color(tokens.popover, tokens.muted, 0.36)
    } else if active {
        mix_color(tokens.popover, tokens.primary, 0.18)
    } else if pressed {
        mix_color(tokens.popover, tokens.foreground, 0.08)
    } else if hovered {
        mix_color(tokens.popover, tokens.foreground, 0.055)
    } else {
        mix_color(tokens.popover, tokens.foreground, 0.025)
    };
    let tint = if !enabled {
        tokens.muted_foreground
    } else if active {
        tokens.primary
    } else {
        tokens.popover_foreground
    };

    ctx.encoder.draw_rect(rect, soft_border(tokens.border), spacing.radius_md);
    ctx.encoder.draw_rect(rect.inset(1.0, 1.0), fill, spacing.radius_md - 1.0);
    if let Some(vector_icon) = icon {
        vector_icon.paint(ctx, rect.inset(7.0, 7.0), tint);
        return;
    }
    // Fallback: draw a simple eyedropper shape
    ctx.encoder.draw_line(
        Point::new(rect.x + 10.0, rect.y + 18.0),
        Point::new(rect.x + 18.0, rect.y + 10.0),
        2.0,
        tint,
    );
    ctx.encoder.draw_line(
        Point::new(rect.x + 15.0, rect.y + 8.0),
        Point::new(rect.x + 20.0, rect.y + 13.0),
        2.0,
        tint,
    );
    ctx.encoder.draw_line(
        Point::new(rect.x + 8.0, rect.y + 20.0),
        Point::new(rect.x + 12.0, rect.y + 16.0),
        2.0,
        tint,
    );
    ctx.encoder
        .draw_rect(Rect::new(rect.x + 7.0, rect.y + 21.0, 4.0, 2.0), tint, 1.0);
}

// ── Square color area ────────────────────────────────────────────────────────────

pub(super) fn paint_square_color_area(
    ctx: &mut PaintContext,
    outer: Rect,
    hue: f32,
    hsv: HsvColor,
) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let area = outer.inset(1.0, 1.0);
    ctx.encoder.draw_rect(outer, soft_border(tokens.border), spacing.radius_lg);
    let hue_color = Color::from_hsv(HsvColor { h: hue, s: 1.0, v: 1.0, a: 1.0 });
    ctx.encoder.draw_rect(area, hue_color, spacing.radius_lg - 1.0);
    ctx.encoder.draw_gradient_rect(
        area,
        [
            Color::WHITE,
            Color::TRANSPARENT,
            Color::WHITE,
            Color::TRANSPARENT,
        ],
        spacing.radius_lg - 1.0,
    );
    ctx.encoder.draw_gradient_rect(
        area,
        [
            Color::TRANSPARENT,
            Color::TRANSPARENT,
            Color::BLACK,
            Color::BLACK,
        ],
        spacing.radius_lg - 1.0,
    );
    let x = area.x + area.width * hsv.s;
    let y = area.y + area.height * (1.0 - hsv.v);
    paint_crosshair(ctx, Point::new(x, y));
}

// ── Wheel color area ─────────────────────────────────────────────────────────────

pub(super) fn paint_wheel_color_area(
    ctx: &mut PaintContext,
    outer: Rect,
    wheel: Rect,
    hue: f32,
    hsv: HsvColor,
) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    ctx.encoder.draw_rect(
        outer,
        mix_color(tokens.popover, tokens.foreground, 0.025),
        spacing.radius_lg,
    );

    let center = wheel.center();
    let radius = wheel.width.min(wheel.height) * 0.5;
    let geometry_radius = radius + WHEEL_EDGE_OVERDRAW;
    ctx.encoder.draw_rect(
        wheel.inset(-2.0, -2.0),
        soft_border(tokens.border),
        radius + 2.0,
    );
    let center_color = Color::from_hsv(HsvColor { h: hue, s: 0.0, v: hsv.v, a: 1.0 });
    let mut vertices = Vec::with_capacity(WHEEL_SEGMENTS * 3);
    for segment in 0..WHEEL_SEGMENTS {
        let a0 = segment as f32 / WHEEL_SEGMENTS as f32 * TAU;
        let a1 = (segment + 1) as f32 / WHEEL_SEGMENTS as f32 * TAU;
        let h0 = a0.to_degrees();
        let h1 = a1.to_degrees();
        let p0 = Point::new(
            center.x + a0.cos() * geometry_radius,
            center.y + a0.sin() * geometry_radius,
        );
        let p1 = Point::new(
            center.x + a1.cos() * geometry_radius,
            center.y + a1.sin() * geometry_radius,
        );
        let c0 = Color::from_hsv(HsvColor { h: h0, s: 1.0, v: hsv.v, a: 1.0 });
        let c1 = Color::from_hsv(HsvColor { h: h1, s: 1.0, v: hsv.v, a: 1.0 });
        vertices.push((center, center_color));
        vertices.push((p0, c0));
        vertices.push((p1, c1));
    }
    ctx.encoder.draw_colored_triangles_in_rect(&vertices, wheel, radius);

    let angle = hue.to_radians();
    let r = radius * hsv.s.clamp(0.0, 1.0);
    paint_crosshair(
        ctx,
        Point::new(center.x + angle.cos() * r, center.y + angle.sin() * r),
    );
}

// ── Hue bar ──────────────────────────────────────────────────────────────────────

pub(super) fn paint_hue_bar(ctx: &mut PaintContext, bar: Rect, hue: f32) {
    let tokens = &ctx.theme.colors;
    ctx.encoder.draw_rect(bar.inset(-1.0, -1.0), soft_border(tokens.border), 7.0);

    let segment_w = bar.width / HUE_SEGMENTS as f32;
    for segment in 0..HUE_SEGMENTS {
        let h0 = segment as f32 / HUE_SEGMENTS as f32 * 360.0;
        let h1 = (segment + 1) as f32 / HUE_SEGMENTS as f32 * 360.0;
        let c0 = Color::from_hsv(HsvColor { h: h0, s: 1.0, v: 1.0, a: 1.0 });
        let c1 = Color::from_hsv(HsvColor { h: h1, s: 1.0, v: 1.0, a: 1.0 });
        ctx.encoder.draw_gradient_rect(
            Rect::new(
                bar.x + segment as f32 * segment_w,
                bar.y,
                segment_w + 0.5,
                bar.height,
            ),
            [c0, c1, c0, c1],
            0.0,
        );
    }

    let x = bar.x + bar.width * (hue / 360.0).clamp(0.0, 1.0);
    paint_bar_handle(ctx, bar, x);
}

// ── Alpha bar ────────────────────────────────────────────────────────────────────

pub(super) fn paint_alpha_bar(ctx: &mut PaintContext, bar: Rect, color: Color) {
    let tokens = &ctx.theme.colors;
    ctx.encoder.draw_rect(bar.inset(-1.0, -1.0), soft_border(tokens.border), 7.0);
    paint_checkerboard(ctx, bar, 6.0, 7.0);

    let mut transparent = color;
    transparent.a = 0.0;
    let mut opaque = color;
    opaque.a = 1.0;
    ctx.encoder
        .draw_gradient_rect(bar, [transparent, opaque, transparent, opaque], 0.0);

    let x = bar.x + bar.width * color.a.clamp(0.0, 1.0);
    paint_bar_handle(ctx, bar, x);
}

// ── Crosshair / bar handle ───────────────────────────────────────────────────────

pub(super) fn paint_crosshair(ctx: &mut PaintContext, point: Point) {
    let tokens = &ctx.theme.colors;
    ctx.encoder.draw_rect(
        Rect::new(point.x - 7.0, point.y - 6.0, 14.0, 14.0),
        tokens.color_handle_shadow,
        7.0,
    );
    ctx.encoder.draw_rect(
        Rect::new(point.x - 6.0, point.y - 6.0, 12.0, 12.0),
        tokens.color_handle_outer,
        6.0,
    );
    ctx.encoder.draw_rect(
        Rect::new(point.x - 3.0, point.y - 3.0, 6.0, 6.0),
        tokens.color_handle_inner,
        3.0,
    );
}

pub(super) fn paint_bar_handle(ctx: &mut PaintContext, bar: Rect, x: f32) {
    let tokens = &ctx.theme.colors;
    let x = x.clamp(bar.x, bar.x + bar.width);
    ctx.encoder.draw_rect(
        Rect::new(x - 2.0, bar.y - 3.0, 4.0, bar.height + 6.0),
        tokens.color_handle_strong_shadow,
        2.0,
    );
    ctx.encoder.draw_rect(
        Rect::new(x - 1.0, bar.y - 2.0, 2.0, bar.height + 4.0),
        tokens.color_handle_outer,
        1.0,
    );
}

// ── Trigger paint ────────────────────────────────────────────────────────────────

pub(super) fn paint_trigger(
    ctx: &mut PaintContext,
    bounds: Rect,
    color: Color,
    color_rect: Rect,
    enabled: bool,
    open: bool,
    pressed: bool,
) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let fill = if !enabled {
        mix_color(tokens.popover, tokens.muted, 0.36)
    } else if open {
        mix_color(tokens.popover, tokens.primary, 0.08)
    } else if pressed {
        mix_color(tokens.popover, tokens.foreground, 0.06)
    } else {
        mix_color(tokens.popover, tokens.foreground, 0.025)
    };
    ctx.encoder.draw_rect(bounds, soft_border(tokens.border), spacing.radius_md);
    ctx.encoder.draw_rect(bounds.inset(1.0, 1.0), fill, spacing.radius_md - 1.0);
    ctx.encoder.draw_rect(
        color_rect.inset(-1.0, -1.0),
        soft_border(tokens.border),
        spacing.radius_sm,
    );
    paint_checkerboard(ctx, color_rect, 6.0, 7.0);
    ctx.encoder.draw_rect(color_rect, color, spacing.radius_sm);
    if !enabled {
        ctx.encoder.draw_rect(
            color_rect,
            color_with_alpha(tokens.popover, 0.36),
            spacing.radius_sm,
        );
    }
}

// ── Shadow helper (re-exported for tests) ─────────────────────────────────────────

fn paint_shadow(ctx: &mut PaintContext, bounds: Rect, corner_radius: f32) {
    crate::paint::paint_shadow(ctx, bounds, corner_radius);
}
