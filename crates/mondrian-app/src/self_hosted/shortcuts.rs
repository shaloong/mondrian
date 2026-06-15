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
    app_shell_open_project_dialog_action, app_shell_save_project_as_dialog_action,
};

/// Register the default global shortcuts for a self-hosted editor window.
pub fn register_default_shortcuts(router: &mut EventRouter) {
    register(
        router,
        ShortcutBinding::ctrl(KeyCode::N),
        app_shell_new_project_dialog_action(),
    );
    register(
        router,
        ShortcutBinding::ctrl(KeyCode::O),
        app_shell_open_project_dialog_action(),
    );
    register(
        router,
        ShortcutBinding::ctrl(KeyCode::I),
        app_shell_import_media_dialog_action(),
    );
    register(
        router,
        ShortcutBinding::ctrl(KeyCode::S),
        Action::SaveProject,
    );
    register(
        router,
        ShortcutBinding::ctrl_shift(KeyCode::S),
        app_shell_save_project_as_dialog_action(),
    );

    register(router, ShortcutBinding::ctrl(KeyCode::Z), Action::Undo);
    register(
        router,
        ShortcutBinding::ctrl_shift(KeyCode::Z),
        Action::Redo,
    );

    register(
        router,
        ShortcutBinding::new(KeyCode::Digit1, ctrl_alt()),
        Action::SwitchWorkspace(WorkspacePreset::Editing),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::Digit2, ctrl_alt()),
        Action::SwitchWorkspace(WorkspacePreset::Color),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::Digit3, ctrl_alt()),
        Action::SwitchWorkspace(WorkspacePreset::Audio),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::Digit4, ctrl_alt()),
        Action::SwitchWorkspace(WorkspacePreset::Compositing),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::Digit5, ctrl_alt()),
        Action::SwitchWorkspace(WorkspacePreset::Export),
    );

    register(
        router,
        ShortcutBinding::new(KeyCode::V, ctrl_alt()),
        Action::FocusPanel(PanelKind::Viewer),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::T, ctrl_alt()),
        Action::FocusPanel(PanelKind::Timeline),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::P, ctrl_alt()),
        Action::FocusPanel(PanelKind::Project),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::A, ctrl_alt()),
        Action::FocusPanel(PanelKind::Assets),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::E, ctrl_alt()),
        Action::FocusPanel(PanelKind::Effects),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::G, ctrl_alt()),
        Action::FocusPanel(PanelKind::NodeGraph),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::X, ctrl_alt()),
        Action::FocusPanel(PanelKind::Export),
    );
    register(
        router,
        ShortcutBinding::new(KeyCode::Backspace, ctrl_alt()),
        Action::FocusPanel(PanelKind::Console),
    );
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
    use mondrian_ui_core::shortcut::ShortcutManager;

    #[test]
    fn default_shortcuts_cover_file_commands_and_workspace_switching() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());

        register_default_shortcuts(&mut router);

        assert_eq!(
            router.shortcut_manager().resolve(KeyCode::S, Modifiers::ctrl()),
            Some(Action::SaveProject)
        );
        assert_eq!(
            router.shortcut_manager().resolve(KeyCode::Digit4, ctrl_alt()),
            Some(Action::SwitchWorkspace(WorkspacePreset::Compositing))
        );
        assert_eq!(
            router.shortcut_manager().resolve(KeyCode::T, ctrl_alt()),
            Some(Action::FocusPanel(PanelKind::Timeline))
        );
    }

    #[test]
    fn default_shortcuts_do_not_bind_plain_space_for_text_input_safety() {
        let mut router = EventRouter::new(mondrian_ui_core::types::WidgetId::new());

        register_default_shortcuts(&mut router);

        assert_eq!(
            router.shortcut_manager().resolve(KeyCode::Space, Modifiers::none()),
            None
        );
    }
}
