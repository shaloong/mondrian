//! Reusable list surface for editor panels.
//!
//! This widget is intended for assets, effects, presets, and other panel
//! browsers. It owns selection and scrolling locally, while exposing typed
//! action adapters so application state can remain outside the UI crate.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::time::{Duration, Instant};

const DOUBLE_CLICK_MAX_AGE: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_MAX_DISTANCE: f32 = 5.0;

/// Dynamic action factory used when a panel-list item changes state.
pub type PanelListAction = dyn Fn(usize, &PanelListItem) -> Action;

/// Item rendered by [`PanelList`].
#[derive(Debug, Clone)]
pub struct PanelListItem {
    pub title: String,
    pub subtitle: String,
    pub badge: Option<String>,
    pub accent: Option<Color>,
    pub disabled: bool,
    pub select_action: Option<Action>,
    pub activate_action: Option<Action>,
}

impl PanelListItem {
    /// Create a selectable panel-list item.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: String::new(),
            badge: None,
            accent: None,
            disabled: false,
            select_action: None,
            activate_action: None,
        }
    }

    /// Set secondary text shown below the title.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set a compact right-aligned status badge.
    pub fn with_badge(mut self, badge: impl Into<String>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Set a left accent swatch.
    pub fn with_accent(mut self, accent: Color) -> Self {
        self.accent = Some(accent);
        self
    }

    /// Mark the item as disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Dispatch a static action when this item becomes selected.
    pub fn with_select_action(mut self, action: Action) -> Self {
        self.select_action = Some(action);
        self
    }

    /// Dispatch a static action when this item is activated.
    pub fn with_activate_action(mut self, action: Action) -> Self {
        self.activate_action = Some(action);
        self
    }
}

/// Scrollable, keyboard-navigable list for panel content.
pub struct PanelList {
    id: WidgetId,
    title: String,
    subtitle: String,
    items: Vec<PanelListItem>,
    bounds: Rect,
    viewport: Rect,
    selected: Option<usize>,
    hovered: Option<usize>,
    scroll_y: f32,
    row_height: f32,
    scrollbar_hovered: bool,
    scrollbar_dragging: bool,
    drag_start_y: f32,
    drag_start_scroll_y: f32,
    last_click: Option<PanelListClick>,
    focused: bool,
    focus_visible: bool,
    on_select: Option<Box<PanelListAction>>,
    on_activate: Option<Box<PanelListAction>>,
}

#[derive(Debug, Clone, Copy)]
struct PanelListClick {
    index: usize,
    position: Point,
    time: Instant,
}

impl PanelList {
    /// Create a panel list with a title and rows.
    pub fn new(title: impl Into<String>, items: Vec<PanelListItem>) -> Self {
        Self {
            id: WidgetId::new(),
            title: title.into(),
            subtitle: String::new(),
            items,
            bounds: Rect::ZERO,
            viewport: Rect::ZERO,
            selected: None,
            hovered: None,
            scroll_y: 0.0,
            row_height: 50.0,
            scrollbar_hovered: false,
            scrollbar_dragging: false,
            drag_start_y: 0.0,
            drag_start_scroll_y: 0.0,
            last_click: None,
            focused: false,
            focus_visible: false,
            on_select: None,
            on_activate: None,
        }
    }

    /// Set a small explanatory subtitle below the title.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set the row height. Values below 36 px are clamped for readability.
    pub fn with_row_height(mut self, row_height: f32) -> Self {
        self.row_height = row_height.max(36.0);
        self
    }

    /// Set the selected item if it is valid and enabled.
    pub fn with_selected(mut self, index: Option<usize>) -> Self {
        self.set_selected(index);
        self
    }

    /// Dispatch a dynamic action when selection changes.
    pub fn on_select(mut self, action: impl Fn(usize, &PanelListItem) -> Action + 'static) -> Self {
        self.on_select = Some(Box::new(action));
        self
    }

    /// Dispatch a dynamic action when the current item is activated.
    pub fn on_activate(
        mut self,
        action: impl Fn(usize, &PanelListItem) -> Action + 'static,
    ) -> Self {
        self.on_activate = Some(Box::new(action));
        self
    }

    /// Replace all items and clamp selection/scroll state.
    pub fn set_items(&mut self, items: Vec<PanelListItem>) {
        self.items = items;
        self.selected = self.selected.filter(|idx| self.is_enabled_index(*idx));
        self.hovered = None;
        self.last_click = None;
        self.clamp_scroll();
    }

    /// Currently selected item index.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Current vertical scroll offset in content pixels.
    pub fn scroll_offset_y(&self) -> f32 {
        self.scroll_y
    }

    /// Set selected item without dispatching actions.
    pub fn set_selected(&mut self, index: Option<usize>) {
        self.selected = index.filter(|idx| self.is_enabled_index(*idx));
        self.ensure_selected_visible();
    }

    fn header_height(&self) -> f32 {
        if self.subtitle.is_empty() {
            42.0
        } else {
            60.0
        }
    }

    fn content_height(&self) -> f32 {
        self.items.len() as f32 * self.row_height
    }

    fn max_scroll_y(&self) -> f32 {
        (self.content_height() - self.viewport.height).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll_y());
    }

    fn set_scroll_y(&mut self, scroll_y: f32) -> bool {
        let old = self.scroll_y;
        self.scroll_y = scroll_y.clamp(0.0, self.max_scroll_y());
        (self.scroll_y - old).abs() > 0.01
    }

    fn scrollbar_track_rect(&self) -> Option<Rect> {
        (self.max_scroll_y() > 0.0 && self.viewport.height > 0.0).then_some(Rect::new(
            self.viewport.x + self.viewport.width - 6.0,
            self.viewport.y + 2.0,
            4.0,
            (self.viewport.height - 4.0).max(0.0),
        ))
    }

    fn scrollbar_thumb_rect(&self) -> Option<Rect> {
        let track = self.scrollbar_track_rect()?;
        let content_height = self.content_height();
        if content_height <= 0.0 {
            return None;
        }
        let thumb_height = (self.viewport.height / content_height * track.height)
            .clamp(24.0, track.height.max(24.0));
        let travel = (track.height - thumb_height).max(0.0);
        let y = if self.max_scroll_y() <= 0.0 {
            track.y
        } else {
            track.y + (self.scroll_y / self.max_scroll_y()) * travel
        };
        Some(Rect::new(
            track.x,
            y,
            track.width,
            thumb_height.min(track.height),
        ))
    }

    fn scroll_y_for_thumb_delta(&self, delta_y: f32) -> f32 {
        let Some(track) = self.scrollbar_track_rect() else {
            return self.scroll_y;
        };
        let Some(thumb) = self.scrollbar_thumb_rect() else {
            return self.scroll_y;
        };
        let travel = (track.height - thumb.height).max(1.0);
        self.drag_start_scroll_y + delta_y / travel * self.max_scroll_y()
    }

    fn is_enabled_index(&self, index: usize) -> bool {
        self.items.get(index).is_some_and(|item| !item.disabled)
    }

    fn index_at(&self, point: Point) -> Option<usize> {
        if !self.viewport.contains(point) {
            return None;
        }
        if self.scrollbar_track_rect().is_some_and(|track| track.contains(point)) {
            return None;
        }
        let rel_y = point.y - self.viewport.y + self.scroll_y;
        let index = (rel_y / self.row_height).floor() as usize;
        (index < self.items.len()).then_some(index)
    }

    fn next_enabled_from(&self, start: usize, direction: i32) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }

        let mut index = start as i32;
        while index >= 0 && (index as usize) < self.items.len() {
            let candidate = index as usize;
            if self.is_enabled_index(candidate) {
                return Some(candidate);
            }
            index += direction;
        }
        None
    }

    fn first_enabled(&self) -> Option<usize> {
        self.next_enabled_from(0, 1)
    }

    fn last_enabled(&self) -> Option<usize> {
        self.items
            .len()
            .checked_sub(1)
            .and_then(|last| self.next_enabled_from(last, -1))
    }

    fn move_selection(&mut self, direction: i32) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        let start = match (self.selected, direction) {
            (Some(index), 1) => (index + 1).min(self.items.len().saturating_sub(1)),
            (Some(index), -1) => index.saturating_sub(1),
            (_, 1) => 0,
            (_, -1) => self.items.len().saturating_sub(1),
            _ => 0,
        };
        self.next_enabled_from(start, direction)
    }

    fn ensure_selected_visible(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        if self.viewport.height <= 0.0 {
            return;
        }
        let top = index as f32 * self.row_height;
        let bottom = top + self.row_height;
        if top < self.scroll_y {
            self.scroll_y = top;
        } else if bottom > self.scroll_y + self.viewport.height {
            self.scroll_y = bottom - self.viewport.height;
        }
        self.clamp_scroll();
    }

    fn select_from_input(&mut self, index: usize, ctx: &mut EventContext) -> EventResult {
        if !self.is_enabled_index(index) {
            return EventResult::Handled;
        }
        if self.selected != Some(index) {
            self.selected = Some(index);
            self.ensure_selected_visible();
            self.dispatch_select(index, ctx);
            ctx.request_repaint();
        }
        EventResult::Handled
    }

    fn click_is_activation(&self, index: usize, position: Point, now: Instant) -> bool {
        let Some(last) = self.last_click else {
            return false;
        };
        if last.index != index || now.duration_since(last.time) > DOUBLE_CLICK_MAX_AGE {
            return false;
        }
        let dx = position.x - last.position.x;
        let dy = position.y - last.position.y;
        dx * dx + dy * dy <= DOUBLE_CLICK_MAX_DISTANCE * DOUBLE_CLICK_MAX_DISTANCE
    }

    fn select_or_activate_from_input(
        &mut self,
        index: usize,
        position: Point,
        ctx: &mut EventContext,
    ) -> EventResult {
        if !self.is_enabled_index(index) {
            self.last_click = None;
            return EventResult::Handled;
        }

        let now = Instant::now();
        let activate = self.click_is_activation(index, position, now);
        self.last_click = Some(PanelListClick { index, position, time: now });

        if self.selected != Some(index) {
            self.selected = Some(index);
            self.ensure_selected_visible();
            self.dispatch_select(index, ctx);
            ctx.request_repaint();
        }
        if activate {
            self.dispatch_activate(index, ctx);
        }
        EventResult::Handled
    }

    fn activate_selected(&self, ctx: &mut EventContext) -> EventResult {
        let Some(index) = self.selected.filter(|idx| self.is_enabled_index(*idx)) else {
            return EventResult::Ignored;
        };
        self.dispatch_activate(index, ctx);
        EventResult::Handled
    }

    fn dispatch_select(&self, index: usize, ctx: &mut EventContext) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        if let Some(action) = item.select_action.clone() {
            (ctx.dispatch)(action);
        }
        if let Some(factory) = &self.on_select {
            (ctx.dispatch)(factory(index, item));
        }
    }

    fn dispatch_activate(&self, index: usize, ctx: &mut EventContext) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        if let Some(action) = item.activate_action.clone() {
            (ctx.dispatch)(action);
        }
        if let Some(factory) = &self.on_activate {
            (ctx.dispatch)(factory(index, item));
        }
    }

    fn paint_row(&self, ctx: &mut PaintContext, index: usize, row: Rect) {
        let item = &self.items[index];
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let selected = self.selected == Some(index);
        let hovered = self.hovered == Some(index) && !item.disabled;

        let fill = if selected {
            colors.accent
        } else if hovered {
            colors.muted
        } else {
            colors.card
        };
        if selected {
            let mut ring = colors.ring;
            ring.a = 0.42;
            ctx.encoder.draw_rect(row.inset(-1.0, -1.0), ring, spacing.radius_sm + 1.0);
        }
        ctx.encoder.draw_rect(row, fill, spacing.radius_sm);

        let accent = item.accent.unwrap_or(colors.secondary);
        let swatch = Rect::new(row.x + 8.0, row.y + 13.0, 6.0, row.height - 26.0);
        ctx.encoder.draw_rect(swatch, accent, 3.0);

        let text_x = row.x + 22.0;
        let badge_reserved = if item.badge.is_some() { 58.0 } else { 8.0 };
        let text_width = (row.width - 30.0 - badge_reserved).max(24.0);
        let title_color = if item.disabled {
            colors.muted_foreground
        } else if selected {
            colors.accent_foreground
        } else {
            colors.foreground
        };

        ctx.encoder.push_clip(row.inset(4.0, 2.0));
        ctx.encoder.draw_text_box(
            &item.title,
            ctx.theme.typography.body.font_size,
            snap_point(Point::new(text_x, row.y + 7.0)),
            text_width,
            title_color,
        );
        if !item.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &item.subtitle,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(text_x, row.y + 28.0)),
                text_width,
                colors.muted_foreground,
            );
        }
        ctx.encoder.pop_clip();

        if let Some(badge) = &item.badge {
            let badge_rect = Rect::new(row.x + row.width - 54.0, row.y + 13.0, 44.0, 22.0);
            ctx.encoder.draw_rect(badge_rect, colors.secondary, spacing.radius_sm);
            ctx.encoder.push_clip(badge_rect);
            ctx.encoder.draw_text(
                badge,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(badge_rect.x + 8.0, badge_rect.y + 4.0)),
                colors.secondary_foreground,
            );
            ctx.encoder.pop_clip();
        }
    }
}

impl Widget for PanelList {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            240.0,
            self.header_height() + self.content_height(),
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let padding = 8.0;
        let header = self.header_height();
        self.viewport = Rect::new(
            bounds.x + padding,
            bounds.y + header,
            (bounds.width - padding * 2.0).max(0.0),
            (bounds.height - header - padding).max(0.0),
        );
        self.clamp_scroll();
        self.ensure_selected_visible();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.bounds.contains(*position) {
                    self.focus_visible = false;
                    if let Some(thumb) = self.scrollbar_thumb_rect() {
                        if thumb.contains(*position) {
                            self.scrollbar_dragging = true;
                            self.drag_start_y = position.y;
                            self.drag_start_scroll_y = self.scroll_y;
                            ctx.request_pointer_capture(self.id);
                            ctx.request_repaint();
                            return EventResult::Handled;
                        }
                    }
                    if let Some(track) = self.scrollbar_track_rect() {
                        if track.contains(*position) {
                            let page = self.viewport.height.max(self.row_height);
                            let thumb_y =
                                self.scrollbar_thumb_rect().map_or(track.y, |rect| rect.y);
                            let target = if position.y < thumb_y {
                                self.scroll_y - page
                            } else {
                                self.scroll_y + page
                            };
                            if self.set_scroll_y(target) {
                                ctx.request_repaint();
                            }
                            return EventResult::Handled;
                        }
                    }
                    if let Some(index) = self.index_at(*position) {
                        return self.select_or_activate_from_input(index, *position, ctx);
                    }
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.scrollbar_dragging => {
                self.scrollbar_dragging = false;
                ctx.release_pointer_capture(self.id);
                ctx.request_repaint();
                return EventResult::Handled;
            }
            UiEvent::MouseMove { position, .. } => {
                if self.scrollbar_dragging {
                    if self
                        .set_scroll_y(self.scroll_y_for_thumb_delta(position.y - self.drag_start_y))
                    {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
                let scrollbar_hovered =
                    self.scrollbar_thumb_rect().is_some_and(|thumb| thumb.contains(*position));
                let mut handled = false;
                if scrollbar_hovered != self.scrollbar_hovered {
                    self.scrollbar_hovered = scrollbar_hovered;
                    ctx.request_repaint();
                    handled = true;
                }
                let hover = self.index_at(*position);
                if hover != self.hovered {
                    self.hovered = hover;
                    ctx.request_repaint();
                    handled = true;
                }
                if handled {
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseWheel { delta, position, .. } => {
                if self.bounds.contains(*position) {
                    if self.set_scroll_y(self.scroll_y + *delta) {
                        ctx.request_repaint();
                    }
                    return EventResult::Handled;
                }
            }
            UiEvent::FocusGained => {
                self.focused = true;
                self.focus_visible = true;
                return EventResult::Handled;
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.focus_visible = false;
                self.last_click = None;
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Down, .. } if self.focused => {
                if let Some(index) = self.move_selection(1) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Up, .. } if self.focused => {
                if let Some(index) = self.move_selection(-1) {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Home, .. } if self.focused => {
                if let Some(index) = self.first_enabled() {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::End, .. } if self.focused => {
                if let Some(index) = self.last_enabled() {
                    return self.select_from_input(index, ctx);
                }
                return EventResult::Ignored;
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } if self.focused => {
                return self.activate_selected(ctx);
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        ctx.encoder.draw_rect(self.bounds, colors.card, 0.0);

        let title_pos = snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 12.0));
        ctx.encoder.draw_text(
            &self.title,
            ctx.theme.typography.body.font_size,
            title_pos,
            colors.foreground,
        );
        if !self.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &self.subtitle,
                ctx.theme.typography.small.font_size,
                snap_point(Point::new(self.bounds.x + 12.0, self.bounds.y + 34.0)),
                (self.bounds.width - 24.0).max(0.0),
                colors.muted_foreground,
            );
        }

        let divider_y = self.viewport.y - 7.0;
        ctx.encoder.draw_line(
            Point::new(self.bounds.x, divider_y),
            Point::new(self.bounds.x + self.bounds.width, divider_y),
            1.0,
            colors.border,
        );

        ctx.encoder.push_clip(self.viewport);
        let first = (self.scroll_y / self.row_height).floor().max(0.0) as usize;
        let last = ((self.scroll_y + self.viewport.height) / self.row_height).ceil() as usize + 1;
        for index in first..last.min(self.items.len()) {
            let y = self.viewport.y + index as f32 * self.row_height - self.scroll_y;
            let row = Rect::new(
                self.viewport.x,
                y + 2.0,
                self.viewport.width,
                self.row_height - 4.0,
            );
            self.paint_row(ctx, index, row);
        }
        ctx.encoder.pop_clip();

        if let Some(thumb) = self.scrollbar_thumb_rect() {
            let mut thumb_color = colors.scrollbar_thumb;
            thumb_color.a = if self.scrollbar_hovered || self.scrollbar_dragging {
                0.88
            } else {
                0.64
            };
            ctx.encoder.draw_rect(thumb, thumb_color, thumb.width * 0.5);
        }

        if self.focus_visible {
            let mut ring = colors.ring;
            ring.a = 0.34;
            ctx.encoder
                .draw_rect(self.bounds.inset(-2.0, -2.0), ring, spacing.radius_sm + 2.0);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests, PointerCaptureRequest};
    use mondrian_ui_theme::ThemePreset;

    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip};

    fn custom_action(name: &str) -> Action {
        match name {
            "select-a" => Action::SaveProject,
            "dynamic-0-A" => Action::CloseProject,
            "activate-c" => Action::Play,
            _ => Action::Pause,
        }
    }

    fn dispatching_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        requests: &'a mut EventRequests,
        dispatch: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        EventContext {
            focus,
            shortcut,
            tooltip,
            dispatch,
            platform: &NoopPlatformService,
            requests,
        }
    }

    #[derive(Default)]
    struct RecordingEncoder {
        rects: usize,
        rect_bounds: Vec<Rect>,
        lines: usize,
        texts: Vec<String>,
        clips: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {
            self.clips += 1;
        }

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects += 1;
            self.rect_bounds.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }

        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.into());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: Color,
        ) {
            self.texts.push(text.into());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}
        fn pop_transform(&mut self) {}
    }

    fn sample_items() -> Vec<PanelListItem> {
        vec![
            PanelListItem::new("A").with_select_action(custom_action("select-a")),
            PanelListItem::new("B").disabled(true),
            PanelListItem::new("C").with_activate_action(custom_action("activate-c")),
        ]
    }

    #[test]
    fn click_selects_enabled_item_and_dispatches_once() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Assets", sample_items())
            .on_select(|index, item| custom_action(&format!("dynamic-{index}-{}", item.title)));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(30.0, 74.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(list.selected_index(), Some(0));
        let actions = actions.borrow();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0], custom_action("select-a"));
        assert_eq!(actions[1], custom_action("dynamic-0-A"));
        assert!(requests.repaint);
    }

    #[test]
    fn second_click_on_same_enabled_item_dispatches_activation() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 220.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        let click = UiEvent::MouseDown {
            position: Point::new(30.0, 174.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };

        assert_eq!(list.event(&click, &mut ctx), EventResult::Handled);
        assert_eq!(list.selected_index(), Some(2));
        assert!(actions.borrow().is_empty());

        assert_eq!(list.event(&click, &mut ctx), EventResult::Handled);
        assert_eq!(actions.borrow().as_slice(), &[custom_action("activate-c")]);
    }

    #[test]
    fn disabled_item_consumes_click_without_selection_or_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(30.0, 124.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(list.selected_index(), None);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn keyboard_navigation_skips_disabled_items_and_activation_dispatches() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = PanelList::new("Effects", sample_items());
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(&UiEvent::FocusGained, &mut ctx);
        list.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        list.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        list.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(list.selected_index(), Some(2));
        assert_eq!(actions.borrow().last(), Some(&custom_action("activate-c")));
    }

    #[test]
    fn wheel_scrolls_and_clamps_content() {
        let items = (0..12).map(|index| PanelListItem::new(format!("Item {index}"))).collect();
        let mut list = PanelList::new("Long", items);
        list.layout(Rect::new(0.0, 0.0, 240.0, 140.0));

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(
            &UiEvent::MouseWheel {
                delta: 90.0,
                position: Point::new(30.0, 80.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(list.scroll_offset_y() > 0.0);
        assert!(requests.repaint);
    }

    #[test]
    fn scrollbar_thumb_drag_updates_offset_and_releases_capture() {
        let items = (0..16).map(|index| PanelListItem::new(format!("Item {index}"))).collect();
        let mut list = PanelList::new("Long", items);
        list.layout(Rect::new(0.0, 0.0, 240.0, 150.0));
        let thumb = list.scrollbar_thumb_rect().expect("overflowing list should have thumb");
        let start = Point::new(thumb.x + 2.0, thumb.y + 2.0);

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        list.event(
            &UiEvent::MouseDown {
                position: start,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(list.id()))
        );

        list.event(
            &UiEvent::MouseMove {
                position: Point::new(start.x, start.y + 24.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(list.scroll_offset_y() > 0.0);

        list.event(
            &UiEvent::MouseUp {
                position: Point::new(start.x, start.y + 24.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Release(list.id()))
        );
    }

    #[test]
    fn scrollbar_track_click_pages_without_selecting_row() {
        let items = (0..16).map(|index| PanelListItem::new(format!("Item {index}"))).collect();
        let mut list = PanelList::new("Long", items);
        list.layout(Rect::new(0.0, 0.0, 240.0, 150.0));
        let track = list.scrollbar_track_rect().expect("overflowing list should have track");

        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = dispatching_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = list.event(
            &UiEvent::MouseDown {
                position: Point::new(track.x + 1.0, track.y + track.height - 2.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(list.scroll_offset_y() > 0.0);
        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn set_items_clamps_selection_when_selected_item_disappears() {
        let mut list = PanelList::new("Assets", sample_items()).with_selected(Some(2));
        list.layout(Rect::new(0.0, 0.0, 240.0, 180.0));

        list.set_items(vec![PanelListItem::new("Only")]);

        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn paint_draws_header_rows_and_clips_row_text() {
        let mut list = PanelList::new(
            "Assets",
            vec![PanelListItem::new("Media").with_subtitle("Imported footage").with_badge("4K")],
        )
        .with_subtitle("Project library");
        list.layout(Rect::new(0.0, 0.0, 260.0, 160.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 160.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.rects >= 3);
        assert_eq!(encoder.lines, 1);
        assert!(encoder.clips >= 2);
        assert!(encoder.texts.iter().any(|text| text == "Assets"));
        assert!(encoder.texts.iter().any(|text| text == "Imported footage"));
    }

    #[test]
    fn selected_row_paints_outer_ring_before_row_fill() {
        let mut list =
            PanelList::new("Assets", vec![PanelListItem::new("Selected")]).with_selected(Some(0));
        list.layout(Rect::new(0.0, 0.0, 260.0, 140.0));

        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 260.0, 140.0),
        };
        list.paint(&mut ctx);

        assert!(encoder.rect_bounds.windows(2).any(|pair| {
            pair[0].width > pair[1].width
                && pair[0].height > pair[1].height
                && (pair[0].x - pair[1].x).abs() <= 1.1
                && (pair[0].y - pair[1].y).abs() <= 1.1
        }));
    }
}
