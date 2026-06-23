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
use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::{TreeWalker, Widget};
use mondrian_ui_theme::set_theme_preset;

use crate::app::ui_actions::{
    AssetsOpenFolderPayload, PreferencesShortcutPayload, PreferencesShortcutReboundPayload,
    PreferencesThemePayload, APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG,
    APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, APP_SHELL_OPEN_PROJECT_DIALOG,
    APP_SHELL_OPEN_RECENT_PROJECT, APP_SHELL_PENDING_CLOSE_CANCEL, APP_SHELL_PENDING_CLOSE_DISCARD,
    APP_SHELL_PENDING_CLOSE_SAVE_CONTINUE, APP_SHELL_PREFERENCES_SHORTCUT_DISABLED,
    APP_SHELL_PREFERENCES_SHORTCUT_REBOUND, APP_SHELL_PREFERENCES_SHORTCUT_RESET,
    APP_SHELL_PREFERENCES_THEME_CHANGED, APP_SHELL_QUIT, APP_SHELL_RECOVER_PROJECT,
    APP_SHELL_WINDOW_DRAG, APP_SHELL_WINDOW_MINIMIZE, APP_SHELL_WINDOW_TOGGLE_MAXIMIZE,
    ASSETS_NAMESPACE, ASSETS_OPEN_FOLDER,
};
use crate::app::{discover_crash_recovery_candidates, AppState, CrashRecoveryCandidate};
use crate::self_hosted::action_availability::app_state_action_enabled;
use crate::self_hosted::action_queue::PendingUiActions;
use crate::self_hosted::asset_thumbnails::AssetThumbnailCache;
use crate::self_hosted::pending_close_dialog::PendingCloseDialogAction;
use crate::self_hosted::preferences_store::{
    load_self_hosted_preferences, persist_self_hosted_preferences_to, self_hosted_preferences_path,
    SelfHostedPreferences,
};
use crate::self_hosted::preview::SelfHostedPreviewService;
use crate::self_hosted::shell::{try_resolve_app_shell_action, SelfHostedAppRoot};
use crate::self_hosted::shortcuts::{
    default_shortcuts, is_known_shortcut_id, SelfHostedShortcutBinding, SelfHostedShortcutKey,
    SelfHostedShortcutOverride,
};
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
    asset_thumbnails: AssetThumbnailCache,
    preview_service: SelfHostedPreviewService,
    mode: SelfHostedUiMode,
    ui_dirty: Cell<bool>,
    pending_close_action: Option<PendingCloseAction>,
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
        let asset_thumbnails = AssetThumbnailCache::new();
        let preview_service = SelfHostedPreviewService::new();
        let root = SelfHostedAppRoot::from_app_state_with_preferences_thumbnails_and_preview(
            &app_state,
            &preferences,
            Some(&asset_thumbnails),
            Some(&preview_service),
        );
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
            asset_thumbnails,
            preview_service,
            mode,
            ui_dirty: Cell::new(false),
            pending_close_action: None,
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
        let next_mode = mode_for_app_state(&self.app_state.borrow());
        if next_mode == self.mode && widget_tree_has_transient_interaction(self.active_root()) {
            self.ui_dirty.set(true);
            return;
        }
        self.normalize_asset_folder_selection();
        self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
            &self.app_state.borrow(),
            &self.preferences,
            Some(&self.asset_thumbnails),
            Some(&self.preview_service),
        );
        self.sync_mode_from_app_state(bounds);
        TreeWalker::layout(self.active_root_mut(), bounds);
    }

    /// Poll background host tasks. Returns true when a repaint was requested by
    /// refreshed model data.
    pub fn poll_background_tasks(&mut self, bounds: Rect) -> bool {
        let thumbnails_changed = self.asset_thumbnails.poll_finished();
        let preview_changed = self.preview_service.poll_finished();
        if !thumbnails_changed && !preview_changed {
            return false;
        }
        self.mark_dirty();
        self.refresh_if_dirty(bounds);
        true
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
            if self.take_pending_close_response(&mut commands, &action) {
                needs_layout = true;
                continue;
            }
            if self.take_guarded_close_or_quit(&mut commands, &action) {
                needs_layout = true;
                continue;
            }
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

            let workspace_before = self.root.workspace_preset();
            let current_project_path = self.app_state.borrow().current_project_path.clone();
            let action = match self.root.try_handle_shell_action(
                action,
                platform,
                current_project_path.as_deref(),
            ) {
                Ok(Some(action)) => {
                    self.persist_root_workspace_change(workspace_before);
                    action
                }
                Ok(None) => {
                    self.persist_root_workspace_change(workspace_before);
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
        let next = mode_for_app_state(&self.app_state.borrow());
        if self.mode != next {
            self.mode = next;
            self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
                &self.app_state.borrow(),
                &self.preferences,
                Some(&self.asset_thumbnails),
                Some(&self.preview_service),
            );
            TreeWalker::layout(self.active_root_mut(), bounds);
        }
    }

    fn is_action_enabled(&self, action: &Action) -> bool {
        app_state_action_enabled(action, &self.app_state.borrow())
    }

    pub fn sync_workspace_layout_from_root(&mut self) {
        if self.mode != SelfHostedUiMode::Workspace {
            return;
        }
        if self.root.sync_custom_workspace_layout_from_dock() {
            self.persist_workspace_preferences_from_root();
        }
    }

    fn persist_root_workspace_change(&mut self, previous: WorkspacePreset) {
        let current = self.root.workspace_preset();
        if current != previous
            || self.preferences.custom_workspace_layout
                != self.root.custom_workspace_layout().cloned()
        {
            self.persist_workspace_preferences_from_root();
        }
    }

    fn persist_workspace_preferences_from_root(&mut self) {
        let preset = self.root.workspace_preset();
        let custom_workspace_layout = self.root.custom_workspace_layout().cloned();
        if self.preferences.workspace_preset == preset
            && self.preferences.custom_workspace_layout == custom_workspace_layout
        {
            return;
        }
        self.preferences.workspace_preset = preset;
        self.preferences.custom_workspace_layout = custom_workspace_layout;
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
        let Some(result) = parse_preferences_update(action) else {
            return false;
        };
        match result {
            Ok(update) => {
                match update {
                    PreferencesUpdate::Theme(payload) => {
                        self.preferences.theme_preset = payload.preset;
                        set_theme_preset(payload.preset);
                    }
                    PreferencesUpdate::ShortcutDisabled(payload) => {
                        if !is_known_shortcut_id(&payload.id) {
                            self.app_state
                                .borrow_mut()
                                .set_status_hint("Shortcut preference is no longer valid", true);
                            self.mark_dirty();
                            return true;
                        }
                        self.preferences.shortcut_overrides.retain(|entry| entry.id != payload.id);
                        self.preferences
                            .shortcut_overrides
                            .push(SelfHostedShortcutOverride { id: payload.id, binding: None });
                    }
                    PreferencesUpdate::ShortcutReset(payload) => {
                        self.preferences.shortcut_overrides.retain(|entry| entry.id != payload.id);
                    }
                    PreferencesUpdate::ShortcutRebound(payload) => {
                        if !is_known_shortcut_id(&payload.id) {
                            self.app_state
                                .borrow_mut()
                                .set_status_hint("Shortcut preference is no longer valid", true);
                            self.mark_dirty();
                            return true;
                        }
                        let Some(key) = SelfHostedShortcutKey::from_preference_name(&payload.key)
                        else {
                            self.app_state
                                .borrow_mut()
                                .set_status_hint("Shortcut key is no longer valid", true);
                            self.mark_dirty();
                            return true;
                        };
                        apply_shortcut_rebind(
                            &mut self.preferences.shortcut_overrides,
                            payload.id,
                            SelfHostedShortcutBinding {
                                key,
                                ctrl: payload.ctrl,
                                alt: payload.alt,
                                shift: payload.shift,
                                meta: payload.meta,
                            },
                        );
                    }
                }
                if let Err(err) =
                    persist_self_hosted_preferences_to(&self.preferences_path, &self.preferences)
                {
                    tracing::warn!("failed to persist self-hosted preferences: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("Preferences could not be saved: {err}"), true);
                }
                self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
                    &self.app_state.borrow(),
                    &self.preferences,
                    Some(&self.asset_thumbnails),
                    Some(&self.preview_service),
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
                self.root.refresh_from_app_state_with_preferences_thumbnails_and_preview(
                    &self.app_state.borrow(),
                    &self.preferences,
                    Some(&self.asset_thumbnails),
                    Some(&self.preview_service),
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

    fn take_guarded_close_or_quit(
        &mut self,
        commands: &mut SelfHostedShellCommands,
        action: &Action,
    ) -> bool {
        let Some(pending) = close_request_from_action(action) else {
            return false;
        };

        if self.app_state.borrow().has_unsaved_project_changes() {
            self.pending_close_action = Some(pending);
            self.root.show_pending_close_dialog(pending.dialog_action());
            return true;
        }

        self.execute_pending_close_action(commands, pending);
        true
    }

    fn take_pending_close_response(
        &mut self,
        commands: &mut SelfHostedShellCommands,
        action: &Action,
    ) -> bool {
        let Action::Custom { namespace, name, .. } = action else {
            return false;
        };
        if namespace != APP_SHELL_NAMESPACE {
            return false;
        }

        match name.as_str() {
            APP_SHELL_PENDING_CLOSE_SAVE_CONTINUE => {
                let Some(pending) = self.pending_close_action else {
                    self.root.close_pending_close_dialog();
                    return true;
                };
                if let Err(err) = self.app_state.borrow().save_project() {
                    tracing::warn!("closing project after save failed: {err}");
                    self.app_state
                        .borrow_mut()
                        .set_status_hint(format!("保存项目失败：{err}"), true);
                    self.mark_dirty();
                    return true;
                }
                self.pending_close_action = None;
                self.root.close_pending_close_dialog();
                self.execute_pending_close_action(commands, pending);
                true
            }
            APP_SHELL_PENDING_CLOSE_DISCARD => {
                let Some(pending) = self.pending_close_action.take() else {
                    self.root.close_pending_close_dialog();
                    return true;
                };
                self.root.close_pending_close_dialog();
                self.execute_pending_close_action(commands, pending);
                true
            }
            APP_SHELL_PENDING_CLOSE_CANCEL => {
                self.pending_close_action = None;
                self.root.close_pending_close_dialog();
                true
            }
            _ => false,
        }
    }

    fn execute_pending_close_action(
        &mut self,
        commands: &mut SelfHostedShellCommands,
        pending: PendingCloseAction,
    ) {
        match pending {
            PendingCloseAction::CloseProject => {
                if let Err(err) = self.dispatch_editor_action(Action::CloseProject) {
                    tracing::warn!("close project failed: {err}");
                }
                self.refresh_recovery_candidates();
                self.mark_dirty();
            }
            PendingCloseAction::QuitApp => {
                if self.app_state.borrow().has_open_project() {
                    if let Err(err) = self.dispatch_editor_action(Action::CloseProject) {
                        tracing::warn!("close project before quit failed: {err}");
                    }
                    self.refresh_recovery_candidates();
                    self.mark_dirty();
                }
                commands.quit = true;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingCloseAction {
    CloseProject,
    QuitApp,
}

impl PendingCloseAction {
    fn dialog_action(self) -> PendingCloseDialogAction {
        match self {
            Self::CloseProject => PendingCloseDialogAction::CloseProject,
            Self::QuitApp => PendingCloseDialogAction::QuitApp,
        }
    }
}

fn close_request_from_action(action: &Action) -> Option<PendingCloseAction> {
    match action {
        Action::CloseProject => Some(PendingCloseAction::CloseProject),
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_QUIT =>
        {
            Some(PendingCloseAction::QuitApp)
        }
        _ => None,
    }
}

fn mode_for_app_state(state: &AppState) -> SelfHostedUiMode {
    if state.has_open_project() {
        SelfHostedUiMode::Workspace
    } else {
        SelfHostedUiMode::Startup
    }
}

fn widget_tree_has_transient_interaction(widget: &dyn Widget) -> bool {
    widget.accepts_text_input()
        || widget.overlay_hit_test(Point::new(-1_000_000.0, -1_000_000.0))
        || (0..widget.child_count())
            .any(|index| widget.child(index).is_some_and(widget_tree_has_transient_interaction))
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
    let metadata = std::fs::metadata(project_file).ok();
    let modified = metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .map(recent_project_modified_label)
        .unwrap_or_else(|| "未知时间".to_owned());
    let size = metadata
        .as_ref()
        .map(|metadata| format_file_size(metadata.len()))
        .unwrap_or_else(|| "--".to_owned());
    format!("{modified} • {size}")
}

fn recent_project_modified_label(modified: SystemTime) -> String {
    let age_secs = SystemTime::now()
        .duration_since(modified)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    if age_secs < 60 {
        "刚刚".to_owned()
    } else if age_secs < 3600 {
        format!("{} 分钟前", age_secs / 60)
    } else if age_secs < 86_400 {
        format!("{} 小时前", age_secs / 3600)
    } else if age_secs < 172_800 {
        "昨天".to_owned()
    } else if age_secs < 604_800 {
        format!("{} 天前", age_secs / 86_400)
    } else {
        format!("{} 周前", age_secs / 604_800)
    }
}

fn format_file_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else if size >= 10.0 {
        format!("{size:.0} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
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

fn apply_shortcut_rebind(
    overrides: &mut Vec<SelfHostedShortcutOverride>,
    id: String,
    binding: SelfHostedShortcutBinding,
) {
    let core_binding = binding.to_core();
    overrides.retain(|entry| {
        entry.id != id
            && entry.binding.map(|existing| existing.to_core() != core_binding).unwrap_or(true)
    });

    let defaults = default_shortcuts();
    for shortcut in &defaults {
        if shortcut.id == id || shortcut.binding != core_binding {
            continue;
        }
        if !overrides.iter().any(|entry| entry.id == shortcut.id) {
            overrides
                .push(SelfHostedShortcutOverride { id: shortcut.id.to_owned(), binding: None });
        }
    }

    if defaults
        .iter()
        .find(|shortcut| shortcut.id == id)
        .is_some_and(|shortcut| shortcut.binding == core_binding)
    {
        return;
    }

    overrides.push(SelfHostedShortcutOverride { id, binding: Some(binding) });
}

enum PreferencesUpdate {
    Theme(PreferencesThemePayload),
    ShortcutDisabled(PreferencesShortcutPayload),
    ShortcutReset(PreferencesShortcutPayload),
    ShortcutRebound(PreferencesShortcutReboundPayload),
}

fn parse_preferences_update(
    action: &Action,
) -> Option<Result<PreferencesUpdate, serde_json::Error>> {
    match action {
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES_THEME_CHANGED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::Theme))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_SHORTCUT_DISABLED =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ShortcutDisabled))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES_SHORTCUT_RESET =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ShortcutReset))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_PREFERENCES_SHORTCUT_REBOUND =>
        {
            Some(serde_json::from_value(payload.clone()).map(PreferencesUpdate::ShortcutRebound))
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
    use mondrian_editor_state::state::PanelKind;
    use mondrian_editor_state::Action;
    use mondrian_platform::{FileFilter, NoopPlatformService};
    use mondrian_timeline::Sequence;
    use mondrian_ui_core::tree::TreeWalker;
    use mondrian_ui_core::types::{Modifiers, MouseButton, Point, Rect, SplitDirection};
    use mondrian_ui_core::widget::EventContext;
    use mondrian_ui_core::{EventRequests, EventResult, UiEvent, Widget};
    use mondrian_ui_theme::{current_theme, ThemePreset};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::self_hosted::preferences_store::{
        load_self_hosted_preferences_from, SelfHostedPreferences,
    };
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use crate::self_hosted::workspace_layout::SelfHostedWorkspaceLayout;

    #[derive(Default)]
    struct CountingPlatform {
        open_file_dialog_calls: AtomicUsize,
    }

    struct StartupProjectPlatform {
        project_file: PathBuf,
    }

    #[derive(Default)]
    struct ProjectDialogPlatform {
        open_paths: Option<Vec<PathBuf>>,
        save_path: Option<PathBuf>,
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

    impl PlatformService for ProjectDialogPlatform {
        fn clipboard_copy(&self, _text: &str) {}

        fn clipboard_paste(&self) -> Option<String> {
            None
        }

        fn open_file_dialog(&self, _title: &str, _filters: &[FileFilter]) -> Option<Vec<PathBuf>> {
            self.open_paths.clone()
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Option<PathBuf> {
            self.save_path.clone()
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

    fn workspace_app_state() -> AppState {
        let mut state = AppState::new();
        state.sequence = Some(Sequence::new("Edit"));
        state.current_project_path = Some(PathBuf::from("E:/projects/edit.mdp"));
        state
    }

    fn saved_workspace_app_state(name: &str) -> AppState {
        let mut state = AppState::new();
        let project_file = temp_preferences_path(name).with_extension("mdp");
        state
            .create_new_project_at(
                project_file,
                "Edit",
                1920,
                1080,
                mondrian_core::Rational::FPS_2997,
            )
            .expect("project should be created");
        state.sequence.as_mut().expect("active sequence").name = "Changed".to_owned();
        state
    }

    fn create_project_file(name: &str) -> PathBuf {
        let project_file = temp_preferences_path(name).with_extension("mdp");
        let mut state = AppState::new();
        state
            .create_new_project_at(
                project_file.clone(),
                name,
                1920,
                1080,
                mondrian_core::Rational::FPS_2997,
            )
            .expect("project should be created");
        project_file
    }

    fn cleanup_project_file(project_file: &Path) {
        let _ = std::fs::remove_file(project_file);
        let _ = std::fs::remove_dir_all(project_runtime_root_for_test(project_file));
    }

    fn write_minimal_wav(path: &Path) {
        let sample_rate = 8_000u32;
        let channels = 1u16;
        let bits_per_sample = 16u16;
        let samples = [0i16; 16];
        let data_size = (samples.len() * std::mem::size_of::<i16>()) as u32;
        let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
        let block_align = channels * bits_per_sample / 8;
        let mut bytes = Vec::with_capacity(44 + data_size as usize);

        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&byte_rate.to_le_bytes());
        bytes.extend_from_slice(&block_align.to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_size.to_le_bytes());
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }

        std::fs::write(path, bytes).expect("write wav fixture");
    }

    fn dispatch_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        requests: &'a mut EventRequests,
    ) -> EventContext<'a> {
        event_ctx(focus, shortcut, tooltip, requests, &|_| {})
    }

    fn drag_first_splitter_to(root: &mut SelfHostedAppRoot, ratio: f32) {
        let (zone, direction) = root.dock().collect_grab_zones()[0];
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let target = match direction {
            SplitDirection::Horizontal => Point::new(bounds.width * ratio, zone.center().y),
            SplitDirection::Vertical => Point::new(zone.center().x, bounds.height * ratio),
        };
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatch_ctx(&mut focus, &mut shortcut, &mut tooltip, &mut requests);

        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseDown {
                    position: zone.center(),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseUp {
                    position: target,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }

    fn click_root(root: &mut SelfHostedAppRoot, position: Point) {
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = dispatch_ctx(&mut focus, &mut shortcut, &mut tooltip, &mut requests);
        assert_eq!(
            root.event(
                &UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.event(
                &UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
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
    fn host_open_project_dialog_switches_to_workspace_and_records_recent_project() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let project_file = create_project_file("open-dialog");
        let preferences_path = temp_preferences_path("open-dialog-preferences");
        let platform = ProjectDialogPlatform {
            open_paths: Some(vec![project_file.clone()]),
            save_path: None,
        };
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences::default(),
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_open_project_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.mode(), SelfHostedUiMode::Workspace);
        assert_eq!(
            host.app_state().current_project_path.as_deref(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );
        assert_eq!(
            load_self_hosted_preferences_from(&preferences_path).recent_projects,
            vec![project_file.clone()]
        );

        cleanup_project_file(&project_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_open_recent_project_uses_startup_shell_action_boundary() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let project_file = create_project_file("open-recent");
        let mut preferences = SelfHostedPreferences::default();
        preferences.record_recent_project(project_file.clone());
        let preferences_path = temp_preferences_path("open-recent-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            preferences,
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(
            crate::app::ui_actions::app_shell_open_recent_project_action(
                crate::app::ui_actions::AppShellOpenRecentProjectPayload {
                    project_file: project_file.clone(),
                },
            ),
        );
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.mode(), SelfHostedUiMode::Workspace);
        assert_eq!(
            host.app_state().current_project_path.as_deref(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );

        cleanup_project_file(&project_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_save_project_action_clears_unsaved_fingerprint_delta() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(saved_workspace_app_state("save-project-action"));
        let project_file = host.app_state().current_project_path.clone().expect("project path");
        assert!(host.app_state().has_unsaved_project_changes());
        let pending = PendingUiActions::default();

        pending.push(Action::SaveProject);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(!host.app_state().has_unsaved_project_changes());

        cleanup_project_file(&project_file);
    }

    #[test]
    fn host_save_as_dialog_updates_project_path_and_recent_project() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let preferences_path = temp_preferences_path("save-as-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            saved_workspace_app_state("save-as-source"),
            SelfHostedPreferences::default(),
            preferences_path.clone(),
        );
        let source_file =
            host.app_state().current_project_path.clone().expect("source project path");
        let target_file = temp_preferences_path("save-as-target").with_extension("mdp");
        let platform = ProjectDialogPlatform {
            open_paths: None,
            save_path: Some(target_file.clone()),
        };
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_save_project_as_dialog_action());
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(
            host.app_state().current_project_path.as_deref(),
            Some(target_file.as_path())
        );
        assert!(target_file.exists());
        assert_eq!(
            host.preferences().recent_projects,
            vec![target_file.clone()]
        );

        cleanup_project_file(&source_file);
        cleanup_project_file(&target_file);
        let _ = std::fs::remove_file(preferences_path);
    }

    #[test]
    fn host_recovers_autosave_candidate_and_records_recent_project() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let project_file = create_project_file("recover-host");
        let mut autosave_state = AppState::new();
        autosave_state
            .open_project_file(project_file.clone())
            .expect("project should open for autosave");
        autosave_state.sequence.as_mut().expect("active sequence").name =
            "Recovered Edit".to_owned();
        let autosave_file = autosave_state
            .write_autosave_snapshot(2, 7)
            .expect("autosave snapshot should write");
        let preferences_path = temp_preferences_path("recover-host-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences::default(),
            preferences_path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_recover_project_action(
            crate::app::ui_actions::ProjectRecoverFromAutosavePayload {
                project_file: project_file.clone(),
                autosave_file,
            },
        ));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.mode(), SelfHostedUiMode::Workspace);
        assert_eq!(
            host.app_state().current_project_path.as_deref(),
            Some(project_file.as_path())
        );
        assert_eq!(
            host.app_state().sequence.as_ref().map(|sequence| sequence.name.as_str()),
            Some("Recovered Edit")
        );
        assert_eq!(
            host.preferences().recent_projects,
            vec![project_file.clone()]
        );

        cleanup_project_file(&project_file);
        let _ = std::fs::remove_file(preferences_path);
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
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
            },
            temp_preferences_path("initial-workspace"),
        );

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Compositing);
        assert!((host.root().dock().ratio() - 0.42).abs() < f32::EPSILON);
    }

    #[test]
    fn host_builds_root_from_persisted_custom_workspace_layout() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let layout = SelfHostedWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.37,
            first: Box::new(SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 1,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Assets, PanelKind::Effects],
            }),
            second: Box::new(SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Viewer,
                active_index: 0,
                hidden_tabs: Vec::new(),
                tabs: vec![PanelKind::Viewer],
            }),
        };
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            workspace_app_state(),
            SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Custom,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: Some(layout.clone()),
            },
            temp_preferences_path("initial-custom-workspace"),
        );

        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(host.mode(), SelfHostedUiMode::Workspace);
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Custom);
        assert!((host.root().dock().ratio() - 0.37).abs() < f32::EPSILON);
        assert_eq!(host.root().workspace_layout(), Some(layout));
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
    fn host_defers_dirty_refresh_while_shell_overlay_is_open() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let mut host = SelfHostedUiHost::new(workspace_app_state());
        TreeWalker::layout(host.root_mut(), bounds);

        let menu_bounds = host.root().menu_bar_bounds_for_test();
        let menu_point = Point::new(menu_bounds.x + 16.0, menu_bounds.center().y);
        click_root(host.root_mut(), menu_point);
        assert!(
            widget_tree_has_transient_interaction(host.active_root()),
            "test setup should leave the File menu overlay open"
        );

        host.mark_dirty();
        host.refresh_if_dirty(bounds);

        assert!(
            widget_tree_has_transient_interaction(host.active_root()),
            "dirty model refresh should not rebuild away an active menu/search overlay"
        );
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
    fn host_guards_unsaved_close_project_with_pending_modal() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(host.root.has_pending_close_dialog());
        assert_eq!(
            host.pending_close_action,
            Some(PendingCloseAction::CloseProject)
        );
    }

    #[test]
    fn host_can_cancel_pending_close_project() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        pending.push(crate::app::ui_actions::app_shell_pending_close_cancel_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(!host.root.has_pending_close_dialog());
        assert_eq!(host.pending_close_action, None);
    }

    #[test]
    fn host_can_discard_pending_close_project() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        pending.push(crate::app::ui_actions::app_shell_pending_close_discard_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(!host.app_state().has_open_project());
        assert!(!host.root.has_pending_close_dialog());
    }

    #[test]
    fn host_can_save_and_continue_pending_close_project() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(saved_workspace_app_state("save-close"));
        let pending = PendingUiActions::default();

        pending.push(Action::CloseProject);
        host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert!(host.root.has_pending_close_dialog());
        pending.push(crate::app::ui_actions::app_shell_pending_close_save_continue_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(!host.app_state().has_open_project());
        assert!(!host.root.has_pending_close_dialog());
    }

    #[test]
    fn host_guards_unsaved_quit_until_discarded() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let mut host = SelfHostedUiHost::new(workspace_app_state());
        let pending = PendingUiActions::default();

        pending.push(crate::app::ui_actions::app_shell_quit_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );
        assert_eq!(commands, SelfHostedShellCommands::default());
        assert!(host.app_state().has_open_project());
        assert!(host.root.has_pending_close_dialog());

        pending.push(crate::app::ui_actions::app_shell_pending_close_discard_action());
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(
            commands,
            SelfHostedShellCommands { quit: true, ..SelfHostedShellCommands::default() }
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
    fn host_applies_and_persists_shortcut_preference_updates() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let path = temp_preferences_path("shortcut-preferences");
        let mut preferences = SelfHostedPreferences::default();
        preferences
            .shortcut_overrides
            .push(SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None });
        let mut host =
            SelfHostedUiHost::new_with_preferences_path(AppState::new(), preferences, path.clone());
        let pending = PendingUiActions::default();

        pending.push(
            crate::app::ui_actions::app_shell_preferences_shortcut_disabled_action(
                "file.save_project",
            ),
        );
        pending.push(
            crate::app::ui_actions::app_shell_preferences_shortcut_reset_action("panel.inspector"),
        );
        pending.push(
            crate::app::ui_actions::app_shell_preferences_shortcut_rebound_action(
                crate::app::ui_actions::PreferencesShortcutReboundPayload {
                    id: "file.save_project".to_owned(),
                    key: "I".to_owned(),
                    ctrl: true,
                    alt: true,
                    shift: false,
                    meta: false,
                },
            ),
        );
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(
            host.preferences().shortcut_overrides,
            vec![
                SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
                SelfHostedShortcutOverride {
                    id: "file.save_project".to_owned(),
                    binding: Some(SelfHostedShortcutBinding {
                        key: SelfHostedShortcutKey::I,
                        ctrl: true,
                        alt: true,
                        shift: false,
                        meta: false,
                    }),
                },
            ]
        );
        assert_eq!(
            load_self_hosted_preferences_from(&path).shortcut_overrides,
            host.preferences().shortcut_overrides
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
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
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
    fn host_promotes_dragged_builtin_workspace_to_persisted_custom_layout() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-custom-layout");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            workspace_app_state(),
            SelfHostedPreferences::default(),
            path.clone(),
        );
        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Editing);

        drag_first_splitter_to(host.root_mut(), 0.78);
        let dragged_ratio = host.root().dock().ratio();
        host.sync_workspace_layout_from_root();

        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Custom);
        assert_eq!(host.preferences().workspace_preset, WorkspacePreset::Custom);
        assert!(host.preferences().custom_workspace_layout.is_some());
        assert!((host.root().dock().ratio() - dragged_ratio).abs() < f32::EPSILON);
        let loaded = load_self_hosted_preferences_from(&path);
        assert_eq!(loaded.workspace_preset, WorkspacePreset::Custom);
        assert_eq!(
            loaded.custom_workspace_layout,
            host.preferences().custom_workspace_layout
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_persists_toggle_panel_hidden_custom_layout() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-toggle-layout");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            workspace_app_state(),
            SelfHostedPreferences::default(),
            path.clone(),
        );
        TreeWalker::layout(host.root_mut(), Rect::new(0.0, 0.0, 1280.0, 720.0));
        let pending = PendingUiActions::default();

        pending.push(Action::TogglePanel(PanelKind::Inspector));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Custom);
        let layout = host.preferences().custom_workspace_layout.as_ref().expect("layout");
        assert!(!layout.contains_panel(PanelKind::Inspector));
        assert!(layout.contains_panel(PanelKind::Viewer));
        let loaded = load_self_hosted_preferences_from(&path);
        assert_eq!(loaded.workspace_preset, WorkspacePreset::Custom);
        assert_eq!(
            loaded.custom_workspace_layout,
            host.preferences().custom_workspace_layout
        );

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn host_persists_workspace_changes_from_panel_focus_fallback() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let path = temp_preferences_path("workspace-focus-preferences");
        let mut host = SelfHostedUiHost::new_with_preferences_path(
            AppState::new(),
            SelfHostedPreferences {
                version: 1,
                theme_preset: ThemePreset::Dark,
                workspace_preset: WorkspacePreset::Editing,
                recent_projects: Vec::new(),
                shortcut_overrides: Vec::new(),
                custom_workspace_layout: None,
            },
            path.clone(),
        );
        let pending = PendingUiActions::default();

        pending.push(Action::FocusPanel(PanelKind::Export));
        let commands = host.drain_pending_actions(
            &pending,
            Rect::new(0.0, 0.0, 1280.0, 720.0),
            &NoopPlatformService,
        );

        assert_eq!(commands, SelfHostedShellCommands::default());
        assert_eq!(host.root().workspace_preset(), WorkspacePreset::Export);
        assert_eq!(host.preferences().workspace_preset, WorkspacePreset::Export);
        assert_eq!(
            load_self_hosted_preferences_from(&path).workspace_preset,
            WorkspacePreset::Export
        );

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
    fn host_import_media_dialog_places_selected_files_in_target_asset_folder() {
        let _theme_guard = crate::self_hosted::test_utils::theme_test_guard();
        let library_root = temp_asset_library_dir("asset-dialog-import-library");
        let media_root = temp_asset_library_dir("asset-dialog-import-media");
        std::fs::create_dir_all(&media_root).expect("media root");
        let media_path = media_root.join("dialog-tone.wav");
        write_minimal_wav(&media_path);
        let library = AssetLibrary::open(library_root.clone()).expect("open asset library");
        let folder_id = library.create_folder("Rushes", None).expect("create folder");
        let mut state = workspace_app_state();
        state.asset_library = Some(library);
        let mut host = SelfHostedUiHost::new(state);
        let pending = PendingUiActions::default();
        let platform = ProjectDialogPlatform {
            open_paths: Some(vec![media_path.clone()]),
            save_path: None,
        };

        pending.push(
            crate::app::ui_actions::app_shell_import_media_dialog_action_with_target(
                crate::app::ui_actions::ImportMediaDialogPayload {
                    folder_id: Some(folder_id.clone()),
                },
            ),
        );
        let commands =
            host.drain_pending_actions(&pending, Rect::new(0.0, 0.0, 1280.0, 720.0), &platform);

        assert_eq!(commands, SelfHostedShellCommands::default());
        let assets = host
            .app_state()
            .asset_library
            .as_ref()
            .expect("library")
            .list_assets()
            .expect("list assets");
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].name, "dialog-tone.wav");
        assert_eq!(assets[0].folder_id.as_deref(), Some(folder_id.as_str()));
        assert!(host
            .app_state()
            .status_hint
            .as_ref()
            .is_some_and(|(message, is_error)| !*is_error && message.contains("已导入 1")));

        let _ = std::fs::remove_dir_all(library_root);
        let _ = std::fs::remove_dir_all(media_root);
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
