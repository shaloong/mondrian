//! Local semantic identity and keyboard release intent for primitive author controls.

use mondrian_ui_core::{AccessibilityRole, ControlInspectionState, WidgetId};

#[derive(Default)]
pub(crate) struct ControlInspectionIdentity {
    key: Option<String>,
}

impl ControlInspectionIdentity {
    pub(crate) fn set_key(&mut self, key: Option<String>) {
        self.key = key.filter(|key| !key.is_empty());
    }

    pub(crate) fn has_key(&self) -> bool {
        self.key.is_some()
    }

    pub(crate) fn inspect(
        &self,
        widget_id: WidgetId,
        role: AccessibilityRole,
        focused: bool,
        focus_visible: bool,
        admitted: bool,
    ) -> Option<ControlInspectionState> {
        if !admitted {
            return None;
        }
        Some(ControlInspectionState {
            key: self.key.clone()?,
            widget_id,
            role,
            focused,
            focus_visible,
            keyboard_activation: None,
            numeric_input: None,
        })
    }

    pub(crate) fn restore(
        &self,
        state: &ControlInspectionState,
        role: AccessibilityRole,
        admitted: bool,
    ) -> Option<(WidgetId, bool, bool)> {
        self.restore_with_keyboard_activation(state, role, admitted, false)
    }

    pub(crate) fn restore_with_keyboard_activation(
        &self,
        state: &ControlInspectionState,
        role: AccessibilityRole,
        admitted: bool,
        supports_keyboard_activation: bool,
    ) -> Option<(WidgetId, bool, bool)> {
        self.restore_supported_inputs(state, role, admitted, supports_keyboard_activation, false)
    }

    pub(crate) fn restore_with_numeric_input(
        &self,
        state: &ControlInspectionState,
        role: AccessibilityRole,
        admitted: bool,
    ) -> Option<(WidgetId, bool, bool)> {
        self.restore_supported_inputs(state, role, admitted, false, true)
    }

    fn restore_supported_inputs(
        &self,
        state: &ControlInspectionState,
        role: AccessibilityRole,
        admitted: bool,
        supports_keyboard_activation: bool,
        supports_numeric_input: bool,
    ) -> Option<(WidgetId, bool, bool)> {
        (admitted
            && state.role == role
            && self.key.as_deref() == Some(state.key.as_str())
            && (state.keyboard_activation.is_none()
                || (supports_keyboard_activation && state.focused))
            && (state.numeric_input.is_none() || (supports_numeric_input && state.focused))
            && !(state.keyboard_activation.is_some() && state.numeric_input.is_some()))
        .then_some((state.widget_id, state.focused, state.focus_visible))
    }
}

/// A button's single matching-key release intent; pointer state stays in its widget.
#[derive(Default)]
pub(crate) struct KeyboardActivationState {
    lease: Option<mondrian_ui_core::KeyboardActivationLease>,
}

impl KeyboardActivationState {
    pub(crate) fn arm(&mut self, key: mondrian_ui_core::KeyCode) {
        use mondrian_ui_core::{KeyCode, KeyboardActivationKey, KeyboardActivationLease};
        self.lease = match key {
            KeyCode::Enter => Some(KeyboardActivationLease { key: KeyboardActivationKey::Enter }),
            KeyCode::Space => Some(KeyboardActivationLease { key: KeyboardActivationKey::Space }),
            _ => None,
        };
    }

    pub(crate) fn release(&mut self, key: mondrian_ui_core::KeyCode) -> bool {
        use mondrian_ui_core::{KeyCode, KeyboardActivationKey};
        let matches = self.lease.is_some_and(|lease| {
            matches!(
                (lease.key, key),
                (KeyboardActivationKey::Enter, KeyCode::Enter)
                    | (KeyboardActivationKey::Space, KeyCode::Space)
            )
        });
        if matches {
            self.lease = None;
        }
        matches
    }

    pub(crate) fn cancel(&mut self) -> bool {
        self.lease.take().is_some()
    }

    pub(crate) fn is_armed(&self) -> bool {
        self.lease.is_some()
    }

    pub(crate) fn inspection_lease(
        &self,
        focused: bool,
    ) -> Option<mondrian_ui_core::KeyboardActivationLease> {
        focused.then_some(self.lease).flatten()
    }

    pub(crate) fn restore(&mut self, lease: Option<mondrian_ui_core::KeyboardActivationLease>) {
        self.lease = lease;
    }
}
