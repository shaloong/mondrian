//! 列表控件
//!
//! 可滚动的垂直列表，支持单项选择和点击派发 Action。
//! 滚动委托给 ScrollView，避免重复实现滚动逻辑。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::scroll::ScrollView;

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
        }
    }

    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    pub fn set_selected(&mut self, idx: Option<usize>) {
        self.selected = idx;
        self.rebuild_content();
    }

    fn rebuild_content(&mut self) {
        let content = build_list_column(&self.items, self.selected, self.hovered, self.row_height);
        self.scroll = ScrollView::new(Some(content));
        if self.bounds.width > 0.0 || self.bounds.height > 0.0 {
            self.scroll.layout(self.bounds);
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
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                if self.bounds.contains(*position) {
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
            _ => {}
        }

        self.scroll.event(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.encoder.push_clip(self.bounds);
        self.scroll.paint(ctx);
        ctx.encoder.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
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
            ctx.encoder.draw_text(
                &self.label,
                font_size,
                Point::new(self.bounds.x + 8.0, self.bounds.y + 5.0),
                tokens.foreground,
            );
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
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

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
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
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
}
