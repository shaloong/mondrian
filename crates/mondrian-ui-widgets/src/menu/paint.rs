//! Menu painting: trigger chrome, popup chrome, row paint, separators, scrollbar.

use mondrian_core::Color;
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use crate::paint::{horizontal_stroke_rect, paint_popover_shadow};
use crate::text_metrics::measure_single_line;
use crate::vector_icon::VectorIcon;

use super::geometry::{content_height, visible_content_height};
use super::model::{DropdownTriggerStyle, MenuRowPaint, MenuVisualTokens, MENU_ARROW_SPACE};

// ── Trigger paint ────────────────────────────────────────────────────────────────

pub(crate) fn paint_menu_trigger(
    ctx: &mut PaintContext,
    rect: Rect,
    label: &str,
    open: bool,
    hovered: bool,
    style: DropdownTriggerStyle,
) {
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    match style {
        DropdownTriggerStyle::Filled => {
            let fill = if open || hovered {
                tokens.surface_2
            } else {
                tokens.surface
            };
            ctx.encoder.draw_rect(rect, fill, visual.trigger_radius(style));
            paint_menu_trigger_label(ctx, rect, label, tokens.foreground, true, style);
            paint_menu_arrow(ctx, rect);
        }
        DropdownTriggerStyle::MenuBar => {
            if open || hovered {
                ctx.encoder.draw_rect(rect, tokens.surface_2, visual.trigger_radius(style));
            }
            paint_menu_trigger_label(ctx, rect, label, tokens.text_secondary, false, style);
        }
    }
}

pub(crate) fn paint_menu_trigger_label(
    ctx: &mut PaintContext,
    rect: Rect,
    label: &str,
    color: Color,
    reserve_arrow: bool,
    style: DropdownTriggerStyle,
) {
    if label.is_empty() {
        return;
    }
    let reserved_right = if reserve_arrow { MENU_ARROW_SPACE } else { 0.0 };
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let padding_x = visual.trigger_padding_x(style);
    let text_width = rect.width - padding_x * 2.0 - reserved_right;
    if text_width <= 0.0 {
        return;
    }
    let clip = Rect::new(rect.x + padding_x, rect.y, text_width, rect.height);
    ctx.push_clip(clip);
    let font_size = trigger_font_size(ctx, style);
    ctx.encoder.draw_text(
        label,
        font_size,
        Point::new(rect.x + padding_x, trigger_text_y(rect, font_size, style)),
        color,
    );
    ctx.pop_clip();
}

// ── Popup chrome ─────────────────────────────────────────────────────────────────

pub(crate) fn paint_menu_popup_chrome(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let radius = visual.popup_radius;
    paint_popover_shadow(ctx, rect, radius);
    ctx.encoder.draw_rect(rect, tokens.border_strong, radius);
    ctx.encoder.draw_rect(
        rect.inset(visual.popup_border_inset, visual.popup_border_inset),
        tokens.popover,
        (radius - visual.popup_border_inset).max(0.0),
    );
}

// ── Row paint ────────────────────────────────────────────────────────────────────

pub(crate) fn paint_menu_row(
    ctx: &mut PaintContext,
    rect: Rect,
    label: &str,
    shortcut: Option<&str>,
    icon: Option<&VectorIcon>,
    reserve_icon_lane: bool,
    state: MenuRowPaint,
) {
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let font_size = visual.row_font_size;
    let fill = if state.hovered && state.enabled {
        let mut hover = tokens.surface_2;
        hover.a *= visual.row_hover_alpha;
        hover
    } else {
        tokens.popover
    };
    ctx.encoder.draw_rect(rect, fill, visual.row_radius);
    let text_color = if state.enabled {
        if state.hovered {
            tokens.foreground
        } else {
            tokens.popover_foreground
        }
    } else {
        tokens.text_disabled
    };
    if reserve_icon_lane {
        if state.active {
            let check_color = if state.enabled {
                tokens.popover_foreground
            } else {
                tokens.text_disabled
            };
            paint_menu_checkmark(ctx, rect, check_color);
        } else if let Some(icon) = icon {
            let icon_size = visual.row_icon_size.min(rect.height).max(1.0);
            let icon_rect = Rect::new(
                rect.x + visual.row_padding_x,
                rect.y + (rect.height - icon_size).max(0.0) * 0.5,
                icon_size,
                icon_size,
            );
            icon.paint(ctx, icon_rect, text_color);
        }
    }

    let icon_lane_width = if reserve_icon_lane {
        visual.row_icon_size + visual.row_icon_gap
    } else {
        0.0
    };
    let text_x = rect.x + visual.row_padding_x + icon_lane_width;
    let shortcut_width = shortcut
        .filter(|shortcut| !shortcut.is_empty())
        .map(|shortcut| measure_single_line(shortcut, visual.shortcut_font_size).0)
        .unwrap_or(0.0);
    let shortcut_x = rect.x + rect.width - visual.row_padding_x - shortcut_width;
    let text_right = if shortcut_width > 0.0 {
        (shortcut_x - visual.row_shortcut_gap).max(text_x)
    } else {
        rect.x + rect.width - visual.row_padding_x
    };
    let text_clip = Rect::new(text_x, rect.y, (text_right - text_x).max(0.0), rect.height);
    if text_clip.width > 0.0 {
        ctx.push_clip(text_clip);
        ctx.encoder.draw_text(
            label,
            font_size,
            Point::new(text_x, menu_row_text_y(rect, font_size)),
            text_color,
        );
        ctx.pop_clip();
    }
    if let Some(shortcut) = shortcut.filter(|shortcut| !shortcut.is_empty()) {
        let shortcut_clip = Rect::new(
            shortcut_x.max(text_x),
            rect.y,
            (rect.x + rect.width - visual.row_padding_x - shortcut_x).max(0.0),
            rect.height,
        );
        if shortcut_clip.width > 0.0 {
            ctx.push_clip(shortcut_clip);
            ctx.encoder.draw_text(
                shortcut,
                visual.shortcut_font_size,
                Point::new(
                    shortcut_clip.x,
                    menu_row_text_y(rect, visual.shortcut_font_size),
                ),
                tokens.text_secondary,
            );
            ctx.pop_clip();
        }
    }
}

pub(crate) fn paint_menu_separator(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let line = horizontal_stroke_rect(
        rect.y + rect.height * 0.5,
        rect.x + visual.separator_inset_x,
        rect.width - visual.separator_inset_x * 2.0,
        visual.separator_width,
    );
    let mut border = tokens.border;
    border.a *= visual.separator_alpha;
    ctx.encoder.draw_rect(line, border, 0.0);
}

pub(crate) fn paint_menu_scrollbar(
    ctx: &mut PaintContext,
    menu_rect: Rect,
    visible_content_height: f32,
    content_height: f32,
    scroll_offset: f32,
) {
    let max_scroll_y = (content_height - visible_content_height).max(0.0);
    if max_scroll_y <= 0.0 || content_height <= 0.0 {
        return;
    }
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let track = Rect::new(
        menu_rect.x + menu_rect.width - visual.scrollbar_right_inset,
        menu_rect.y + visual.scrollbar_top_inset,
        visual.scrollbar_width,
        (menu_rect.height - visual.scrollbar_top_inset * 2.0).max(1.0),
    );
    let thumb_h = (track.height * (visible_content_height / content_height))
        .max(visual.scrollbar_min_thumb_height)
        .min(track.height);
    let thumb_range = (track.height - thumb_h).max(0.0);
    let thumb_y = track.y + (scroll_offset / max_scroll_y) * thumb_range;
    ctx.encoder.draw_rect(
        Rect::new(track.x, thumb_y, track.width, thumb_h),
        tokens.muted_foreground,
        ctx.theme.spacing.radius_full,
    );
}

// ── Private paint helpers ─────────────────────────────────────────────────────────

fn paint_menu_arrow(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let arrow_x = rect.x + rect.width - visual.arrow_right_inset;
    let arrow_y = rect.y + rect.height * 0.5;
    ctx.encoder.draw_triangles(
        &[
            Point::new(
                arrow_x - visual.arrow_half_width,
                arrow_y - visual.arrow_top_offset,
            ),
            Point::new(
                arrow_x + visual.arrow_half_width,
                arrow_y - visual.arrow_top_offset,
            ),
            Point::new(arrow_x, arrow_y + visual.arrow_bottom_offset),
        ],
        tokens.foreground,
    );
}

pub(crate) fn menu_row_text_y(rect: Rect, font_size: f32) -> f32 {
    rect.y + ((rect.height - font_size * 1.3) * 0.5).max(0.0)
}

fn paint_menu_checkmark(ctx: &mut PaintContext, rect: Rect, color: Color) {
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    let x = rect.x + visual.row_padding_x + visual.check_start_offset_x;
    let y = rect.y + rect.height * 0.5;
    ctx.encoder.draw_line(
        Point::new(x, y + 0.5),
        Point::new(x + 3.2, y + 3.8),
        1.5,
        color,
    );
    ctx.encoder.draw_line(
        Point::new(x + 3.0, y + 3.8),
        Point::new(x + visual.check_width, y - 4.0),
        1.5,
        color,
    );
}

fn trigger_font_size(ctx: &PaintContext, style: DropdownTriggerStyle) -> f32 {
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    match style {
        DropdownTriggerStyle::Filled => ctx.theme.typography.body.font_size,
        DropdownTriggerStyle::MenuBar => visual.menu_bar_font_size,
    }
}

fn trigger_text_y(rect: Rect, font_size: f32, style: DropdownTriggerStyle) -> f32 {
    match style {
        DropdownTriggerStyle::Filled => rect.y + 5.0,
        DropdownTriggerStyle::MenuBar => rect.y + ((rect.height - font_size) * 0.5).max(0.0) - 0.5,
    }
}

// ── Open menu paint ──────────────────────────────────────────────────────────────

/// Paint the full open popup: chrome, rows, separators, scrollbar.
pub(crate) fn paint_open_menu(
    ctx: &mut PaintContext,
    bounds: Rect,
    items: &[crate::menu::model::MenuItem],
    trigger_label: &str,
    trigger_style: DropdownTriggerStyle,
    max_visible_items: usize,
    item_height: f32,
    hovered_index: Option<usize>,
    scroll_offset: f32,
    overlay_viewport: &std::cell::Cell<Option<Rect>>,
    open: bool,
) {
    use super::geometry::{icon_lane_width, item_rect, menu_rect, rect_has_paintable_area};

    if !open {
        return;
    }

    if !rect_has_paintable_area(ctx.clip_rect) {
        overlay_viewport.set(None);
        return;
    }
    overlay_viewport.set(Some(ctx.clip_rect));

    let menu_bg = menu_rect(
        bounds,
        items,
        trigger_label,
        trigger_style,
        max_visible_items,
        item_height,
        Some(ctx.clip_rect),
    );

    paint_menu_popup_chrome(ctx, menu_bg);
    ctx.push_clip(menu_bg.inset(1.0, 1.0));
    let reserve_icon_lane = icon_lane_width(items) > 0.0;

    for (i, item) in items.iter().enumerate() {
        let ir = item_rect(menu_bg, i, item_height, scroll_offset);
        if !ir.intersects(&menu_bg) {
            continue;
        }

        if item.is_separator() {
            paint_menu_separator(ctx, ir);
            continue;
        }

        paint_menu_row(
            ctx,
            ir,
            &item.label,
            item.shortcut.as_deref(),
            item.icon.as_ref(),
            reserve_icon_lane,
            MenuRowPaint {
                enabled: item.enabled,
                active: item.checked,
                hovered: hovered_index == Some(i),
            },
        );
    }
    ctx.pop_clip();

    let total_items = items.len();
    let visible_count = super::geometry::visible_item_count(total_items, max_visible_items);
    paint_menu_scrollbar(
        ctx,
        menu_bg,
        visible_content_height(visible_count, item_height),
        content_height(total_items, item_height),
        scroll_offset,
    );
}

/// Paint disabled trigger (filled rect + muted label, no arrow, no focus ring).
pub(crate) fn paint_disabled_trigger(
    ctx: &mut PaintContext,
    label: &str,
    bounds: Rect,
    style: DropdownTriggerStyle,
) {
    let rect = super::geometry::trigger_rect(bounds, style);
    let tokens = &ctx.theme.colors;
    let visual = MenuVisualTokens::from_theme(ctx.theme);
    ctx.encoder.draw_rect(rect, tokens.muted, visual.trigger_radius(style));
    paint_menu_trigger_label(ctx, rect, label, tokens.muted_foreground, false, style);
}
