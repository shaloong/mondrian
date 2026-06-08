//! Checkbox 控件
//!
//! 布尔值勾选框，点击切换状态，派发 Action。

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

/// Checkbox Widget —— 可切换的勾选框
pub struct Checkbox {
    id: WidgetId,
    label: String,
    checked: bool,
    bounds: Rect,
    hovered: bool,
    pressed: bool,
    pub on_toggle: Option<Action>,
}

impl Checkbox {
    pub fn new(label: impl Into<String>, checked: bool) -> Self {
        Self {
            id: WidgetId::new(),
            label: label.into(),
            checked,
            bounds: Rect::ZERO,
            hovered: false,
            pressed: false,
            on_toggle: None,
        }
    }

    pub fn on_toggle(mut self, action: Action) -> Self {
        self.on_toggle = Some(action);
        self
    }

    pub fn is_checked(&self) -> bool {
        self.checked
    }

    pub fn set_checked(&mut self, checked: bool) {
        self.checked = checked;
    }
}

impl Widget for Checkbox {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        let char_count = self.label.chars().count() as f32;
        let preferred = Size::new(16.0 + 8.0 + 12.0 * char_count, 22.0);
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
                self.pressed = true;
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                if self.pressed && self.bounds.contains(*position) {
                    self.checked = !self.checked;
                    if let Some(action) = &self.on_toggle {
                        (ctx.dispatch)(action.clone());
                    }
                }
                self.pressed = false;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                self.hovered = self.bounds.contains(*position);
                EventResult::Ignored
            }
            UiEvent::FocusGained => {
                self.hovered = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.hovered = false;
                self.pressed = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let box_size = 16.0;
        let box_rect = Rect::new(
            self.bounds.x + 2.0,
            self.bounds.y + (self.bounds.height - box_size) * 0.5,
            box_size,
            box_size,
        );

        // Fill color
        let fill = if self.checked {
            tokens.primary
        } else if self.hovered {
            tokens.accent
        } else {
            tokens.card
        };

        // Border color
        let border_color = if self.checked || self.hovered {
            tokens.primary
        } else {
            tokens.border
        };

        // Rounded border: draw slightly larger rounded rect behind fill
        let border_inset = 1.0;
        let border_rect = box_rect.inset(-border_inset, -border_inset);
        ctx.encoder
            .draw_rect(border_rect, border_color, spacing.radius_sm + border_inset);
        // Fill on top
        ctx.encoder.draw_rect(box_rect, fill, spacing.radius_sm);

        // Check mark — two line segments forming a V
        if self.checked {
            let side = box_rect.width.min(box_rect.height);
            let cx = box_rect.x + box_rect.width * 0.5;
            let cy = box_rect.y + box_rect.height * 0.5;
            let start = Point::new(cx - side * 0.20, cy + side * 0.04);
            let mid = Point::new(cx - side * 0.04, cy + side * 0.20);
            let end = Point::new(cx + side * 0.24, cy - side * 0.18);
            ctx.encoder.draw_line(start, mid, 3.0, tokens.foreground);
            ctx.encoder.draw_line(mid, end, 3.0, tokens.foreground);
        }

        // Label text
        if !self.label.is_empty() {
            let font_size = ctx.theme.typography.body.font_size;
            let tx = self.bounds.x + 20.0;
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

    #[test]
    fn checkbox_new_unchecked() {
        let cb = Checkbox::new("Option", false);
        assert!(!cb.is_checked());
    }

    #[test]
    fn checkbox_new_checked() {
        let cb = Checkbox::new("Option", true);
        assert!(cb.is_checked());
    }

    #[test]
    fn checkbox_click_toggles() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let pos = Point::new(50.0, 11.0);
        cb.event(
            &UiEvent::MouseDown {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!cb.is_checked()); // not yet toggled on MouseDown
        cb.event(
            &UiEvent::MouseUp {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(cb.is_checked()); // toggled on MouseUp
    }

    #[test]
    fn checkbox_click_twice() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let pos = Point::new(50.0, 11.0);
        let md = UiEvent::MouseDown {
            position: pos,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };
        let mu = UiEvent::MouseUp {
            position: pos,
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        };
        cb.event(&md, &mut ctx);
        cb.event(&mu, &mut ctx);
        assert!(cb.is_checked());
        cb.event(&md, &mut ctx);
        cb.event(&mu, &mut ctx);
        assert!(!cb.is_checked());
    }

    #[test]
    fn checkbox_release_outside_no_toggle() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        cb.event(
            &UiEvent::MouseDown {
                position: Point::new(50.0, 11.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        cb.event(
            &UiEvent::MouseUp {
                position: Point::new(200.0, 200.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(!cb.is_checked());
        assert!(cell.into_inner().is_empty());
    }

    #[test]
    fn checkbox_set_checked() {
        let mut cb = Checkbox::new("Opt", false);
        cb.set_checked(true);
        assert!(cb.is_checked());
    }

    #[test]
    fn checkbox_on_toggle_dispatches() {
        let mut cb = Checkbox::new("Opt", false).on_toggle(Action::TogglePlay);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| {
            cell.borrow_mut().push(a);
        };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        let pos = Point::new(50.0, 11.0);
        cb.event(
            &UiEvent::MouseDown {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        cb.event(
            &UiEvent::MouseUp {
                position: pos,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(cell.into_inner(), vec![Action::TogglePlay]);
    }

    #[test]
    fn checkbox_mouse_move_in_sets_hovered() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = cb.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 11.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert!(cb.hovered);
    }

    #[test]
    fn checkbox_mouse_move_out_clears_hovered() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        cb.hovered = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = cb.event(
            &UiEvent::MouseMove {
                position: Point::new(200.0, 11.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert!(!cb.hovered);
    }

    #[test]
    fn checkbox_mouse_move_same_state_no_rehandle() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));
        cb.hovered = true;
        let mut f = DummyFocus;
        let mut s = DummyShortcut;
        let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        let r = cb.event(
            &UiEvent::MouseMove {
                position: Point::new(50.0, 11.0),
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert_eq!(r, EventResult::Ignored);
        assert!(cb.hovered);
    }
}
