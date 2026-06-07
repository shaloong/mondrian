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
    fn id(&self) -> WidgetId { self.id }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        let char_count = self.label.chars().count() as f32;
        Size::new(16.0 + 8.0 + 12.0 * char_count, 22.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                self.checked = !self.checked;
                if let Some(action) = &self.on_toggle {
                    (ctx.dispatch)(action.clone());
                }
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                let now_inside = self.bounds.contains(*position);
                if now_inside != self.hovered {
                    self.hovered = now_inside;
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::FocusGained => {
                self.hovered = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.hovered = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let tokens = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;

        let box_size = 14.0;
        let box_rect = Rect::new(
            self.bounds.x + 2.0,
            self.bounds.y + (self.bounds.height - box_size) * 0.5,
            box_size,
            box_size,
        );

        // Checkbox background
        let fill = if self.checked {
            tokens.primary
        } else if self.hovered {
            tokens.accent
        } else {
            tokens.card
        };
        let border = if self.checked || self.hovered {
            tokens.primary
        } else {
            tokens.border
        };

        ctx.encoder.draw_rect(box_rect, fill, spacing.radius_sm);
        ctx.encoder.draw_rect(box_rect, border, 0.0);

        // Check mark (simple cross)
        if self.checked {
            let inset = 3.0;
            let cx = box_rect.x + box_rect.width * 0.5;
            let cy = box_rect.y + box_rect.height * 0.5;
            ctx.encoder.draw_line(
                Point::new(box_rect.x + inset, cy),
                Point::new(cx, box_rect.y + box_rect.height - inset),
                1.5,
                tokens.foreground,
            );
            ctx.encoder.draw_line(
                Point::new(cx, box_rect.y + box_rect.height - inset),
                Point::new(box_rect.x + box_rect.width - inset, box_rect.y + inset),
                1.5,
                tokens.foreground,
            );
        }

        // Label text drawn by app-level TextRenderer
        if !self.label.is_empty() {
            let tx = self.bounds.x + 20.0;
            let ty = self.bounds.y + (self.bounds.height - 12.0) * 0.5;
            ctx.encoder.draw_text(&self.label, 13.0, Point::new(tx, ty), tokens.foreground);
        }
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

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| { cell.borrow_mut().push(a); };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        cb.event(&UiEvent::MouseDown {
            position: Point::new(50.0, 11.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(cb.is_checked());
    }

    #[test]
    fn checkbox_click_twice() {
        let mut cb = Checkbox::new("Opt", false);
        cb.layout(Rect::new(0.0, 0.0, 100.0, 22.0));

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &|_| {});

        cb.event(&UiEvent::MouseDown {
            position: Point::new(50.0, 11.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        }, &mut ctx);
        cb.event(&UiEvent::MouseDown {
            position: Point::new(50.0, 11.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        }, &mut ctx);
        assert!(!cb.is_checked());
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

        let mut f = DummyFocus; let mut s = DummyShortcut; let mut t = DummyTooltip;
        let cell = RefCell::new(Vec::new());
        let dispatch_fn = |a: Action| { cell.borrow_mut().push(a); };
        let mut ctx = make_event_ctx(&mut f, &mut s, &mut t, &dispatch_fn);

        cb.event(&UiEvent::MouseDown {
            position: Point::new(50.0, 11.0),
            button: MouseButton::Left,
            modifiers: Modifiers::none(),
        }, &mut ctx);
        assert_eq!(cell.into_inner(), vec![Action::TogglePlay]);
    }
}
