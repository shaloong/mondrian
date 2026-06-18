//! Self-hosted UI host state.
//!
//! Window entrypoints own native event loops and rendering surfaces. This host
//! owns the reusable application/UI state bridge: root widget, `AppState`, and
//! refresh policy after widget-dispatched actions.

use std::cell::{Cell, Ref, RefCell};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use mondrian_editor_state::state::WorkspacePreset;
use mondrian_platform::PlatformService;
use mondrian_ui_core::types::Rect;
use mondrian_ui_core::{TreeWalker, Widget};
use mondrian_ui_theme::set_theme_preset;

use crate::app::ui_actions::{
    AssetsOpenFolderPayload, PreferencesThemePayload, APP_SHELL_CANCEL_NEW_PROJECT_DIALOG,
    APP_SHELL_CLOSE_MODAL, APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_NEW_PROJECT_DRAFT_CHANGED,
    APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_OPEN_RECENT_PROJECT,
    APP_SHELL_PREFERENCES_THEME_CHANGED, APP_SHELL_QUIT, APP_SHELL_RECOVER_PROJECT,
    APP_SHELL_WINDOW_DRAG, APP_SHELL_WINDOW_MINIMIZE, APP_SHELL_WINDOW_TOGGLE_MAXIMIZE,
    ASSETS_NAMESPACE, ASSETS_OPEN_FOLDER,
};
use crate::app::{discover_crash_recovery_candidates, AppState, CrashRecoveryCandidate};
use crate::self_hosted::action_queue::PendingUiActions;
use crate::self_hosted::menu_bar::app_state_action_enabled;
use crate::self_hosted::preferences_store::{
    load_self_hosted_preferences, persist_self_hosted_preferences_to, self_hosted_preferences_path,
    SelfHostedPreferences,
};
use crate::self_hosted::shell::{try_resolve_app_shell_action, SelfHostedAppRoot};
use crate::self_hosted::startup::{
    SelfHostedStartupScreen, StartupRecentProject, StartupRecoveryProject,
};
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
    /// The native window should minimize.
    pub minimize: bool,
    /// The native window should toggle maximized state.
    pub toggle_maximize: bool,
    /// The native window should begin an OS-level drag move.
    pub begin_window_drag: bool,
}

/// Product shell mode owned by the self-hosted host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfHostedUiMode {
    /// Startup surface shown before a project is opened.
    Startup,
    /// Main editing workspace.
    Workspace,
}

/// Product-facing self-hosted UI session state.
pub struct SelfHostedUiHost {
    startup: SelfHostedStartupScreen,
    root: SelfHostedAppRoot,
    app_state: RefCell<AppState>,
    preferences: SelfHostedPreferences,
    preferences_path: PathBuf,
    recovery_candidates: Vec<CrashRecoveryCandidate>,
    mode: SelfHostedUiMode,
    ui_dirty: Cell<bool>,
}

impl SelfHostedUiHost {
    /// Create a host from an initial application state snapshot.
    pub fn new(app_state: AppState) -> Self {
        Self::new_with_preferences_path(
            app_state,
            load_self_hosted_preferences(),
            self_hosted_preferences_path(),
        )
    }

    /// Create a host from explicit preferences and path.
    pub(crate) fn new_with_preferences_path(
        app_state: AppState,
        preferences: SelfHostedPreferences,
        preferences_path: PathBuf,
    ) -> Self {
        set_theme_preset(preferences.theme_preset);
        let root = SelfHostedAppRoot::from_app_state_with_preferences(&app_state, &preferences);
        let mode = if app_state.has_open_project() {
            SelfHostedUiMode::Workspace
        } else {
            SelfHostedUiMode::Startup
        };
        let recovery_candidates = discover_crash_recovery_candidates();
        let mut startup = SelfHostedStartupScreen::new();
        startup.set_recent_projects(startup_recent_projects_from_preferences(&preferences));
        startup.set_recovery_projects(startup_recovery_projects_from_candidates(
            &recovery_candidates,
        ));
        Self {
            startup,
            root,
            app_state: RefCell::new(app_state),
            preferences,
            preferences_path,
            recovery_candidates,
            mode,
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

    /// Current visible product mode.
    pub fn mode(&self) -> SelfHostedUiMode {
        self.mode
    }

    /// Immutable access to the widget currently visible in the native window.
    pub fn active_root(&self) -> &dyn Widget {
        match self.mode {
            SelfHostedUiMode::Startup => &self.startup,
            SelfHostedUiMode::Workspace => &self.root,
        }
    }

    /// Mutable access to the widget currently visible in the native window.
    pub fn active_root_mut(&mut self) -> &mut dyn Widget {
        match self.mode {
            SelfHostedUiMode::Startup => &mut self.startup,
            SelfHostedUiMode::Workspace => &mut self.root,
        }
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
            self.sync_mode_from_app_state(bounds);
            return;
        }
        self.normalize_asset_folder_selection();
        self.root
            .refresh_from_app_state_with_preferences(&self.app_state.borrow(), &self.preferences);
        self.sync_mode_from_app_state(bounds);
        TreeWalker::layout(self.active_root_mut(), bounds);
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
            if self.take_startup_action(&action, bounds, platform) {
                needs_layout = true;
                continue;
            }
            if self.take_preferences_update(&action, bounds) {
                needs_layout = true;
                continue;
            }
            if self.take_asset_browser_navigation(&action, bounds) {
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
            if let Err(err) = self.dispatch_editor_action(action) {
                tracing::warn!("custom UI action failed: {err}");
            }
            self.mark_dirty();
            needs_layout = true;
        }

        self.refresh_if_dirty(bounds);
        if needs_layout {
            TreeWalker::layout(self.active_root_mut(), bounds);
        }
        commands
    }

    fn sync_mode_from_app_state(&mut self, bounds: Rect) {
        let next = if self.app_state.borrow().has_open_project() {
            SelfHostedUiMode::Workspace
        } else {
            SelfHostedUiMode::Startup
        };
        if self.mode != next {
            self.mode = next;
            self.root.refresh_from_app_state_with_preferences(
                &self.app_state.borrow(),
                &self.preferences,
            );
            TreeWalker::layout(self.active_root_mut(), bounds);
        }
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

    fn take_startup_action(
        &mut self,
        action: &Action,
        bounds: Rect,
        platform: &dyn PlatformService,
    ) -> bool {
        if self.mode != SelfHostedUiMode::Startup {
            return false;
        }

        if is_startup_local_shell_action(action) {
            match self.startup.try_handle_shell_action(action.clone(), platform) {
                Ok(Some(resolved)) => {
                    if let Err(err) = self.dispatch_editor_action(resolved) {
                        tracing::warn!("startup local action failed: {err}");
                    }
                    self.mark_dirty();
                    self.refresh_if_dirty(bounds);
                }
                Ok(None) => {
                    TreeWalker::layout(self.active_root_mut(), bounds);
                }
                Err(err) => {
                    tracing::warn!("startup local shell action failed: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("Startup action failed: {err}"), true);
                    self.mark_dirty();
                }
            }
            return true;
        }

        if !is_startup_project_action(action) {
            return false;
        }

        match try_resolve_app_shell_action(action.clone(), platform, None) {
            Ok(Some(resolved)) => {
                if let Err(err) = self.dispatch_editor_action(resolved) {
                    tracing::warn!("startup action failed: {err}");
                }
                self.mark_dirty();
                self.refresh_if_dirty(bounds);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!("startup shell action failed: {err}");
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("Startup action failed: {err}"), true);
                self.mark_dirty();
            }
        }
        true
    }

    fn dispatch_editor_action(&mut self, action: Action) -> mondrian_core::Result<()> {
        let previous_project_path = self.app_state.borrow().current_project_path.clone();
        let result = self.app_state.borrow_mut().dispatch_action(action);
        if result.is_ok() {
            let current_project_path = self.app_state.borrow().current_project_path.clone();
            if current_project_path.is_some() && current_project_path != previous_project_path {
                if let Some(path) = current_project_path {
                    self.record_recent_project(path);
                }
                self.refresh_recovery_candidates();
            }
        }
        result
    }

    fn record_recent_project(&mut self, project_file: PathBuf) {
        self.preferences.record_recent_project(project_file);
        self.sync_startup_recent_projects();
        if let Err(err) =
            persist_self_hosted_preferences_to(&self.preferences_path, &self.preferences)
        {
            tracing::warn!("failed to persist self-hosted recent projects: {err}");
            self.app_state
                .borrow_mut()
                .set_status_hint(format!("Recent projects could not be saved: {err}"), true);
            self.mark_dirty();
        }
    }

    fn sync_startup_recent_projects(&mut self) {
        self.startup
            .set_recent_projects(startup_recent_projects_from_preferences(&self.preferences));
    }

    fn refresh_recovery_candidates(&mut self) {
        self.recovery_candidates = discover_crash_recovery_candidates();
        self.startup.set_recovery_projects(startup_recovery_projects_from_candidates(
            &self.recovery_candidates,
        ));
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
                self.root.refresh_from_app_state_with_preferences(
                    &self.app_state.borrow(),
                    &self.preferences,
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

    fn take_asset_browser_navigation(&mut self, action: &Action, bounds: Rect) -> bool {
        let Some(result) = parse_asset_browser_navigation(action) else {
            return false;
        };
        match result {
            Ok(payload) => {
                let folder_id = self.valid_asset_folder_id(payload.folder_id);
                self.root.set_asset_folder_id(folder_id);
                self.root.refresh_from_app_state_with_preferences(
                    &self.app_state.borrow(),
                    &self.preferences,
                );
                TreeWalker::layout(&mut self.root, bounds);
                true
            }
            Err(err) => {
                tracing::warn!("invalid self-hosted asset browser action: {err}");
                self.app_state
                    .borrow_mut()
                    .set_status_hint(format!("Asset browser action failed: {err}"), true);
                self.mark_dirty();
                true
            }
        }
    }

    fn valid_asset_folder_id(&self, folder_id: Option<String>) -> Option<String> {
        let folder_id = folder_id?;
        let exists = {
            let app_state = self.app_state.borrow();
            app_state
                .asset_library
                .as_ref()
                .and_then(|library| match library.list_folders() {
                    Ok(folders) => Some(folders.iter().any(|folder| folder.id == folder_id)),
                    Err(err) => {
                        tracing::warn!("failed to list asset folders for navigation: {err}");
                        None
                    }
                })
                .unwrap_or(false)
        };
        exists.then_some(folder_id)
    }

    fn normalize_asset_folder_selection(&mut self) {
        let current = self.root.asset_folder_id().map(str::to_owned);
        let valid = self.valid_asset_folder_id(current.clone());
        if valid != current {
            self.root.set_asset_folder_id(valid);
        }
    }
}

fn is_startup_project_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && (name == APP_SHELL_OPEN_PROJECT_DIALOG
                    || name == APP_SHELL_OPEN_RECENT_PROJECT
                    || name == APP_SHELL_RECOVER_PROJECT)
    )
}

fn is_startup_local_shell_action(action: &Action) -> bool {
    matches!(
        action,
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && (name == APP_SHELL_NEW_PROJECT_DIALOG
                    || name == APP_SHELL_NEW_PROJECT_DRAFT_CHANGED
                    || name == APP_SHELL_CANCEL_NEW_PROJECT_DIALOG
                    || name == APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG
                    || name == APP_SHELL_CLOSE_MODAL)
    )
}

fn startup_recent_projects_from_preferences(
    preferences: &SelfHostedPreferences,
) -> Vec<StartupRecentProject> {
    preferences
        .recent_projects
        .iter()
        .map(|project_file| StartupRecentProject {
            project_file: project_file.clone(),
            title: recent_project_title(project_file),
            subtitle: recent_project_subtitle(project_file),
        })
        .collect()
}

fn startup_recovery_projects_from_candidates(
    candidates: &[CrashRecoveryCandidate],
) -> Vec<StartupRecoveryProject> {
    candidates
        .iter()
        .map(|candidate| {
            let snapshots = if candidate.total_snapshots > 1 {
                format!("，共 {} 个恢复点", candidate.total_snapshots)
            } else {
                String::new()
            };
            StartupRecoveryProject {
                project_file: candidate.project_file.clone(),
                autosave_file: candidate.autosave_file.clone(),
                title: recent_project_title(&candidate.project_file),
                detail: format!(
                    "{}{} · {}",
                    recovery_age_label(candidate.saved_at_unix_ms),
                    snapshots,
                    recent_project_subtitle(&candidate.project_file)
                ),
            }
        })
        .collect()
}

fn recent_project_title(project_file: &Path) -> String {
    project_file
        .file_stem()
        .or_else(|| project_file.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| project_file.display().to_string())
}

fn recovery_age_label(saved_at_unix_ms: u64) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(saved_at_unix_ms);
    let age_secs = now_ms.saturating_sub(saved_at_unix_ms) / 1000;
    if age_secs < 60 {
        format!("{age_secs} 秒前")
    } else if age_secs < 3600 {
        format!("{} 分钟前", age_secs / 60)
    } else if age_secs < 86_400 {
        format!("{} 小时前", age_secs / 3600)
    } else {
        format!("{} 天前", age_secs / 86_400)
    }
}

fn recent_project_subtitle(project_file: &Path) -> String {
    project_file
        .parent()
        .map(|parent| parent.display().to_string())
        .filter(|parent| !parent.trim().is_empty())
        .unwrap_or_else(|| project_file.display().to_string())
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
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_WINDOW_MINIMIZE =>
        {
            commands.minimize = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_WINDOW_TOGGLE_MAXIMIZE =>
        {
            commands.toggle_maximize = true;
            true
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_WINDOW_DRAG =>
        {
            commands.begin_window_drag = true;
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

fn parse_asset_browser_navigation(
    action: &Action,
) -> Option<Result<AssetsOpenFolderPayload, serde_json::Error>> {
    match action {
        Action::Custom { namespace, name, payload }
            if namespace == ASSETS_NAMESPACE && name == ASSETS_OPEN_FOLDER =>
        {
            Some(serde_json::from_value(payload.clone()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_assets::AssetLibrary;
    use mondrian_core::types::AssetId;
    use mondrian_editor_state::Action;
    use mondrian_platform::{FileFilter, NoopPlatformService};
    use mondrian_ui_theme::{current_theme, ThemePreset};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::self_hosted::preferences_store::{
        load_self_hosted_preferences_from, SelfHostedPreferences,
    };

    #[derive(Default)]
    struct CountingPlatform {
        open_file_dialog_calls: AtomicUsize,
    }

    struct StartupProjectPlatform {
        project_file: PathBuf,
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

    impl PlatformService for StartupProjectPlatform {
        fn clipboard_copy(&self, _text: &str) {}

        fn clipboard_paste(&self) -> Option<String> {
            None
        }

        fn open_file_dialog(&self, _title: &str, _filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
            None
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Option<PathBuf> {
            Some(self.project_file.clone())
        }

        fn open_folder_dialog(&self, _title: &str) -> Option<PathBuf> {
            None
        }

        fn open_url(&self, _url: &str) {}

        fn reveal_in_file_manager(&self, _path: &Path) {}

        fn send_notification(&self, _title: &str, _body: &str) {}
    }

    fn temp_preferences_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("mondrian-host-{name}-{nanos}.json"))
    }

    fn temp_asset_library_dir(name: &str) -> PathBuf {
        temp_preferences_path(name).with_extension("assets")
    }

    fn project_runtime_root_for_test(project_file: &Path) -> PathBuf {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let stem = project_file.file_stem().and_then(|s| s.to_str()).unwrap_or("project");
        let mut hasher = DefaultHasher::new();
        project_file.to_string_lossy().hash(&mut hasher);
        let hash = hasher.finish();
        std::env::temp_dir()
            .join("mondrian-runtime")
            .join(format!("mondrian_{stem}_{hash:x}"))
    }

    #[test]
    fn host_builds_root_from_initial_app_state() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(AppState::new());

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.mode(), SelfHostedUiMode::Startup);
        assert!(!host.root().dock().collect_grab_zones().is_empty());
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn startup_new_project_action_switches_to_workspace_mode() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let project_file = temp_preferences_path("startup-project").with_extension("mdp");
        let preferences_path = temp_preferences_path("startup-project-preferences");
        let runtime_root = project_runtime_root_for_test(&project_file);
        let platform = StartupProjectPlatform { project_file: project_file.clone() };
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences::default(),
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_new_project_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.mode(), SelfHostedUiMode::Startup);
        assert!(host.startup.has_modal());
        assert!(!host.app_state().has_open_project());

        pending.push(crate::app::ui_actions::app_shell_confirm_new_project_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.mode(), SelfHostedUiMode::Workspace);
        assert!(!host.startup.has_modal());
        assert!(host.app_state().has_open_project());
        assert_eq!(
            host.app_state().current_project_path.as_deref(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );
        assert_eq!(host.startup.recent_project_count(), 1);
        assert_eq!(
            load_self_hosted_preferences_from(&preferences_path).recent_projects,
            vec![project_file.clone()]
        );

        let _ = std::fs::remove_file(project_file);
        let _ = std::fs::remove_file(preferences_path);
        let _ = std::fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn host_loads_startup_recent_projects_from_preferences() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let project_file = temp_preferences_path("startup-recent").with_extension("mdp");
        let mut preferences = SelfHostedPreferences::default();
        preferences.record_recent_project(project_file.clone());

        let host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            preferences,
            temp_preferences_path("startup-recent-preferences"),
        );

        assert_eq!(host.mode(), SelfHostedUiMode::Startup);
        assert_eq!(host.startup.recent_project_count(), 1);
    }

    #[test]
    fn recovery_candidates_map_to_startup_rows() {
        let project_file = PathBuf::from("E:/projects/recover.mdp");
        let autosave_file = PathBuf::from("E:/runtime/autosave/project.autosave.mdp");
        let candidates = vec![CrashRecoveryCandidate {
            project_file: project_file.clone(),
            autosave_file: autosave_file.clone(),
            saved_at_unix_ms: 0,
            total_snapshots: 2,
        }];

        let rows = startup_recovery_projects_from_candidates(&candidates);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_file, project_file);
        assert_eq!(rows[0].autosave_file, autosave_file);
        assert_eq!(rows[0].title, "recover");
        assert!(rows[0].detail.contains("2 个恢复点"));
    }

    #[test]
    fn host_builds_root_from_persisted_workspace_preference() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Compositing,
                recent_projects: Vec::new(),
            },
            temp_preferences_path("initial-workspace"),
        );

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Compositing);
        assert!((host.root().dock().ratio() - 0.35).abs() < f32::EPSILON);
    }

    #[test]
    fn host_drains_actions_and_refreshes_root() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
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
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
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
            SelfHostedShellCommands {
                quit: true,
                toggle_fullscreen: true,
                ..SelfHostedShellCommands::default()
            }
        );
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_returns_custom_chrome_window_commands() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(AppState::new());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_window_minimize_action());
        pending.push(crate::app::ui_actions::app_shell_window_toggle_maximize_action());
        pending.push(crate::app::ui_actions::app_shell_window_drag_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(
            commands,
            SelfHostedShellCommands {
                minimize: true,
                toggle_maximize: true,
                begin_window_drag: true,
                ..SelfHostedShellCommands::default()
            }
        );
        assert!(!host.app_state().has_open_project());
    }

    #[test]
    fn host_applies_and_persists_theme_preference_updates() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let path = temp_preferences_path("theme-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences::default(),
            path.clone(),
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
    }

    #[test]
    fn host_persists_workspace_preference_updates() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
            },
            path.clone(),
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
    }

    #[test]
    fn host_reports_unknown_app_shell_actions_as_status_errors() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
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
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
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
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
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
    fn host_handles_asset_folder_navigation_as_shell_local_state() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let root = temp_asset_library_dir("asset-folder-navigation");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = AppState::new();
        state.asset_library = Some(library);
        let mut host = SelfHostedUiHost::new(state);
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_open_folder_action(
            crate::app::ui_actions::AssetsOpenFolderPayload { folder_id: Some(folder_id.clone()) },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.root().asset_folder_id(), Some(folder_id.as_str()));
        assert!(host.app_state().status_hint.is_none());

        pending.push(crate::app::ui_actions::assets_open_folder_action(
            crate::app::ui_actions::AssetsOpenFolderPayload { folder_id: None },
        ));
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(host.root().asset_folder_id(), None);
        assert!(host.app_state().status_hint.is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn host_clears_deleted_asset_folder_selection_after_dispatch() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let root = temp_asset_library_dir("asset-folder-delete-normalize");
        let library = AssetLibrary::open(root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = AppState::new();
        state.asset_library = Some(library);
        let mut host = SelfHostedUiHost::new(state);
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::assets_open_folder_action(
            crate::app::ui_actions::AssetsOpenFolderPayload { folder_id: Some(folder_id.clone()) },
        ));
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(host.root().asset_folder_id(), Some(folder_id.as_str()));

        pending.push(crate::app::ui_actions::assets_delete_folder_action(
            crate::app::ui_actions::AssetsDeleteFolderPayload { folder_id: folder_id.clone() },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.root().asset_folder_id(), None);
        assert!(host
            .app_state()
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("Rushes")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn host_refreshes_root_after_failed_editor_action() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
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

        assert!(
            !host.ui_dirty.get(),
            "failed editor actions should refresh the root immediately"
        );
    }
}
