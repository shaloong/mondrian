//! Numeric input widget.
//!
//! This wraps `TextInput` so numeric fields reuse the same focus, IME,
//! selection, clipboard, and clipping behavior as normal text inputs while
//! keeping numeric parsing/clamping in one reusable component.

use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

use crate::TextInput;

/// Adapter that maps the current numeric value to an editor [`Action`].
pub type NumberInputChangeAction = dyn Fn(f64) -> Action;

/// Single-line numeric input.
pub struct NumberInput {
    input: TextInput,
    min: f64,
    max: f64,
    step: Option<f64>,
    decimals: usize,
    on_change: Option<Box<NumberInputChangeAction>>,
}

impl NumberInput {
    /// Create a number input with a clamped initial value.
    pub fn new(value: f64, min: f64, max: f64) -> Self {
        let (min, max) = ordered_range(min, max);
        let value = value.clamp(min, max);
        Self {
            input: TextInput::new("").with_text(format_number(value, 0)),
            min,
            max,
            step: None,
            decimals: 0,
            on_change: None,
        }
    }

    /// Set the placeholder shown when the field is empty.
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        let text = self.input.text().to_owned();
        self.input = TextInput::new(placeholder).with_text(text);
        self
    }

    /// Set the number of decimals used for initial/displayed values.
    pub fn with_decimals(mut self, decimals: usize) -> Self {
        self.decimals = decimals.min(6);
        if let Some(value) = parse_number(self.input.text()) {
            self.input.set_text(format_number(value, self.decimals));
        }
        self
    }

    /// Quantize submitted values to a fixed step.
    pub fn with_step(mut self, step: f64) -> Self {
        self.step = (step.is_finite() && step > 0.0).then_some(step);
        self
    }

    /// Dispatch an action whenever user input parses as a valid numeric value.
    pub fn on_change(mut self, action: impl Fn(f64) -> Action + 'static) -> Self {
        self.on_change = Some(Box::new(action));
        self
    }

    /// Current raw text.
    pub fn text(&self) -> &str {
        self.input.text()
    }

    /// Current parsed and normalized value, if the raw text is numeric.
    pub fn value(&self) -> Option<f64> {
        parse_number(self.input.text()).map(|value| self.normalize(value))
    }

    fn normalize(&self, value: f64) -> f64 {
        let clamped = value.clamp(self.min, self.max);
        if let Some(step) = self.step {
            let steps = ((clamped - self.min) / step).round();
            (self.min + steps * step).clamp(self.min, self.max)
        } else {
            clamped
        }
    }

    fn dispatch_if_valid(&self, ctx: &mut EventContext) {
        let Some(factory) = &self.on_change else {
            return;
        };
        let Some(value) = self.value() else {
            return;
        };
        (ctx.dispatch)(factory(value));
    }
}

impl Widget for NumberInput {
    fn id(&self) -> WidgetId {
        self.input.id()
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        self.input.measure(constraint)
    }

    fn layout(&mut self, bounds: Rect) {
        self.input.layout(bounds);
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        let before = self.input.text().to_owned();
        let result = self.input.event(event, ctx);
        if result == EventResult::Handled && self.input.text() != before {
            self.dispatch_if_valid(ctx);
        }
        result
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.input.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.input.hit_test(point)
    }

    fn can_focus(&self) -> bool {
        self.input.can_focus()
    }

    fn panel_kind(&self) -> Option<mondrian_editor_state::state::PanelKind> {
        self.input.panel_kind()
    }
}

fn ordered_range(min: f64, max: f64) -> (f64, f64) {
    if min <= max {
        (min, max)
    } else {
        (max, min)
    }
}

fn parse_number(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value = trimmed.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}

fn format_number(value: f64, decimals: usize) -> String {
    if decimals == 0 {
        format!("{value:.0}")
    } else {
        format!("{value:.decimals$}")
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use mondrian_editor_state::Action;
    use mondrian_ui_core::types::{KeyCode, Modifiers, Point, Rect};
    use mondrian_ui_core::{EventResult, UiEvent};

    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    fn number_action(value: f64) -> Action {
        assert!(value.is_finite());
        Action::ToggleFullscreen
    }

    fn layout(input: &mut NumberInput) {
        input.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
    }

    fn click(input: &mut NumberInput, ctx: &mut EventContext) {
        input.event(
            &UiEvent::MouseDown {
                position: Point::new(12.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        );
    }

    #[test]
    fn dispatches_normalized_number_after_text_change() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(0.0, 0.0, 100.0).with_step(5.0).on_change(number_action);
        layout(&mut input);

        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("42".into()), &mut ctx);

        assert_eq!(input.text(), "42");
        assert_eq!(input.value(), Some(40.0));
        assert_eq!(actions.borrow().as_slice(), &[number_action(40.0)]);
    }

    #[test]
    fn invalid_text_is_kept_locally_without_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(12.0, 0.0, 100.0).on_change(number_action);
        layout(&mut input);

        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("abc".into()), &mut ctx);

        assert_eq!(input.text(), "abc");
        assert_eq!(input.value(), None);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn delegates_focus_and_blur_to_text_input() {
        let mut input = NumberInput::new(1.0, 0.0, 10.0);
        layout(&mut input);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let dispatch = |_| {};
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);

        click(&mut input, &mut ctx);
        assert!(input.can_focus());

        let result = input.event(
            &UiEvent::MouseDown {
                position: Point::new(240.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Ignored);
        assert_eq!(input.text(), "1");
    }
}
