//! Dropdown Menu 控件
//!
//! 点击展开菜单列表，点击选项或外部区域关闭，派发 Action。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// 菜单选项
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: Action,
    pub enabled: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>, action: Action) -> Self {
        Self { label: label.into(), action, enabled: true }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
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
    open: bool,
    hovered_index: Option<usize>,
    item_height: f32,
}

impl Dropdown {
    pub fn new(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            items,
            bounds: Rect::ZERO,
            open: false,
            hovered_index: None,
            item_height: 24.0,
        }
    }

    fn trigger_rect(&self) -> Rect {
        Rect::new(self.bounds.x, self.bounds.y, self.bounds.width, 28.0)
    }

    fn menu_width(&self) -> f32 {
        self.bounds.width.max(120.0)
    }

    fn menu_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + 28.0,
            self.menu_width(),
            self.item_height * self.items.len() as f32 + 4.0,
        )
    }

    fn item_rect(&self, index: usize) -> Rect {
        Rect::new(
            self.bounds.x + 2.0,
            self.bounds.y + 28.0 + index as f32 * self.item_height,
            self.menu_width() - 4.0,
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

impl Widget for Dropdown {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        if self.open {
            let w = self
                .items
                .iter()
                .map(|m| m.label.chars().count() as f32 * 8.0 + 32.0)
                .fold(120.0f32, f32::max);
            let h = self.item_height * self.items.len() as f32 + 4.0;
            Size::new(w, h + 28.0)
        } else {
            Size::new(120.0, 28.0)
        }
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.open {
            match event {
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    if self.trigger_rect().contains(*position) {
                        self.open = false;
                        self.hovered_index = None;
                        return EventResult::Handled;
                    }
                    if let Some(i) = self.item_at(*position) {
                        if self.items[i].enabled {
                            (ctx.dispatch)(self.items[i].action.clone());
                            self.open = false;
                            self.hovered_index = None;
                        }
                        return EventResult::Handled;
                    }
                    if !self.menu_rect().contains(*position) {
                        self.open = false;
                        self.hovered_index = None;
                        return EventResult::Handled;
                    }
                    return EventResult::Handled;
                }
                UiEvent::MouseMove { position, .. } => {
                    self.hovered_index = self.item_at(*position);
                    return EventResult::Handled;
                }
                UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                    self.open = false;
                    self.hovered_index = None;
                    return EventResult::Handled;
                }
                _ => {}
            }
        } else if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event {
            if self.bounds.contains(*position) {
                self.open = true;
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let font_size = ctx.theme.typography.body.font_size;

        // Trigger button
        let btn_rect = self.trigger_rect();
        let bg = if self.open {
            tokens.primary
        } else {
            tokens.card
        };
        ctx.encoder.draw_rect(btn_rect, bg, spacing.radius_sm);

        // Dropdown arrow indicator
        if !self.label.is_empty() {
            ctx.encoder.draw_text(
                &self.label,
                font_size,
                Point::new(btn_rect.x + 8.0, btn_rect.y + 5.0),
                tokens.foreground,
            );
        }
        let arrow_x = btn_rect.x + btn_rect.width - 16.0;
        let arrow_y = btn_rect.y + btn_rect.height * 0.5;
        ctx.encoder.draw_line(
            Point::new(arrow_x - 4.0, arrow_y - 2.0),
            Point::new(arrow_x, arrow_y + 2.0),
            1.5,
            tokens.foreground,
        );
        ctx.encoder.draw_line(
            Point::new(arrow_x, arrow_y + 2.0),
            Point::new(arrow_x + 4.0, arrow_y - 2.0),
            1.5,
            tokens.foreground,
        );

        // Menu items
        if self.open {
            // Menu background
            let menu_bg = self.menu_rect();
            ctx.encoder.draw_rect(menu_bg, tokens.border, 0.0);
            ctx.encoder
                .draw_rect(menu_bg.inset(1.0, 1.0), tokens.popover, spacing.radius_sm);

            for (i, item) in self.items.iter().enumerate() {
                let item_rect = self.item_rect(i);

                let fill = if self.hovered_index == Some(i) && item.enabled {
                    tokens.accent
                } else {
                    tokens.popover
                };
                ctx.encoder.draw_rect(item_rect, fill, 0.0);

                // Item label
                ctx.encoder.draw_text(
                    &item.label,
                    font_size,
                    Point::new(item_rect.x + 6.0, item_rect.y + 5.0),
                    if item.enabled {
                        tokens.foreground
                    } else {
                        tokens.muted_foreground
                    },
                );
                if !item.enabled {
                    ctx.encoder.draw_line(
                        Point::new(item_rect.x + 4.0, item_rect.y + item_rect.height * 0.5),
                        Point::new(
                            item_rect.x + item_rect.width - 4.0,
                            item_rect.y + item_rect.height * 0.5,
                        ),
                        1.0,
                        tokens.muted_foreground,
                    );
                }
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        if self.bounds.contains(point) {
            return true;
        }
        if self.open {
            return self.menu_rect().contains(point);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use std::cell::RefCell;

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

        // Click first item at y = 28 + 0*24 = 28 → should dispatch SaveProject
        d.event(
            &UiEvent::MouseDown {
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
}
