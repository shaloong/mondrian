//! Self-hosted UI host state.
//!
//! Window entrypoints own native event loops and rendering surfaces. This host
//! owns the reusable application/UI state bridge: root widget, `AppState`, and
//! refresh policy after widget-dispatched actions.

use std::cell::{Cell, Ref, RefCell};
use std::path::PathBuf;

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_panel_console::tracing_layer::LogBuffer;
use mondrian_platform::PlatformService;
use mondrian_ui_core::types::Rect;
use mondrian_ui_core::TreeWalker;
use mondrian_ui_theme::set_theme_preset;

use crate::app::ui_actions::{
    PreferencesThemePayload, APP_SHELL_NAMESPACE, APP_SHELL_PREFERENCES_THEME_CHANGED,
    APP_SHELL_QUIT,
};
use crate::app::AppState;
use crate::self_hosted::action_queue::PendingUiActions;
use crate::self_hosted::menu_bar::app_state_action_enabled;
use crate::self_hosted::preferences_store::{
    load_self_hosted_preferences, persist_self_hosted_preferences_to, self_hosted_preferences_path,
    SelfHostedPreferences,
};
use crate::self_hosted::shell::SelfHostedAppRoot;
use mondrian_editor_state::Action;

/// Window-host commands produced while draining self-hosted UI actions.
///
/// These are native shell side effects, not editor-state mutations. Entrypoints
/// apply them after the widget tree and `AppState` borrows have ended.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SelfHostedShellCommands {
    /// The native window should request application exit.
    pub quit: bool,
    /// The native window should toggle fullscreen mode.
    pub toggle_fullscreen: bool,
}

/// Product-facing self-hosted UI session state.
pub struct SelfHostedUiHost {
    root: SelfHostedAppRoot,
    app_state: RefCell<AppState>,
    preferences: SelfHostedPreferences,
    preferences_path: PathBuf,
    console_log_buffer: Option<LogBuffer>,
    ui_dirty: Cell<bool>,
}

impl SelfHostedUiHost {
    /// Create a host from an initial application state snapshot.
    pub fn new(app_state: AppState) -> Self {
        Self::new_with_preferences_path(
            app_state,
            load_self_hosted_preferences(),
            self_hosted_preferences_path(),
            None,
        )
    }

    /// Create a host that includes runtime tracing entries in the Console tab.
    pub fn new_with_console_log_buffer(app_state: AppState, console_log_buffer: LogBuffer) -> Self {
        Self::new_with_preferences_path(
            app_state,
            load_self_hosted_preferences(),
            self_hosted_preferences_path(),
            Some(console_log_buffer),
        )
    }

    /// Create a host from explicit preferences and path.
    pub(crate) fn new_with_preferences_path(
        app_state: AppState,
        preferences: SelfHostedPreferences,
        preferences_path: PathBuf,
        console_log_buffer: Option<LogBuffer>,
    ) -> Self {
        set_theme_preset(preferences.theme_preset);
        let root = SelfHostedAppRoot::from_app_state_with_preferences_and_runtime_logs(
            &app_state,
            &preferences,
            console_log_buffer.as_ref(),
        );
        Self {
            root,
            app_state: RefCell::new(app_state),
            preferences,
            preferences_path,
            console_log_buffer,
            ui_dirty: Cell::new(false),
        }
    }

    /// Immutable access to the root widget.
    pub fn root(&self) -> &SelfHostedAppRoot {
        &self.root
    }

    /// Mutable access to the root widget for event routing and layout.
    pub fn root_mut(&mut self) -> &mut SelfHostedAppRoot {
        &mut self.root
    }

    /// Read-only access to the current app state.
    pub fn app_state(&self) -> Ref<'_, AppState> {
        self.app_state.borrow()
    }

    /// Current persisted self-hosted preferences snapshot.
    pub fn preferences(&self) -> &SelfHostedPreferences {
        &self.preferences
    }

    /// Mark the root as needing a model refresh from `AppState`.
    pub fn mark_dirty(&self) {
        self.ui_dirty.set(true);
    }

    /// Refresh the root widget models when editor state changed.
    pub fn refresh_if_dirty(&mut self, bounds: Rect) {
        if !self.ui_dirty.replace(false) {
            return;
        }
        self.root.refresh_from_app_state_with_preferences_and_runtime_logs(
            &self.app_state.borrow(),
            &self.preferences,
            self.console_log_buffer.as_ref(),
        );
        TreeWalker::layout(&mut self.root, bounds);
    }

    /// Drain queued widget actions through shell-local handling and `AppState`.
    pub fn drain_pending_actions(
        &mut self,
        pending_actions: &PendingUiActions,
        bounds: Rect,
        platform: &dyn PlatformService,
    ) -> SelfHostedShellCommands {
        let mut commands = SelfHostedShellCommands::default();
        let actions = pending_actions.take_all();
        if actions.is_empty() {
            self.refresh_if_dirty(bounds);
            return commands;
        }

        let mut needs_layout = false;
        for action in actions {
            if take_shell_window_command(&mut commands, &action) {
                continue;
            }
            if self.take_preferences_update(&action, bounds) {
                needs_layout = true;
                continue;
            }
            if !self.is_action_enabled(&action) {
                tracing::debug!(?action, "disabled custom UI action ignored");
                continue;
            }

            let switched_workspace = match &action {
                Action::SwitchWorkspace(preset) => Some(*preset),
                _ => None,
            };
            let current_project_path = self.app_state.borrow().current_project_path.clone();
            let action = match self.root.try_handle_shell_action(
                action,
                platform,
                current_project_path.as_deref(),
            ) {
                Ok(Some(action)) => action,
                Ok(None) => {
                    if let Some(preset) = switched_workspace {
                        self.persist_workspace_preset(preset);
                    }
                    needs_layout = true;
                    continue;
                }
                Err(err) => {
                    tracing::warn!("custom UI shell action failed: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("UI shell action failed: {err}"), true);
                    self.mark_dirty();
                    needs_layout = true;
                    continue;
                }
            };

            if !self.is_action_enabled(&action) {
                tracing::debug!(?action, "disabled resolved custom UI action ignored");
                continue;
            }
            tracing::debug!(?action, "custom UI action");
            if let Err(err) = self.app_state.borrow_mut().dispatch_action(action) {
                tracing::warn!("custom UI action failed: {err}");
            }
            self.mark_dirty();
            needs_layout = true;
        }

        self.refresh_if_dirty(bounds);
        if needs_layout {
            TreeWalker::layout(&mut self.root, bounds);
        }
        commands
    }

    fn is_action_enabled(&self, action: &Action) -> bool {
        app_state_action_enabled(action, &self.app_state.borrow())
    }

    fn persist_workspace_preset(&mut self, preset: WorkspacePreset) {
        if self.preferences.workspace_preset == preset {
            return;
        }
        self.preferences.workspace_preset = preset;
        if let Err(err) =
            persist_self_hosted_preferences_to(&self.preferences_path, &self.preferences)
        {
            tracing::warn!("failed to persist self-hosted workspace preference: {err}");
            self.app_state.borrow_mut().set_status_hint(
                format!("Workspace preference could not be saved: {err}"),
                true,
            );
            self.mark_dirty();
        }
    }

    fn take_preferences_update(&mut self, action: &Action, bounds: Rect) -> bool {
        let Some(result) = parse_preferences_theme_update(action) else {
            return false;
        };
        match result {
            Ok(payload) => {
                self.preferences.theme_preset = payload.preset;
                set_theme_preset(payload.preset);
                if let Err(err) =
                    persist_self_hosted_preferences_to(&self.preferences_path, &self.preferences)
                {
                    tracing::warn!("failed to persist self-hosted preferences: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("Preferences could not be saved: {err}"), true);
                }
                self.root.refresh_from_app_state_with_preferences_and_runtime_logs(
                    &self.app_state.borrow(),
                    &self.preferences,
                    self.console_log_buffer.as_ref(),
                );
                TreeWalker::layout(&mut self.root, bounds);
                true
            }
            Err(err) => {
                tracing::warn!("invalid self-hosted preferences action: {err}");
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("Preferences action failed: {err}"), true);
                self.mark_dirty();
                true
            }
        }
    }
}

fn take_shell_window_command(commands: &mut SelfHostedShellCommands, action: &Action) -> bool {
    match action {
        Action::ToggleFullscreen => {
            commands.toggle_fullscreen = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_QUIT =>
        {
            commands.quit = true;
            true
        }
        _ => false,
    }
}

fn parse_preferences_theme_update(
    action: &Action,
) -> Option<Result<PreferencesThemePayload, serde_json::Error>> {
    match action {
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES_THEME_CHANGED =>
        {
            Some(serde_json::from_value(payload.clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::types::AssetId;
    use mondrian_editor_state::Action;
    use mondrian_platform::{FileFilter, NoopPlatformService};
    use mondrian_ui_core::widget::{DrawCommandEncoder, PaintContext, Widget};
    use mondrian_ui_core::Point;
    use mondrian_ui_theme::{
        current_theme, set_theme_preset as set_global_theme_preset, ThemePreset,
    };
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::self_hosted::preferences_store::{
        load_self_hosted_preferences_from, SelfHostedPreferences,
    };

    #[derive(Default)]
    struct RecordingEncoder {
        texts: Vec<String>,
    }

    #[derive(Default)]
    struct CountingPlatform {
        open_file_dialog_calls: AtomicUsize,
    }

    impl PlatformService for CountingPlatform {
        fn clipboard_copy(&self, _text: &str) {}

        fn clipboard_paste(&self) -> Option<String> {
            None
        }

        fn open_file_dialog(&self, _title: &str, _filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
            self.open_file_dialog_calls.fetch_add(1, Ordering::Relaxed);
            Some(vec![PathBuf::from("E:/media/a.mov")])
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Option<PathBuf> {
            None
        }

        fn open_folder_dialog(&self, _title: &str) -> Option<PathBuf> {
            None
        }

        fn open_url(&self, _url: &str) {}

        fn reveal_in_file_manager(&self, _path: &Path) {}

        fn send_notification(&self, _title: &str, _body: &str) {}
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, _bounds: Rect, _color: mondrian_core::Color, _corner_radius: f32) {}

        fn draw_line(
            &mut self,
            _start: Point,
            _end: Point,
            _width: f32,
            _color: mondrian_core::Color,
        ) {
        }

        fn draw_text(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_string());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn temp_preferences_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mondrian-host-{name}-{nanos}.json"))
    }

    #[test]
    fn host_builds_root_from_initial_app_state() {
        let mut host = SelfHostedUiHost::new(AppState::new());

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert!(!host.root().dock().collect_grab_zones().is_empty());
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_builds_root_from_persisted_workspace_preference() {
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Compositing,
            },
            temp_preferences_path("initial-workspace"),
            None,
        );

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Compositing);
        assert!((host.root().dock().ratio() - 0.35).abs() < f32::EPSILON);
    }

    #[test]
    fn host_drains_actions_and_refreshes_root() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::NoOp);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(host.root().dock().ratio() > 0.0);
    }

    #[test]
    fn host_returns_window_commands_without_dispatching_to_app_state() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::ToggleFullscreen);
        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(
            commands,
            SelfHostedShellCommands { quit: true, toggle_fullscreen: true }
        );
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_applies_and_persists_theme_preference_updates() {
        let path = temp_preferences_path("theme-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences::default(),
            path.clone(),
            None,
        );
        let pending = PendingUiActions::default();

        pending.push(
            crate::app::ui_actions::app_shell_preferences_theme_changed_action(ThemePreset::Light),
        );
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.preferences().theme_preset, ThemePreset::Light);
        assert_eq!(current_theme().name, "Light");
        assert_eq!(
            load_self_hosted_preferences_from(&path).theme_preset,
            ThemePreset::Light
        );

        std::fs::remove_file(path).ok();
        set_global_theme_preset(ThemePreset::Dark);
    }

    #[test]
    fn host_persists_workspace_preference_updates() {
        let path = temp_preferences_path("workspace-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Editing,
            },
            path.clone(),
            None,
        );
        let pending = PendingUiActions::default();

        pending.push(Action::SwitchWorkspace(WorkspacePreset::Export));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Export);
        assert_eq!(host.preferences().workspace_preset, WorkspacePreset::Export);
        let loaded = load_self_hosted_preferences_from(&path);
        assert_eq!(loaded.workspace_preset, WorkspacePreset::Export);
        assert_eq!(loaded.theme_preset, ThemePreset::Dark);

        std::fs::remove_file(path).ok();
        set_global_theme_preset(ThemePreset::Dark);
    }

    #[test]
    fn host_reports_unknown_app_shell_actions_as_status_errors() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::Custom {
            namespace: crate::app::ui_actions::APP_SHELL_NAMESPACE.into(),
            name: "missing_command".into(),
            payload: serde_json::Value::Null,
        });
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(
            host.app_state().status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("missing_command")
            })
        );
    }

    #[test]
    fn host_ignores_unavailable_editor_actions_before_dispatch() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(Action::ImportMedia(vec![PathBuf::from("E:/media/a.mov")]));
        pending.push(Action::Undo);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(host.app_state().status_hint.is_none());
        assert!(!host.app_state().can_undo_action());
    }

    #[test]
    fn host_ignores_unavailable_app_shell_dialogs_before_platform_access() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();
        let platform = CountingPlatform::default();

        pending.push(crate::app::ui_actions::app_shell_import_media_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(platform.open_file_dialog_calls.load(Ordering::Relaxed), 0);
        assert!(host.app_state().status_hint.is_none());
    }

    #[test]
    fn host_refreshes_root_after_failed_editor_action() {
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_prepare_drag_action(
            crate::app::ui_actions::AssetsPrepareDragPayload { asset_id: AssetId::new() },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(
            host.app_state().status_hint.as_ref().is_some_and(|(message, is_error)| {
                *is_error && message.contains("素材准备失败")
            }),
            "status hint: {:?}",
            host.app_state().status_hint
        );

        let mut encoder = RecordingEncoder::default();
        let theme = ThemePreset::Dark.build();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 1280.0, 720.0),
        };
        host.root().paint(&mut ctx);

        assert!(
            encoder.texts.iter().any(|text| text == "Error"),
            "painted texts: {:?}",
            encoder.texts
        );
        assert!(
            encoder.texts.iter().any(|text| text.contains("素材准备失败")),
            "painted texts: {:?}",
            encoder.texts
        );
    }
}
