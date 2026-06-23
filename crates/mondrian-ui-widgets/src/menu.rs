//! Dropdown Menu 控件
//!
//! 点击展开菜单列表，点击选项或外部区域关闭，派发 Action。

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::cell::Cell;

use crate::paint::{horizontal_stroke_rect, paint_focus_ring, paint_popover_shadow};
use crate::text_metrics::measure_single_line;
use crate::vector_icon::VectorIcon;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MenuRowPaint {
    pub enabled: bool,
    pub active: bool,
    pub hovered: bool,
}

const MENU_MEASURE_FONT_SIZE: f32 = 13.0;
const MENU_MIN_WIDTH: f32 = 160.0;
const MENU_TRIGGER_HEIGHT: f32 = 28.0;
const MENU_BAR_TRIGGER_HEIGHT: f32 = 22.0;
const MENU_TRIGGER_PADDING_X: f32 = 8.0;
const MENU_ARROW_SPACE: f32 = 24.0;
const MENU_POPUP_PADDING: f32 = 6.0;
const MENU_ROW_PADDING_X: f32 = 10.0;
const MENU_ROW_ICON_SIZE: f32 = 15.0;
const MENU_ROW_ICON_GAP: f32 = 8.0;
const MENU_ROW_SHORTCUT_GAP: f32 = 24.0;
const MENU_SCROLLBAR_SPACE: f32 = 8.0;
const MENU_VIEWPORT_MARGIN: f32 = 4.0;

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

    let left = viewport.x + MENU_VIEWPORT_MARGIN;
    let right = viewport.x + viewport.width - MENU_VIEWPORT_MARGIN;
    let top = viewport.y + MENU_VIEWPORT_MARGIN;
    let bottom = viewport.y + viewport.height - MENU_VIEWPORT_MARGIN;
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

pub(crate) fn rect_has_paintable_area(rect: Rect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width > 0.0
        && rect.height > 0.0
}

/// Visual treatment for a dropdown trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropdownTriggerStyle {
    /// Filled rounded rectangle suitable for form and toolbar dropdowns.
    Filled,
    /// Lightweight transparent trigger suitable for native-style menu bars.
    MenuBar,
}

pub(crate) fn paint_menu_trigger(
    ctx: &mut PaintContext,
    rect: Rect,
    label: &str,
    open: bool,
    style: DropdownTriggerStyle,
) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    match style {
        DropdownTriggerStyle::Filled => {
            let fill = if open {
                tokens.surface_2
            } else {
                tokens.surface
            };
            ctx.encoder.draw_rect(rect, fill, spacing.radius_sm);
            paint_menu_trigger_label(ctx, rect, label, tokens.foreground, true, style);
            paint_menu_arrow(ctx, rect);
        }
        DropdownTriggerStyle::MenuBar => {
            if open {
                ctx.encoder.draw_rect(rect, tokens.surface_2, spacing.radius_sm);
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
    let padding_x = trigger_padding_x(style);
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

fn trigger_padding_x(style: DropdownTriggerStyle) -> f32 {
    match style {
        DropdownTriggerStyle::Filled => MENU_TRIGGER_PADDING_X,
        DropdownTriggerStyle::MenuBar => 7.0,
    }
}

fn trigger_font_size(ctx: &PaintContext, style: DropdownTriggerStyle) -> f32 {
    match style {
        DropdownTriggerStyle::Filled => ctx.theme.typography.body.font_size,
        DropdownTriggerStyle::MenuBar => 12.5,
    }
}

fn trigger_text_y(rect: Rect, font_size: f32, style: DropdownTriggerStyle) -> f32 {
    match style {
        DropdownTriggerStyle::Filled => rect.y + 5.0,
        DropdownTriggerStyle::MenuBar => rect.y + ((rect.height - font_size) * 0.5).max(0.0) - 0.5,
    }
}

pub(crate) fn paint_menu_popup_chrome(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let radius = 8.0_f32.min(spacing.radius_lg);
    paint_popover_shadow(ctx, rect, radius);
    ctx.encoder.draw_rect(rect, tokens.border_strong, radius);
    ctx.encoder.draw_rect(
        rect.inset(1.0, 1.0),
        tokens.popover,
        (radius - 1.0).max(0.0),
    );
}

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
    let spacing = &ctx.theme.spacing;
    let font_size = MENU_MEASURE_FONT_SIZE;
    let fill = if state.hovered && state.enabled {
        let mut hover = tokens.surface_2;
        hover.a *= 0.72;
        hover
    } else {
        tokens.popover
    };
    ctx.encoder.draw_rect(rect, fill, spacing.radius_sm);
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
                tokens.primary
            } else {
                tokens.text_disabled
            };
            paint_menu_checkmark(ctx, rect, check_color);
        } else if let Some(icon) = icon {
            let icon_size = MENU_ROW_ICON_SIZE.min(rect.height).max(1.0);
            let icon_rect = Rect::new(
                rect.x + MENU_ROW_PADDING_X,
                rect.y + (rect.height - icon_size).max(0.0) * 0.5,
                icon_size,
                icon_size,
            );
            icon.paint(ctx, icon_rect, text_color);
        }
    }

    let icon_lane_width = if reserve_icon_lane {
        MENU_ROW_ICON_SIZE + MENU_ROW_ICON_GAP
    } else {
        0.0
    };
    let text_x = rect.x + MENU_ROW_PADDING_X + icon_lane_width;
    let shortcut_width = shortcut
        .filter(|shortcut| !shortcut.is_empty())
        .map(|shortcut| measure_single_line(shortcut, font_size).0)
        .unwrap_or(0.0);
    let shortcut_x = rect.x + rect.width - MENU_ROW_PADDING_X - shortcut_width;
    let text_right = if shortcut_width > 0.0 {
        (shortcut_x - MENU_ROW_SHORTCUT_GAP).max(text_x)
    } else {
        rect.x + rect.width - MENU_ROW_PADDING_X
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
            (rect.x + rect.width - MENU_ROW_PADDING_X - shortcut_x).max(0.0),
            rect.height,
        );
        if shortcut_clip.width > 0.0 {
            ctx.push_clip(shortcut_clip);
            ctx.encoder.draw_text(
                shortcut,
                12.0,
                Point::new(shortcut_clip.x, menu_row_text_y(rect, 12.0)),
                tokens.text_secondary,
            );
            ctx.pop_clip();
        }
    }
}

pub(crate) fn paint_menu_separator(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let line = horizontal_stroke_rect(
        rect.y + rect.height * 0.5,
        rect.x + 6.0,
        rect.width - 12.0,
        1.0,
    );
    let mut border = tokens.border;
    border.a *= 0.86;
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
    let spacing = &ctx.theme.spacing;
    let tokens = &ctx.theme.colors;
    let track = Rect::new(
        menu_rect.x + menu_rect.width - 5.0,
        menu_rect.y + 3.0,
        3.0,
        (menu_rect.height - 6.0).max(1.0),
    );
    let thumb_h = (track.height * (visible_content_height / content_height))
        .max(16.0)
        .min(track.height);
    let thumb_range = (track.height - thumb_h).max(0.0);
    let thumb_y = track.y + (scroll_offset / max_scroll_y) * thumb_range;
    ctx.encoder.draw_rect(
        Rect::new(track.x, thumb_y, track.width, thumb_h),
        tokens.muted_foreground,
        spacing.radius_full,
    );
}

fn paint_menu_arrow(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let arrow_x = rect.x + rect.width - 16.0;
    let arrow_y = rect.y + rect.height * 0.5;
    ctx.encoder.draw_triangles(
        &[
            Point::new(arrow_x - 4.0, arrow_y - 2.0),
            Point::new(arrow_x + 4.0, arrow_y - 2.0),
            Point::new(arrow_x, arrow_y + 3.0),
        ],
        tokens.foreground,
    );
}

fn menu_row_text_y(rect: Rect, font_size: f32) -> f32 {
    rect.y + ((rect.height - font_size * 1.3) * 0.5).max(0.0)
}

fn paint_menu_checkmark(ctx: &mut PaintContext, rect: Rect, color: Color) {
    let x = rect.x + MENU_ROW_PADDING_X + 1.0;
    let y = rect.y + rect.height * 0.5;
    ctx.encoder.draw_line(
        Point::new(x, y + 0.5),
        Point::new(x + 3.2, y + 3.8),
        1.5,
        color,
    );
    ctx.encoder.draw_line(
        Point::new(x + 3.0, y + 3.8),
        Point::new(x + 9.5, y - 4.0),
        1.5,
        color,
    );
}

/// Menu row behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItemKind {
    /// Clickable menu row that may dispatch an action when enabled.
    Action,
    /// Visual divider row that never dispatches an action.
    Separator,
}

/// 菜单选项
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: Action,
    /// Component-local command emitted by popups that should not dispatch an
    /// editor action.
    pub local_command: Option<String>,
    pub enabled: bool,
    pub kind: MenuItemKind,
    pub icon: Option<VectorIcon>,
    pub shortcut: Option<String>,
    pub checked: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action,
            local_command: None,
            enabled: true,
            kind: MenuItemKind::Action,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Create a menu row that reports a component-local command instead of
    /// dispatching an app action.
    pub fn local(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            action: Action::NoOp,
            local_command: Some(command.into()),
            enabled: true,
            kind: MenuItemKind::Action,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Create a visual divider row.
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            action: Action::DeselectAll,
            local_command: None,
            enabled: false,
            kind: MenuItemKind::Separator,
            icon: None,
            shortcut: None,
            checked: false,
        }
    }

    /// Paint a vector icon before this item label.
    pub fn with_icon(mut self, icon: VectorIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Paint a right-aligned keyboard shortcut hint for this item.
    pub fn with_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Mark this item as representing the current checked/selected state.
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn is_separator(&self) -> bool {
        self.kind == MenuItemKind::Separator
    }

    pub fn is_activatable(&self) -> bool {
        self.enabled && !self.is_separator()
    }
}

pub(crate) fn menu_item_text_width(item: &MenuItem) -> f32 {
    let label_width = measure_single_line(&item.label, MENU_MEASURE_FONT_SIZE).0;
    let Some(shortcut) = item.shortcut.as_deref().filter(|shortcut| !shortcut.is_empty()) else {
        return label_width;
    };
    label_width + MENU_ROW_SHORTCUT_GAP + measure_single_line(shortcut, MENU_MEASURE_FONT_SIZE).0
}

/// Dropdown 菜单
///
/// 点击按钮展开选项列表，选中后关闭。
pub struct Dropdown {
    id: WidgetId,
    #[allow(dead_code)]
    label: String,
    items: Vec<MenuItem>,
    bounds: Rect,
    enabled: bool,
    open: bool,
    hovered_index: Option<usize>,
    pressed_index: Option<usize>,
    item_height: f32,
    max_visible_items: usize,
    scroll_offset: f32,
    suppress_next_release: bool,
    focused: bool,
    focus_visible: bool,
    overlay_viewport: Cell<Option<Rect>>,
    trigger_style: DropdownTriggerStyle,
}

impl Dropdown {
    pub fn new(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            items,
            bounds: Rect::ZERO,
            enabled: true,
            open: false,
            hovered_index: None,
            pressed_index: None,
            item_height: 28.0,
            max_visible_items: 8,
            scroll_offset: 0.0,
            suppress_next_release: false,
            focused: false,
            focus_visible: false,
            overlay_viewport: Cell::new(None),
            trigger_style: DropdownTriggerStyle::Filled,
        }
    }

    /// Set whether the dropdown accepts input and participates in focus.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        if !enabled {
            self.open = false;
            self.hovered_index = None;
            self.pressed_index = None;
            self.suppress_next_release = false;
            self.focused = false;
            self.focus_visible = false;
        }
        self
    }

    /// Disable the dropdown.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the dropdown is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Select the visual style for the closed trigger.
    pub fn with_trigger_style(mut self, style: DropdownTriggerStyle) -> Self {
        self.trigger_style = style;
        self
    }

    /// Whether the popup menu is currently open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Update checked state for all rows matching an app action.
    pub fn set_checked_for_action(&mut self, action: &Action, checked: bool) {
        for item in &mut self.items {
            if !item.is_separator() && item.action == *action {
                item.checked = checked;
            }
        }
    }

    /// Return checked state for the first row matching an app action.
    pub fn checked_for_action(&self, action: &Action) -> Option<bool> {
        self.items
            .iter()
            .find(|item| !item.is_separator() && item.action == *action)
            .map(|item| item.checked)
    }

    /// Whether a point is inside the closed trigger chrome.
    pub fn trigger_contains(&self, point: Point) -> bool {
        self.trigger_rect().contains(point)
    }

    /// Close the popup menu if it is open.
    pub fn close_menu(&mut self, ctx: &mut EventContext) {
        if self.open {
            self.close(ctx);
        }
    }

    /// Open the popup menu from parent-level coordination.
    ///
    /// This is intentionally different from trigger-click opening: a parent
    /// menu bar may open a sibling menu on hover, and that should not suppress
    /// the next pointer release because it belongs to a fresh item click.
    pub fn open_menu(&mut self, ctx: &mut EventContext) {
        self.open_with_release_suppression(ctx, false);
    }

    /// Limit how many rows are visible before the open menu scrolls.
    pub fn with_max_visible_items(mut self, max_visible_items: usize) -> Self {
        self.max_visible_items = max_visible_items.max(1);
        self.clamp_scroll_offset();
        self
    }

    fn trigger_rect(&self) -> Rect {
        let height = self.trigger_height().min(self.bounds.height.max(0.0));
        Rect::new(
            self.bounds.x,
            self.bounds.y + ((self.bounds.height - height) * 0.5).max(0.0),
            self.bounds.width,
            height,
        )
    }

    fn trigger_height(&self) -> f32 {
        match self.trigger_style {
            DropdownTriggerStyle::Filled => MENU_TRIGGER_HEIGHT,
            DropdownTriggerStyle::MenuBar => MENU_BAR_TRIGGER_HEIGHT,
        }
    }

    fn preferred_trigger_width(&self) -> f32 {
        let (label_width, _) = measure_single_line(&self.label, MENU_MEASURE_FONT_SIZE);
        match self.trigger_style {
            DropdownTriggerStyle::Filled => {
                MENU_MIN_WIDTH.max(label_width + MENU_TRIGGER_PADDING_X * 2.0 + MENU_ARROW_SPACE)
            }
            DropdownTriggerStyle::MenuBar => {
                (label_width + trigger_padding_x(self.trigger_style) * 2.0).max(28.0)
            }
        }
    }

    fn preferred_menu_width(&self) -> f32 {
        let longest_item = self
            .items
            .iter()
            .filter(|item| !item.is_separator())
            .map(menu_item_text_width)
            .fold(0.0, f32::max);
        let scrollbar = if self.items.len() > self.max_visible_items {
            MENU_SCROLLBAR_SPACE
        } else {
            0.0
        };
        self.preferred_trigger_width()
            .max(MENU_MIN_WIDTH)
            .max(longest_item + MENU_ROW_PADDING_X * 2.0 + self.icon_lane_width() + scrollbar)
    }

    fn icon_lane_width(&self) -> f32 {
        if self.items.iter().any(|item| item.icon.is_some() || item.checked) {
            MENU_ROW_ICON_SIZE + MENU_ROW_ICON_GAP
        } else {
            0.0
        }
    }

    fn menu_width(&self) -> f32 {
        self.bounds.width.max(self.preferred_menu_width())
    }

    fn visible_item_count(&self) -> usize {
        self.items.len().min(self.max_visible_items.max(1))
    }

    fn visible_content_height(&self) -> f32 {
        self.visible_item_count() as f32 * self.item_height
    }

    fn content_height(&self) -> f32 {
        self.items.len() as f32 * self.item_height
    }

    fn max_scroll_y(&self) -> f32 {
        (self.content_height() - self.visible_content_height()).max(0.0)
    }

    fn clamp_scroll_offset(&mut self) {
        self.scroll_offset = self.scroll_offset.clamp(0.0, self.max_scroll_y());
    }

    fn menu_rect(&self) -> Rect {
        anchored_menu_rect(
            self.trigger_rect(),
            self.menu_width(),
            self.visible_content_height() + MENU_POPUP_PADDING * 2.0,
            2.0,
            self.overlay_viewport.get(),
        )
    }

    fn item_rect(&self, index: usize) -> Rect {
        let menu = self.menu_rect();
        Rect::new(
            menu.x + MENU_POPUP_PADDING,
            menu.y + MENU_POPUP_PADDING + index as f32 * self.item_height - self.scroll_offset,
            (menu.width - MENU_POPUP_PADDING * 2.0).max(1.0),
            self.item_height,
        )
    }

    fn item_at(&self, position: Point) -> Option<usize> {
        if !self.menu_rect().contains(position) {
            return None;
        }
        let relative_y =
            position.y - (self.menu_rect().y + MENU_POPUP_PADDING) + self.scroll_offset;
        if relative_y < 0.0 {
            return None;
        }
        let index = (relative_y / self.item_height).floor() as usize;
        if index < self.items.len() && self.item_rect(index).contains(position) {
            Some(index)
        } else {
            None
        }
    }

    fn first_activatable_index(&self) -> Option<usize> {
        self.items.iter().position(MenuItem::is_activatable)
    }

    fn next_activatable_index(&self, direction: i32) -> Option<usize> {
        let activatable: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.is_activatable().then_some(index))
            .collect();
        if activatable.is_empty() {
            return None;
        }
        let current = self
            .hovered_index
            .and_then(|index| activatable.iter().position(|candidate| *candidate == index));
        let next = match (current, direction) {
            (Some(index), d) if d < 0 => (index + activatable.len() - 1) % activatable.len(),
            (Some(index), _) => (index + 1) % activatable.len(),
            (None, d) if d < 0 => activatable.len() - 1,
            (None, _) => 0,
        };
        activatable.get(next).copied()
    }

    fn ensure_hover_visible(&mut self) {
        let Some(index) = self.hovered_index else {
            return;
        };
        let row_top = index as f32 * self.item_height;
        let row_bottom = row_top + self.item_height;
        let view_top = self.scroll_offset;
        let view_bottom = self.scroll_offset + self.visible_content_height();
        if row_top < view_top {
            self.scroll_offset = row_top;
        } else if row_bottom > view_bottom {
            self.scroll_offset = row_bottom - self.visible_content_height();
        }
        self.clamp_scroll_offset();
    }

    fn activate_hovered(&mut self, ctx: &mut EventContext) -> bool {
        let Some(index) = self.hovered_index else {
            return false;
        };
        if !self.items[index].is_activatable() {
            return false;
        }
        (ctx.dispatch)(self.items[index].action.clone());
        self.close(ctx);
        true
    }

    fn open(&mut self, ctx: &mut EventContext) {
        self.open_with_release_suppression(ctx, true);
    }

    fn open_with_release_suppression(
        &mut self,
        ctx: &mut EventContext,
        suppress_next_release: bool,
    ) {
        self.open = true;
        self.hovered_index = self.first_activatable_index();
        self.pressed_index = None;
        self.suppress_next_release = suppress_next_release;
        self.clamp_scroll_offset();
        self.ensure_hover_visible();
        ctx.request_pointer_capture(self.id);
    }

    fn close(&mut self, ctx: &mut EventContext) {
        self.open = false;
        self.hovered_index = None;
        self.pressed_index = None;
        self.suppress_next_release = false;
        ctx.release_pointer_capture(self.id);
    }

    fn paint_open_menu(&self, ctx: &mut PaintContext) {
        if !self.open {
            return;
        }

        if !rect_has_paintable_area(ctx.clip_rect) {
            self.overlay_viewport.set(None);
            return;
        }
        self.overlay_viewport.set(Some(ctx.clip_rect));
        let menu_bg = self.menu_rect();

        paint_menu_popup_chrome(ctx, menu_bg);
        ctx.push_clip(menu_bg.inset(1.0, 1.0));
        let reserve_icon_lane = self.icon_lane_width() > 0.0;

        for (i, item) in self.items.iter().enumerate() {
            let item_rect = self.item_rect(i);
            if !item_rect.intersects(&menu_bg) {
                continue;
            }

            if item.is_separator() {
                paint_menu_separator(ctx, item_rect);
                continue;
            }

            paint_menu_row(
                ctx,
                item_rect,
                &item.label,
                item.shortcut.as_deref(),
                item.icon.as_ref(),
                reserve_icon_lane,
                MenuRowPaint {
                    enabled: item.enabled,
                    active: item.checked,
                    hovered: self.hovered_index == Some(i),
                },
            );
        }
        ctx.pop_clip();

        paint_menu_scrollbar(
            ctx,
            menu_bg,
            self.visible_content_height(),
            self.content_height(),
            self.scroll_offset,
        );
    }
}

impl Widget for Dropdown {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            self.preferred_trigger_width(),
            self.trigger_height(),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.clamp_scroll_offset();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            if self.open {
                self.close(ctx);
            }
            self.focused = false;
            self.focus_visible = false;
            return EventResult::Ignored;
        }
        if self.open {
            match event {
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    if self.trigger_rect().contains(*position) {
                        self.close(ctx);
                        return EventResult::Handled;
                    }
                    if let Some(i) = self.item_at(*position) {
                        self.pressed_index = Some(i);
                        return EventResult::Handled;
                    }
                    if !self.menu_rect().contains(*position) {
                        self.close(ctx);
                        return EventResult::Handled;
                    }
                    self.pressed_index = None;
                    return EventResult::Handled;
                }
                UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                    if self.suppress_next_release {
                        self.suppress_next_release = false;
                        return EventResult::Handled;
                    }
                    let released_index = self.item_at(*position);
                    if let (Some(pressed), Some(released)) = (self.pressed_index, released_index) {
                        if pressed == released && self.items[released].is_activatable() {
                            (ctx.dispatch)(self.items[released].action.clone());
                            self.close(ctx);
                        } else {
                            self.pressed_index = None;
                        }
                        return EventResult::Handled;
                    }
                    self.pressed_index = None;
                    if !self.trigger_rect().contains(*position)
                        && !self.menu_rect().contains(*position)
                    {
                        self.close(ctx);
                    }
                    return EventResult::Handled;
                }
                UiEvent::MouseMove { position, .. } => {
                    self.hovered_index =
                        self.item_at(*position).filter(|i| self.items[*i].is_activatable());
                    return EventResult::Handled;
                }
                UiEvent::MouseWheel { delta, position, .. } => {
                    if self.menu_rect().contains(*position) && self.max_scroll_y() > 0.0 {
                        let old = self.scroll_offset;
                        self.scroll_offset += *delta;
                        self.clamp_scroll_offset();
                        if (self.scroll_offset - old).abs() > 0.01 {
                            ctx.request_repaint();
                        }
                    }
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.close(ctx);
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Down, modifiers }
                    if *modifiers == Modifiers::none() =>
                {
                    self.hovered_index = self.next_activatable_index(1);
                    self.ensure_hover_visible();
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Up, modifiers }
                    if *modifiers == Modifiers::none() =>
                {
                    self.hovered_index = self.next_activatable_index(-1);
                    self.ensure_hover_visible();
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                    if *modifiers == Modifiers::none() =>
                {
                    self.activate_hovered(ctx);
                    return EventResult::Handled;
                }
                _ => {}
            }
        } else {
            match event {
                UiEvent::FocusGained => {
                    self.focused = true;
                    self.focus_visible = true;
                    return EventResult::Handled;
                }
                UiEvent::FocusLost => {
                    self.focused = false;
                    self.focus_visible = false;
                    return EventResult::Handled;
                }
                UiEvent::KeyDown {
                    key: KeyCode::Enter | KeyCode::Space | KeyCode::Down,
                    modifiers,
                } if self.focused && *modifiers == Modifiers::none() => {
                    self.focus_visible = false;
                    self.open(ctx);
                    return EventResult::Handled;
                }
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    if self.trigger_rect().contains(*position) {
                        self.focus_visible = false;
                        self.open(ctx);
                        return EventResult::Handled;
                    }
                }
                _ => {}
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if self.enabled {
            paint_menu_trigger(
                ctx,
                self.trigger_rect(),
                &self.label,
                self.open,
                self.trigger_style,
            );
        } else {
            let rect = self.trigger_rect();
            let tokens = &ctx.theme.colors;
            ctx.encoder.draw_rect(rect, tokens.muted, ctx.theme.spacing.radius_sm);
            paint_menu_trigger_label(
                ctx,
                rect,
                &self.label,
                tokens.muted_foreground,
                false,
                self.trigger_style,
            );
        }
        if self.focus_visible && !self.open {
            paint_focus_ring(ctx, self.trigger_rect(), ctx.theme.spacing.radius_sm);
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        self.paint_open_menu(ctx);
    }

    fn overlay_hit_test(&self, _point: Point) -> bool {
        self.enabled && self.open
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::state::PanelKind;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use std::cell::RefCell;

    #[derive(Debug, Clone, Copy, PartialEq)]
    struct SoftShadowCommand {
        bounds: Rect,
        color: Color,
        corner_radius: f32,
        blur_radius: f32,
        spread: f32,
        offset: glam::Vec2,
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
        soft_shadows: Vec<SoftShadowCommand>,
        clips: Vec<Rect>,
        clip_pops: usize,
        lines: usize,
        texts: Vec<String>,
        triangles: usize,
        raster_images: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, bounds: Rect, color: mondrian_core::Color, _corner_radius: f32) {
            self.rects.push(bounds);
            self.rect_colors.push(color);
        }

        fn draw_soft_shadow(
            &mut self,
            bounds: Rect,
            color: mondrian_core::Color,
            corner_radius: f32,
            blur_radius: f32,
            spread: f32,
            offset: glam::Vec2,
        ) {
            self.soft_shadows.push(SoftShadowCommand {
                bounds,
                color,
                corner_radius,
                blur_radius,
                spread,
                offset,
            });
        }

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
            self.lines += 1;
        }

        fn draw_triangles(&mut self, vertices: &[Point], _color: mondrian_core::Color) {
            self.triangles += vertices.len() / 3;
        }

        fn draw_raster_image(
            &mut self,
            _key: &str,
            _bounds: Rect,
            _width: u32,
            _height: u32,
            _rgba: std::sync::Arc<[u8]>,
            _tint: mondrian_core::Color,
        ) {
            self.raster_images += 1;
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.into());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn test_icon() -> VectorIcon {
        VectorIcon::from_svg_str(
            r#"<svg viewBox="0 0 16 16" xmlns="http://www.w3.org/2000/svg">
                <path d="M3 2L13 8L3 14Z" fill="black"/>
            </svg>"#,
        )
        .expect("test icon should parse")
    }

    #[test]
    fn dropdown_new_is_closed() {
        let d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        assert!(!d.open);
    }

    #[test]
    fn dropdown_updates_checked_state_for_matching_action() {
        let mut d = Dropdown::new(
            "View",
            vec![
                MenuItem::new("Viewer", Action::TogglePanel(PanelKind::Viewer)),
                MenuItem::new("Timeline", Action::TogglePanel(PanelKind::Timeline)),
            ],
        );

        d.set_checked_for_action(&Action::TogglePanel(PanelKind::Viewer), true);

        assert_eq!(
            d.checked_for_action(&Action::TogglePanel(PanelKind::Viewer)),
            Some(true)
        );
        assert_eq!(
            d.checked_for_action(&Action::TogglePanel(PanelKind::Timeline)),
            Some(false)
        );
    }

    #[test]
    fn dropdown_click_opens() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.open);
    }

    #[test]
    fn dropdown_exposes_trigger_and_open_state_for_menu_bar_coordination() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(20.0, 10.0, 120.0, 28.0));
        assert!(!d.is_open());
        assert!(d.trigger_contains(Point::new(30.0, 20.0)));
        assert!(!d.trigger_contains(Point::new(30.0, 48.0)));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.open_menu(&mut ctx);
        assert!(d.is_open());
        assert!(!d.suppress_next_release);
        d.close_menu(&mut ctx);
        assert!(!d.is_open());
    }

    #[test]
    fn disabled_dropdown_ignores_click_and_focus() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        )
        .disabled();
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert!(!d.open);
        assert!(!d.can_focus());
        assert!(ctx.requests.pointer_capture.is_none());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn dropdown_select_dispatches_and_closes() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Save", Action::SaveProject),
                MenuItem::new("Quit", Action::CloseProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        // Open
        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.open);

        // Opening suppresses the release that belongs to the trigger click.
        d.event(
            &UiEvent::MouseUp {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.open);
        assert!(cell.borrow().is_empty());

        // Click first item at y = 28 + 0*24 = 28 → should dispatch SaveProject
        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        d.event(
            &UiEvent::MouseUp {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!d.open);
        let actions = cell.into_inner();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], Action::SaveProject);
    }

    #[test]
    fn dropdown_click_trigger_when_open_closes() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let result = d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(!d.open);
    }

    #[test]
    fn dropdown_disabled_item_consumes_without_dispatch_or_close() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Save", Action::SaveProject),
                MenuItem::new("Disabled", Action::CloseProject).disabled(),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 64.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(d.open);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn dropdown_open_requests_capture_and_release_does_not_select() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut requests = EventRequests::default();
        let platform = NoopPlatformService;
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &dispatch_fn,
            platform: &platform,
            requests: &mut requests,
        };

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(d.id))
        );

        d.event(
            &UiEvent::MouseUp {
                position: Point::new(60.0, 40.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(d.open);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn dropdown_outside_click_closes_when_open() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(300.0, 300.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(!d.open);
    }

    #[test]
    fn dropdown_wheel_scrolls_visible_menu_down_and_up() {
        let items: Vec<MenuItem> = (0..12)
            .map(|i| MenuItem::new(format!("Item {i}"), Action::SaveProject))
            .collect();
        let mut d = Dropdown::new("File", items).with_max_visible_items(3);
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.event(
            &UiEvent::MouseWheel {
                delta: 48.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(d.scroll_offset > 0.0);
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;

        d.event(
            &UiEvent::MouseWheel {
                delta: -999.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.scroll_offset, 0.0);
        assert!(ctx.requests.repaint);
        ctx.requests.repaint = false;

        d.event(
            &UiEvent::MouseWheel {
                delta: -999.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.scroll_offset, 0.0);
        assert!(!ctx.requests.repaint);
    }

    #[test]
    fn dropdown_keyboard_navigation_skips_disabled_and_separators() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Disabled", Action::CloseProject).disabled(),
                MenuItem::separator(),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.event(
            &UiEvent::MouseDown {
                position: Point::new(60.0, 14.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.hovered_index, Some(0));

        d.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(d.hovered_index, Some(3));

        d.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert!(!d.open);
        assert_eq!(cell.into_inner(), vec![Action::SaveProject]);
    }

    #[test]
    fn dropdown_keyboard_navigation_wraps_up_from_first_item() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        d.open(&mut ctx);
        assert_eq!(d.hovered_index, Some(0));
        d.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(d.hovered_index, Some(1));
    }

    #[test]
    fn dropdown_keyboard_navigation_ignores_modified_keys_when_open() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.open(&mut ctx);
        d.hovered_index = Some(1);
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers::shift()),
            (KeyCode::Enter, Modifiers::ctrl()),
            (KeyCode::Space, Modifiers::shift()),
        ] {
            assert_eq!(
                d.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert!(d.open);
            assert_eq!(d.hovered_index, Some(1));
        }
        assert!(cell.borrow().is_empty());
    }

    #[test]
    fn dropdown_trigger_ignores_modified_keyboard_open_chords() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        assert_eq!(
            d.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Enter, Modifiers::shift()),
            (KeyCode::Space, Modifiers::ctrl()),
        ] {
            assert_eq!(
                d.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert!(!d.open);
        }
    }

    #[test]
    fn dropdown_overlay_hit_test_catches_outside_clicks_while_open() {
        let mut d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        assert!(!d.hit_test(Point::new(300.0, 300.0)));
        assert!(!d.overlay_hit_test(Point::new(300.0, 300.0)));

        d.open = true;
        assert!(!d.hit_test(Point::new(300.0, 300.0)));
        assert!(d.overlay_hit_test(Point::new(300.0, 300.0)));
    }

    #[test]
    fn dropdown_open_state_does_not_change_layout_measurement() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );

        let closed = d.measure(LayoutConstraint::LOOSE);
        d.open = true;
        let open = d.measure(LayoutConstraint::LOOSE);

        assert_eq!(open, closed);
        assert_eq!(open, Size::new(160.0, 28.0));
    }

    #[test]
    fn menu_bar_trigger_measures_as_compact_text_without_arrow_space() {
        let menu_bar_dropdown = Dropdown::new("File", vec![MenuItem::new("Open", Action::Copy)])
            .with_trigger_style(DropdownTriggerStyle::MenuBar);
        let filled_dropdown = Dropdown::new("File", vec![MenuItem::new("Open", Action::Copy)]);

        let menu_bar_size = menu_bar_dropdown.measure(LayoutConstraint::LOOSE);
        let filled_size = filled_dropdown.measure(LayoutConstraint::LOOSE);

        assert_eq!(menu_bar_size.height, MENU_BAR_TRIGGER_HEIGHT);
        assert!(menu_bar_size.width < 48.0);
        assert_eq!(filled_size.height, MENU_TRIGGER_HEIGHT);
        assert!(filled_size.width >= MENU_MIN_WIDTH);
    }

    #[test]
    fn dropdown_trigger_clips_long_label_before_arrow() {
        let mut d = Dropdown::new(
            "Very long trigger label that must not cover the arrow",
            vec![MenuItem::new("Open", Action::CloseProject)],
        );
        d.layout(Rect::new(0.0, 0.0, 80.0, 28.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint(&mut ctx);

        assert_eq!(
            encoder.texts,
            vec!["Very long trigger label that must not cover the arrow"]
        );
        assert_eq!(encoder.triangles, 1);
        assert_eq!(encoder.clips, vec![Rect::new(8.0, 0.0, 40.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn disabled_dropdown_trigger_clips_long_label_inside_control() {
        let mut d = Dropdown::new(
            "Disabled trigger label that should stay clipped",
            vec![MenuItem::new("Open", Action::CloseProject)],
        )
        .disabled();
        d.layout(Rect::new(0.0, 0.0, 80.0, 28.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint(&mut ctx);

        assert_eq!(
            encoder.texts,
            vec!["Disabled trigger label that should stay clipped"]
        );
        assert_eq!(encoder.triangles, 0);
        assert_eq!(encoder.clips, vec![Rect::new(8.0, 0.0, 64.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn dropdown_measures_trigger_label_without_using_popup_items() {
        let mut d = Dropdown::new(
            "Mode",
            vec![MenuItem::new(
                "A very long menu option that should widen the popup",
                Action::CloseProject,
            )],
        );

        assert_eq!(d.measure(LayoutConstraint::LOOSE), Size::new(160.0, 28.0));

        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;

        assert!(d.menu_rect().width > 300.0);
    }

    #[test]
    fn dropdown_long_trigger_label_expands_closed_measurement() {
        let d = Dropdown::new(
            "Very long color mode selector",
            vec![MenuItem::new("HEX", Action::CloseProject)],
        );

        let measured = d.measure(LayoutConstraint::LOOSE);

        assert!(measured.width > 180.0);
        assert_eq!(measured.height, 28.0);
    }

    #[test]
    fn dropdown_open_menu_paints_in_overlay_layer() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);

        let mut encoder = RecordingEncoder::default();
        {
            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            d.paint(&mut ctx);
        }
        assert_eq!(encoder.rects.len(), 1);
        assert_eq!(encoder.triangles, 1);

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint_overlay(&mut ctx);
        assert!(
            encoder.rects.len() > 1,
            "open dropdown should draw menu chrome during overlay paint"
        );
    }

    #[test]
    fn dropdown_menu_flips_above_bottom_viewport_and_keeps_hit_testing() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);
        let mut d = Dropdown::new(
            "Mode",
            vec![
                MenuItem::new("Alpha", Action::Play),
                MenuItem::new("Beta", Action::Pause),
            ],
        );
        d.layout(Rect::new(20.0, 110.0, 120.0, 28.0));
        d.open_menu(&mut ctx);

        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut paint_ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 180.0, 150.0),
        };
        d.paint_overlay(&mut paint_ctx);

        let menu = d.menu_rect();
        assert!(menu.y < d.trigger_rect().y);
        assert!(menu.y + menu.height <= 146.0);

        let beta = d.item_rect(1).center();
        assert_eq!(
            d.event(
                &UiEvent::MouseDown {
                    position: beta,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            d.event(
                &UiEvent::MouseUp {
                    position: beta,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(actions.borrow().as_slice(), &[Action::Pause]);
    }

    #[test]
    fn anchored_menu_rect_ignores_invalid_viewports() {
        let anchor = Rect::new(20.0, 30.0, 120.0, 28.0);
        let expected = Rect::new(20.0, 58.0, 160.0, 80.0);

        for viewport in [
            Rect::new(f32::NAN, 0.0, 240.0, 180.0),
            Rect::new(0.0, f32::INFINITY, 240.0, 180.0),
            Rect::new(0.0, 0.0, 0.0, 180.0),
            Rect::new(0.0, 0.0, 240.0, -1.0),
        ] {
            assert_eq!(
                anchored_menu_rect(anchor, 160.0, 80.0, 0.0, Some(viewport)),
                expected
            );
        }
    }

    #[test]
    fn dropdown_overlay_skips_paint_when_clip_is_invalid_or_empty() {
        for clip_rect in [
            Rect::new(0.0, 0.0, 0.0, 120.0),
            Rect::new(0.0, 0.0, f32::INFINITY, 120.0),
        ] {
            let mut d = Dropdown::new(
                "File",
                vec![MenuItem::new("Open", Action::OpenProject("".into()))],
            );
            d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
            d.open = true;
            let theme = mondrian_ui_theme::ThemePreset::Dark.build();
            let mut encoder = RecordingEncoder::default();

            let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
            d.paint_overlay(&mut ctx);

            assert!(encoder.rects.is_empty());
            assert!(encoder.clips.is_empty());
            assert!(encoder.texts.is_empty());
            assert_eq!(d.overlay_viewport.get(), None);
        }
    }

    #[test]
    fn menu_popup_shadow_uses_analytic_theme_shadow_tokens() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);
        let mut encoder = RecordingEncoder::default();
        let popup = Rect::new(20.0, 30.0, 120.0, 64.0);

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_popup_chrome(&mut ctx, popup);

        assert_eq!(encoder.soft_shadows.len(), 2);
        assert_eq!(encoder.soft_shadows[0].bounds, popup);
        assert_eq!(
            encoder.soft_shadows[0].blur_radius,
            theme.spacing.shadow_xl.blur
        );
        assert_eq!(
            encoder.soft_shadows[0].offset,
            glam::Vec2::new(
                theme.spacing.shadow_xl.offset_x,
                theme.spacing.shadow_xl.offset_y
            )
        );
        assert_eq!(
            encoder.soft_shadows[1].blur_radius,
            theme.spacing.shadow_md.blur
        );
        assert!(encoder.soft_shadows[0].color.a > encoder.soft_shadows[1].color.a);
        assert_eq!(encoder.rects[0], popup);
        assert_eq!(encoder.rect_colors[0], theme.colors.border_strong);
    }

    #[test]
    fn dropdown_disabled_item_paints_text_without_strikethrough() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::new("Disabled", Action::CloseProject).disabled(),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint_overlay(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Disabled"));
        assert_eq!(encoder.lines, 0);
    }

    #[test]
    fn menu_row_clips_label_to_padded_text_lane() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 120.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 64.0, 24.0),
            "Disabled option with a very long label",
            None,
            None,
            false,
            MenuRowPaint { enabled: false, active: false, hovered: false },
        );

        assert_eq!(
            encoder.texts,
            vec!["Disabled option with a very long label"]
        );
        assert_eq!(encoder.clips, vec![Rect::new(20.0, 20.0, 44.0, 24.0)]);
        assert_eq!(encoder.clip_pops, 1);
        assert_eq!(encoder.lines, 0);
    }

    #[test]
    fn menu_row_paints_optional_icon_and_aligns_label_after_icon_lane() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 180.0, 80.0);
        let mut encoder = RecordingEncoder::default();
        let icon = test_icon();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 128.0, 24.0),
            "Open",
            None,
            Some(&icon),
            true,
            MenuRowPaint { enabled: true, active: false, hovered: false },
        );

        assert_eq!(encoder.texts, vec!["Open"]);
        assert_eq!(encoder.triangles + encoder.raster_images, 1);
        assert!(icon.triangle_count() > 0);
        assert_eq!(encoder.clips, vec![Rect::new(43.0, 20.0, 85.0, 24.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }

    #[test]
    fn menu_row_checked_state_paints_checkmark_without_accent_selection_chrome() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 180.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 128.0, 28.0),
            "Inspector",
            Some("Ctrl+Alt+I"),
            None,
            true,
            MenuRowPaint { enabled: true, active: true, hovered: false },
        );

        assert_eq!(encoder.lines, 2);
        assert_eq!(encoder.rects[0], Rect::new(10.0, 20.0, 128.0, 28.0));
        assert_eq!(encoder.rect_colors[0], theme.colors.popover);
        assert!(
            !encoder.rect_colors.iter().any(|color| *color == theme.colors.primary),
            "checked rows should not paint blue row chrome"
        );
    }

    #[test]
    fn menu_row_paints_shortcut_in_trailing_lane() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 220.0, 80.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_row(
            &mut ctx,
            Rect::new(10.0, 20.0, 160.0, 24.0),
            "Save Project With Long Label",
            Some("Ctrl+S"),
            None,
            false,
            MenuRowPaint { enabled: true, active: false, hovered: false },
        );

        assert_eq!(
            encoder.texts,
            vec!["Save Project With Long Label", "Ctrl+S"]
        );
        assert_eq!(encoder.clips.len(), 2);
        assert!(encoder.clips[0].x < encoder.clips[1].x);
        assert_eq!(encoder.clip_pops, 2);
    }

    #[test]
    fn dropdown_popup_width_reserves_icon_lane_when_items_have_icons() {
        let label = "Compact menu item with enough text";
        let mut plain = Dropdown::new(
            "File",
            vec![MenuItem::new(label, Action::OpenProject("".into()))],
        );
        let mut iconized = Dropdown::new(
            "File",
            vec![MenuItem::new(label, Action::OpenProject("".into())).with_icon(test_icon())],
        );
        plain.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        iconized.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        assert!(iconized.menu_rect().width > plain.menu_rect().width);
    }

    #[test]
    fn dropdown_popup_width_reserves_shortcut_lane() {
        let label = "Save Project With Media Cache";
        let mut plain = Dropdown::new("File", vec![MenuItem::new(label, Action::SaveProject)]);
        let mut with_shortcut = Dropdown::new(
            "File",
            vec![MenuItem::new(label, Action::SaveProject).with_shortcut("Ctrl+S")],
        );
        plain.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        with_shortcut.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        assert!(with_shortcut.menu_rect().width > plain.menu_rect().width);
    }

    #[test]
    fn menu_item_shortcut_builder_preserves_action_semantics() {
        let item = MenuItem::new("Save", Action::SaveProject).with_shortcut("Ctrl+S");

        assert_eq!(item.action, Action::SaveProject);
        assert_eq!(item.shortcut.as_deref(), Some("Ctrl+S"));
        assert!(item.is_activatable());
    }

    #[test]
    fn dropdown_separator_paints_geometry_not_text() {
        let mut d = Dropdown::new(
            "File",
            vec![
                MenuItem::new("Open", Action::OpenProject("".into())),
                MenuItem::separator(),
                MenuItem::new("Save", Action::SaveProject),
            ],
        );
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
        d.open = true;
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 140.0);
        let mut encoder = RecordingEncoder::default();

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        d.paint_overlay(&mut ctx);

        assert_eq!(encoder.texts, vec!["Open".to_string(), "Save".to_string()]);
        assert!(
            encoder.rects.iter().any(|rect| rect.height == 1.0),
            "separator should be drawn as a geometric divider"
        );
    }
}
