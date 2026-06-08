//! 右键菜单控件
//!
//! 在指定位置弹出菜单项列表。点击选项或外部区域关闭。

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

    fn bounds_rect(&self) -> Rect {
        Rect::new(
            self.anchor.x,
            self.anchor.y,
            self.min_width + 8.0,
            8.0 + self.items.len() as f32 * self.item_height,
        )
    }

    fn item_rect(&self, idx: usize) -> Rect {
        Rect::new(
            self.anchor.x + 4.0,
            self.anchor.y + 4.0 + idx as f32 * self.item_height,
            self.min_width,
            self.item_height,
        )
    }

    fn item_at(&self, position: Point) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .position(|(i, _)| self.item_rect(i).contains(position))
    }
}

impl Widget for ContextMenu {
    fn id(&self) -> WidgetId {
        self.id
    }

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
                if let Some(index) = self.item_at(*position) {
                    if self.items[index].enabled {
                        (ctx.dispatch)(self.items[index].action.clone());
                        self.visible = false;
                    }
                    return EventResult::Handled;
                }
                if !self.bounds_rect().contains(*position) {
                    self.visible = false;
                }
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                self.hovered = self.item_at(*position);
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
        if !self.visible {
            return;
        }

        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let bg = self.bounds_rect();

        ctx.encoder.draw_rect(bg, tokens.border, spacing.radius_md);
        ctx.encoder.draw_rect(bg.inset(1.0, 1.0), tokens.popover, spacing.radius_md);

        for (i, item) in self.items.iter().enumerate() {
            let r = self.item_rect(i);
            let fill = if !item.enabled {
                tokens.popover
            } else if self.hovered == Some(i) {
                tokens.accent
            } else {
                tokens.popover
            };
            ctx.encoder.draw_rect(r, fill, 0.0);

            let text_color = if item.enabled {
                tokens.foreground
            } else {
                tokens.muted_foreground
            };
            ctx.encoder.draw_text(
                &item.label,
                ctx.theme.typography.body.font_size,
                Point::new(r.x + 8.0, r.y + 4.0),
                text_color,
            );
        }
    }

    fn hit_test(&self, _p: Point) -> bool {
        self.visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::Action;
    use std::cell::RefCell;

    #[test]
    fn context_menu_select_dispatches() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
            ],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        menu.event(
            &UiEvent::MouseDown {
                position: Point::new(174.0, 117.0), // first item
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!menu.visible);
        assert_eq!(cell.into_inner(), vec![Action::Cut]);
    }

    #[test]
    fn context_menu_click_outside_closes() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![MenuItem::new("Copy", Action::Copy)],
        );
        menu.layout(Rect::ZERO);
        assert!(menu.visible);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});
        menu.event(
            &UiEvent::MouseDown {
                position: Point::new(10.0, 10.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!menu.visible);
    }

    #[test]
    fn context_menu_disabled_item_consumes_without_dispatch_or_close() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Disabled", Action::Paste).disabled(),
            ],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let result = menu.event(
            &UiEvent::MouseDown {
                position: Point::new(120.0, 138.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(menu.visible);
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn context_menu_hover_tracks_items() {
        let mut menu = ContextMenu::new(
            Point::new(100.0, 100.0),
            vec![
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
            ],
        );
        menu.layout(Rect::ZERO);

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        menu.event(
            &UiEvent::MouseMove {
                position: Point::new(120.0, 138.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(menu.hovered, Some(1));

        menu.event(
            &UiEvent::MouseMove {
                position: Point::new(10.0, 10.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(menu.hovered, None);
    }
}
