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
    inspection_identity: crate::control_inspection::ControlInspectionIdentity,
    model: NumberInputModel<T>,
    focused: bool,
    focus_visible: bool,
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
            inspection_identity: crate::control_inspection::ControlInspectionIdentity::default(),
            model,
            focused: false,
            focus_visible: false,
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
            self.focus_visible = false;
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

    /// Update an unfocused field from its fresh numeric presentation model.
    ///
    /// The existing hard-range and exact formatting contract applies. No user
    /// callback runs. An unchanged committed value is a strict no-op; a focused
    /// text session, including draft selection, history and IME preedit, is never
    /// overwritten. External author changes during editing require the normal
    /// fresh-model/draft-admission boundary rather than using this setter.
    /// Returns whether the idle displayed baseline changed.
    pub fn set_unfocused_value(&mut self, value: T) -> bool {
        if self.focused || self.input.is_composing() {
            return false;
        }
        let Some(update) = self.model.set_from_model(value) else {
            return false;
        };
        self.input.set_text(update.text);
        true
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
            && TextInput::is_local_history_chord(*key, *modifiers)
            && !self.input.owns_local_history()
            && self.input.text() == self.model.display_text()
        {
            return EventResult::Ignored;
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
            self.focus_visible = false;
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
            self.focus_visible = false;
        } else if result == EventResult::Handled
            && matches!(
                event,
                UiEvent::FocusGained { .. } | UiEvent::MouseDown { .. }
            )
        {
            self.focused = true;
            self.focus_visible =
                matches!(event, UiEvent::FocusGained { source } if source.is_focus_visible());
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
        self.input.clear_local_history();
        changed
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.input.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.input.hit_test(point)
    }

    fn edit_command_available(
        &self,
        command: mondrian_ui_core::widget::EditCommand,
    ) -> Option<bool> {
        self.input.edit_command_available(command)
    }

    fn edit_command(
        &mut self,
        command: mondrian_ui_core::widget::EditCommand,
        ctx: &mut EventContext,
    ) -> EventResult {
        self.input.edit_command(command, ctx)
    }

    fn can_focus(&self) -> bool {
        self.input.can_focus()
    }

    fn set_control_inspection_key(&mut self, key: Option<String>) -> bool {
        self.inspection_identity.set_key(key);
        true
    }

    fn can_inspect_text_draft(&self) -> bool {
        self.focused && self.inspection_identity.has_key() && self.input.can_inspect_draft()
    }

    fn control_inspection_state(&self) -> Option<mondrian_ui_core::ControlInspectionState> {
        if self.focused && !self.can_inspect_text_draft() {
            return None;
        }
        let mut state = self.inspection_identity.inspect(
            self.id(),
            mondrian_ui_core::AccessibilityRole::TextInput,
            self.focused,
            self.focus_visible,
            self.input.is_enabled() && !self.input.is_composing(),
        )?;
        state.numeric_input = if self.can_inspect_text_draft() {
            Some(Box::new(mondrian_ui_core::NumericInputInspectionState {
                contract: self.model.inspection_contract(),
                draft: self.input.inspection_draft()?,
            }))
        } else {
            None
        };
        Some(state)
    }

    fn restore_control_inspection_state(
        &mut self,
        state: &mondrian_ui_core::ControlInspectionState,
    ) -> bool {
        if state
            .numeric_input
            .as_ref()
            .is_some_and(|numeric| numeric.contract != self.model.inspection_contract())
        {
            return false;
        }
        let Some((id, focused, focus_visible)) =
            self.inspection_identity.restore_with_numeric_input(
                state,
                mondrian_ui_core::AccessibilityRole::TextInput,
                self.input.is_enabled() && !self.input.is_composing(),
            )
        else {
            return false;
        };
        if !self.input.restore_numeric_input_inspection(
            id,
            focused,
            state.numeric_input.as_ref().map(|numeric| &numeric.draft),
        ) {
            return false;
        }
        self.focused = focused;
        self.focus_visible = focus_visible;
        true
    }

    fn accessibility(&self) -> Option<mondrian_ui_core::AccessibilityNode> {
        self.input.accessibility()
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
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

    #[test]
    fn numeric_inspection_retains_local_selection_and_executes_only_fresh_callback() {
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut original = NumberInput::new(10.0_f64, -100.0, 100.0)
            .with_step(0.25)
            .with_decimals(2)
            .on_change(|_| Action::Play);
        assert!(original.set_control_inspection_key(Some("exact-numeric-owner".to_owned())));
        for event in [
            UiEvent::focus_gained_keyboard(),
            UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            UiEvent::TextInput("42.125".to_owned()),
            UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::none() },
            UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
            UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::shift() },
        ] {
            original.event(&event, &mut ctx);
        }
        assert!(actions.borrow().is_empty());
        let inspected = original.control_inspection_state().expect("local keyboard draft");
        let values = Rc::new(RefCell::new(Vec::<f64>::new()));
        let callback_values = Rc::clone(&values);
        let mut replacement = NumberInput::new(10.0_f64, -100.0, 100.0)
            .with_step(0.25)
            .with_decimals(2)
            .on_change(move |value| {
                callback_values.borrow_mut().push(value);
                Action::Copy
            });
        replacement.set_control_inspection_key(Some("exact-numeric-owner".to_owned()));
        assert!(replacement.restore_control_inspection_state(&inspected));
        replacement.event(&UiEvent::TextInput("3".to_owned()), &mut ctx);
        assert_eq!(
            replacement.text(),
            "43.125",
            "fresh model resumes the exact original raw text, grapheme caret and selection"
        );
        replacement.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(values.borrow().as_slice(), &[43.125]);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::Copy],
            "the old callback is never retained"
        );
        replacement.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(values.borrow().as_slice(), &[43.125]);
        assert_eq!(
            actions.borrow().as_slice(),
            &[Action::Copy],
            "confirmed Enter dispatches once"
        );
    }

    #[test]
    fn numeric_inspection_requires_exact_declared_value_range_step_precision_and_enabled_owner() {
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut original =
            NumberInput::new(10.0_f64, -100.0, 100.0).with_step(0.25).with_decimals(2);
        assert!(original.set_control_inspection_key(Some("numeric-contract".to_owned())));
        original.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        original.event(
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            &mut ctx,
        );
        original.event(&UiEvent::TextInput("42.125".to_owned()), &mut ctx);
        let inspected = original.control_inspection_state().expect("local draft");
        for boundary in 0..5 {
            let (min, max, step, decimals, enabled) = match boundary {
                0 => (0.0, 100.0, 0.25, 2, true),
                1 => (-100.0, 200.0, 0.25, 2, true),
                2 => (-100.0, 100.0, 0.5, 2, true),
                3 => (-100.0, 100.0, 0.25, 3, true),
                4 => (-100.0, 100.0, 0.25, 2, false),
                _ => unreachable!(),
            };
            let mut replacement = NumberInput::new(10.0_f64, min, max)
                .with_step(step)
                .with_decimals(decimals)
                .enabled(enabled);
            replacement.set_control_inspection_key(Some("numeric-contract".to_owned()));
            assert!(
                !replacement.restore_control_inspection_state(&inspected),
                "changed numeric contract {boundary} refuses draft"
            );
            assert_eq!(
                replacement.value(),
                Some(10.0),
                "fresh authoritative numeric baseline is untouched"
            );
        }
        let exact = 1.0000000000000002_f64;
        let mut original = NumberInput::new(exact, 0.0, 2.0);
        original.set_control_inspection_key(Some("precise-float".to_owned()));
        original.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let inspected = original.control_inspection_state().expect("f64 baseline");
        let mut rounded = NumberInput::new(1.0_f64, 0.0, 2.0);
        rounded.set_control_inspection_key(Some("precise-float".to_owned()));
        assert!(
            !rounded.restore_control_inspection_state(&inspected),
            "distinct f64 bits must not compare through f32 slider coordinates or tolerance"
        );
        let mut narrowed = NumberInput::new(exact as f32, 0.0, 2.0);
        narrowed.set_control_inspection_key(Some("precise-float".to_owned()));
        assert!(
            !narrowed.restore_control_inspection_state(&inspected),
            "changing declared numeric representation cannot retain a draft"
        );
        let exact = 9_007_199_254_740_993_i64;
        let mut original = NumberInput::new(exact, i64::MIN, i64::MAX);
        original.set_control_inspection_key(Some("precise-integer".to_owned()));
        original.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        let inspected = original.control_inspection_state().expect("i64 baseline");
        let mut replacement = NumberInput::new(exact + 1, i64::MIN, i64::MAX);
        replacement.set_control_inspection_key(Some("precise-integer".to_owned()));
        assert!(
            !replacement.restore_control_inspection_state(&inspected),
            "large adjacent i64 baselines remain distinct without narrowing"
        );
        assert_eq!(replacement.value(), Some(exact + 1));
    }

    #[test]
    fn numeric_inspection_excludes_ime_preedit_and_pointer_selection_ownership() {
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(10.0_f64, -100.0, 100.0);
        layout(&mut input);
        assert!(input.set_control_inspection_key(Some("numeric-input-intent".to_owned())));
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        assert!(input.control_inspection_state().is_some());
        input.event(&UiEvent::ImePreedit("42".to_owned()), &mut ctx);
        assert!(input.control_inspection_state().is_none(),"preedit stays owned by the platform IME session and cannot be copied into a replacement");
        input.event(&UiEvent::ImePreedit(String::new()), &mut ctx);
        assert!(input.control_inspection_state().is_some());
        input.event(
            &UiEvent::MouseDown {
                position: Point::new(12.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(
            input.control_inspection_state().is_none(),
            "pointer selection and capture stay owned by the current widget"
        );
        input.event(
            &UiEvent::MouseUp {
                position: Point::new(12.0, 12.0),
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );
        assert!(
            input.control_inspection_state().is_some(),
            "finished pointer selection permits a later plain numeric draft inspection"
        );
    }

    #[test]
    fn numeric_local_history_reprojects_only_exact_baseline_and_ends_at_author_confirmation() {
        let actions = RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut original = NumberInput::new(10.0_f64, -100.0, 100.0).on_change(|_| Action::Play);
        original.set_control_inspection_key(Some("numeric-local-history".to_owned()));
        let key = |key, modifiers| UiEvent::KeyDown { key, modifiers };
        let primary = crate::test_utils::primary_text_modifiers();
        let redo = Modifiers { shift: true, ..primary };
        for event in [
            UiEvent::focus_gained_keyboard(),
            key(KeyCode::A, primary),
            UiEvent::TextInput("42.125".to_owned()),
        ] {
            original.event(&event, &mut ctx);
        }
        assert_eq!(
            original.event(&key(KeyCode::Z, primary), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(original.text(), "10");
        let retained = original.control_inspection_state().expect("admitted local Redo branch");
        let mut fresh = NumberInput::new(10.0_f64, -100.0, 100.0).on_change(|_| Action::Pause);
        fresh.set_control_inspection_key(Some("numeric-local-history".to_owned()));
        assert!(fresh.restore_control_inspection_state(&retained));
        assert_eq!(
            fresh.event(&key(KeyCode::Z, redo), &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            fresh.text(),
            "42.125",
            "same baseline fresh projection retains the owned local Redo branch"
        );
        assert!(actions.borrow().is_empty());
        let mut changed = NumberInput::new(11.0_f64, -100.0, 100.0);
        changed.set_control_inspection_key(Some("numeric-local-history".to_owned()));
        assert!(
            !changed.restore_control_inspection_state(&retained),
            "authoritative baseline changes must reject the draft and its history together"
        );
        fresh.event(&key(KeyCode::Enter, Modifiers::none()), &mut ctx);
        assert_eq!(
            &*actions.borrow(),
            &[Action::Pause],
            "confirmation uses only the fresh callback once"
        );
        assert_eq!(fresh.event(&key(KeyCode::Z, primary), &mut ctx), EventResult::Ignored, "confirmed numeric input releases local Undo to the author history without reverting its own displayed model");
        assert_eq!(fresh.text(), "42.125");
        assert_eq!(&*actions.borrow(), &[Action::Pause]);
    }

    #[test]
    fn numeric_history_inspection_admission_bounds_memory_indices_and_owned_intent() {
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut original = NumberInput::new(10.0_f64, -100.0, 100.0);
        original.set_control_inspection_key(Some("numeric-bounded-history".to_owned()));
        for event in [
            UiEvent::focus_gained_keyboard(),
            UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            UiEvent::TextInput("42.125".to_owned()),
        ] {
            original.event(&event, &mut ctx);
        }
        let valid = original.control_inspection_state().expect("real local history payload");
        let fresh = || {
            let mut input = NumberInput::new(10.0_f64, -100.0, 100.0);
            input.set_control_inspection_key(Some("numeric-bounded-history".to_owned()));
            input
        };
        assert!(fresh().restore_control_inspection_state(&valid));
        let noop = {
            let first = &valid.numeric_input.as_ref().expect("numeric").draft.history.entries[0];
            mondrian_ui_core::TextInputHistoryInspectionEntry {
                text_before: first.text_after.clone(),
                text_after: first.text_after.clone(),
                cursor_before: first.cursor_after,
                cursor_after: first.cursor_after,
                anchor_before: first.anchor_after,
                anchor_after: first.anchor_after,
            }
        };
        let mut count_boundary = valid.clone();
        {
            let history =
                &mut count_boundary.numeric_input.as_mut().expect("numeric").draft.history;
            history.entries.resize(
                mondrian_ui_core::widget::TEXT_INPUT_HISTORY_MAX_ENTRIES,
                noop.clone(),
            );
            history.position = history.entries.len();
        }
        assert!(
            fresh().restore_control_inspection_state(&count_boundary),
            "the exact entry count boundary is admissible"
        );
        {
            let history =
                &mut count_boundary.numeric_input.as_mut().expect("numeric").draft.history;
            history.entries.push(noop);
            history.position += 1;
        }
        let mut cases = vec![count_boundary];
        let mut bad = valid.clone();
        bad.numeric_input.as_mut().expect("numeric").draft.history.position = usize::MAX;
        cases.push(bad);
        let mut bad = valid.clone();
        bad.numeric_input.as_mut().expect("numeric").draft.history.owns_input = false;
        cases.push(bad);
        let mut bad = valid.clone();
        bad.numeric_input.as_mut().expect("numeric").draft.history.entries[0]
            .cursor_before
            .grapheme_index = usize::MAX;
        cases.push(bad);
        let mut bad = valid.clone();
        bad.numeric_input.as_mut().expect("numeric").draft.history.entries[0].text_before =
            "10\n".to_owned();
        cases.push(bad);
        let mut bad = valid.clone();
        let draft = &mut bad.numeric_input.as_mut().expect("numeric").draft;
        let over_budget = "0".repeat(mondrian_ui_core::widget::TEXT_INPUT_HISTORY_MAX_BYTES + 1);
        draft.text = over_budget.clone();
        draft.history.entries[0].text_after = over_budget;
        cases.push(bad);
        for invalid in cases {
            let mut replacement = fresh();
            let new_id = replacement.id();
            assert!(
                !replacement.restore_control_inspection_state(&invalid),
                "invalid local history must refuse the whole inspection payload"
            );
            assert_eq!(
                replacement.id(),
                new_id,
                "refusal cannot install the stale widget identity"
            );
            assert_eq!(replacement.text(), "10");
            assert!(!replacement.accepts_text_input());
        }
    }

    #[test]
    fn associated_single_line_numeric_inspection_refuses_invalid_edge_affinity_before_installing_identity(
    ) {
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut original = NumberInput::new(12.0_f64, 0.0, 100.0);
        original.set_control_inspection_key(Some("exact-associated-number".to_owned()));
        for event in [
            UiEvent::focus_gained_keyboard(),
            UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            UiEvent::TextInput("42".to_owned()),
        ] {
            original.event(&event, &mut ctx);
        }
        let valid = original.control_inspection_state().expect("valid bounded history");
        let mut wrong_current = valid.clone();
        wrong_current.numeric_input.as_mut().expect("numeric").draft.cursor.affinity =
            mondrian_ui_core::TextCaretAffinity::BeforeNext;
        let mut wrong_history = valid.clone();
        wrong_history.numeric_input.as_mut().expect("numeric").draft.history.entries[0]
            .cursor_before
            .affinity = mondrian_ui_core::TextCaretAffinity::BeforeNext;
        let mut wrong_anchor = valid.clone();
        wrong_anchor.numeric_input.as_mut().expect("numeric").draft.selection_anchor =
            Some(mondrian_ui_core::TextInputCaretPosition {
                grapheme_index: 0,
                affinity: mondrian_ui_core::TextCaretAffinity::AfterPrevious,
            });
        for invalid in [wrong_current, wrong_history, wrong_anchor] {
            let mut fresh = NumberInput::new(12.0_f64, 0.0, 100.0);
            fresh.set_control_inspection_key(Some("exact-associated-number".to_owned()));
            let new_id = fresh.id();
            assert!(!fresh.restore_control_inspection_state(&invalid));
            assert_eq!(
                fresh.id(),
                new_id,
                "whole payload refusal precedes stale identity installation"
            );
            assert_eq!(fresh.text(), "12");
            assert!(!fresh.accepts_text_input());
        }
    }

    #[test]
    fn unfocused_model_sync_preserves_exact_values_without_dispatch_or_quantization() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let mut input = NumberInput::new(12.0, -100.0, 100.0)
            .with_decimals(2)
            .with_step(0.1)
            .on_change(record_value(Rc::clone(&values)));
        let exact = 42.123456789012344_f64;
        assert!(input.set_unfocused_value(exact));
        assert_eq!(
            input.text().parse::<f64>().expect("exact text").to_bits(),
            exact.to_bits()
        );
        assert_eq!(input.value().expect("numeric").to_bits(), exact.to_bits());
        assert!(!input.set_unfocused_value(exact));
        assert!(!input.set_unfocused_value(f64::NAN));
        assert!(!input.set_unfocused_value(f64::INFINITY));
        assert_eq!(
            input.value().expect("unchanged numeric").to_bits(),
            exact.to_bits()
        );
        assert!(input.set_unfocused_value(1000.0));
        assert_eq!(input.value(), Some(100.0));
        assert!(input.set_unfocused_value(-0.0));
        assert_eq!(
            input.text().parse::<f64>().expect("negative zero").to_bits(),
            (-0.0_f64).to_bits()
        );
        assert!(input.set_unfocused_value(0.0));
        assert_eq!(
            input.text().parse::<f64>().expect("positive zero").to_bits(),
            0.0_f64.to_bits()
        );
        assert!(
            values.borrow().is_empty(),
            "model projection never invokes an author callback"
        );
        let mut integer = NumberInput::new(0_i64, i64::MIN, i64::MAX).with_step(2_i64);
        assert!(integer.set_unfocused_value(i64::MAX));
        assert_eq!(integer.text().parse::<i64>(), Ok(i64::MAX));
        assert!(integer.set_unfocused_value(i64::MIN));
        assert_eq!(integer.text().parse::<i64>(), Ok(i64::MIN));
        let mut disabled = NumberInput::new(1.0_f64, 0.0, 100.0).disabled();
        assert!(disabled.set_unfocused_value(exact));
        assert_eq!(
            disabled.text().parse::<f64>().expect("readable disabled value").to_bits(),
            exact.to_bits()
        );
        assert!(disabled.accessibility().expect("metadata").state.disabled);
        assert!(!disabled.can_focus());
    }

    #[test]
    fn unfocused_model_sync_refuses_to_overwrite_focused_draft_selection_and_history() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input =
            NumberInput::new(12.0, 0.0, 100.0).on_change(record_value(Rc::clone(&values)));
        input.set_control_inspection_key(Some("numeric-peer-exact-owner".to_owned()));
        layout(&mut input);
        input.event(
            &UiEvent::FocusGained { source: FocusSource::Keyboard },
            &mut ctx,
        );
        input.event(
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("42".to_owned()), &mut ctx);
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Home, modifiers: Modifiers::shift() },
            &mut ctx,
        );
        let before = input.control_inspection_state().expect("bounded owned draft");
        assert_eq!(input.text(), "42");
        assert!(!input.set_unfocused_value(12.0));
        assert!(!input.set_unfocused_value(80.0));
        assert_eq!(
            input.control_inspection_state().expect("retained owned draft"),
            before,
            "same and changed presentation requests preserve an active local session byte for byte"
        );
        assert!(values.borrow().is_empty());
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.text(), "12");
        assert!(input.set_unfocused_value(80.0));
        assert_eq!(input.text(), "80");
        assert!(values.borrow().is_empty());
    }

    #[test]
    fn unfocused_model_sync_preserves_preedit_then_confirm_dispatches_once() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input =
            NumberInput::new(12.0, 0.0, 100.0).on_change(record_value(Rc::clone(&values)));
        layout(&mut input);
        input.event(
            &UiEvent::FocusGained { source: FocusSource::Keyboard },
            &mut ctx,
        );
        input.event(
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("42".to_owned()), &mut ctx);
        input.event(&UiEvent::ImePreedit("17".to_owned()), &mut ctx);
        assert!(input.input.is_composing());
        assert!(!input.set_unfocused_value(12.0));
        assert!(!input.set_unfocused_value(80.0));
        assert!(input.input.is_composing());
        assert_eq!(input.text(), "42");
        assert!(values.borrow().is_empty());
        input.event(&UiEvent::ImeCancel, &mut ctx);
        assert!(!input.input.is_composing());
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(&*values.borrow(), &[42.0]);
        input.event(&UiEvent::FocusLost, &mut ctx);
        assert!(input.set_unfocused_value(80.0));
        assert_eq!(input.text(), "80");
        input.event(
            &UiEvent::FocusGained { source: FocusSource::Keyboard },
            &mut ctx,
        );
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(
            &*values.borrow(),
            &[42.0],
            "synchronized baseline is not a second author edit"
        );
    }
}
