//! Menu geometry: popup positioning, row layout, scroll math, hit-testing.

use mondrian_ui_core::types::{Point, Rect};

use super::model::{
    menu_item_text_width_with_metrics, DropdownTriggerStyle, MenuItem, MenuMetrics,
};

// ── Popup anchoring ──────────────────────────────────────────────────────────────

/// Place a floating menu rect anchored at `anchor` with optional viewport clamping.
pub(crate) fn anchored_menu_rect(
    anchor: Rect,
    width: f32,
    height: f32,
    gap: f32,
    viewport: Option<Rect>,
) -> Rect {
    let width = width.max(1.0);
    let height = height.max(1.0);
    let below = Rect::new(anchor.x, anchor.y + anchor.height + gap, width, height);
    let Some(viewport) = viewport.filter(|rect| rect_has_paintable_area(*rect)) else {
        return below;
    };

    let metrics = MenuMetrics::current();
    let left = viewport.x + metrics.viewport_margin;
    let right = viewport.x + viewport.width - metrics.viewport_margin;
    let top = viewport.y + metrics.viewport_margin;
    let bottom = viewport.y + viewport.height - metrics.viewport_margin;
    let x = anchor.x.clamp(left, (right - width).max(left));
    let below_y = anchor.y + anchor.height + gap;
    let above_y = anchor.y - gap - height;
    let space_below = bottom - below_y;
    let space_above = above_y + height - top;
    let mut y = if space_below < height && space_above > space_below {
        above_y
    } else {
        below_y
    };
    y = y.clamp(top, (bottom - height).max(top));
    Rect::new(x, y, width, height)
}

/// Whether a rect has a finite, positive area suitable for painting.
pub(crate) fn rect_has_paintable_area(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}

// ── Dropdown trigger geometry ────────────────────────────────────────────────────

pub(crate) fn trigger_rect(bounds: Rect, trigger_style: DropdownTriggerStyle) -> Rect {
    let height = trigger_height(trigger_style).min(bounds.height.max(0.0));
    Rect::new(
        bounds.x,
        bounds.y + ((bounds.height - height) * 0.5).max(0.0),
        bounds.width,
        height,
    )
}

pub(crate) fn trigger_height(trigger_style: DropdownTriggerStyle) -> f32 {
    MenuMetrics::current().trigger_height(trigger_style)
}

pub(crate) fn trigger_padding_x(trigger_style: DropdownTriggerStyle) -> f32 {
    MenuMetrics::current().trigger_padding_x(trigger_style)
}

pub(crate) fn preferred_trigger_width(label: &str, trigger_style: DropdownTriggerStyle) -> f32 {
    use crate::text_metrics::measure_single_line;
    let metrics = MenuMetrics::current();
    let (label_width, _) = measure_single_line(label, metrics.measure_font_size);
    match trigger_style {
        DropdownTriggerStyle::Filled => metrics
            .min_width
            .max(label_width + metrics.trigger_padding_x * 2.0 + metrics.arrow_space),
        DropdownTriggerStyle::MenuBar => {
            (label_width + trigger_padding_x(trigger_style) * 2.0).max(28.0)
        }
    }
}

// ── Menu popup geometry ──────────────────────────────────────────────────────────

pub(crate) fn icon_lane_width(items: &[MenuItem]) -> f32 {
    let metrics = MenuMetrics::current();
    if items.iter().any(|item| item.icon.is_some() || item.checked) {
        metrics.row_icon_size + metrics.row_icon_gap
    } else {
        0.0
    }
}

pub(crate) fn preferred_menu_width(
    items: &[MenuItem],
    trigger_label: &str,
    trigger_style: DropdownTriggerStyle,
    max_visible_items: usize,
) -> f32 {
    let metrics = MenuMetrics::current();
    let longest_item = items
        .iter()
        .filter(|item| !item.is_separator())
        .map(|item| menu_item_text_width_with_metrics(item, metrics))
        .fold(0.0, f32::max);
    let scrollbar = if items.len() > max_visible_items {
        metrics.scrollbar_space
    } else {
        0.0
    };
    preferred_trigger_width(trigger_label, trigger_style)
        .max(metrics.min_width)
        .max(longest_item + metrics.row_padding_x * 2.0 + icon_lane_width(items) + scrollbar)
}

pub(crate) fn menu_rect(
    bounds: Rect,
    items: &[MenuItem],
    trigger_label: &str,
    trigger_style: DropdownTriggerStyle,
    max_visible_items: usize,
    item_height: f32,
    overlay_viewport: Option<Rect>,
) -> Rect {
    let metrics = MenuMetrics::current();
    let avail_height = overlay_viewport
        .map(|vp| (vp.height - metrics.popup_viewport_pad).max(0.0))
        .unwrap_or(f32::MAX);
    let popup_max = metrics.popup_max_height.min(avail_height);
    let viewport_capped = (popup_max / item_height).floor().max(1.0) as usize;
    let count = items.len().min(max_visible_items.max(1)).min(viewport_capped);
    let visible_height = count as f32 * item_height;
    let width = bounds.width.max(preferred_menu_width(
        items,
        trigger_label,
        trigger_style,
        max_visible_items,
    ));
    anchored_menu_rect(
        trigger_rect(bounds, trigger_style),
        width,
        visible_height + metrics.popup_padding * 2.0,
        metrics.popup_gap,
        overlay_viewport,
    )
}

pub(crate) fn item_rect(
    menu_rect: Rect,
    index: usize,
    item_height: f32,
    scroll_offset: f32,
) -> Rect {
    let metrics = MenuMetrics::current();
    Rect::new(
        menu_rect.x + metrics.popup_padding,
        menu_rect.y + metrics.popup_padding + index as f32 * item_height - scroll_offset,
        (menu_rect.width - metrics.popup_padding * 2.0).max(1.0),
        item_height,
    )
}

pub(crate) fn item_at(
    menu_rect: Rect,
    items_len: usize,
    item_height: f32,
    scroll_offset: f32,
    position: Point,
) -> Option<usize> {
    if !menu_rect.contains(position) {
        return None;
    }
    let relative_y =
        position.y - (menu_rect.y + MenuMetrics::current().popup_padding) + scroll_offset;
    if relative_y < 0.0 {
        return None;
    }
    let index = (relative_y / item_height).floor() as usize;
    if index < items_len
        && item_rect(menu_rect, index, item_height, scroll_offset).contains(position)
    {
        Some(index)
    } else {
        None
    }
}

// ── Scroll helpers ───────────────────────────────────────────────────────────────

pub(crate) fn visible_item_count(total_items: usize, max_visible_items: usize) -> usize {
    total_items.min(max_visible_items.max(1))
}

pub(crate) fn visible_content_height(visible_items: usize, item_height: f32) -> f32 {
    visible_items as f32 * item_height
}

pub(crate) fn content_height(total_items: usize, item_height: f32) -> f32 {
    total_items as f32 * item_height
}

pub(crate) fn max_scroll_y(total_items: usize, visible_items: usize, item_height: f32) -> f32 {
    (content_height(total_items, item_height) - visible_content_height(visible_items, item_height))
        .max(0.0)
}

pub(crate) fn clamp_scroll_offset(scroll_offset: f32, max_y: f32) -> f32 {
    scroll_offset.clamp(0.0, max_y)
}

/// Compute the scroll offset needed to bring the hovered index into view.
pub(crate) fn scroll_to_visible(
    hovered_index: Option<usize>,
    scroll_offset: f32,
    visible_items: usize,
    total_items: usize,
    item_height: f32,
) -> f32 {
    let Some(index) = hovered_index else {
        return scroll_offset;
    };
    let row_top = index as f32 * item_height;
    let row_bottom = row_top + item_height;
    let view_top = scroll_offset;
    let view_bottom = scroll_offset + visible_items as f32 * item_height;
    let new = if row_top < view_top {
        row_top
    } else if row_bottom > view_bottom {
        row_bottom - visible_items as f32 * item_height
    } else {
        return scroll_offset;
    };
    clamp_scroll_offset(new, max_scroll_y(total_items, visible_items, item_height))
}

// ── Submenu ─────────────────────────────────────────────────────────────────────────

/// Compute the bounding rect for a submenu that opens to the right of
/// `parent_item_rect`.
pub(crate) fn submenu_rect(
    parent_item_rect: Rect,
    children: &[MenuItem],
    max_visible_items: usize,
    item_height: f32,
) -> Rect {
    let metrics = MenuMetrics::current();
    let count = children.len().min(max_visible_items);
    let height = count as f32 * item_height + metrics.popup_padding * 2.0;
    let longest = children
        .iter()
        .filter(|c| !c.is_separator())
        .map(|item| menu_item_text_width_with_metrics(item, metrics))
        .fold(0.0, f32::max);
    let width =
        (longest + metrics.row_padding_x * 2.0 + metrics.row_icon_size + metrics.row_icon_gap)
            .max(metrics.min_width);
    Rect::new(
        parent_item_rect.x + parent_item_rect.width,
        parent_item_rect.y - metrics.popup_padding,
        width,
        height,
    )
}
