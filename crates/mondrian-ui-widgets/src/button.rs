//! Button 控件
//!
//! 支持 Normal / Hovered / Pressed 三态 + 点击派发 Action。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// 按钮状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Normal,
    Hovered,
    Pressed,
}

/// Button Widget —— 可点击的标签按钮
pub struct Button {
    id: WidgetId,
    label: String,
    bounds: Rect,
    state: ButtonState,
    pub on_click: Option<Action>,
}

impl Button {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            bounds: Rect::ZERO,
            state: ButtonState::Normal,
            on_click: None,
        }
    }

    pub fn on_click(mut self, action: Action) -> Self {
        self.on_click = Some(action);
        self
    }

    pub fn state(&self) -> ButtonState {
        self.state
    }
}

impl Widget for Button {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let char_count = self.label.chars().count() as f32;
        let preferred = Size::new(12.0 * char_count + 24.0, 28.0);
        constraint.constrain(preferred)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.state = ButtonState::Pressed;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.state == ButtonState::Pressed {
                    if self.bounds.contains(*position) {
                        if let Some(action) = &self.on_click {
                            (ctx.dispatch)(action.clone());
                        }
                    }
                    self.state = ButtonState::Normal;
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseMove { position, .. } if self.state != ButtonState::Pressed => {
                let was_hovered = self.state == ButtonState::Hovered;
                let now_inside = self.bounds.contains(*position);
                if now_inside && !was_hovered {
                    self.state = ButtonState::Hovered;
                    return EventResult::Handled;
                } else if !now_inside && was_hovered {
                    self.state = ButtonState::Normal;
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::FocusGained => {
                self.state = ButtonState::Hovered;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.state = ButtonState::Normal;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let font_size = ctx.theme.typography.button.font_size;

        let bg = match self.state {
            ButtonState::Normal => tokens.card,
            ButtonState::Hovered => tokens.accent,
            ButtonState::Pressed => tokens.muted,
        };

        ctx.encoder.draw_rect(self.bounds, bg, spacing.radius_md);
        if !self.label.is_empty() {
            let tx = mondrian_ui_core::types::center_text_x(self.bounds, &self.label, font_size);
            let ty = self.bounds.y + (self.bounds.height - font_size * 1.3).max(0.0) * 0.5;
            ctx.encoder.draw_text(
                &self.label,
                font_size,
                Point::new(tx, ty),
                tokens.foreground,
            );
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use std::cell::RefCell;

    fn event_ctx_with_capture<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        dispatch_fn: &'a dyn Fn(Action),
    ) -> EventContext<'a> {
        make_event_ctx(focus, shortcut, tooltip, dispatch_fn)
    }

    #[test]
    fn button_new_is_normal() {
        let b = Button::new("Click");
        assert_eq!(b.state(), ButtonState::Normal);
    }

    #[test]
    fn button_measure_non_empty() {
        let b = Button::new("Hello");
        let s = b.measure(LayoutConstraint::LOOSE);
        assert!(s.width > 0.0);
        assert!(s.height > 0.0);
    }

    #[test]
    fn button_mouse_down_in_bounds_sets_pressed() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(b.state(), ButtonState::Pressed);
    }

    #[test]
    fn button_mouse_down_outside_bounds_ignored() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        let r = b.event(
            &UiEvent::MouseDown {
                position: Point::new(200.0, 200.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Normal);
    }

    #[test]
    fn button_click_dispatches_action() {
        let mut b = Button::new("OK").on_click(Action::Play);
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        b.event(
            &UiEvent::MouseUp {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(b.state(), ButtonState::Normal);
        let actions = cell.into_inner();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0], Action::Play);
    }

    #[test]
    fn button_release_outside_no_click() {
        let mut b = Button::new("OK").on_click(Action::Play);
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        b.event(
            &UiEvent::MouseUp {
                position: Point::new(200.0, 200.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn button_focus_gained_sets_hovered() {
        let mut b = Button::new("OK");
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(&UiEvent::FocusGained, &mut ctx);
        assert_eq!(b.state(), ButtonState::Hovered);
    }

    #[test]
    fn button_focus_lost_clears_hovered() {
        let mut b = Button::new("OK");
        b.state = ButtonState::Hovered;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(b.state(), ButtonState::Normal);
    }

    #[test]
    fn button_no_click_without_action() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = event_ctx_with_capture(&mut f, &mut s, &mut t, &dispatch_fn);

        b.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        b.event(
            &UiEvent::MouseUp {
                position: Point::new(50.0, 15.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn button_mouse_move_in_sets_hovered() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = b.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Handled);
        assert_eq!(b.state(), ButtonState::Hovered);
    }

    #[test]
    fn button_mouse_move_out_clears_hovered() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        b.state = ButtonState::Hovered;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = b.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Handled);
        assert_eq!(b.state(), ButtonState::Normal);
    }

    #[test]
    fn button_mouse_move_during_press_no_hover_change() {
        let mut b = Button::new("OK");
        b.layout(Rect::new(0.0, 0.0, 100.0, 30.0));
        b.state = ButtonState::Pressed;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = b.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 15.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        // During press, MouseMove does NOT change state (guard: self.state != Pressed)
        assert_eq!(r, EventResult::Ignored);
        assert_eq!(b.state(), ButtonState::Pressed);
    }
}
