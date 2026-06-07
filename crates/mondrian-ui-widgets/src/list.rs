//! 列表控件
//!
//! 可滚动的垂直列表，支持单项选择和点击派发 Action。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

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
pub struct List {
    id: WidgetId,
    items: Vec<ListItem>,
    bounds: Rect,
    selected: Option<usize>,
    hovered: Option<usize>,
    scroll_offset: f32,
    row_height: f32,
}

impl List {
    pub fn new(items: Vec<ListItem>) -> Self {
        Self {
            id: WidgetId::new(),
            items,
            bounds: Rect::ZERO,
            selected: None,
            hovered: None,
            scroll_offset: 0.0,
            row_height: 28.0,
        }
    }

    pub fn selected_index(&self) -> Option<usize> { self.selected }
    pub fn set_selected(&mut self, idx: Option<usize>) { self.selected = idx; }
}

impl Widget for List {
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, _c: LayoutConstraint) -> Size {
        Size::new(200.0, self.items.len() as f32 * self.row_height)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                let rel_y = position.y - self.bounds.y + self.scroll_offset;
                let idx = (rel_y / self.row_height) as usize;
                if idx < self.items.len() && self.bounds.contains(*position) {
                    self.selected = Some(idx);
                    if let Some(action) = &self.items[idx].action {
                        (ctx.dispatch)(action.clone());
                    }
                    return EventResult::Handled;
                }
            }
            UiEvent::MouseMove { position, .. } => {
                let rel_y = position.y - self.bounds.y + self.scroll_offset;
                let idx = (rel_y / self.row_height) as usize;
                self.hovered = if idx < self.items.len() && self.bounds.contains(*position) {
                    Some(idx)
                } else {
                    None
                };
            }
            UiEvent::MouseWheel { delta, .. } => {
                let max_scroll = (self.items.len() as f32 * self.row_height - self.bounds.height).max(0.0);
                self.scroll_offset = (self.scroll_offset + delta).clamp(0.0, max_scroll);
                return EventResult::Handled;
            }
            _ => {}
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        ctx.encoder.push_clip(self.bounds);

        let start_idx = (self.scroll_offset / self.row_height) as usize;
        let end_idx = ((self.scroll_offset + self.bounds.height) / self.row_height + 1.0) as usize;
        let visible = start_idx.min(self.items.len())..end_idx.min(self.items.len());

        for i in visible {
            let item = &self.items[i];
            let y = self.bounds.y + i as f32 * self.row_height - self.scroll_offset;
            let row = Rect::new(self.bounds.x + 2.0, y, self.bounds.width - 4.0, self.row_height);

            let fill = if self.selected == Some(i) {
                tokens.primary
            } else if self.hovered == Some(i) {
                tokens.accent
            } else {
                tokens.card
            };
            ctx.encoder.draw_rect(row, fill, spacing.radius_sm);
            if !item.label.is_empty() {
                ctx.encoder.draw_text(&item.label, 13.0, Point::new(row.x + 8.0, row.y + 5.0), tokens.foreground);
            }
        }

        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip, make_event_ctx};

    #[test]
    fn list_click_selects_item() {
        let mut list = List::new(vec![
            ListItem::new("A"), ListItem::new("B"), ListItem::new("C"),
        ]);
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        list.event(&UiEvent::MouseDown {
            position: Point::new(100.0, 42.0), // second item (y=28-56)
            button: MouseButton::Left, modifiers: Modifiers::none(),
        }, &mut ctx);
        assert_eq!(list.selected_index(), Some(1));
    }

    #[test]
    fn list_click_outside_ignored() {
        let mut list = List::new(vec![ListItem::new("A")]);
        list.layout(Rect::new(0.0, 0.0, 200.0, 100.0));

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
        list.event(&UiEvent::MouseDown {
            position: Point::new(300.0, 50.0),
            button: MouseButton::Left, modifiers: Modifiers::none(),
        }, &mut ctx);
        assert_eq!(list.selected_index(), None);
    }
}
