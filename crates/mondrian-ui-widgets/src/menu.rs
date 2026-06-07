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
}

impl Widget for Dropdown {
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        if self.open {
            let w = self.items.iter()
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
            // When open: handle item hover and click
            match event {
                UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                    let menu_y = self.bounds.y + 28.0;
                    for (i, _item) in self.items.iter().enumerate() {
                        let item_rect = Rect::new(
                            self.bounds.x, menu_y + i as f32 * self.item_height,
                            self.bounds.width.max(120.0), self.item_height,
                        );
                        if item_rect.contains(*position) && self.items[i].enabled {
                            (ctx.dispatch)(self.items[i].action.clone());
                            self.open = false;
                            return EventResult::Handled;
                        }
                    }
                    // Click outside menu → close
                    if !self.bounds.contains(*position) {
                        self.open = false;
                        return EventResult::Handled;
                    }
                }
                UiEvent::MouseMove { position, .. } => {
                    let menu_y = self.bounds.y + 28.0;
                    self.hovered_index = self.items.iter().enumerate()
                        .position(|(i, _)| {
                            let r = Rect::new(self.bounds.x, menu_y + i as f32 * self.item_height,
                                self.bounds.width.max(120.0), self.item_height);
                            r.contains(*position)
                        });
                    return EventResult::Handled;
                }
                _ => return EventResult::Handled,
            }
        } else {
            // Closed: click to open
            if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event {
                if self.bounds.contains(*position) {
                    self.open = true;
                    return EventResult::Handled;
                }
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        // Trigger button
        let btn_rect = Rect::new(self.bounds.x, self.bounds.y, self.bounds.width, 28.0);
        let bg = if self.open { tokens.primary } else { tokens.card };
        ctx.encoder.draw_rect(btn_rect, bg, spacing.radius_sm);

        // Dropdown arrow indicator
        let arrow_x = btn_rect.x + btn_rect.width - 16.0;
        let arrow_y = btn_rect.y + btn_rect.height * 0.5;
        ctx.encoder.draw_line(
            Point::new(arrow_x - 4.0, arrow_y - 2.0),
            Point::new(arrow_x, arrow_y + 2.0),
            1.5, tokens.foreground,
        );
        ctx.encoder.draw_line(
            Point::new(arrow_x, arrow_y + 2.0),
            Point::new(arrow_x + 4.0, arrow_y - 2.0),
            1.5, tokens.foreground,
        );

        // Menu items
        if self.open {
            let menu_y = self.bounds.y + 28.0;
            let menu_w = self.bounds.width.max(120.0);

            // Menu background
            let menu_bg = Rect::new(self.bounds.x, menu_y, menu_w,
                self.item_height * self.items.len() as f32 + 4.0);
            ctx.encoder.draw_rect(menu_bg, tokens.popover, spacing.radius_sm);
            ctx.encoder.draw_rect(menu_bg, tokens.border, 0.0);

            for (i, item) in self.items.iter().enumerate() {
                let item_rect = Rect::new(self.bounds.x + 2.0, menu_y + i as f32 * self.item_height,
                    menu_w - 4.0, self.item_height);

                let fill = if self.hovered_index == Some(i) && item.enabled {
                    tokens.accent
                } else {
                    tokens.popover
                };
                ctx.encoder.draw_rect(item_rect, fill, 0.0);

                // Text color
                let text_color = if item.enabled { tokens.foreground } else { tokens.muted_foreground };
                // Draw a small indicator line for hover / disabled
                if !item.enabled {
                    ctx.encoder.draw_line(
                        Point::new(item_rect.x + 4.0, item_rect.y + item_rect.height * 0.5),
                        Point::new(item_rect.x + item_rect.width - 4.0, item_rect.y + item_rect.height * 0.5),
                        1.0, text_color,
                    );
                }
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{DummyFocus, DummyShortcut, DummyTooltip, make_event_ctx};
    use std::cell::RefCell;

    #[test]
    fn dropdown_new_is_closed() {
        let d = Dropdown::new("File", vec![
            MenuItem::new("Open", Action::OpenProject("".into())),
        ]);
        assert!(!d.open);
    }

    #[test]
    fn dropdown_click_opens() {
        let mut d = Dropdown::new("File", vec![
            MenuItem::new("Open", Action::OpenProject("".into())),
        ]);
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| { cell.borrow_mut().push(a); };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        d.event(&UiEvent::MouseDown {
            position: Point::new(60.0, 14.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(d.open);
    }

    #[test]
    fn dropdown_select_dispatches_and_closes() {
        let mut d = Dropdown::new("File", vec![
            MenuItem::new("Save", Action::SaveProject),
            MenuItem::new("Quit", Action::CloseProject),
        ]);
        d.layout(Rect::new(0.0, 0.0, 120.0, 28.0));

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| { cell.borrow_mut().push(a); };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        // Open
        d.event(&UiEvent::MouseDown {
            position: Point::new(60.0, 14.0),
            button: MouseButton::Left, modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(d.open);

        // Click first item at y = 28 + 0*24 = 28 → should dispatch SaveProject
        d.event(&UiEvent::MouseDown {
            position: Point::new(60.0, 40.0),
            button: MouseButton::Left, modifiers: Modifiers::none(),
        }, &mut ctx);

        assert!(!d.open);
        let actions = cell.into_inner();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], Action::SaveProject);
    }
}
