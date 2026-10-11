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
pub use model::{NumberInputValue, NumericInputAdjustment};

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
    accessible_name: Option<String>,
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
            accessible_name: None,
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

    /// Current admitted baseline without parsing the display or local draft.
    pub fn committed_value(&self) -> T {
        self.model.committed_value()
    }

    /// Inclusive hard range after the sole numeric model's normalization.
    pub fn range(&self) -> (T, T) {
        self.model.range()
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

    fn apply_keyboard_adjustment(
        &mut self,
        source: T,
        adjustment: NumericInputAdjustment,
        ctx: &mut EventContext,
    ) -> EventResult {
        let value = self.model.adjusted_value(source, adjustment);
        self.set_value_from_keyboard(value, ctx)
    }

    /// Adjust this idle numeric baseline for its paired numeric control.
    ///
    /// The exact NumberInput model owns the increment and hard bounds. This
    /// never parses a draft, replaces a focused text/IME session or uses pointer
    /// coordinates. A recognized hard-bound no-op remains owned without a
    /// callback; a change uses the same fresh on_change adapter as text entry.
    pub fn adjust_unfocused_from_keyboard(
        &mut self,
        adjustment: NumericInputAdjustment,
        ctx: &mut EventContext,
    ) -> EventResult {
        if self.focused || !self.input.can_focus() || self.input.is_composing() {
            return EventResult::Ignored;
        }
        self.apply_keyboard_adjustment(self.model.committed_value(), adjustment, ctx);
        EventResult::Handled
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
            // Left/Right and Home/End remain text navigation. Numeric entry
            // and its paired Slider reuse the same typed adjustment/model.
            if matches!(
                key,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            ) && let Some(adjustment) = NumericInputAdjustment::from_key(*key, *modifiers)
            {
                let source = self.value().unwrap_or_else(|| self.model.committed_value());
                return self.apply_keyboard_adjustment(source, adjustment, ctx);
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

    fn set_accessible_name(&mut self, name: Option<String>) -> bool {
        self.accessible_name = name.filter(|name| !name.is_empty());
        true
    }

    fn accessibility(&self) -> Option<mondrian_ui_core::AccessibilityNode> {
        self.input.accessibility().map(|mut node| {
            if let Some(name) = &self.accessible_name {
                node.name = Some(name.clone());
            }
            node
        })
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

    #[test]
    fn numeric_accessibility_typed_baseline_and_range_do_not_parse_local_drafts() {
        let exact = 0.12345678901234568_f64;
        let mut input = NumberInput::new(exact, 100.0, -100.0).with_decimals(2);
        assert_eq!(input.committed_value().to_bits(), exact.to_bits());
        assert_eq!(input.range(), (-100.0, 100.0));
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        input.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
        input.event(
            &UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            &mut ctx,
        );
        input.event(&UiEvent::TextInput("incomplete -".to_owned()), &mut ctx);
        assert_eq!(input.value(), None);
        assert_eq!(input.committed_value().to_bits(), exact.to_bits());
        assert_eq!(input.range(), (-100.0, 100.0));
        assert_eq!(
            input.accessibility().expect("actual draft node").value,
            Some(mondrian_ui_core::AccessibilityValue::Text(
                "incomplete -".to_owned()
            ))
        );
        input.event(
            &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert_eq!(input.committed_value().to_bits(), exact.to_bits());
        let invalid = NumberInput::new(f64::NAN, f64::INFINITY, f64::NAN);
        assert_eq!(invalid.range(), (0.0, 1.0));
        assert_eq!(invalid.committed_value(), 0.0);
        let clamped = NumberInput::new(1000.0_f64, 10.0, -10.0);
        assert_eq!(clamped.committed_value(), 10.0);
        assert_eq!(clamped.range(), (-10.0, 10.0));
        let integer = NumberInput::new(i64::MAX, i64::MAX, i64::MIN);
        assert_eq!(integer.committed_value(), i64::MAX);
        assert_eq!(integer.range(), (i64::MIN, i64::MAX));
        let signed_zero = NumberInput::new(-0.0_f64, -1.0, 1.0);
        assert_eq!(
            signed_zero.committed_value().to_bits(),
            (-0.0_f64).to_bits()
        );
    }

    #[test]
    fn numeric_accessibility_projections_preserve_declared_float_and_integer_precision() {
        use mondrian_ui_core::AccessibilityValue;
        for exact in [0.12345678901234568_f64, -0.0, f64::MAX, f64::MIN_POSITIVE] {
            let AccessibilityValue::Number { value, min, max } =
                exact.accessibility_value(-f64::MAX, f64::MAX)
            else {
                panic!("Float64 projection")
            };
            assert_eq!(value.to_bits(), exact.to_bits());
            assert_eq!(min.to_bits(), (-f64::MAX).to_bits());
            assert_eq!(max.to_bits(), f64::MAX.to_bits());
        }
        let original = 0.12345679_f32;
        assert_eq!(
            original.accessibility_value(-1.0, 1.0),
            AccessibilityValue::Number { value: f64::from(original), min: -1.0, max: 1.0 }
        );
        for exact in [
            i64::MIN,
            -9_007_199_254_740_993,
            9_007_199_254_740_993,
            i64::MAX,
        ] {
            assert_eq!(
                exact.accessibility_value(i64::MIN, i64::MAX),
                AccessibilityValue::Integer { value: exact, min: i64::MIN, max: i64::MAX }
            );
        }
    }

    #[test]
    fn numeric_accessibility_name_refresh_retains_only_admitted_draft_and_fresh_callback() {
        let old_values = Rc::new(RefCell::new(Vec::new()));
        let new_values = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut old =
            NumberInput::new(12.0, -100.0, 100.0).on_change(record_value(Rc::clone(&old_values)));
        old.set_control_inspection_key(Some("exact-numeric-accessible-owner".to_owned()));
        assert!(old.set_accessible_name(Some("Gain (dB)".to_owned())));
        for event in [
            UiEvent::focus_gained_keyboard(),
            UiEvent::KeyDown {
                key: KeyCode::A,
                modifiers: crate::test_utils::primary_text_modifiers(),
            },
            UiEvent::TextInput("42.123456789012344".to_owned()),
        ] {
            old.event(&event, &mut ctx);
        }
        let state = old.control_inspection_state().expect("admitted draft");
        let mut fresh =
            NumberInput::new(12.0, -100.0, 100.0).on_change(record_value(Rc::clone(&new_values)));
        fresh.set_control_inspection_key(Some("exact-numeric-accessible-owner".to_owned()));
        assert!(fresh.set_accessible_name(Some("增益 (dB)".to_owned())));
        assert!(fresh.restore_control_inspection_state(&state));
        let node = fresh.accessibility().expect("fresh node");
        assert_eq!(node.name.as_deref(), Some("增益 (dB)"));
        assert_eq!(
            node.value,
            Some(mondrian_ui_core::AccessibilityValue::Text(
                "42.123456789012344".to_owned()
            ))
        );
        assert_eq!(fresh.committed_value(), 12.0);
        assert_eq!(fresh.range(), (-100.0, 100.0));
        fresh.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            &mut ctx,
        );
        assert!(old_values.borrow().is_empty());
        assert_eq!(&*new_values.borrow(), &[42.123456789012344]);
        let mut different = NumberInput::new(12.0_f64, -100.0, 100.0);
        different.set_control_inspection_key(Some("different-owner".to_owned()));
        different.set_accessible_name(Some("Other (dB)".to_owned()));
        assert!(!different.restore_control_inspection_state(&state));
        assert_eq!(different.text(), "12");
        let mut disabled = NumberInput::new(12.0_f64, -100.0, 100.0).disabled();
        disabled.set_control_inspection_key(Some("exact-numeric-accessible-owner".to_owned()));
        disabled.set_accessible_name(Some("Read only (dB)".to_owned()));
        assert!(!disabled.restore_control_inspection_state(&state));
        let node = disabled.accessibility().expect("readable disabled node");
        assert!(node.state.disabled && !node.state.focusable && !node.state.focused);
        assert_eq!(node.name.as_deref(), Some("Read only (dB)"));
        assert_eq!(
            node.value,
            Some(mondrian_ui_core::AccessibilityValue::Text("12".to_owned()))
        );
    }

    #[test]
    fn numeric_unfocused_keyboard_adapter_keeps_exact_storage_and_owns_boundary_noop() {
        let values = Rc::new(RefCell::new(Vec::<i64>::new()));
        let observed = Rc::clone(&values);
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let initial = 9_007_199_254_740_993_i64;
        let mut input =
            NumberInput::new(initial, i64::MIN, i64::MAX)
                .with_step(1)
                .on_change(move |value| {
                    observed.borrow_mut().push(value);
                    None::<Action>
                });
        assert_eq!(
            input.adjust_unfocused_from_keyboard(
                NumericInputAdjustment::Step { increase: true, coarse: false },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert_eq!(input.committed_value(), initial + 1);
        assert_eq!(
            input.text().parse::<i64>().expect("exact text"),
            initial + 1
        );
        assert_eq!(values.borrow().as_slice(), &[initial + 1]);
        assert!(input.set_unfocused_value(i64::MAX));
        assert_eq!(
            input.adjust_unfocused_from_keyboard(
                NumericInputAdjustment::Step { increase: true, coarse: false },
                &mut ctx
            ),
            EventResult::Handled
        );
        assert_eq!(input.committed_value(), i64::MAX);
        assert_eq!(
            values.borrow().as_slice(),
            &[initial + 1],
            "hard-bound ownership is a no-op, not duplicate author feedback"
        );
    }

    #[test]
    fn numeric_unfocused_keyboard_adapter_refuses_focused_draft_and_disabled_input() {
        let values = Rc::new(RefCell::new(Vec::new()));
        let dispatch = |_: Action| {};
        let mut focus = DummyFocus;
        let mut shortcuts = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut ctx = make_event_ctx(&mut focus, &mut shortcuts, &mut tooltip, &dispatch);
        let mut input = NumberInput::new(-100.0_f64, -120.0, 24.0)
            .with_step(0.1)
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
        input.event(&UiEvent::TextInput("-101.25".to_owned()), &mut ctx);
        let draft = input.text().to_owned();
        assert_eq!(
            input.adjust_unfocused_from_keyboard(
                NumericInputAdjustment::Step { increase: true, coarse: false },
                &mut ctx
            ),
            EventResult::Ignored
        );
        assert_eq!(input.text(), draft);
        assert_eq!(input.committed_value(), -100.0);
        assert!(values.borrow().is_empty());
        let mut disabled = NumberInput::new(-100.0_f64, -120.0, 24.0)
            .disabled()
            .on_change(record_value(Rc::clone(&values)));
        assert_eq!(
            disabled.adjust_unfocused_from_keyboard(NumericInputAdjustment::Maximum, &mut ctx),
            EventResult::Ignored
        );
        assert_eq!(disabled.committed_value(), -100.0);
        assert!(values.borrow().is_empty());
    }
}
