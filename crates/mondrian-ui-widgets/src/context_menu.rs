//! 右键菜单控件
//!
//! 在指定位置弹出菜单项列表。点击选项或外部区域关闭。

#[allow(unused_imports)]
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::menu::MenuItem;

/// 右键弹出菜单
///
/// 通常由父容器在检测到右键点击时创建并插入 Widget 树。
pub struct ContextMenu {
    id: WidgetId,
    items: Vec<MenuItem>,
    anchor: Point,
    bounds: Rect,
    item_height: f32,
    min_width: f32,
    visible: bool,
    hovered: Option<usize>,
}

impl ContextMenu {
    pub fn new(anchor: Point, items: Vec<MenuItem>) -> Self {
        Self {
            id: WidgetId::new(),
            items,
            anchor,
            bounds: Rect::ZERO,
            item_height: 26.0,
            min_width: 140.0,
            visible: true,
            hovered: None,
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    fn menu_rect(&self, idx: usize) -> Rect {
        Rect::new(
            self.anchor.x + 4.0,
            self.anchor.y + 4.0 + idx as f32 * self.item_height,
            self.min_width,
            self.item_height,
        )
    }
}

impl Widget for ContextMenu {
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, _c: LayoutConstraint) -> Size {
        if self.visible {
            let h = 8.0 + self.items.len() as f32 * self.item_height;
            Size::new(self.min_width + 8.0, h)
        } else {
            Size::ZERO
        }
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.visible {
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                for (i, item) in self.items.iter().enumerate() {
                    if self.menu_rect(i).contains(*position) && item.enabled {
                        (ctx.dispatch)(item.action.clone());
                        self.visible = false;
                        return EventResult::Handled;
                    }
                }
                // Click outside → close
                self.visible = false;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                self.hovered = self.items.iter().enumerate()
                    .find(|(i, _)| self.menu_rect(*i).contains(*position))
                    .map(|(i, _)| i);
                EventResult::Handled
            }
            UiEvent::MouseDown { button: MouseButton::Right, .. } => {
                self.visible = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        if !self.visible { return; }

        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let total_h = 8.0 + self.items.len() as f32 * self.item_height;
        let bg = Rect::new(self.anchor.x, self.anchor.y, self.min_width + 8.0, total_h);

        ctx.encoder.draw_rect(bg, tokens.popover, spacing.radius_md);
        ctx.encoder.draw_rect(bg, tokens.border, 0.0);

        for (i, item) in self.items.iter().enumerate() {
            let r = self.menu_rect(i);
            let fill = if !item.enabled { tokens.popover }
            else if self.hovered == Some(i) { tokens.accent }
            else { tokens.popover };
            ctx.encoder.draw_rect(r, fill, 0.0);
        }
    }

    fn hit_test(&self, _p: Point) -> bool { self.visible }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip, make_event_ctx};

    #[test]
    fn context_menu_select_dispatches() {
        let mut menu = ContextMenu::new(Point::new(100.0, 100.0), vec![
            MenuItem::new("Cut", Action::Cut),
            MenuItem::new("Copy", Action::Copy),
        ]);
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| { cell.borrow_mut().push(a); };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        menu.event(&UiEvent::MouseDown {
            position: Point::new(174.0, 117.0), // first item
            button: MouseButton::Left, modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(!menu.visible);
        assert_eq!(cell.into_inner(), vec![Action::Cut]);
    }

    #[test]
    fn context_menu_click_outside_closes() {
        let mut menu = ContextMenu::new(Point::new(100.0, 100.0), vec![
            MenuItem::new("Copy", Action::Copy),
        ]);
        menu.layout(Rect::ZERO);
        assert!(menu.visible);

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
        menu.event(&UiEvent::MouseDown {
            position: Point::new(10.0, 10.0),
            button: MouseButton::Left, modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(!menu.visible);
    }
}
