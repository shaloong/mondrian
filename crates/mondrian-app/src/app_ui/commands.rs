//! App UI command registry.
//!
//! Commands are the presentation-facing layer above [`Action`]. Menus,
//! shortcuts, command palette entries, and future plugin entry points should
//! read ids, labels, default bindings, and action factories from this module
//! instead of each keeping a private command table.

use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_ui_core::shortcut::ShortcutBinding;
use mondrian_ui_core::types::{KeyCode, Modifiers};

use crate::app::ui_actions::{
    app_shell_about_action, app_shell_import_media_dialog_action,
    app_shell_new_project_dialog_action, app_shell_open_project_dialog_action,
    app_shell_preferences_action, app_shell_project_settings_action, app_shell_quit_action,
    app_shell_save_project_as_dialog_action, timeline_create_basic_title_action,
};

/// Stable command descriptor consumed by menus, shortcuts, and preferences.
#[derive(Debug, Clone)]
pub struct AppUiCommandDescriptor {
    /// Stable id used by user preferences and future plugin APIs.
    pub id: &'static str,
    /// Short user-facing label for command pickers and shortcut preferences.
    pub title: &'static str,
    /// Menu-facing label. This may include ellipses where the command opens UI.
    pub menu_title: &'static str,
    /// Product category for grouping in menus and preferences.
    pub category: AppUiCommandCategory,
    /// Built-in key binding before user overrides are applied.
    pub default_shortcut: Option<ShortcutBinding>,
    action: fn() -> Action,
}

impl AppUiCommandDescriptor {
    /// Build the executable action for this command.
    pub fn action(&self) -> Action {
        (self.action)()
    }
}

/// Product-level command grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppUiCommandCategory {
    /// Project lifecycle and media import/export commands.
    File,
    /// Editing operations that act on current selection or clipboard state.
    Edit,
    /// Viewer and display commands.
    View,
    /// Panel focus and visibility commands.
    Window,
    /// Workspace preset commands.
    Workspace,
    /// Timeline editing commands.
    Timeline,
    /// Playback and playhead navigation commands.
    Transport,
    /// Product help and metadata commands.
    Help,
}

/// Return the descriptor table for built-in app UI commands.
pub fn default_commands() -> Vec<AppUiCommandDescriptor> {
    vec![
        command(
            "file.new_project",
            "新建项目",
            "新建项目...",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl(KeyCode::N)),
            action_new_project,
        ),
        command(
            "file.open_project",
            "打开项目",
            "打开项目...",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl(KeyCode::O)),
            action_open_project,
        ),
        command(
            "file.import_media",
            "导入媒体",
            "媒体...",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl(KeyCode::I)),
            action_import_media,
        ),
        command(
            "file.save_project",
            "保存",
            "保存",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl(KeyCode::S)),
            action_save_project,
        ),
        command(
            "file.save_project_as",
            "另存为",
            "另存为...",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl_shift(KeyCode::S)),
            action_save_project_as,
        ),
        command(
            "file.project_settings",
            "项目设置",
            "项目设置...",
            AppUiCommandCategory::File,
            None,
            action_project_settings,
        ),
        command(
            "file.close_project",
            "关闭项目",
            "关闭项目",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl(KeyCode::W)),
            action_close_project,
        ),
        command(
            "app.quit",
            "退出",
            "退出",
            AppUiCommandCategory::File,
            Some(ShortcutBinding::ctrl(KeyCode::Q)),
            action_quit,
        ),
        command(
            "edit.undo",
            "撤销",
            "撤销",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl(KeyCode::Z)),
            action_undo,
        ),
        command(
            "edit.redo",
            "重做",
            "重做",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl_shift(KeyCode::Z)),
            action_redo,
        ),
        command(
            "edit.cut",
            "剪切",
            "剪切",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl(KeyCode::X)),
            action_cut,
        ),
        command(
            "edit.copy",
            "复制",
            "复制",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl(KeyCode::C)),
            action_copy,
        ),
        command(
            "edit.paste",
            "粘贴",
            "粘贴",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl(KeyCode::V)),
            action_paste,
        ),
        command(
            "edit.duplicate",
            "创建副本",
            "创建副本",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl(KeyCode::D)),
            action_duplicate,
        ),
        command(
            "edit.delete_selection",
            "删除所选",
            "删除所选",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::new(KeyCode::Delete, Modifiers::none())),
            action_delete_selection,
        ),
        command(
            "edit.ripple_delete_selection",
            "波纹删除",
            "波纹删除",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::new(KeyCode::Delete, Modifiers::shift())),
            action_ripple_delete_selection,
        ),
        command(
            "edit.select_all",
            "全选",
            "全选",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::ctrl(KeyCode::A)),
            action_select_all,
        ),
        command(
            "edit.deselect_all",
            "取消选择",
            "取消选择",
            AppUiCommandCategory::Edit,
            Some(ShortcutBinding::new(KeyCode::Escape, Modifiers::none())),
            action_deselect_all,
        ),
        command(
            "app.preferences",
            "偏好设置",
            "偏好设置...",
            AppUiCommandCategory::Edit,
            None,
            action_preferences,
        ),
        command(
            "view.toggle_fullscreen",
            "切换全屏",
            "切换全屏",
            AppUiCommandCategory::View,
            Some(ShortcutBinding::new(KeyCode::F11, Modifiers::none())),
            action_toggle_fullscreen,
        ),
        command(
            "timeline.split_at_playhead",
            "在播放头处分割",
            "在播放头处分割",
            AppUiCommandCategory::Timeline,
            Some(ShortcutBinding::ctrl(KeyCode::K)),
            action_split_at_playhead,
        ),
        command(
            "timeline.create_basic_title",
            "基础标题",
            "基础标题",
            AppUiCommandCategory::Timeline,
            None,
            action_create_basic_title,
        ),
        command(
            "timeline.mark_in",
            "标记入点",
            "标记入点",
            AppUiCommandCategory::Timeline,
            Some(ShortcutBinding::new(KeyCode::I, Modifiers::none())),
            action_mark_in,
        ),
        command(
            "timeline.mark_out",
            "标记出点",
            "标记出点",
            AppUiCommandCategory::Timeline,
            Some(ShortcutBinding::new(KeyCode::O, Modifiers::none())),
            action_mark_out,
        ),
        command(
            "transport.go_to_start",
            "跳到开始",
            "跳到开始",
            AppUiCommandCategory::Transport,
            Some(ShortcutBinding::new(KeyCode::Home, Modifiers::none())),
            action_go_to_start,
        ),
        command(
            "transport.go_to_end",
            "跳到结尾",
            "跳到结尾",
            AppUiCommandCategory::Transport,
            Some(ShortcutBinding::new(KeyCode::End, Modifiers::none())),
            action_go_to_end,
        ),
        command(
            "transport.step_back",
            "后退一帧",
            "后退一帧",
            AppUiCommandCategory::Transport,
            Some(ShortcutBinding::new(KeyCode::Left, Modifiers::none())),
            action_step_back,
        ),
        command(
            "transport.step_forward",
            "前进一帧",
            "前进一帧",
            AppUiCommandCategory::Transport,
            Some(ShortcutBinding::new(KeyCode::Right, Modifiers::none())),
            action_step_forward,
        ),
        command(
            "workspace.editing",
            "编辑工作区",
            "编辑",
            AppUiCommandCategory::Workspace,
            Some(ShortcutBinding::new(KeyCode::Digit1, ctrl_alt())),
            action_workspace_editing,
        ),
        command(
            "workspace.color",
            "调色工作区",
            "调色",
            AppUiCommandCategory::Workspace,
            Some(ShortcutBinding::new(KeyCode::Digit2, ctrl_alt())),
            action_workspace_color,
        ),
        command(
            "workspace.audio",
            "音频工作区",
            "音频",
            AppUiCommandCategory::Workspace,
            Some(ShortcutBinding::new(KeyCode::Digit3, ctrl_alt())),
            action_workspace_audio,
        ),
        command(
            "workspace.compositing",
            "合成工作区",
            "合成",
            AppUiCommandCategory::Workspace,
            Some(ShortcutBinding::new(KeyCode::Digit4, ctrl_alt())),
            action_workspace_compositing,
        ),
        command(
            "workspace.export",
            "导出工作区",
            "导出",
            AppUiCommandCategory::Workspace,
            Some(ShortcutBinding::new(KeyCode::Digit5, ctrl_alt())),
            action_workspace_export,
        ),
        command(
            "panel.assets",
            "资源面板",
            "资源",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::A, ctrl_alt())),
            action_focus_assets,
        ),
        command(
            "panel.viewer",
            "查看器面板",
            "查看器",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::V, ctrl_alt())),
            action_focus_viewer,
        ),
        command(
            "panel.timeline",
            "时间线面板",
            "时间线",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::T, ctrl_alt())),
            action_focus_timeline,
        ),
        command(
            "panel.inspector",
            "检查器面板",
            "检查器",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::I, ctrl_alt())),
            action_focus_inspector,
        ),
        command(
            "panel.effects",
            "效果面板",
            "效果",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::E, ctrl_alt())),
            action_focus_effects,
        ),
        command(
            "panel.node_graph",
            "节点图面板",
            "节点图",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::G, ctrl_alt())),
            action_focus_node_graph,
        ),
        command(
            "panel.export",
            "导出面板",
            "导出",
            AppUiCommandCategory::Window,
            Some(ShortcutBinding::new(KeyCode::X, ctrl_alt())),
            action_focus_export,
        ),
        command(
            "app.about",
            "关于",
            "关于",
            AppUiCommandCategory::Help,
            None,
            action_about,
        ),
    ]
}

/// Find one built-in command by stable id.
pub fn command_by_id(id: &str) -> Option<AppUiCommandDescriptor> {
    default_commands().into_iter().find(|command| command.id == id)
}

/// Find the first command whose action matches `action`.
pub fn command_for_action(action: &Action) -> Option<AppUiCommandDescriptor> {
    default_commands().into_iter().find(|command| command.action() == *action)
}

fn command(
    id: &'static str,
    title: &'static str,
    menu_title: &'static str,
    category: AppUiCommandCategory,
    default_shortcut: Option<ShortcutBinding>,
    action: fn() -> Action,
) -> AppUiCommandDescriptor {
    AppUiCommandDescriptor {
        id,
        title,
        menu_title,
        category,
        default_shortcut,
        action,
    }
}

fn ctrl_alt() -> Modifiers {
    Modifiers { ctrl: true, alt: true, ..Modifiers::none() }
}

fn action_new_project() -> Action {
    app_shell_new_project_dialog_action()
}
fn action_open_project() -> Action {
    app_shell_open_project_dialog_action()
}
fn action_import_media() -> Action {
    app_shell_import_media_dialog_action()
}
fn action_create_basic_title() -> Action {
    timeline_create_basic_title_action()
}
fn action_save_project() -> Action {
    Action::SaveProject
}
fn action_save_project_as() -> Action {
    app_shell_save_project_as_dialog_action()
}
fn action_project_settings() -> Action {
    app_shell_project_settings_action()
}
fn action_close_project() -> Action {
    Action::CloseProject
}
fn action_quit() -> Action {
    app_shell_quit_action()
}
fn action_undo() -> Action {
    Action::Undo
}
fn action_redo() -> Action {
    Action::Redo
}
fn action_cut() -> Action {
    Action::Cut
}
fn action_copy() -> Action {
    Action::Copy
}
fn action_paste() -> Action {
    Action::Paste
}
fn action_duplicate() -> Action {
    Action::Duplicate
}
fn action_delete_selection() -> Action {
    Action::DeleteSelection
}
fn action_ripple_delete_selection() -> Action {
    Action::RippleDeleteSelection
}
fn action_select_all() -> Action {
    Action::SelectAll
}
fn action_deselect_all() -> Action {
    Action::DeselectAll
}
fn action_preferences() -> Action {
    app_shell_preferences_action()
}
fn action_toggle_fullscreen() -> Action {
    Action::ToggleFullscreen
}
fn action_split_at_playhead() -> Action {
    Action::SplitClipAtPlayhead
}
fn action_mark_in() -> Action {
    Action::MarkInAtPlayhead
}
fn action_mark_out() -> Action {
    Action::MarkOutAtPlayhead
}
fn action_go_to_start() -> Action {
    Action::GoToStart
}
fn action_go_to_end() -> Action {
    Action::GoToEnd
}
fn action_step_back() -> Action {
    Action::StepBack
}
fn action_step_forward() -> Action {
    Action::StepForward
}
fn action_workspace_editing() -> Action {
    Action::SwitchWorkspace(WorkspacePreset::Editing)
}
fn action_workspace_color() -> Action {
    Action::SwitchWorkspace(WorkspacePreset::Color)
}
fn action_workspace_audio() -> Action {
    Action::SwitchWorkspace(WorkspacePreset::Audio)
}
fn action_workspace_compositing() -> Action {
    Action::SwitchWorkspace(WorkspacePreset::Compositing)
}
fn action_workspace_export() -> Action {
    Action::SwitchWorkspace(WorkspacePreset::Export)
}
fn action_focus_assets() -> Action {
    Action::FocusPanel(PanelKind::Assets)
}
fn action_focus_viewer() -> Action {
    Action::FocusPanel(PanelKind::Viewer)
}
fn action_focus_timeline() -> Action {
    Action::FocusPanel(PanelKind::Timeline)
}
fn action_focus_inspector() -> Action {
    Action::FocusPanel(PanelKind::Inspector)
}
fn action_focus_effects() -> Action {
    Action::FocusPanel(PanelKind::Effects)
}
fn action_focus_node_graph() -> Action {
    Action::FocusPanel(PanelKind::NodeGraph)
}
fn action_focus_export() -> Action {
    Action::FocusPanel(PanelKind::Export)
}
fn action_about() -> Action {
    app_shell_about_action()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn command_ids_are_unique() {
        let mut ids = BTreeSet::new();
        for command in default_commands() {
            assert!(
                ids.insert(command.id),
                "duplicate command id {}",
                command.id
            );
        }
    }

    #[test]
    fn built_in_shortcut_commands_are_registry_backed() {
        let commands = default_commands();
        assert!(commands.iter().any(|command| {
            command.id == "file.save_project"
                && command.default_shortcut.as_ref() == Some(&ShortcutBinding::ctrl(KeyCode::S))
                && command.action() == Action::SaveProject
        }));
        assert!(commands.iter().any(|command| {
            command.id == "panel.inspector"
                && command.default_shortcut.as_ref()
                    == Some(&ShortcutBinding::new(KeyCode::I, ctrl_alt()))
                && command.action() == Action::FocusPanel(PanelKind::Inspector)
        }));
    }

    #[test]
    fn action_lookup_accepts_toggle_panel_shortcut_aliases() {
        assert_eq!(
            command_for_action(&Action::FocusPanel(PanelKind::Inspector)).map(|command| command.id),
            Some("panel.inspector")
        );
    }
}
