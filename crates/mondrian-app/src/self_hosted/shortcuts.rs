//! Default keyboard shortcuts for the self-hosted shell.
//!
//! The router resolves these only after the focused widget ignores a `KeyDown`,
//! so text inputs and panel-specific key handling keep priority.

use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_ui_core::shortcut::ShortcutBinding;
use mondrian_ui_core::types::{KeyCode, Modifiers};
use mondrian_ui_events::EventRouter;

use crate::app::ui_actions::{
    app_shell_import_media_dialog_action, app_shell_new_project_dialog_action,
    app_shell_open_project_dialog_action, app_shell_quit_action,
    app_shell_save_project_as_dialog_action,
};

/// A self-hosted shell shortcut together with its menu-facing display label.
#[derive(Debug, Clone)]
pub struct SelfHostedShortcut {
    /// Key/modifier binding registered with the shortcut router.
    pub binding: ShortcutBinding,
    /// Editor or shell action dispatched by the binding.
    pub action: Action,
    /// Menu-facing shortcut label.
    pub label: &'static str,
}

/// Default shortcut descriptors used by both the router and menu hints.
pub fn default_shortcuts() -> Vec<SelfHostedShortcut> {
    vec![
        shortcut(
            ShortcutBinding::ctrl(KeyCode::N),
            app_shell_new_project_dialog_action(),
            "Ctrl+N",
        ),
        shortcut(
            ShortcutBinding::ctrl(KeyCode::O),
            app_shell_open_project_dialog_action(),
            "Ctrl+O",
        ),
        shortcut(
            ShortcutBinding::ctrl(KeyCode::I),
            app_shell_import_media_dialog_action(),
            "Ctrl+I",
        ),
        shortcut(
            ShortcutBinding::ctrl(KeyCode::S),
            Action::SaveProject,
            "Ctrl+S",
        ),
        shortcut(
            ShortcutBinding::ctrl_shift(KeyCode::S),
            app_shell_save_project_as_dialog_action(),
            "Ctrl+Shift+S",
        ),
        shortcut(
            ShortcutBinding::ctrl(KeyCode::Q),
            app_shell_quit_action(),
            "Ctrl+Q",
        ),
        shortcut(
            ShortcutBinding::ctrl(KeyCode::W),
            Action::CloseProject,
            "Ctrl+W",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::F11, Modifiers::none()),
            Action::ToggleFullscreen,
            "F11",
        ),
        shortcut(ShortcutBinding::ctrl(KeyCode::Z), Action::Undo, "Ctrl+Z"),
        shortcut(
            ShortcutBinding::ctrl_shift(KeyCode::Z),
            Action::Redo,
            "Ctrl+Shift+Z",
        ),
        shortcut(ShortcutBinding::ctrl(KeyCode::X), Action::Cut, "Ctrl+X"),
        shortcut(ShortcutBinding::ctrl(KeyCode::C), Action::Copy, "Ctrl+C"),
        shortcut(ShortcutBinding::ctrl(KeyCode::V), Action::Paste, "Ctrl+V"),
        shortcut(
            ShortcutBinding::new(KeyCode::Digit1, ctrl_alt()),
            Action::SwitchWorkspace(WorkspacePreset::Editing),
            "Ctrl+Alt+1",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::Digit2, ctrl_alt()),
            Action::SwitchWorkspace(WorkspacePreset::Color),
            "Ctrl+Alt+2",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::Digit3, ctrl_alt()),
            Action::SwitchWorkspace(WorkspacePreset::Audio),
            "Ctrl+Alt+3",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::Digit4, ctrl_alt()),
            Action::SwitchWorkspace(WorkspacePreset::Compositing),
            "Ctrl+Alt+4",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::Digit5, ctrl_alt()),
            Action::SwitchWorkspace(WorkspacePreset::Export),
            "Ctrl+Alt+5",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::V, ctrl_alt()),
            Action::FocusPanel(PanelKind::Viewer),
            "Ctrl+Alt+V",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::T, ctrl_alt()),
            Action::FocusPanel(PanelKind::Timeline),
            "Ctrl+Alt+T",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::A, ctrl_alt()),
            Action::FocusPanel(PanelKind::Assets),
            "Ctrl+Alt+A",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::E, ctrl_alt()),
            Action::FocusPanel(PanelKind::Effects),
            "Ctrl+Alt+E",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::G, ctrl_alt()),
            Action::FocusPanel(PanelKind::NodeGraph),
            "Ctrl+Alt+G",
        ),
        shortcut(
            ShortcutBinding::new(KeyCode::X, ctrl_alt()),
            Action::FocusPanel(PanelKind::Export),
            "Ctrl+Alt+X",
        ),
    ]
}

/// Shortcut hint shown for an action in self-hosted menus.
pub fn shortcut_label_for_action(action: &Action) -> Option<&'static str> {
    default_shortcuts()
        .into_iter()
        .find(|shortcut| shortcut.action == *action)
        .map(|shortcut| shortcut.label)
}

fn shortcut(binding: ShortcutBinding, action: Action, label: &'static str) -> SelfHostedShortcut {
    SelfHostedShortcut { binding, action, label }
}

/// Register the default global shortcuts for a self-hosted editor window.
pub fn register_default_shortcuts(router: &mut EventRouter) {
    for shortcut in default_shortcuts() {
        register(router, shortcut.binding, shortcut.action);
    }
}

fn register(router: &mut EventRouter, binding: ShortcutBinding, action: Action) {
    router.shortcut_manager_mut().register_global(binding, action);
}

fn ctrl_alt() -> Modifiers {
    Modifiers { ctrl: true, alt: true, ..Modifiers::none() }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(
            router
                .shortcut_manager()
                .resolve(KeyCode::T, ctrl_alt(), ShortcutContext::default()),
            Some(Action::FocusPanel(PanelKind::Timeline))
        );
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
                KeyCode::W,
                Modifiers::ctrl(),
                ShortcutContext::default()
            ),
            Some(Action::CloseProject)
        );
    }

    #[test]
    fn shortcut_labels_share_the_default_descriptor_table() {
        assert_eq!(
            shortcut_label_for_action(&Action::SaveProject),
            Some("Ctrl+S")
        );
        assert_eq!(shortcut_label_for_action(&Action::Copy), Some("Ctrl+C"));
        assert_eq!(
            shortcut_label_for_action(&Action::SwitchWorkspace(WorkspacePreset::Audio)),
            Some("Ctrl+Alt+3")
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
}
