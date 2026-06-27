//! Viewer surface paint: checkerboard, icons, safe guides, status badge.

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use crate::paint::{color_with_alpha, horizontal_stroke_rect, mix_color, vertical_stroke_rect};

use super::model as viewer_model;
use super::ViewerStatusTone;

// ── Constants ────────────────────────────────────────────────────────────────────

const CHECKER_TILE_SIZE: f32 = 16.0;

// ── Checkerboard ─────────────────────────────────────────────────────────────────

pub(super) fn paint_checkerboard(ctx: &mut PaintContext, canvas: Rect) {
    if canvas.width <= 0.0 || canvas.height <= 0.0 {
        return;
    }
    let colors = &ctx.theme.colors;
    let mut dark = colors.checkerboard_dark;
    let mut light = colors.checkerboard_light;
    dark.a *= 0.28;
    light.a *= 0.28;
    ctx.encoder.draw_rect(canvas, dark, 0.0);

    let columns = (canvas.width / CHECKER_TILE_SIZE).ceil().max(1.0) as usize;
    let rows = (canvas.height / CHECKER_TILE_SIZE).ceil().max(1.0) as usize;
    for row in 0..rows {
        for column in 0..columns {
            if (row + column) % 2 == 0 {
                let x = canvas.x + column as f32 * CHECKER_TILE_SIZE;
                let y = canvas.y + row as f32 * CHECKER_TILE_SIZE;
                ctx.encoder.draw_rect(
                    Rect::new(
                        x,
                        y,
                        (canvas.x + canvas.width - x).clamp(0.0, CHECKER_TILE_SIZE),
                        (canvas.y + canvas.height - y).clamp(0.0, CHECKER_TILE_SIZE),
                    ),
                    light,
                    0.0,
                );
            }
        }
    }
}

// ── Triangle icons ───────────────────────────────────────────────────────────────

pub(super) fn paint_left_triangle(ctx: &mut PaintContext, rect: Rect, left: f32, color: Color) {
    ctx.encoder.draw_triangles(
        &[
            Point::new(rect.x + left + 10.0, rect.y + 6.0),
            Point::new(rect.x + left + 10.0, rect.y + 20.0),
            Point::new(rect.x + left, rect.y + 13.0),
        ],
        color,
    );
}

pub(super) fn paint_right_triangle(ctx: &mut PaintContext, rect: Rect, left: f32, color: Color) {
    ctx.encoder.draw_triangles(
        &[
            Point::new(rect.x + left, rect.y + 6.0),
            Point::new(rect.x + left, rect.y + 20.0),
            Point::new(rect.x + left + 10.0, rect.y + 13.0),
        ],
        color,
    );
}

// ── Mark-in / Mark-out icons ─────────────────────────────────────────────────────

pub(super) fn paint_mark_in_icon(ctx: &mut PaintContext, rect: Rect, color: Color) {
    let x = rect.x.round();
    let y = rect.y.round();
    ctx.encoder.draw_rect(Rect::new(x + 7.0, y + 7.0, 2.0, 12.0), color, 1.0);
    ctx.encoder.draw_triangles(
        &[
            Point::new(x + 12.0, y + 8.0),
            Point::new(x + 12.0, y + 18.0),
            Point::new(x + 18.0, y + 13.0),
        ],
        color,
    );
}

pub(super) fn paint_mark_out_icon(ctx: &mut PaintContext, rect: Rect, color: Color) {
    let x = rect.x.round();
    let y = rect.y.round();
    ctx.encoder.draw_triangles(
        &[
            Point::new(x + 12.0, y + 13.0),
            Point::new(x + 18.0, y + 8.0),
            Point::new(x + 18.0, y + 18.0),
        ],
        color,
    );
    ctx.encoder.draw_rect(Rect::new(x + 20.0, y + 7.0, 2.0, 12.0), color, 1.0);
}

// ── Safe-area guides ─────────────────────────────────────────────────────────────

pub(super) fn paint_safe_guides(ctx: &mut PaintContext, canvas: Rect, enabled: bool) {
    let Some((action, title)) = viewer_model::safe_guide_rects(canvas) else {
        return;
    };
    let colors = &ctx.theme.colors;
    let mut guide = colors.safe_guide;
    let mut inner_guide = colors.safe_guide_inner;
    if !enabled {
        guide.a *= 0.56;
        inner_guide.a *= 0.56;
    }
    draw_rect_outline(ctx, action, guide);
    draw_rect_outline(ctx, title, inner_guide);
}

fn draw_rect_outline(ctx: &mut PaintContext, rect: Rect, color: Color) {
    ctx.encoder.draw_rect(
        horizontal_stroke_rect(rect.y, rect.x, rect.width, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        horizontal_stroke_rect(rect.y + rect.height, rect.x, rect.width, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        vertical_stroke_rect(rect.x, rect.y, rect.height, 1.0),
        color,
        0.0,
    );
    ctx.encoder.draw_rect(
        vertical_stroke_rect(rect.x + rect.width, rect.y, rect.height, 1.0),
        color,
        0.0,
    );
}

// ── Status badge ─────────────────────────────────────────────────────────────────

pub(super) fn status_badge_colors(
    enabled: bool,
    status_tone: ViewerStatusTone,
    ctx: &PaintContext,
) -> (Color, Color) {
    let colors = &ctx.theme.colors;
    if !enabled {
        return (
            mix_color(colors.popover, colors.muted, 0.28),
            colors.muted_foreground,
        );
    }
    match status_tone {
        ViewerStatusTone::Neutral => (
            mix_color(colors.popover, colors.foreground, 0.045),
            colors.popover_foreground,
        ),
        ViewerStatusTone::Accent => (
            mix_color(colors.popover, colors.primary, 0.18),
            colors.primary,
        ),
        ViewerStatusTone::Success => (color_with_alpha(colors.success, 0.20), colors.success),
        ViewerStatusTone::Warning => (color_with_alpha(colors.warning, 0.20), colors.warning),
        ViewerStatusTone::Error => (color_with_alpha(colors.error, 0.20), colors.error),
    }
}
