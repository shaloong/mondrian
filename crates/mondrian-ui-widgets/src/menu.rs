//! Dropdown Menu 控件
//!
//! 点击展开菜单列表，点击选项或外部区域关闭，派发 Action。

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::paint::{mix_color, paint_focus_ring, paint_shadow};
use crate::text_metrics::measure_single_line;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct MenuRowPaint {
    pub enabled: bool,
    pub active: bool,
    pub hovered: bool,
}

const MENU_MEASURE_FONT_SIZE: f32 = 13.0;
const MENU_MIN_WIDTH: f32 = 120.0;
const MENU_TRIGGER_HEIGHT: f32 = 28.0;
const MENU_TRIGGER_PADDING_X: f32 = 8.0;
const MENU_ARROW_SPACE: f32 = 24.0;
const MENU_ROW_PADDING_X: f32 = 16.0;
const MENU_SCROLLBAR_SPACE: f32 = 8.0;

pub(crate) fn paint_menu_trigger(ctx: &mut PaintContext, rect: Rect, label: &str, open: bool) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let fill = if open { tokens.primary } else { tokens.card };
    ctx.encoder.draw_rect(rect, fill, spacing.radius_sm);
    paint_menu_trigger_label(ctx, rect, label, tokens.foreground, true);
    paint_menu_arrow(ctx, rect);
}

pub(crate) fn paint_menu_trigger_label(
    ctx: &mut PaintContext,
    rect: Rect,
    label: &str,
    color: Color,
    reserve_arrow: bool,
) {
    if label.is_empty() {
        return;
    }
    let reserved_right = if reserve_arrow { MENU_ARROW_SPACE } else { 0.0 };
    let text_width = rect.width - MENU_TRIGGER_PADDING_X * 2.0 - reserved_right;
    if text_width <= 0.0 {
        return;
    }
    let clip = Rect::new(
        rect.x + MENU_TRIGGER_PADDING_X,
        rect.y,
        text_width,
        rect.height,
    );
    ctx.encoder.push_clip(clip);
    ctx.encoder.draw_text(
        label,
        ctx.theme.typography.body.font_size,
        Point::new(rect.x + MENU_TRIGGER_PADDING_X, rect.y + 5.0),
        color,
    );
    ctx.encoder.pop_clip();
}

pub(crate) fn paint_menu_popup_chrome(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    paint_shadow(ctx, rect, spacing.radius_sm);
    ctx.encoder.draw_rect(rect, tokens.border, 0.0);
    ctx.encoder.draw_rect(rect.inset(1.0, 1.0), tokens.popover, spacing.radius_sm);
}

pub(crate) fn paint_menu_row(ctx: &mut PaintContext, rect: Rect, label: &str, state: MenuRowPaint) {
    let tokens = &ctx.theme.colors;
    let spacing = &ctx.theme.spacing;
    let font_size = ctx.theme.typography.body.font_size;
    let fill = if state.active {
        mix_color(tokens.popover, tokens.primary, 0.16)
    } else if state.hovered && state.enabled {
        tokens.accent
    } else {
        tokens.popover
    };
    ctx.encoder.draw_rect(rect, fill, spacing.radius_sm);
    if state.active {
        ctx.encoder.draw_rect(
            Rect::new(
                rect.x + 4.0,
                rect.y + 7.0,
                3.0,
                (rect.height - 14.0).max(1.0),
            ),
            tokens.primary,
            2.0,
        );
    }
    let text_color = if state.enabled {
        if state.active {
            tokens.foreground
        } else {
            tokens.popover_foreground
        }
    } else {
        tokens.muted_foreground
    };
    ctx.encoder.draw_text(
        label,
        font_size,
        Point::new(rect.x + if state.active { 12.0 } else { 6.0 }, rect.y + 5.0),
        text_color,
    );
}

pub(crate) fn paint_menu_separator(ctx: &mut PaintContext, rect: Rect) {
    let tokens = &ctx.theme.colors;
    let line = Rect::new(
        rect.x + 8.0,
        rect.y + (rect.height * 0.5).floor(),
        (rect.width - 16.0).max(1.0),
        1.0,
    );
    ctx.encoder.draw_rect(line, tokens.border, 0.0);
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
    pub enabled: bool,
    pub kind: MenuItemKind,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        Self {
            label: label.into(),
            action,
            enabled: true,
            kind: MenuItemKind::Action,
        }
    }

    /// Create a visual divider row.
    pub fn separator() -> Self {
        Self {
            label: String::new(),
            action: Action::DeselectAll,
            enabled: false,
            kind: MenuItemKind::Separator,
        }
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
            item_height: 24.0,
            max_visible_items: 8,
            scroll_offset: 0.0,
            suppress_next_release: false,
            focused: false,
            focus_visible: false,
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

    /// Whether the popup menu is currently open.
    pub fn is_open(&self) -> bool {
        self.open
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

    /// Limit how many rows are visible before the open menu scrolls.
    pub fn with_max_visible_items(mut self, max_visible_items: usize) -> Self {
        self.max_visible_items = max_visible_items.max(1);
        self.clamp_scroll_offset();
        self
    }

    fn trigger_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            MENU_TRIGGER_HEIGHT,
        )
    }

    fn preferred_trigger_width(&self) -> f32 {
        let (label_width, _) = measure_single_line(&self.label, MENU_MEASURE_FONT_SIZE);
        MENU_MIN_WIDTH.max(label_width + MENU_TRIGGER_PADDING_X * 2.0 + MENU_ARROW_SPACE)
    }

    fn preferred_menu_width(&self) -> f32 {
        let longest_item = self
            .items
            .iter()
            .filter(|item| !item.is_separator())
            .map(|item| measure_single_line(&item.label, MENU_MEASURE_FONT_SIZE).0)
            .fold(0.0, f32::max);
        let scrollbar = if self.items.len() > self.max_visible_items {
            MENU_SCROLLBAR_SPACE
        } else {
            0.0
        };
        self.preferred_trigger_width()
            .max(MENU_MIN_WIDTH)
            .max(longest_item + MENU_ROW_PADDING_X * 2.0 + scrollbar)
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
        Rect::new(
            self.bounds.x,
            self.bounds.y + MENU_TRIGGER_HEIGHT,
            self.menu_width(),
            self.visible_content_height() + 4.0,
        )
    }

    fn item_rect(&self, index: usize) -> Rect {
        Rect::new(
            self.bounds.x + 2.0,
            self.bounds.y + MENU_TRIGGER_HEIGHT + 2.0 + index as f32 * self.item_height
                - self.scroll_offset,
            self.menu_width() - 4.0,
            self.item_height,
        )
    }

    fn item_at(&self, position: Point) -> Option<usize> {
        if !self.menu_rect().contains(position) {
            return None;
        }
        let relative_y =
            position.y - (self.bounds.y + MENU_TRIGGER_HEIGHT + 2.0) + self.scroll_offset;
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
        self.open = true;
        self.hovered_index = self.first_activatable_index();
        self.pressed_index = None;
        self.suppress_next_release = true;
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

        let menu_bg = self.menu_rect();

        paint_menu_popup_chrome(ctx, menu_bg);
        ctx.encoder.push_clip(menu_bg.inset(1.0, 1.0));

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
                MenuRowPaint {
                    enabled: item.enabled,
                    active: false,
                    hovered: self.hovered_index == Some(i),
                },
            );
        }
        ctx.encoder.pop_clip();

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
            MENU_TRIGGER_HEIGHT,
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
                        self.scroll_offset += *delta;
                        self.clamp_scroll_offset();
                    }
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.close(ctx);
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Down, .. } => {
                    self.hovered_index = self.next_activatable_index(1);
                    self.ensure_hover_visible();
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Up, .. } => {
                    self.hovered_index = self.next_activatable_index(-1);
                    self.ensure_hover_visible();
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } => {
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
                    ..
                } if self.focused => {
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
            paint_menu_trigger(ctx, self.trigger_rect(), &self.label, self.open);
        } else {
            let rect = self.trigger_rect();
            let tokens = &ctx.theme.colors;
            ctx.encoder.draw_rect(rect, tokens.muted, ctx.theme.spacing.radius_sm);
            paint_menu_trigger_label(ctx, rect, &self.label, tokens.muted_foreground, false);
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
    use crate::paint::{shadow_color, shadow_rect};
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use std::cell::RefCell;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        rect_colors: Vec<Color>,
        clips: Vec<Rect>,
        clip_pops: usize,
        lines: usize,
        texts: Vec<String>,
        triangles: usize,
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

    #[test]
    fn dropdown_new_is_closed() {
        let d = Dropdown::new(
            "File",
            vec![MenuItem::new("Open", Action::OpenProject("".into()))],
        );
        assert!(!d.open);
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

        d.open(&mut ctx);
        assert!(d.is_open());
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

        d.event(
            &UiEvent::MouseWheel {
                delta: -999.0,
                position: Point::new(60.0, 40.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(d.scroll_offset, 0.0);
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
        assert_eq!(open, Size::new(120.0, 28.0));
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

        assert_eq!(d.measure(LayoutConstraint::LOOSE), Size::new(120.0, 28.0));

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
    fn menu_popup_shadow_uses_theme_shadow_token() {
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let clip_rect = Rect::new(0.0, 0.0, 200.0, 120.0);
        let mut encoder = RecordingEncoder::default();
        let popup = Rect::new(20.0, 30.0, 120.0, 64.0);

        let mut ctx = PaintContext { encoder: &mut encoder, theme: &theme, clip_rect };
        paint_menu_popup_chrome(&mut ctx, popup);

        let shadow = &theme.spacing.shadow_md;
        assert_eq!(encoder.rects[0], shadow_rect(popup, shadow));
        assert_eq!(encoder.rect_colors[0], shadow_color(shadow));
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
