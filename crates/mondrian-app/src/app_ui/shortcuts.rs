//! Default keyboard shortcuts for the app UI shell.
//!
//! The router resolves these only after the focused widget ignores a `KeyDown`,
//! so text inputs and panel-specific key handling keep priority.

use mondrian_editor_state::Action;
use mondrian_ui_core::shortcut::ShortcutBinding;
use mondrian_ui_core::types::{KeyCode, Modifiers};
use mondrian_ui_events::EventRouter;
use serde::{Deserialize, Serialize};

use crate::app_ui::commands::default_commands;

/// A app UI shell shortcut together with its menu-facing display label.
#[derive(Debug, Clone)]
pub struct AppUiShortcut {
    /// Stable preference id for this command binding.
    pub id: &'static str,
    /// Key/modifier binding registered with the shortcut router.
    pub binding: ShortcutBinding,
    /// Editor or shell action dispatched by the binding.
    pub action: Action,
    /// Menu-facing shortcut label.
    pub label: String,
}

/// User override for one app UI shortcut descriptor.
///
/// `binding: None` disables the descriptor. Unknown ids are ignored when the
/// active shortcut table is built, which keeps alpha preference files safe to
/// load across descriptor reshuffles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppUiShortcutOverride {
    /// Stable id from [`AppUiShortcut::id`].
    pub id: String,
    /// Replacement binding, or `None` to disable the shortcut.
    pub binding: Option<AppUiShortcutBinding>,
}

/// Serializable key binding stored in app UI preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppUiShortcutBinding {
    pub key: AppUiShortcutKey,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl AppUiShortcutBinding {
    /// Build a stored binding from the router binding type.
    pub fn from_core(binding: &ShortcutBinding) -> Option<Self> {
        Some(Self {
            key: AppUiShortcutKey::from_key_code(binding.key)?,
            ctrl: binding.modifiers.ctrl,
            alt: binding.modifiers.alt,
            shift: binding.modifiers.shift,
            meta: binding.modifiers.meta,
        })
    }

    /// Convert this stored binding into the router binding type.
    pub fn to_core(self) -> ShortcutBinding {
        ShortcutBinding::new(
            self.key.to_core(),
            Modifiers {
                ctrl: self.ctrl,
                alt: self.alt,
                shift: self.shift,
                meta: self.meta,
            },
        )
    }

    /// User-facing shortcut hint.
    pub fn label(self) -> String {
        self.label_for_platform(cfg!(target_os = "macos"))
    }

    fn label_for_platform(self, macos: bool) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push(if macos { "⌃" } else { "Ctrl" });
        }
        if self.alt {
            parts.push(if macos { "⌥" } else { "Alt" });
        }
        if self.shift {
            parts.push(if macos { "⇧" } else { "Shift" });
        }
        if self.meta {
            parts.push(if macos { "⌘" } else { "Meta" });
        }
        parts.push(self.key.label_for_platform(macos));
        if macos {
            parts.join("")
        } else {
            parts.join("+")
        }
    }
}

/// Serializable subset of keys supported by app UI shortcut preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppUiShortcutKey {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Escape,
    Tab,
    Enter,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
}

impl AppUiShortcutKey {
    /// Convert a runtime UI key code into a serializable shortcut key.
    pub fn from_key_code(key: KeyCode) -> Option<Self> {
        Some(match key {
            KeyCode::A => Self::A,
            KeyCode::B => Self::B,
            KeyCode::C => Self::C,
            KeyCode::D => Self::D,
            KeyCode::E => Self::E,
            KeyCode::F => Self::F,
            KeyCode::G => Self::G,
            KeyCode::H => Self::H,
            KeyCode::I => Self::I,
            KeyCode::J => Self::J,
            KeyCode::K => Self::K,
            KeyCode::L => Self::L,
            KeyCode::M => Self::M,
            KeyCode::N => Self::N,
            KeyCode::O => Self::O,
            KeyCode::P => Self::P,
            KeyCode::Q => Self::Q,
            KeyCode::R => Self::R,
            KeyCode::S => Self::S,
            KeyCode::T => Self::T,
            KeyCode::U => Self::U,
            KeyCode::V => Self::V,
            KeyCode::W => Self::W,
            KeyCode::X => Self::X,
            KeyCode::Y => Self::Y,
            KeyCode::Z => Self::Z,
            KeyCode::Digit0 => Self::Digit0,
            KeyCode::Digit1 => Self::Digit1,
            KeyCode::Digit2 => Self::Digit2,
            KeyCode::Digit3 => Self::Digit3,
            KeyCode::Digit4 => Self::Digit4,
            KeyCode::Digit5 => Self::Digit5,
            KeyCode::Digit6 => Self::Digit6,
            KeyCode::Digit7 => Self::Digit7,
            KeyCode::Digit8 => Self::Digit8,
            KeyCode::Digit9 => Self::Digit9,
            KeyCode::F1 => Self::F1,
            KeyCode::F2 => Self::F2,
            KeyCode::F3 => Self::F3,
            KeyCode::F4 => Self::F4,
            KeyCode::F5 => Self::F5,
            KeyCode::F6 => Self::F6,
            KeyCode::F7 => Self::F7,
            KeyCode::F8 => Self::F8,
            KeyCode::F9 => Self::F9,
            KeyCode::F10 => Self::F10,
            KeyCode::F11 => Self::F11,
            KeyCode::F12 => Self::F12,
            KeyCode::Escape => Self::Escape,
            KeyCode::Tab => Self::Tab,
            KeyCode::Enter => Self::Enter,
            KeyCode::Space => Self::Space,
            KeyCode::Backspace => Self::Backspace,
            KeyCode::Delete => Self::Delete,
            KeyCode::Insert => Self::Insert,
            KeyCode::Home => Self::Home,
            KeyCode::End => Self::End,
            KeyCode::PageUp => Self::PageUp,
            KeyCode::PageDown => Self::PageDown,
            KeyCode::Left => Self::Left,
            KeyCode::Right => Self::Right,
            KeyCode::Up => Self::Up,
            KeyCode::Down => Self::Down,
            KeyCode::LeftShift
            | KeyCode::RightShift
            | KeyCode::LeftCtrl
            | KeyCode::RightCtrl
            | KeyCode::LeftAlt
            | KeyCode::RightAlt
            | KeyCode::LeftMeta
            | KeyCode::RightMeta => return None,
        })
    }

    /// Parse a stable serialized key name from preferences/action payloads.
    pub fn from_preference_name(name: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
    }

    /// Stable serialized key name used in preferences/action payloads.
    pub fn preference_name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::M => "M",
            Self::N => "N",
            Self::O => "O",
            Self::P => "P",
            Self::Q => "Q",
            Self::R => "R",
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
            Self::Digit0 => "Digit0",
            Self::Digit1 => "Digit1",
            Self::Digit2 => "Digit2",
            Self::Digit3 => "Digit3",
            Self::Digit4 => "Digit4",
            Self::Digit5 => "Digit5",
            Self::Digit6 => "Digit6",
            Self::Digit7 => "Digit7",
            Self::Digit8 => "Digit8",
            Self::Digit9 => "Digit9",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::Escape => "Escape",
            Self::Tab => "Tab",
            Self::Enter => "Enter",
            Self::Space => "Space",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Insert => "Insert",
            Self::Home => "Home",
            Self::End => "End",
            Self::PageUp => "PageUp",
            Self::PageDown => "PageDown",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Up => "Up",
            Self::Down => "Down",
        }
    }

    fn to_core(self) -> KeyCode {
        match self {
            Self::A => KeyCode::A,
            Self::B => KeyCode::B,
            Self::C => KeyCode::C,
            Self::D => KeyCode::D,
            Self::E => KeyCode::E,
            Self::F => KeyCode::F,
            Self::G => KeyCode::G,
            Self::H => KeyCode::H,
            Self::I => KeyCode::I,
            Self::J => KeyCode::J,
            Self::K => KeyCode::K,
            Self::L => KeyCode::L,
            Self::M => KeyCode::M,
            Self::N => KeyCode::N,
            Self::O => KeyCode::O,
            Self::P => KeyCode::P,
            Self::Q => KeyCode::Q,
            Self::R => KeyCode::R,
            Self::S => KeyCode::S,
            Self::T => KeyCode::T,
            Self::U => KeyCode::U,
            Self::V => KeyCode::V,
            Self::W => KeyCode::W,
            Self::X => KeyCode::X,
            Self::Y => KeyCode::Y,
            Self::Z => KeyCode::Z,
            Self::Digit0 => KeyCode::Digit0,
            Self::Digit1 => KeyCode::Digit1,
            Self::Digit2 => KeyCode::Digit2,
            Self::Digit3 => KeyCode::Digit3,
            Self::Digit4 => KeyCode::Digit4,
            Self::Digit5 => KeyCode::Digit5,
            Self::Digit6 => KeyCode::Digit6,
            Self::Digit7 => KeyCode::Digit7,
            Self::Digit8 => KeyCode::Digit8,
            Self::Digit9 => KeyCode::Digit9,
            Self::F1 => KeyCode::F1,
            Self::F2 => KeyCode::F2,
            Self::F3 => KeyCode::F3,
            Self::F4 => KeyCode::F4,
            Self::F5 => KeyCode::F5,
            Self::F6 => KeyCode::F6,
            Self::F7 => KeyCode::F7,
            Self::F8 => KeyCode::F8,
            Self::F9 => KeyCode::F9,
            Self::F10 => KeyCode::F10,
            Self::F11 => KeyCode::F11,
            Self::F12 => KeyCode::F12,
            Self::Escape => KeyCode::Escape,
            Self::Tab => KeyCode::Tab,
            Self::Enter => KeyCode::Enter,
            Self::Space => KeyCode::Space,
            Self::Backspace => KeyCode::Backspace,
            Self::Delete => KeyCode::Delete,
            Self::Insert => KeyCode::Insert,
            Self::Home => KeyCode::Home,
            Self::End => KeyCode::End,
            Self::PageUp => KeyCode::PageUp,
            Self::PageDown => KeyCode::PageDown,
            Self::Left => KeyCode::Left,
            Self::Right => KeyCode::Right,
            Self::Up => KeyCode::Up,
            Self::Down => KeyCode::Down,
        }
    }

    fn label_for_platform(self, macos: bool) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
            Self::F => "F",
            Self::G => "G",
            Self::H => "H",
            Self::I => "I",
            Self::J => "J",
            Self::K => "K",
            Self::L => "L",
            Self::M => "M",
            Self::N => "N",
            Self::O => "O",
            Self::P => "P",
            Self::Q => "Q",
            Self::R => "R",
            Self::S => "S",
            Self::T => "T",
            Self::U => "U",
            Self::V => "V",
            Self::W => "W",
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
            Self::Digit0 => "0",
            Self::Digit1 => "1",
            Self::Digit2 => "2",
            Self::Digit3 => "3",
            Self::Digit4 => "4",
            Self::Digit5 => "5",
            Self::Digit6 => "6",
            Self::Digit7 => "7",
            Self::Digit8 => "8",
            Self::Digit9 => "9",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::F5 => "F5",
            Self::F6 => "F6",
            Self::F7 => "F7",
            Self::F8 => "F8",
            Self::F9 => "F9",
            Self::F10 => "F10",
            Self::F11 => "F11",
            Self::F12 => "F12",
            Self::Escape if macos => "⎋",
            Self::Escape => "Esc",
            Self::Tab if macos => "⇥",
            Self::Tab => "Tab",
            Self::Enter if macos => "⏎",
            Self::Enter => "Enter",
            Self::Space => "Space",
            Self::Backspace if macos => "⌫",
            Self::Backspace => "Backspace",
            Self::Delete => "Delete",
            Self::Insert => "Insert",
            Self::Home => "Home",
            Self::End => "End",
            Self::PageUp => "Page Up",
            Self::PageDown => "Page Down",
            Self::Left if macos => "◀",
            Self::Left => "Left",
            Self::Right if macos => "▶",
            Self::Right => "Right",
            Self::Up if macos => "▲",
            Self::Up => "Up",
            Self::Down if macos => "▼",
            Self::Down => "Down",
        }
    }
}

/// Default shortcut descriptors used by both the router and menu hints.
pub fn default_shortcuts() -> Vec<AppUiShortcut> {
    default_commands()
        .into_iter()
        .filter_map(|command| {
            command
                .default_shortcut
                .clone()
                .map(|binding| shortcut(command.id, binding, command.action()))
        })
        .collect()
}

/// Resolve the active shortcut table after applying user overrides.
pub fn active_shortcuts(overrides: &[AppUiShortcutOverride]) -> Vec<AppUiShortcut> {
    let mut active: Vec<(AppUiShortcut, bool)> = Vec::new();

    for mut shortcut in default_shortcuts() {
        let mut overridden = false;
        if let Some(override_entry) = overrides.iter().find(|entry| entry.id == shortcut.id) {
            let Some(binding) = override_entry.binding else {
                continue;
            };
            shortcut.binding = binding.to_core();
            shortcut.label = binding.label();
            overridden = true;
        }

        if let Some(existing_index) =
            active.iter().position(|(existing, _)| existing.binding == shortcut.binding)
        {
            let existing_overridden = active[existing_index].1;
            if overridden || !existing_overridden {
                active[existing_index] = (shortcut, overridden);
            }
        } else {
            active.push((shortcut, overridden));
        }
    }

    active.into_iter().map(|(shortcut, _)| shortcut).collect()
}

/// Shortcut hint shown for an action in app UI menus.
pub fn shortcut_label_for_action(action: &Action) -> Option<String> {
    shortcut_label_for_action_with_overrides(action, &[])
}

/// Shortcut hint shown for an action after applying user overrides.
pub fn shortcut_label_for_action_with_overrides(
    action: &Action,
    overrides: &[AppUiShortcutOverride],
) -> Option<String> {
    let lookup_action = shortcut_hint_action(action);
    active_shortcuts(overrides)
        .into_iter()
        .find(|shortcut| shortcut.action == lookup_action)
        .map(|shortcut| shortcut.label)
}

fn shortcut_hint_action(action: &Action) -> Action {
    match action {
        Action::TogglePanel(panel) => Action::FocusPanel(*panel),
        action => action.clone(),
    }
}

fn shortcut(id: &'static str, binding: ShortcutBinding, action: Action) -> AppUiShortcut {
    let label = AppUiShortcutBinding::from_core(&binding)
        .map(AppUiShortcutBinding::label)
        .unwrap_or_default();
    AppUiShortcut { id, binding, action, label }
}

/// Register the default global shortcuts for an app UI editor window.
pub fn register_default_shortcuts(router: &mut EventRouter) {
    register_shortcuts(router, &[]);
}

/// Register the active global shortcuts for an app UI editor window.
pub fn register_shortcuts(router: &mut EventRouter, overrides: &[AppUiShortcutOverride]) {
    for shortcut in active_shortcuts(overrides) {
        register(router, shortcut.binding, shortcut.action);
    }
}

/// Whether a shortcut preference id belongs to the current descriptor table.
pub fn is_known_shortcut_id(id: &str) -> bool {
    default_shortcuts().iter().any(|shortcut| shortcut.id == id)
}

fn register(router: &mut EventRouter, binding: ShortcutBinding, action: Action) {
    router.shortcut_manager_mut().register_global(binding, action);
}

#[cfg(test)]
fn ctrl_alt() -> Modifiers {
    Modifiers { ctrl: true, alt: true, ..Modifiers::none() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::app_shell_open_project_dialog_action;
    use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
    use mondrian_ui_core::shortcut::{ShortcutContext, ShortcutManager};

    #[test]
    fn default_shortcuts_cover_file_commands_and_workspace_switching() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());

        register_default_shortcuts(&mut router);

        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::SaveProject)
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::F11,
                Modifiers::none(),
                ShortcutContext::default()
            ),
            Some(Action::ToggleFullscreen)
        );
        let quit_action = router
            .shortcut_manager()
            .resolve(KeyCode::Q, Modifiers::ctrl(), ShortcutContext::default())
            .expect("quit shortcut");
        match quit_action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, crate::app::ui_actions::APP_SHELL_NAMESPACE);
                assert_eq!(name, crate::app::ui_actions::APP_SHELL_QUIT);
                assert!(payload.is_null());
            }
            other => panic!("expected app-shell quit action, got {other:?}"),
        }
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::Digit4,
                ctrl_alt(),
                ShortcutContext::default()
            ),
            Some(Action::SwitchWorkspace(WorkspacePreset::Compositing))
        );
        for (key, panel) in [
            (KeyCode::V, PanelKind::Viewer),
            (KeyCode::T, PanelKind::Timeline),
            (KeyCode::I, PanelKind::Inspector),
            (KeyCode::A, PanelKind::Assets),
            (KeyCode::E, PanelKind::Effects),
            (KeyCode::G, PanelKind::NodeGraph),
            (KeyCode::X, PanelKind::Export),
        ] {
            assert_eq!(
                router.shortcut_manager().resolve(key, ctrl_alt(), ShortcutContext::default()),
                Some(Action::FocusPanel(panel))
            );
        }
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::C,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::Copy)
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::X,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::Cut)
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::V,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::Paste)
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::D,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::Duplicate)
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::W,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::CloseProject)
        );
    }

    #[test]
    fn default_shortcuts_cover_core_timeline_editing_commands() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());

        register_default_shortcuts(&mut router);

        for (key, modifiers, action) in [
            (KeyCode::Delete, Modifiers::none(), Action::DeleteSelection),
            (
                KeyCode::Delete,
                Modifiers::shift(),
                Action::RippleDeleteSelection,
            ),
            (KeyCode::K, Modifiers::ctrl(), Action::SplitClipAtPlayhead),
            (KeyCode::I, Modifiers::none(), Action::MarkInAtPlayhead),
            (KeyCode::O, Modifiers::none(), Action::MarkOutAtPlayhead),
            (KeyCode::Home, Modifiers::none(), Action::GoToStart),
            (KeyCode::End, Modifiers::none(), Action::GoToEnd),
            (KeyCode::Left, Modifiers::none(), Action::StepBack),
            (KeyCode::Right, Modifiers::none(), Action::StepForward),
            (KeyCode::A, Modifiers::ctrl(), Action::SelectAll),
            (KeyCode::Escape, Modifiers::none(), Action::DeselectAll),
        ] {
            assert_eq!(
                router.shortcut_manager().resolve(key, modifiers, ShortcutContext::default()),
                Some(action)
            );
        }
    }

    #[test]
    fn shortcut_labels_share_the_default_descriptor_table() {
        assert_eq!(
            shortcut_label_for_action(&Action::SaveProject),
            Some("Ctrl+S".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::Copy),
            Some("Ctrl+C".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::Duplicate),
            Some("Ctrl+D".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::SwitchWorkspace(WorkspacePreset::Audio)),
            Some("Ctrl+Alt+3".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::FocusPanel(PanelKind::Inspector)),
            Some("Ctrl+Alt+I".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::SplitClipAtPlayhead),
            Some("Ctrl+K".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::RippleDeleteSelection),
            Some("Shift+Delete".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action(&Action::MarkInAtPlayhead),
            Some("I".to_owned())
        );
    }

    #[test]
    fn default_shortcuts_do_not_bind_plain_space_for_text_input_safety() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());

        register_default_shortcuts(&mut router);

        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::Space,
                Modifiers::none(),
                ShortcutContext::default()
            ),
            None
        );
    }

    #[test]
    fn shortcut_overrides_can_disable_conflicting_global_bindings() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());
        let overrides =
            vec![AppUiShortcutOverride { id: "panel.inspector".to_owned(), binding: None }];

        register_shortcuts(&mut router, &overrides);

        assert_eq!(
            router
                .shortcut_manager()
                .resolve(KeyCode::I, ctrl_alt(), ShortcutContext::default()),
            None
        );
        assert_eq!(
            shortcut_label_for_action_with_overrides(
                &Action::FocusPanel(PanelKind::Inspector),
                &overrides
            ),
            None
        );
    }

    #[test]
    fn shortcut_overrides_rebind_registered_key_and_label() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());
        let replacement = AppUiShortcutBinding {
            key: AppUiShortcutKey::S,
            ctrl: true,
            alt: true,
            shift: false,
            meta: false,
        };
        let overrides = vec![AppUiShortcutOverride {
            id: "file.save_project".to_owned(),
            binding: Some(replacement),
        }];

        register_shortcuts(&mut router, &overrides);

        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            None
        );
        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::S,
                Modifiers { ctrl: true, alt: true, ..Modifiers::none() },
                ShortcutContext::default()
            ),
            Some(Action::SaveProject)
        );
        assert_eq!(
            shortcut_label_for_action_with_overrides(&Action::SaveProject, &overrides),
            Some("Ctrl+Alt+S".to_owned())
        );
    }

    #[test]
    fn shortcut_binding_labels_use_platform_conventions() {
        let binding = AppUiShortcutBinding {
            key: AppUiShortcutKey::Enter,
            ctrl: true,
            alt: true,
            shift: true,
            meta: true,
        };

        assert_eq!(
            binding.label_for_platform(false),
            "Ctrl+Alt+Shift+Meta+Enter"
        );
        assert_eq!(binding.label_for_platform(true), "⌃⌥⇧⌘⏎");
        assert_eq!(AppUiShortcutKey::Escape.label_for_platform(true), "⎋");
        assert_eq!(AppUiShortcutKey::Tab.label_for_platform(true), "⇥");
        assert_eq!(AppUiShortcutKey::Backspace.label_for_platform(true), "⌫");
        assert_eq!(AppUiShortcutKey::Up.label_for_platform(true), "▲");
        assert_eq!(AppUiShortcutKey::Down.label_for_platform(true), "▼");
        assert_eq!(AppUiShortcutKey::Left.label_for_platform(true), "◀");
        assert_eq!(AppUiShortcutKey::Right.label_for_platform(true), "▶");
    }

    #[test]
    fn shortcut_overrides_take_ownership_of_conflicting_default_bindings() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());
        let replacement = AppUiShortcutBinding {
            key: AppUiShortcutKey::O,
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        };
        let overrides = vec![AppUiShortcutOverride {
            id: "file.save_project".to_owned(),
            binding: Some(replacement),
        }];

        register_shortcuts(&mut router, &overrides);

        assert_eq!(
            router.shortcut_manager().resolve(
                KeyCode::O,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::SaveProject)
        );
        assert_eq!(
            shortcut_label_for_action_with_overrides(&Action::SaveProject, &overrides),
            Some("Ctrl+O".to_owned())
        );
        assert_eq!(
            shortcut_label_for_action_with_overrides(
                &app_shell_open_project_dialog_action(),
                &overrides
            ),
            None
        );
    }
}
