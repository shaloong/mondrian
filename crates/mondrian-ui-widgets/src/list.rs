//! 列表控件
//!
//! 可滚动的垂直列表，支持单项选择和点击派发 Action。
//! 滚动委托给 ScrollView，避免重复实现滚动逻辑。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext, PointerCaptureRequest};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::paint::paint_focus_ring;
use crate::scroll::ScrollView;

const LIST_ROW_TEXT_PADDING_X: f32 = 8.0;

/// 列表项
#[derive(Debug, Clone)]
pub struct ListItem {
    pub label: String,
    pub action: Option<Action>,
}

impl ListItem {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), action: None }
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }
}

/// 可滚动的垂直列表
///
/// 内部使用 ScrollView 包裹一个列布局来实现滚动。
pub struct List {
    id: WidgetId,
    items: Vec<ListItem>,
    bounds: Rect,
    selected: Option<usize>,
    hovered: Option<usize>,
    row_height: f32,
    scroll: ScrollView,
    focused: bool,
    focus_visible: bool,
}

impl List {
    pub fn new(items: Vec<ListItem>) -> Self {
        let row_height = 28.0;
        let content = build_list_column(&items, None, None, row_height);
        Self {
            id: WidgetId::new(),
            items,
            bounds: Rect::ZERO,
            selected: None,
            hovered: None,
            row_height,
            scroll: ScrollView::new(Some(content)),
            focused: false,
            focus_visible: false,
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn set_selected(&mut self, idx: Option<usize>) {
        self.selected = idx.filter(|index| *index < self.items.len());
        self.rebuild_content();
    }

    fn rebuild_content(&mut self) {
        let scroll_offset = self.scroll.scroll_offset();
        let content = build_list_column(&self.items, self.selected, self.hovered, self.row_height);
        self.scroll = ScrollView::new(Some(content));
        if self.bounds.width > 0.0 || self.bounds.height > 0.0 {
            self.scroll.layout(self.bounds);
            self.scroll.set_scroll_offset(scroll_offset);
        }
    }

    fn index_at_y(&self, y: f32) -> Option<usize> {
        let scroll_y = self.scroll.scroll_offset().y;
        let rel_y = y - self.bounds.y + scroll_y;
        let idx = (rel_y / self.row_height) as usize;
        if idx < self.items.len() {
            Some(idx)
        } else {
            None
        }
    }

    fn forward_to_scroll(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        let scroll_id = self.scroll.id();
        let result = self.scroll.event(event, ctx);
        match ctx.requests.pointer_capture {
            Some(PointerCaptureRequest::Capture(id)) if id == scroll_id => {
                ctx.request_pointer_capture(self.id);
            }
            Some(PointerCaptureRequest::Release(id)) if id == scroll_id => {
                ctx.release_pointer_capture(self.id);
            }
            _ => {}
        }
        result
    }
}

impl Widget for List {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, c: LayoutConstraint) -> Size {
        let preferred = Size::new(200.0, self.items.len() as f32 * self.row_height);
        c.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.scroll.layout(bounds);
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        let scrollbar_pointer = match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
            | UiEvent::MouseMove { position, .. }
            | UiEvent::MouseUp { position, button: MouseButton::Left, .. } => Some(*position),
            _ => None,
        };
        if let Some(position) = scrollbar_pointer {
            if self.scroll.is_scrollbar_dragging() || self.scroll.scrollbar_hit_test(position) {
                return self.forward_to_scroll(event, ctx);
            }
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.bounds.contains(*position) {
                    self.focus_visible = false;
                    if let Some(idx) = self.index_at_y(position.y) {
                        self.selected = Some(idx);
                        if let Some(action) = &self.items[idx].action {
                            (ctx.dispatch)(action.clone());
                        }
                        self.rebuild_content();
                        return EventResult::Handled;
                    }
                }
            }
            UiEvent::MouseMove { position, .. } => {
                let new_hover = if self.bounds.contains(*position) {
                    self.index_at_y(position.y)
                } else {
                    None
                };
                if new_hover != self.hovered {
                    self.hovered = new_hover;
                    self.rebuild_content();
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
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Down, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if self.items.is_empty() {
                    return EventResult::Ignored;
                }
                let next = self.selected.map_or(0, |s| (s + 1).min(self.items.len() - 1));
                self.set_selected(Some(next));
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Up, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if self.items.is_empty() {
                    return EventResult::Ignored;
                }
                let next = self.selected.map_or(0, |s| s.saturating_sub(1));
                self.set_selected(Some(next));
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                if self.focused && *modifiers == Modifiers::none() =>
            {
                if let Some(idx) = self.selected.filter(|index| *index < self.items.len()) {
                    if let Some(action) = &self.items[idx].action {
                        (ctx.dispatch)(action.clone());
                    }
                    return EventResult::Handled;
                }
                return EventResult::Ignored;
            }
            _ => {}
        }

        self.forward_to_scroll(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.push_clip(self.bounds);
        self.scroll.paint(ctx);
        ctx.pop_clip();
        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, ctx.theme.spacing.radius_sm);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        true
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        &[]
    }

    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut []
    }
}

// ── Internal column widget for list rows ──────────────────────────────

fn build_list_column(
    items: &[ListItem],
    selected: Option<usize>,
    hovered: Option<usize>,
    row_height: f32,
) -> Box<dyn Widget> {
    let children: Vec<Box<dyn Widget>> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let row = ListRow {
                id: WidgetId::new(),
                label: item.label.clone(),
                selected: selected == Some(i),
                hovered: hovered == Some(i),
                bounds: Rect::ZERO,
                row_height,
            };
            Box::new(row) as Box<dyn Widget>
        })
        .collect();

    Box::new(ListColumn {
        id: WidgetId::new(),
        bounds: Rect::ZERO,
        children,
        row_height,
        item_count: items.len(),
    })
}

struct ListColumn {
    id: WidgetId,
    bounds: Rect,
    children: Vec<Box<dyn Widget>>,
    row_height: f32,
    item_count: usize,
}

impl Widget for ListColumn {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, c: LayoutConstraint) -> Size {
        c.constrain(Size::new(200.0, self.item_count as f32 * self.row_height))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let width = (bounds.width - 4.0).max(0.0);
        for (i, child) in self.children.iter_mut().enumerate() {
            child.layout(Rect::new(
                bounds.x + 2.0,
                bounds.y + i as f32 * self.row_height,
                width,
                self.row_height,
            ));
        }
    }

    fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        for child in &self.children {
            child.paint(ctx);
        }
    }

    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }

    fn children(&self) -> &[Box<dyn Widget>] {
        &self.children
    }
    fn children_mut(&mut self) -> &mut [Box<dyn Widget>] {
        &mut self.children
    }
}

struct ListRow {
    id: WidgetId,
    label: String,
    selected: bool,
    hovered: bool,
    bounds: Rect,
    row_height: f32,
}

impl Widget for ListRow {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, c: LayoutConstraint) -> Size {
        c.constrain(Size::new(200.0, self.row_height))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _e: &UiEvent, _c: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let font_size = ctx.theme.typography.body.font_size;

        let fill = if self.selected {
            tokens.primary
        } else if self.hovered {
            tokens.accent
        } else {
            tokens.card
        };
        ctx.encoder.draw_rect(self.bounds, fill, spacing.radius_sm);
        if !self.label.is_empty() {
            let text_clip = Rect::new(
                self.bounds.x + LIST_ROW_TEXT_PADDING_X,
                self.bounds.y,
                (self.bounds.width - LIST_ROW_TEXT_PADDING_X * 2.0).max(0.0),
                self.bounds.height,
            );
            ctx.push_clip(text_clip);
            ctx.encoder.draw_text(
                &self.label,
                font_size,
                Point::new(text_clip.x, self.bounds.y + 5.0),
                tokens.foreground,
            );
            ctx.pop_clip();
        }
    }

    fn hit_test(&self, p: Point) -> bool {
        self.bounds.contains(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use glam::Vec2;
    use mondrian_platform::NoopPlatformService;
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests};
    use mondrian_ui_theme::ThemePreset;
    use std::cell::RefCell;

    #[derive(Default)]
    struct PaintRecorder {
        clips: Vec<Rect>,
        clip_pops: usize,
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for PaintRecorder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {}

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
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

    fn event_ctx<'a>(
        f: &'a mut DummyFocus,
        s: &'a mut DummyShortcut,
        t: &'a mut DummyTooltip,
        dispatch: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        make_event_ctx(f, s, t, dispatch)
    }

    #[test]
    fn list_click_selects_item() {
        let mut list = List::new(vec![
            ListItem::new("A"),
            ListItem::new("B"),
            ListItem::new("C"),
        ]);
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = event_ctx(&mut f, &mut s, &mut t, &|_| {});

        list.event(
            &UiEvent::MouseDown {
                position: Point::new(100.0, 42.0), // second item (y=28-56)
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(list.selected_index(), Some(1));
    }

    #[test]
    fn list_click_outside_ignored() {
        let mut list = List::new(vec![ListItem::new("A")]);
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = event_ctx(&mut f, &mut s, &mut t, &|_| {});
        list.event(
            &UiEvent::MouseDown {
                position: Point::new(300.0, 50.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn list_scrollbar_mouse_down_does_not_select_row_and_translates_capture() {
        let mut list = List::new((0..20).map(|i| ListItem::new(format!("Item {i}"))).collect());
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let scrollbar_point = Point::new(197.0, 12.0);
        assert!(list.scroll.scrollbar_hit_test(scrollbar_point));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let dispatch = |_| {};
        let mut requests = EventRequests::default();
        let platform = NoopPlatformService;
        let mut ctx = EventContext {
            focus: &mut f,
            shortcut: &mut s,
            tooltip: &mut t,
            dispatch: &dispatch,
            platform: &platform,
            requests: &mut requests,
        };

        let result = list.event(
            &UiEvent::MouseDown {
                position: scrollbar_point,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(list.selected_index(), None);
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(PointerCaptureRequest::Capture(list.id))
        );
    }

    #[test]
    fn list_rebuild_content_preserves_scroll_offset() {
        let mut list = List::new((0..20).map(|i| ListItem::new(format!("Item {i}"))).collect());
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        list.scroll.set_scroll_offset(Vec2::new(0.0, 80.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = event_ctx(&mut f, &mut s, &mut t, &|_| {});
        list.event(
            &UiEvent::MouseMove {
                position: Point::new(20.0, 30.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(list.scroll.scroll_offset().y, 80.0);
    }

    #[test]
    fn list_set_selected_clamps_invalid_index() {
        let mut list = List::new(vec![ListItem::new("A")]);

        list.set_selected(Some(2));

        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn empty_list_keyboard_navigation_does_not_select_or_panic() {
        let mut list = List::new(vec![]);
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = event_ctx(&mut f, &mut s, &mut t, &|_| {});

        list.event(&UiEvent::FocusGained, &mut ctx);
        let down = list.event(
            &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
            &mut ctx,
        );
        let enter = list.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(down, EventResult::Ignored);
        assert_eq!(enter, EventResult::Ignored);
        assert_eq!(list.selected_index(), None);
    }

    #[test]
    fn list_keyboard_navigation_ignores_modified_keys() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut list = List::new(vec![
            ListItem::new("A").with_action(Action::Play),
            ListItem::new("B").with_action(Action::TogglePlay),
        ]);
        list.set_selected(Some(1));
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = event_ctx(&mut f, &mut s, &mut t, &dispatch);

        list.event(&UiEvent::FocusGained, &mut ctx);
        for (key, modifiers) in [
            (KeyCode::Down, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers::shift()),
            (KeyCode::Enter, Modifiers::ctrl()),
            (KeyCode::Space, Modifiers::shift()),
        ] {
            assert_eq!(
                list.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
            assert_eq!(list.selected_index(), Some(1));
        }
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn list_row_paint_clips_long_label_to_row_bounds() {
        let mut row = ListRow {
            id: WidgetId::new(),
            label: "A very long list row label".into(),
            selected: false,
            hovered: false,
            bounds: Rect::ZERO,
            row_height: 28.0,
        };
        row.layout(Rect::new(10.0, 20.0, 80.0, 28.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = PaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 200.0, 100.0),
        };

        row.paint(&mut ctx);

        assert_eq!(encoder.texts, vec!["A very long list row label"]);
        assert_eq!(encoder.clips, vec![Rect::new(18.0, 20.0, 64.0, 28.0)]);
        assert_eq!(encoder.clip_pops, 1);
    }
}
