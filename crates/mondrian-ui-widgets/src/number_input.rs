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

mod model;

use model::NumberInputModel;
pub use model::NumberInputValue;

/// Adapter that maps the current numeric value to an editor [`Action`].
pub type NumberInputChangeAction<T = f64> = dyn Fn(T) -> Option<Action>;

/// Single-line numeric input.
pub struct NumberInput<T: NumberInputValue = f64> {
    input: TextInput,
    model: NumberInputModel<T>,
    focused: bool,
    width: f32,
    on_change: Option<Box<NumberInputChangeAction<T>>>,
}

impl<T: NumberInputValue> NumberInput<T> {
    /// Create a number input with a clamped initial value.
    pub fn new(value: T, min: T, max: T) -> Self {
        let model = NumberInputModel::new(value, min, max);
        let text = model.display_text();
        Self {
            input: TextInput::new("").with_text(text),
            model,
            focused: false,
            width: 200.0,
            on_change: None,
        }
    }

    /// Set the placeholder shown when the field is empty.
    pub fn with_placeholder(mut self, placeholder: impl Into<String>) -> Self {
        let text = self.input.text().to_owned();
        self.input = TextInput::new(placeholder).with_text(text);
        self
    }

    /// Set the preferred layout width.
    pub fn with_width(mut self, width: f32) -> Self {
        self.width = width.max(1.0);
        self
    }

    /// Set whether the input accepts user interaction.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.input = self.input.enabled(enabled);
        if !enabled {
            self.focused = false;
            self.input.set_text(self.model.display_text());
        }
        self
    }

    /// Disable user interaction.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Set minimum displayed decimals without rounding the stored value.
    pub fn with_decimals(mut self, decimals: usize) -> Self {
        self.input.set_text(self.model.set_decimals(decimals));
        self
    }

    /// Set the keyboard increment; typed values retain their full precision.
    pub fn with_step(mut self, step: T) -> Self {
        self.model.set_step(step);
        self
    }

    /// Dispatch a confirmed value on Enter, focus loss, or a keyboard increment.
    /// Text remains a local draft until confirmation; Escape discards that draft.
    pub fn on_change<F, R>(mut self, action: F) -> Self
    where
        F: Fn(T) -> R + 'static,
        R: Into<Option<Action>>,
    {
        self.on_change = Some(Box::new(move |value| action(value).into()));
        self
    }

    /// Current raw text.
    pub fn text(&self) -> &str {
        self.input.text()
    }

    /// Current parsed and normalized value, if the raw text is numeric.
    pub fn value(&self) -> Option<T> {
        self.model.normalized_text_value(self.input.text())
    }

    fn dispatch_value(&self, value: T, ctx: &mut EventContext) {
        if let Some(factory) = &self.on_change
            && let Some(action) = factory(value)
        {
            (ctx.dispatch)(action);
        }
    }

    fn set_value_from_keyboard(&mut self, value: T, ctx: &mut EventContext) -> EventResult {
        let previous = self.model.committed_value();
        let Some(update) = self.model.set_from_keyboard(value, self.input.text()) else {
            return EventResult::Ignored;
        };
        self.input.set_text(update.text);
        if previous != update.value {
            self.dispatch_value(update.value, ctx);
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn nudge(&mut self, step: T, increase: bool, ctx: &mut EventContext) -> EventResult {
        let value = self.value().unwrap_or_else(|| self.model.committed_value());
        self.set_value_from_keyboard(value.offset(step, increase), ctx)
    }

    fn dispatch_if_valid(&mut self, ctx: &mut EventContext) -> bool {
        let Some(value) = self.model.commit_valid_text(self.input.text()) else {
            return false;
        };
        self.dispatch_value(value, ctx);
        true
    }

    fn commit_display_text(&mut self) -> bool {
        let Some(update) = self.model.commit_display_text(self.input.text()) else {
            return false;
        };
        self.input.set_text(update.text);
        true
    }
}

impl<T: NumberInputValue> Widget for NumberInput<T> {
    fn id(&self) -> WidgetId {
        self.input.id()
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(
            self.width,
            self.input.measure(LayoutConstraint::LOOSE).height,
        ))
    }

    fn layout(&mut self, bounds: Rect) {
        self.input.layout(bounds);
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.input.is_composing() && matches!(event, UiEvent::KeyDown { .. }) {
            return self.input.event(event, ctx);
        }
        if self.focused
            && self.input.can_focus()
            && let UiEvent::KeyDown { key, modifiers } = event
            && !modifiers.ctrl
            && !modifiers.alt
            && !modifiers.meta
        {
            let step = self.model.keyboard_step(*modifiers);
            match key {
                KeyCode::Up => return self.nudge(step, true, ctx),
                KeyCode::Down => return self.nudge(step, false, ctx),
                KeyCode::PageUp => return self.nudge(self.model.page_step(), true, ctx),
                KeyCode::PageDown => return self.nudge(self.model.page_step(), false, ctx),
                _ => {}
            }
        }
        if self.focused
            && matches!(event, UiEvent::KeyDown { key: KeyCode::Escape, modifiers } if *modifiers == Modifiers::none())
        {
            self.input.set_text(self.model.display_text());
            ctx.request_repaint();
            self.input.event(&UiEvent::FocusLost, ctx);
            ctx.focus.release_focus(self.id());
            self.focused = false;
            return EventResult::Handled;
        }
        if self.focused && matches!(event, UiEvent::KeyDown { key: KeyCode::Enter, .. }) {
            self.commit_pending_input(ctx);
            return EventResult::Handled;
        }
        let result = self.input.event(event, ctx);
        if result == EventResult::Handled && matches!(event, UiEvent::FocusLost) {
            self.commit_pending_input(ctx);
            self.focused = false;
        } else if result == EventResult::Handled
            && matches!(
                event,
                UiEvent::FocusGained { .. } | UiEvent::MouseDown { .. }
            )
        {
            self.focused = true;
        }
        result
    }

    fn commit_pending_input(&mut self, ctx: &mut EventContext) -> bool {
        if !self.focused || !self.input.can_focus() || self.input.is_composing() {
            return false;
        }
        let changed = self.dispatch_if_valid(ctx);
        if self.commit_display_text() || changed {
            ctx.request_repaint();
        }
        changed
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

    fn accepts_text_input(&self) -> bool {
        self.input.accepts_text_input()
    }

    fn panel_kind(&self) -> Option<mondrian_editor_state::state::PanelKind> {
        self.input.panel_kind()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use mondrian_editor_state::Action;
    use mondrian_ui_core::types::{KeyCode, Modifiers, Point, Rect};
    use mondrian_ui_core::{EventResult, UiEvent};

    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};

    fn number_action(value: f64) -> Action {
        assert!(value.is_finite());
        Action::ToggleFullscreen
    }

    fn record_value(values: Rc<RefCell<Vec<f64>>>) -> impl Fn(f64) -> Action {
        move |value| {
            assert!(value.is_finite());
            values.borrow_mut().push(value);
            Action::ToggleFullscreen
        }
    }

    fn layout<T: NumberInputValue>(input: &mut NumberInput<T>) {
        input.layout(Rect::new(0.0, 0.0, 120.0, 28.0));
    }

    fn click<T: NumberInputValue>(input: &mut NumberInput<T>, ctx: &mut EventContext) {
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
    fn numeric_ime_composition_does_not_commit_or_nudge_the_draft() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input =
            NumberInput::new(12.0, 0.0, 100.0).on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("42".to_owned()), &mut ctx);
        input.event(&UiEvent::ImePreedit("17".to_owned()), &mut ctx);
        for key in [KeyCode::Up, KeyCode::Enter] {
            assert_eq!(
                input.event(
                    &UiEvent::KeyDown { key, modifiers: Modifiers::none() },
                    &mut ctx
                ),
                EventResult::Handled
            );
        }
        assert!(!input.commit_pending_input(&mut ctx));
        assert_eq!(input.text(), "42");
        assert!(values.borrow().is_empty());
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(input.accepts_text_input());
        assert_eq!(input.text(), "42");
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(&*values.borrow(), &[42.0]);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::ImePreedit("99".to_owned()), &mut ctx);
        input.event(&UiEvent::ImeCommit("99".to_owned()), &mut ctx);
        assert_eq!(input.value(), Some(99.0));
        assert_eq!(&*values.borrow(), &[42.0]);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.value(), Some(42.0));
        assert_eq!(&*values.borrow(), &[42.0]);
        assert!(!input.accepts_text_input());
    }

    #[test]
    fn integer_entry_keyboard_bounds_and_escape_preserve_exact_values() {
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let original = 9_007_199_254_740_993_i64;
        let recorded = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&recorded);
        let mut input =
            NumberInput::new(original, i64::MIN, i64::MAX)
                .with_step(1)
                .on_change(move |value| {
                    captured.borrow_mut().push(value);
                    Action::ToggleFullscreen
                });
        layout(&mut input);
        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput((i64::MAX - 1).to_string()), &mut ctx);
        assert_eq!(input.value(), Some(i64::MAX - 1));
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        assert_eq!(input.value(), Some(i64::MAX - 1));
        assert!(input.accepts_text_input());
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.value(), Some(i64::MAX));
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        assert_eq!(&*recorded.borrow(), &[i64::MAX]);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.text(), i64::MAX.to_string());
        assert_eq!(input.value(), Some(i64::MAX));
        assert_eq!(&*recorded.borrow(), &[i64::MAX]);
        assert!(!input.accepts_text_input());
        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(
            &UiEvent::TextInput("9223372036854775808".to_owned()),
            &mut ctx,
        );
        assert_eq!(input.value(), None);
        assert_eq!(recorded.borrow().len(), 1);
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(input.value(), Some(i64::MAX));
        assert_eq!(recorded.borrow().len(), 1);
    }

    #[test]
    fn correcting_invalid_integer_text_at_a_boundary_does_not_publish_a_value() {
        let recorded = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&recorded);
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input =
            NumberInput::new(i64::MAX, i64::MIN, i64::MAX)
                .with_step(10)
                .on_change(move |v| {
                    captured.borrow_mut().push(v);
                    Action::ToggleFullscreen
                });
        layout(&mut input);
        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(
            &UiEvent::TextInput("9223372036854775808".to_owned()),
            &mut ctx,
        );
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.value(), Some(i64::MAX));
        assert!(recorded.borrow().is_empty());
    }

    #[test]
    fn untouched_double_focus_and_commit_do_not_publish_a_rounded_value() {
        let recorded = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&recorded);
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let value = 0.12345678901234568;
        let mut input = NumberInput::new(value, -1.0, 1.0)
            .with_decimals(2)
            .with_step(0.1)
            .on_change(move |v| {
                captured.borrow_mut().push(v);
                Action::ToggleFullscreen
            });
        layout(&mut input);
        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(input.value(), Some(value));
        assert!(recorded.borrow().is_empty());
    }

    #[test]
    fn dispatches_number_only_after_text_confirmation() {
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
        assert_eq!(input.value(), Some(42.0));
        assert!(actions.borrow().is_empty());
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert_eq!(actions.borrow().as_slice(), &[number_action(42.0)]);
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
    fn decimals_preserve_initial_fractional_value() {
        let input = NumberInput::new(0.5, 0.0, 1.0).with_decimals(2);

        assert_eq!(input.text(), "0.50");
        assert_eq!(input.value(), Some(0.5));
    }

    #[test]
    fn new_orders_inverted_range() {
        let input = NumberInput::new(50.0, 100.0, 0.0);

        assert_eq!(input.text(), "50");
        assert_eq!(input.value(), Some(50.0));
    }

    #[test]
    fn new_recovers_non_finite_range_and_value() {
        let input = NumberInput::new(f64::NAN, f64::NAN, f64::INFINITY).with_decimals(2);

        assert_eq!(input.text(), "0.00");
        assert_eq!(input.value(), Some(0.0));
    }

    #[test]
    fn enter_formats_valid_text_to_normalized_number_without_duplicate_dispatch() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(0.0, 0.0, 1.0)
            .with_step(0.25)
            .with_decimals(2)
            .on_change(number_action);
        layout(&mut input);

        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("0.62".into()), &mut ctx);

        assert_eq!(input.text(), "0.62");
        assert_eq!(input.value(), Some(0.62));
        assert!(actions.borrow().is_empty());

        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(input.text(), "0.62");
        assert_eq!(input.value(), Some(0.62));
        assert_eq!(actions.borrow().len(), 1);
    }

    #[test]
    fn focus_lost_reverts_invalid_text_to_last_committed_number() {
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

        assert_eq!(
            input.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert_eq!(input.text(), "12");
        assert_eq!(input.value(), Some(12.0));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn disabling_number_input_clears_focus_and_pending_invalid_edit() {
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
        assert!(input.focused);
        assert!(actions.borrow().is_empty());

        input = input.enabled(false);
        assert_eq!(input.text(), "12");
        assert!(!input.focused);

        input = input.enabled(true);
        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(input.text(), "12");
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn invalid_text_reverts_to_most_recent_valid_number() {
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(12.0, 0.0, 100.0).with_step(5.0).on_change(number_action);
        layout(&mut input);

        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("42".into()), &mut ctx);
        assert_eq!(input.value(), Some(42.0));
        assert!(actions.borrow().is_empty());
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(actions.borrow().len(), 1);

        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("abc".into()), &mut ctx);
        assert_eq!(input.text(), "abc");
        assert_eq!(input.value(), None);

        assert_eq!(
            input.event(&UiEvent::FocusLost, &mut ctx),
            EventResult::Handled
        );

        assert_eq!(input.text(), "42");
        assert_eq!(input.value(), Some(42.0));
        assert_eq!(actions.borrow().len(), 1);
    }

    #[test]
    fn arrow_keys_nudge_committed_value_with_configured_step() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(10.0, 0.0, 100.0)
            .with_step(2.0)
            .on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        click(&mut input, &mut ctx);

        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Down, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            input.event(
                &UiEvent::KeyDown {
                    key: KeyCode::PageDown,
                    modifiers: Modifiers::none()
                },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(input.text(), "10");
        assert_eq!(values.borrow().as_slice(), &[12.0, 32.0, 30.0, 10.0]);
    }

    #[test]
    fn arrow_keys_use_decimal_precision_when_step_is_unspecified() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(0.5, 0.0, 1.0)
            .with_decimals(2)
            .on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        click(&mut input, &mut ctx);

        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(input.text(), "0.61");
        assert_eq!(values.borrow().as_slice(), &[0.51, 0.61]);
    }

    #[test]
    fn arrow_keys_ignore_ctrl_alt_and_meta_chords() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(10.0, 0.0, 100.0)
            .with_step(2.0)
            .on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        click(&mut input, &mut ctx);

        for (key, modifiers) in [
            (KeyCode::Up, Modifiers::ctrl()),
            (KeyCode::Up, Modifiers { alt: true, ..Default::default() }),
            (
                KeyCode::Down,
                Modifiers { meta: true, ..Default::default() },
            ),
            (
                KeyCode::PageDown,
                Modifiers { alt: true, shift: true, ..Default::default() },
            ),
            (
                KeyCode::PageUp,
                Modifiers { ctrl: true, shift: true, ..Default::default() },
            ),
        ] {
            assert_eq!(
                input.event(&UiEvent::KeyDown { key, modifiers }, &mut ctx),
                EventResult::Ignored
            );
        }

        assert_eq!(input.text(), "10");
        assert!(values.borrow().is_empty());
    }

    #[test]
    fn arrow_key_at_boundary_does_not_dispatch_duplicate_change() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(100.0, 0.0, 100.0)
            .with_step(1.0)
            .on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        click(&mut input, &mut ctx);

        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Ignored
        );

        assert_eq!(input.text(), "100");
        assert!(values.borrow().is_empty());
    }

    #[test]
    fn arrow_keys_recover_invalid_text_from_last_committed_value() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let actions = RefCell::new(Vec::new());
        let dispatch = |action: Action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(10.0, 0.0, 100.0)
            .with_step(5.0)
            .on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        click(&mut input, &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::A, modifiers: Modifiers::ctrl() },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("abc".into()), &mut ctx);
        assert_eq!(input.value(), None);

        assert_eq!(
            input.event(
                &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(input.text(), "15");
        assert_eq!(values.borrow().as_slice(), &[15.0]);
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
