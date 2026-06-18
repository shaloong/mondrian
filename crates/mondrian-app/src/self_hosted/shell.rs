//! Reusable widget shell for self-hosted Mondrian windows.
//!
//! Developer binaries own native window setup and event-loop plumbing. This
//! module owns the reusable root widget composition above the dock/panel layer.

use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_platform::{FileFilter, PlatformService};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::dock_panel::DockPanel;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::{
    AssetGrid, AssetGridState, PanelList, PanelListState, ScrollView, ScrollViewState,
};
use std::collections::BTreeMap;
use std::path::Path;

use crate::app::ui_actions::{
    assets_import_files_action, export_set_draft_action, project_create_with_settings_action,
    project_recover_from_autosave_action, AppShellOpenRecentProjectPayload,
    AppShellRevealInFileManagerPayload, AssetsImportFilesPayload, ExportDraftUpdatePayload,
    ExportOutputDialogPayload, ImportMediaDialogPayload, NewProjectDraftUpdatePayload,
    PreferencesTabPayload, ProjectRecoverFromAutosavePayload, APP_SHELL_ABOUT,
    APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_EXPORT_OUTPUT_DIALOG,
    APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG,
    APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, APP_SHELL_OPEN_PROJECT_DIALOG,
    APP_SHELL_OPEN_RECENT_PROJECT, APP_SHELL_PREFERENCES, APP_SHELL_PREFERENCES_TAB_CHANGED,
    APP_SHELL_RECOVER_PROJECT, APP_SHELL_REVEAL_IN_FILE_MANAGER, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
};
use crate::app::AppState;
use crate::self_hosted::menu_bar::MenuBar;
use crate::self_hosted::modal::ShellModal;
use crate::self_hosted::new_project_dialog::{
    default_project_file_name, SelfHostedNewProjectDraft,
};
use crate::self_hosted::panels::{
    build_dock_tree_for_preset, AssetThumbnailSource, SelfHostedPanelModels,
};
use crate::self_hosted::preferences_dialog::{PreferencesDialogTab, SelfHostedPreferencesModel};
use crate::self_hosted::preferences_store::SelfHostedPreferences;
use crate::self_hosted::title_bar::{TitleBar, TITLE_BAR_HEIGHT};
use mondrian_core::{MondrianError, Result};

/// Default file extension for Mondrian project containers.
pub const PROJECT_FILE_EXTENSION: &str = "mdp";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DockPanelState {
    owner: PanelKind,
    active_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PanelScrollState {
    owner: PanelKind,
    ordinal: usize,
    state: ScrollViewState,
}

/// File dialog filters for project file commands.
pub fn project_file_filters() -> Vec<FileFilter> {
    vec![FileFilter::new(
        "Mondrian Project",
        vec![PROJECT_FILE_EXTENSION],
    )]
}

/// File dialog filters for media import commands.
pub fn media_import_filters() -> Vec<FileFilter> {
    vec![
        FileFilter::new("Video", vec!["mp4", "mov", "mkv", "webm", "avi"]),
        FileFilter::new("Audio", vec!["mp3", "wav", "aac", "flac", "m4a"]),
    ]
}

/// File dialog filter for timeline export output commands.
pub fn export_output_filters(extension: &str) -> Vec<FileFilter> {
    let extension = normalized_export_extension(extension);
    if extension.is_empty() {
        vec![FileFilter::new(
            "Media",
            vec!["mp4", "mov", "mkv", "gif", "mxf", "webm"],
        )]
    } else {
        vec![FileFilter::new("Export", vec![extension])]
    }
}

fn normalized_export_extension(extension: &str) -> String {
    extension.trim().trim_start_matches('.').trim().to_ascii_lowercase()
}

/// Resolve a self-hosted app-shell action into a concrete editor action.
///
/// Native file dialogs stay behind [`PlatformService`]. Widgets and menus emit
/// stable app-shell requests, while the window entrypoint injects platform
/// capabilities and dispatches only concrete editor actions.
pub fn resolve_app_shell_action(
    action: Action,
    platform: &dyn PlatformService,
    current_project_path: Option<&Path>,
) -> Option<Action> {
    match try_resolve_app_shell_action(action, platform, current_project_path) {
        Ok(action) => action,
        Err(err) => {
            tracing::warn!("self-hosted app-shell action failed: {err}");
            None
        }
    }
}

/// Resolve a self-hosted app-shell action and report protocol errors.
pub fn try_resolve_app_shell_action(
    action: Action,
    platform: &dyn PlatformService,
    current_project_path: Option<&Path>,
) -> Result<Option<Action>> {
    match action {
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_NEW_PROJECT_DIALOG =>
        {
            let path = platform.save_file_dialog(
                "Create Mondrian Project",
                &format!("Untitled.{PROJECT_FILE_EXTENSION}"),
                &project_file_filters(),
            );
            let Some(path) = path else {
                return Ok(None);
            };
            let draft = SelfHostedNewProjectDraft::from_project_path(&path);
            Ok(Some(project_create_with_settings_action(
                draft.into_payload(path),
            )))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_OPEN_PROJECT_DIALOG =>
        {
            let Some(paths) =
                platform.open_file_dialog("Open Mondrian Project", &project_file_filters())
            else {
                return Ok(None);
            };
            Ok(paths.into_iter().next().map(Action::OpenProject))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_OPEN_RECENT_PROJECT =>
        {
            let payload: AppShellOpenRecentProjectPayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            Ok(Some(Action::OpenProject(payload.project_file)))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_RECOVER_PROJECT =>
        {
            let payload: ProjectRecoverFromAutosavePayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            Ok(Some(project_recover_from_autosave_action(payload)))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
        {
            let payload = if payload.is_null() {
                ImportMediaDialogPayload { folder_id: None }
            } else {
                serde_json::from_value(payload).map_err(|err| app_shell_action_error(&name, err))?
            };
            let Some(paths) = platform.open_file_dialog("Import Media", &media_import_filters())
            else {
                return Ok(None);
            };
            Ok((!paths.is_empty()).then(|| {
                if payload.folder_id.is_some() {
                    assets_import_files_action(AssetsImportFilesPayload {
                        paths,
                        folder_id: payload.folder_id,
                    })
                } else {
                    Action::ImportMedia(paths)
                }
            }))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_REVEAL_IN_FILE_MANAGER =>
        {
            let payload: AppShellRevealInFileManagerPayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            platform.reveal_in_file_manager(&payload.path);
            Ok(None)
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            let default_name = current_project_path
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("untitled.{PROJECT_FILE_EXTENSION}"));
            Ok(platform
                .save_file_dialog(
                    "Save Mondrian Project As",
                    &default_name,
                    &project_file_filters(),
                )
                .map(Action::SaveProjectAs))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_EXPORT_OUTPUT_DIALOG =>
        {
            let payload: ExportOutputDialogPayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            let extension = normalized_export_extension(&payload.extension);
            let default_name =
                normalized_export_default_file_name(&payload.default_file_name, &extension);
            Ok(platform
                .save_file_dialog(
                    "Choose Export Output",
                    &default_name,
                    &export_output_filters(&extension),
                )
                .map(|path| {
                    export_set_draft_action(ExportDraftUpdatePayload::OutputPath(
                        path.display().to_string(),
                    ))
                }))
        }
        Action::Custom { namespace, name, .. } if namespace == APP_SHELL_NAMESPACE => {
            Err(unknown_app_shell_action_error(&name))
        }
        action => Ok(Some(action)),
    }
}

fn app_shell_action_error(name: &str, err: serde_json::Error) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: format!("app_shell_action.{name}"),
        reason: format!("invalid action payload: {err}"),
    }
}

fn unknown_app_shell_action_error(name: &str) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: format!("app_shell_action.{name}"),
        reason: format!("unknown self-hosted app-shell action: {name}"),
    }
}

fn normalized_export_default_file_name(default_file_name: &str, extension: &str) -> String {
    let trimmed = default_file_name.trim();
    if !trimmed.is_empty() {
        return trimmed.to_owned();
    }

    if extension.is_empty() {
        "mondrian-export.mp4".to_owned()
    } else {
        format!("mondrian-export.{extension}")
    }
}

fn window_title_for_app_state(state: &AppState) -> String {
    let project = state
        .current_project_path
        .as_ref()
        .and_then(|path| path.file_stem())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Untitled");
    let sequence = state.sequence.as_ref().map(|sequence| sequence.name.as_str());
    match sequence {
        Some(sequence) if !sequence.trim().is_empty() => {
            format!("{project} - {sequence} - Mondrian")
        }
        _ => format!("{project} - Mondrian"),
    }
}

/// Root widget for the self-hosted editor window.
pub struct SelfHostedAppRoot {
    id: WidgetId,
    title_bar: TitleBar,
    dock: DockSplitter,
    models: SelfHostedPanelModels,
    asset_folder_id: Option<String>,
    preferences_model: SelfHostedPreferencesModel,
    workspace_preset: WorkspacePreset,
    modal: Option<ShellModal>,
    bounds: Rect,
}

impl SelfHostedAppRoot {
    /// Build a root widget from the current application state snapshot.
    pub fn from_app_state(state: &AppState) -> Self {
        Self::from_app_state_with_preferences(state, &SelfHostedPreferences::default())
    }

    /// Build a root widget from the current app state and self-hosted shell
    /// preferences.
    pub fn from_app_state_with_preferences(
        state: &AppState,
        preferences: &SelfHostedPreferences,
    ) -> Self {
        Self::from_app_state_with_preferences_and_thumbnails(state, preferences, None)
    }

    /// Build a root widget from app state, preferences, and an optional asset
    /// thumbnail source.
    pub fn from_app_state_with_preferences_and_thumbnails(
        state: &AppState,
        preferences: &SelfHostedPreferences,
        thumbnails: Option<&dyn AssetThumbnailSource>,
    ) -> Self {
        Self::new_with_preferences(
            TitleBar::new(
                window_title_for_app_state(state),
                MenuBar::for_app_state(state),
            ),
            SelfHostedPanelModels::from_app_state_with_asset_folder_and_thumbnails(
                state, None, thumbnails,
            ),
            SelfHostedPreferencesModel::from_app_state(
                state,
                preferences.workspace_preset,
                preferences.theme_preset,
            ),
            preferences.workspace_preset,
        )
    }

    /// Build a root widget from app-facing panel models.
    pub fn from_models(models: SelfHostedPanelModels) -> Self {
        Self::new(
            TitleBar::new("Mondrian", MenuBar::default()),
            models,
            WorkspacePreset::Editing,
        )
    }

    /// Build a root widget using test-only demo fixtures.
    #[cfg(test)]
    pub fn demo() -> Self {
        Self::new(
            TitleBar::new("Mondrian", MenuBar::default()),
            SelfHostedPanelModels::demo(),
            WorkspacePreset::Editing,
        )
    }

    /// Build a root widget from explicit shell parts.
    pub fn new(
        title_bar: TitleBar,
        models: SelfHostedPanelModels,
        workspace_preset: WorkspacePreset,
    ) -> Self {
        Self::new_with_preferences(
            title_bar,
            models,
            SelfHostedPreferencesModel::default(),
            workspace_preset,
        )
    }

    fn new_with_preferences(
        title_bar: TitleBar,
        models: SelfHostedPanelModels,
        preferences_model: SelfHostedPreferencesModel,
        workspace_preset: WorkspacePreset,
    ) -> Self {
        let dock = build_dock_tree_for_preset(models.clone(), workspace_preset);
        Self {
            id: WidgetId::new(),
            title_bar,
            dock,
            models,
            asset_folder_id: None,
            preferences_model,
            workspace_preset,
            modal: None,
            bounds: Rect::ZERO,
        }
    }

    /// Current built-in workspace preset used by the dock tree.
    pub fn workspace_preset(&self) -> WorkspacePreset {
        self.workspace_preset
    }

    /// Currently shown asset-library folder, or root when absent.
    pub fn asset_folder_id(&self) -> Option<&str> {
        self.asset_folder_id.as_deref()
    }

    /// Replace the shell-local asset-library browser folder.
    pub fn set_asset_folder_id(&mut self, folder_id: Option<String>) {
        self.asset_folder_id = folder_id;
    }

    /// Access the inner dock splitter for shell-owned grab zone cursor queries.
    pub fn dock(&self) -> &DockSplitter {
        &self.dock
    }

    /// Access the inner dock splitter for shell-owned layout state migration.
    pub fn dock_mut(&mut self) -> &mut DockSplitter {
        &mut self.dock
    }

    /// Replace panel contents from a fresh model snapshot while preserving the
    /// root widget id and menu state.
    pub fn set_models(&mut self, models: SelfHostedPanelModels) {
        let layout = self.dock.layout_snapshot();
        let dock_panel_state = collect_dock_panel_state(&self.dock);
        let asset_grid_state = collect_asset_grid_state(&self.dock);
        let panel_list_state = collect_panel_list_state(&self.dock);
        let panel_scroll_state = collect_panel_scroll_state(&self.dock);
        self.models = models;
        self.dock = build_dock_tree_for_preset(self.models.clone(), self.workspace_preset);
        self.dock.restore_layout(&layout);
        restore_dock_panel_state(&mut self.dock, &dock_panel_state);
        restore_asset_grid_state(&mut self.dock, &asset_grid_state);
        restore_panel_list_state(&mut self.dock, &panel_list_state);
        restore_panel_scroll_state(&mut self.dock, &panel_scroll_state);
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
            restore_dock_panel_state(&mut self.dock, &dock_panel_state);
            restore_asset_grid_state(&mut self.dock, &asset_grid_state);
            restore_panel_list_state(&mut self.dock, &panel_list_state);
            restore_panel_scroll_state(&mut self.dock, &panel_scroll_state);
        }
    }

    /// Refresh panel contents from the current application state snapshot.
    pub fn refresh_from_app_state(&mut self, state: &AppState) {
        let preferences = SelfHostedPreferences {
            version: 1,
            theme_preset: self.preferences_model.theme_preset,
            workspace_preset: self.workspace_preset,
            recent_projects: Vec::new(),
        };
        self.refresh_from_app_state_with_preferences(state, &preferences);
    }

    /// Refresh panel contents and preferences from a full self-hosted state
    /// snapshot.
    pub fn refresh_from_app_state_with_preferences(
        &mut self,
        state: &AppState,
        preferences: &SelfHostedPreferences,
    ) {
        self.refresh_from_app_state_with_preferences_and_thumbnails(state, preferences, None);
    }

    /// Refresh panel contents and preferences with an optional asset thumbnail
    /// source.
    pub fn refresh_from_app_state_with_preferences_and_thumbnails(
        &mut self,
        state: &AppState,
        preferences: &SelfHostedPreferences,
        thumbnails: Option<&dyn AssetThumbnailSource>,
    ) {
        self.title_bar = TitleBar::new(
            window_title_for_app_state(state),
            MenuBar::for_app_state(state),
        );
        self.set_models(
            SelfHostedPanelModels::from_app_state_with_asset_folder_and_thumbnails(
                state,
                self.asset_folder_id.as_deref(),
                thumbnails,
            ),
        );
        let preferences_model = SelfHostedPreferencesModel::from_app_state(
            state,
            self.workspace_preset,
            preferences.theme_preset,
        );
        self.preferences_model = preferences_model.clone();
        if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_preferences_mut) {
            dialog.set_model(preferences_model);
        }
    }

    /// Activate a dock panel or grouped tab in the default self-hosted layout.
    pub fn activate_panel(&mut self, panel: PanelKind) -> bool {
        let activated = dock_panel_locations(panel).into_iter().any(|(owner, active_index)| {
            activate_panel_in_widget(&mut self.dock, owner, active_index)
        });
        if activated && self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
        activated
    }

    /// Switch to a built-in workspace preset and rebuild the dock tree from the
    /// current shell models.
    pub fn switch_workspace(&mut self, preset: WorkspacePreset) {
        self.workspace_preset = preset;
        self.preferences_model.workspace = preset.display_name().to_owned();
        self.dock = build_dock_tree_for_preset(self.models.clone(), preset);
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    /// Apply a shell-local action and return an editor action when one should
    /// continue to [`AppState`](crate::app::AppState).
    pub fn handle_shell_action(
        &mut self,
        action: Action,
        platform: &dyn PlatformService,
        current_project_path: Option<&Path>,
    ) -> Option<Action> {
        match self.try_handle_shell_action(action, platform, current_project_path) {
            Ok(action) => action,
            Err(err) => {
                tracing::warn!("self-hosted shell action failed: {err}");
                None
            }
        }
    }

    /// Apply a shell-local action and report shell protocol errors.
    pub fn try_handle_shell_action(
        &mut self,
        action: Action,
        platform: &dyn PlatformService,
        current_project_path: Option<&Path>,
    ) -> Result<Option<Action>> {
        match action {
            Action::FocusPanel(panel) | Action::TogglePanel(panel) => {
                self.activate_panel(panel);
                Ok(None)
            }
            Action::SwitchWorkspace(preset) => {
                self.switch_workspace(preset);
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_NEW_PROJECT_DIALOG =>
            {
                self.modal = Some(ShellModal::new_project(SelfHostedNewProjectDraft::default()));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_NEW_PROJECT_DRAFT_CHANGED =>
            {
                if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_new_project_mut) {
                    if let Ok(update) =
                        serde_json::from_value::<NewProjectDraftUpdatePayload>(payload)
                    {
                        dialog.apply_update(update);
                    }
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CANCEL_NEW_PROJECT_DIALOG =>
            {
                self.modal = None;
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG =>
            {
                let draft = self
                    .modal
                    .as_ref()
                    .and_then(ShellModal::as_new_project)
                    .map(|dialog| dialog.draft().clone())
                    .unwrap_or_default();
                if draft.validate().is_err() {
                    return Ok(None);
                }
                let Some(path) = platform.save_file_dialog(
                    "Create Mondrian Project",
                    &default_project_file_name(&draft.name),
                    &project_file_filters(),
                ) else {
                    return Ok(None);
                };
                self.modal = None;
                Ok(Some(project_create_with_settings_action(
                    draft.into_payload(path),
                )))
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_ABOUT =>
            {
                self.modal = Some(ShellModal::about());
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES =>
            {
                self.modal = Some(ShellModal::preferences(self.preferences_model()));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_PREFERENCES_TAB_CHANGED =>
            {
                let tab_payload: PreferencesTabPayload = serde_json::from_value(payload)
                    .map_err(|err| app_shell_action_error(&name, err))?;
                let tab = PreferencesDialogTab::from(tab_payload);
                if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_preferences_mut) {
                    dialog.set_active_tab(tab);
                } else {
                    self.modal = Some(ShellModal::preferences_with_tab(
                        self.preferences_model(),
                        tab,
                    ));
                    if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                        self.layout(self.bounds);
                    }
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_CLOSE_MODAL =>
            {
                self.modal = None;
                Ok(None)
            }
            action => try_resolve_app_shell_action(action, platform, current_project_path),
        }
    }

    fn preferences_model(&self) -> SelfHostedPreferencesModel {
        self.preferences_model.clone()
    }
}

fn dock_panel_locations(panel: PanelKind) -> Vec<(PanelKind, usize)> {
    match panel {
        PanelKind::Assets => vec![(PanelKind::Assets, 0)],
        PanelKind::Effects => vec![(PanelKind::Assets, 1), (PanelKind::Effects, 0)],
        PanelKind::Viewer
        | PanelKind::Timeline
        | PanelKind::Inspector
        | PanelKind::NodeGraph
        | PanelKind::Export => vec![(panel, 0)],
    }
}

fn activate_panel_in_widget(
    widget: &mut dyn Widget,
    owner: PanelKind,
    active_index: usize,
) -> bool {
    if let Some(panel) = widget.as_any_mut().and_then(|any| any.downcast_mut::<DockPanel>()) {
        if panel.kind() == owner {
            panel.set_active_index(active_index);
            return true;
        }
    }

    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            if activate_panel_in_widget(child, owner, active_index) {
                return true;
            }
        }
    }
    false
}

fn collect_dock_panel_state(widget: &dyn Widget) -> Vec<DockPanelState> {
    let mut states = Vec::new();
    collect_dock_panel_state_into(widget, &mut states);
    states
}

fn collect_dock_panel_state_into(widget: &dyn Widget, states: &mut Vec<DockPanelState>) {
    if let Some(panel) = widget.as_any().and_then(|any| any.downcast_ref::<DockPanel>()) {
        states.push(DockPanelState {
            owner: panel.kind(),
            active_index: panel.active_index(),
        });
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            collect_dock_panel_state_into(child, states);
        }
    }
}

fn restore_dock_panel_state(widget: &mut dyn Widget, states: &[DockPanelState]) {
    if let Some(panel) = widget.as_any_mut().and_then(|any| any.downcast_mut::<DockPanel>()) {
        if let Some(state) = states.iter().find(|state| state.owner == panel.kind()) {
            panel.set_active_index(state.active_index);
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_dock_panel_state(child, states);
        }
    }
}

fn collect_asset_grid_state(widget: &dyn Widget) -> BTreeMap<String, AssetGridState> {
    let mut states = BTreeMap::new();
    collect_asset_grid_state_into(widget, &mut states);
    states
}

fn collect_asset_grid_state_into(
    widget: &dyn Widget,
    states: &mut BTreeMap<String, AssetGridState>,
) {
    if let Some(grid) = widget.as_any().and_then(|any| any.downcast_ref::<AssetGrid>()) {
        states.insert(grid.title().to_owned(), grid.state());
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            collect_asset_grid_state_into(child, states);
        }
    }
}

fn restore_asset_grid_state(widget: &mut dyn Widget, states: &BTreeMap<String, AssetGridState>) {
    if let Some(grid) = widget.as_any_mut().and_then(|any| any.downcast_mut::<AssetGrid>()) {
        if let Some(state) = states.get(grid.title()) {
            grid.restore_state(state);
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_asset_grid_state(child, states);
        }
    }
}

fn collect_panel_list_state(widget: &dyn Widget) -> BTreeMap<String, PanelListState> {
    let mut states = BTreeMap::new();
    collect_panel_list_state_into(widget, &mut states);
    states
}

fn collect_panel_list_state_into(
    widget: &dyn Widget,
    states: &mut BTreeMap<String, PanelListState>,
) {
    if let Some(list) = widget.as_any().and_then(|any| any.downcast_ref::<PanelList>()) {
        states.insert(list.title().to_owned(), list.state());
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            collect_panel_list_state_into(child, states);
        }
    }
}

fn restore_panel_list_state(widget: &mut dyn Widget, states: &BTreeMap<String, PanelListState>) {
    if let Some(list) = widget.as_any_mut().and_then(|any| any.downcast_mut::<PanelList>()) {
        if let Some(state) = states.get(list.title()) {
            list.restore_state(state);
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_panel_list_state(child, states);
        }
    }
}

fn collect_panel_scroll_state(widget: &dyn Widget) -> Vec<PanelScrollState> {
    let mut states = Vec::new();
    collect_panel_scroll_state_into(widget, None, &mut states);
    states
}

fn collect_panel_scroll_state_into(
    widget: &dyn Widget,
    owner: Option<PanelKind>,
    states: &mut Vec<PanelScrollState>,
) {
    let owner = widget.panel_kind().or(owner);
    if let (Some(owner), Some(scroll)) = (
        owner,
        widget.as_any().and_then(|any| any.downcast_ref::<ScrollView>()),
    ) {
        states.push(PanelScrollState {
            owner,
            ordinal: states.iter().filter(|state| state.owner == owner).count(),
            state: scroll.state(),
        });
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            collect_panel_scroll_state_into(child, owner, states);
        }
    }
}

fn restore_panel_scroll_state(widget: &mut dyn Widget, states: &[PanelScrollState]) {
    let mut restored = Vec::new();
    restore_panel_scroll_state_into(widget, None, states, &mut restored);
}

fn restore_panel_scroll_state_into(
    widget: &mut dyn Widget,
    owner: Option<PanelKind>,
    states: &[PanelScrollState],
    restored: &mut Vec<(PanelKind, usize)>,
) {
    let owner = widget.panel_kind().or(owner);
    if let Some(owner) = owner {
        if let Some(scroll) = widget.as_any_mut().and_then(|any| any.downcast_mut::<ScrollView>()) {
            let ordinal =
                restored.iter().filter(|(restored_owner, _)| *restored_owner == owner).count();
            if let Some(state) =
                states.iter().find(|state| state.owner == owner && state.ordinal == ordinal)
            {
                scroll.restore_state(&state.state);
            }
            restored.push((owner, ordinal));
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_panel_scroll_state_into(child, owner, states, restored);
        }
    }
}

impl Widget for SelfHostedAppRoot {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(800.0, 600.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.title_bar.layout(Rect::new(
            bounds.x,
            bounds.y,
            bounds.width,
            TITLE_BAR_HEIGHT,
        ));
        self.dock.layout(Rect::new(
            bounds.x,
            bounds.y + TITLE_BAR_HEIGHT,
            bounds.width,
            (bounds.height - TITLE_BAR_HEIGHT).max(0.0),
        ));
        if let Some(modal) = &mut self.modal {
            modal.layout(bounds);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(modal) = &mut self.modal {
            if modal.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
            return EventResult::Handled;
        }
        if self.title_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.dock.event(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.dock.paint(ctx);
        self.title_bar.paint(ctx);
        if let Some(modal) = &self.modal {
            modal.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        2 + usize::from(self.modal.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.dock),
            1 => Some(&self.title_bar),
            2 => self.modal.as_ref().map(|modal| modal as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.dock),
            1 => Some(&mut self.title_bar),
            2 => self.modal.as_mut().map(|modal| modal as &mut dyn Widget),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        app_shell_about_action, app_shell_cancel_new_project_dialog_action,
        app_shell_close_modal_action, app_shell_confirm_new_project_dialog_action,
        app_shell_export_output_dialog_action, app_shell_import_media_dialog_action,
        app_shell_import_media_dialog_action_with_target, app_shell_new_project_dialog_action,
        app_shell_new_project_draft_changed_action, app_shell_open_project_dialog_action,
        app_shell_open_recent_project_action, app_shell_preferences_action,
        app_shell_preferences_tab_changed_action, app_shell_recover_project_action,
        app_shell_reveal_in_file_manager_action, app_shell_save_project_as_dialog_action,
        AppShellOpenRecentProjectPayload, AppShellRevealInFileManagerPayload,
        AssetsImportFilesPayload, ExportDraftUpdatePayload, ExportOutputDialogPayload,
        ImportMediaDialogPayload, NewProjectDraftUpdatePayload, PreferencesTabPayload,
        ProjectCreateWithSettingsPayload, ProjectRecoverFromAutosavePayload, ASSETS_IMPORT_FILES,
        ASSETS_NAMESPACE, EXPORT_NAMESPACE, EXPORT_SET_DRAFT, PROJECT_CREATE_WITH_SETTINGS,
        PROJECT_NAMESPACE, PROJECT_RECOVER_FROM_AUTOSAVE,
    };
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use glam::Vec2;
    use mondrian_core::{Rational, Resolution};
    use mondrian_timeline::sequence::PreviewRenderFormat;
    use mondrian_ui_core::tree::WidgetTreeView;
    use mondrian_ui_core::widget::{DrawCommandEncoder, PaintContext};
    use mondrian_ui_core::EventRequests;
    use mondrian_ui_core::Widget;
    use mondrian_ui_events::hit_test::hit_test_deepest;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    #[derive(Default)]
    struct PaintOrderRecorder {
        texts: Vec<String>,
    }

    impl DrawCommandEncoder for PaintOrderRecorder {
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
            self.texts.push(text.to_owned());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: mondrian_core::Color,
        ) {
            self.texts.push(text.to_owned());
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[derive(Debug, Default)]
    struct FakePlatform {
        open_paths: Option<Vec<PathBuf>>,
        save_path: Option<PathBuf>,
        revealed_paths: Mutex<Vec<PathBuf>>,
    }

    impl PlatformService for FakePlatform {
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

        fn reveal_in_file_manager(&self, path: &Path) {
            self.revealed_paths.lock().expect("revealed path lock").push(path.to_path_buf());
        }

        fn send_notification(&self, _title: &str, _body: &str) {}
    }

    fn drag_root_splitter_to(root: &mut SelfHostedAppRoot, x: f32) {
        let grab = root.dock().collect_grab_zones()[0].0.center();
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseDown {
                    position: grab,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseMove {
                    position: Point::new(x, grab.y),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.dock_mut().event(
                &UiEvent::MouseUp {
                    position: Point::new(x, grab.y),
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
    }

    fn menu_mouse_down(
        menu: &mut MenuBar,
        ctx: &mut EventContext<'_>,
        position: Point,
    ) -> EventResult {
        menu.event(
            &UiEvent::MouseDown {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        )
    }

    fn menu_mouse_up(
        menu: &mut MenuBar,
        ctx: &mut EventContext<'_>,
        position: Point,
    ) -> EventResult {
        menu.event(
            &UiEvent::MouseUp {
                position,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            ctx,
        )
    }

    fn click_menu(menu: &mut MenuBar, ctx: &mut EventContext<'_>, position: Point) {
        menu_mouse_down(menu, ctx, position);
        menu_mouse_up(menu, ctx, position);
    }

    fn active_index_for_dock_panel(widget: &dyn Widget, kind: PanelKind) -> Option<usize> {
        if let Some(panel) = widget.as_any().and_then(|any| any.downcast_ref::<DockPanel>()) {
            if panel.kind() == kind {
                return Some(panel.active_index());
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                if let Some(active) = active_index_for_dock_panel(child, kind) {
                    return Some(active);
                }
            }
        }
        None
    }

    fn panel_list_state_for_title(widget: &dyn Widget, title: &str) -> Option<PanelListState> {
        if let Some(list) = widget.as_any().and_then(|any| any.downcast_ref::<PanelList>()) {
            if list.title() == title {
                return Some(list.state());
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                if let Some(state) = panel_list_state_for_title(child, title) {
                    return Some(state);
                }
            }
        }
        None
    }

    fn asset_grid_state_for_title(widget: &dyn Widget, title: &str) -> Option<AssetGridState> {
        if let Some(grid) = widget.as_any().and_then(|any| any.downcast_ref::<AssetGrid>()) {
            if grid.title() == title {
                return Some(grid.state());
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                if let Some(state) = asset_grid_state_for_title(child, title) {
                    return Some(state);
                }
            }
        }
        None
    }

    fn with_asset_grid_mut_for_title(
        widget: &mut dyn Widget,
        title: &str,
        update: &mut dyn FnMut(&mut AssetGrid),
    ) -> bool {
        if widget
            .as_any()
            .and_then(|any| any.downcast_ref::<AssetGrid>())
            .is_some_and(|grid| grid.title() == title)
        {
            if let Some(grid) = widget.as_any_mut().and_then(|any| any.downcast_mut::<AssetGrid>())
            {
                update(grid);
                return true;
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index) {
                if with_asset_grid_mut_for_title(child, title, update) {
                    return true;
                }
            }
        }
        false
    }

    fn with_panel_list_mut_for_title(
        widget: &mut dyn Widget,
        title: &str,
        update: &mut dyn FnMut(&mut PanelList),
    ) -> bool {
        if widget
            .as_any()
            .and_then(|any| any.downcast_ref::<PanelList>())
            .is_some_and(|list| list.title() == title)
        {
            if let Some(list) = widget.as_any_mut().and_then(|any| any.downcast_mut::<PanelList>())
            {
                update(list);
                return true;
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index) {
                if with_panel_list_mut_for_title(child, title, update) {
                    return true;
                }
            }
        }
        false
    }

    fn scroll_state_for_panel(widget: &dyn Widget, panel: PanelKind) -> Option<ScrollViewState> {
        collect_panel_scroll_state(widget)
            .into_iter()
            .find(|state| state.owner == panel && state.ordinal == 0)
            .map(|state| state.state)
    }

    fn with_scroll_view_mut_for_panel(
        widget: &mut dyn Widget,
        panel: PanelKind,
        update: &mut dyn FnMut(&mut ScrollView),
    ) -> bool {
        with_scroll_view_mut_for_panel_inner(widget, None, panel, update)
    }

    fn with_scroll_view_mut_for_panel_inner(
        widget: &mut dyn Widget,
        owner: Option<PanelKind>,
        panel: PanelKind,
        update: &mut dyn FnMut(&mut ScrollView),
    ) -> bool {
        let owner = widget.panel_kind().or(owner);
        if owner == Some(panel)
            && widget.as_any().and_then(|any| any.downcast_ref::<ScrollView>()).is_some()
        {
            if let Some(scroll) =
                widget.as_any_mut().and_then(|any| any.downcast_mut::<ScrollView>())
            {
                update(scroll);
                return true;
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index) {
                if with_scroll_view_mut_for_panel_inner(child, owner, panel, update) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn app_root_focus_panel_ignores_absent_export_panel_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action =
            root.handle_shell_action(Action::FocusPanel(PanelKind::Export), &platform, None);

        assert_eq!(action, None);
        assert_eq!(active_index_for_dock_panel(&root, PanelKind::Export), None);
    }

    #[test]
    fn app_root_toggle_panel_activates_grouped_effects_tab_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action =
            root.handle_shell_action(Action::TogglePanel(PanelKind::Effects), &platform, None);

        assert_eq!(action, None);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(1)
        );
    }

    #[test]
    fn app_root_switch_workspace_rebuilds_dock_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Export),
            &platform,
            None,
        );

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Export);
        assert!((root.dock().ratio() - 0.55).abs() < f32::EPSILON);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Export),
            Some(0)
        );
    }

    #[test]
    fn app_root_focus_panel_handles_direct_panels_after_workspace_switch() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Color),
            &platform,
            None,
        );

        let action =
            root.handle_shell_action(Action::FocusPanel(PanelKind::Effects), &platform, None);

        assert_eq!(action, None);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Effects),
            Some(0)
        );
    }

    #[test]
    fn media_import_filters_cover_video_and_audio_extensions() {
        let filters = media_import_filters();

        assert!(filters.iter().any(
            |filter| filter.name == "Video" && filter.extensions.iter().any(|ext| ext == "mp4")
        ));
        assert!(filters.iter().any(
            |filter| filter.name == "Audio" && filter.extensions.iter().any(|ext| ext == "wav")
        ));
    }

    #[test]
    fn project_file_filters_cover_mondrian_project_extension() {
        let filters = project_file_filters();

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "Mondrian Project");
        assert!(filters[0]
            .extensions
            .iter()
            .any(|extension| extension == PROJECT_FILE_EXTENSION));
    }

    #[test]
    fn export_output_filters_normalize_requested_extension() {
        let filters = export_output_filters(".MP4 ");

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "Export");
        assert_eq!(filters[0].extensions, vec!["mp4"]);
    }

    #[test]
    fn export_output_filters_fall_back_for_empty_extension() {
        let filters = export_output_filters(" . ");

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "Media");
        assert!(filters[0].extensions.iter().any(|extension| extension == "mp4"));
        assert!(filters[0].extensions.iter().any(|extension| extension == "gif"));
    }

    #[test]
    fn new_project_draft_uses_path_stem_and_preserves_settings_payload() {
        let path = PathBuf::from("E:/projects/Trailer Cut.mdp");
        let mut draft = SelfHostedNewProjectDraft::from_project_path(&path);
        draft.sequence_settings.resolution = Resolution { width: 4096, height: 2160 };
        draft.sequence_settings.frame_rate = Rational::FPS_24;
        draft.sequence_settings.preview.format = PreviewRenderFormat::DnxHrLb;
        draft.project_settings.proxy_enabled = false;

        let payload = draft.into_payload(path.clone());

        assert_eq!(payload.project_file, path);
        assert_eq!(payload.name, "Trailer Cut");
        assert_eq!(payload.sequence_settings.resolution.width, 4096);
        assert_eq!(payload.sequence_settings.frame_rate, Rational::FPS_24);
        assert_eq!(
            payload.sequence_settings.preview.format,
            PreviewRenderFormat::DnxHrLb
        );
        assert!(!payload.project_settings.proxy_enabled);
    }

    #[test]
    fn new_project_draft_validates_sequence_settings() {
        let mut draft = SelfHostedNewProjectDraft::default();

        assert!(draft.validate().is_ok());

        draft.sequence_settings.resolution.width = 1;
        assert!(draft.validate().is_err());
    }

    #[test]
    fn resolve_app_shell_open_project_dialog_returns_open_action() {
        let platform = FakePlatform {
            open_paths: Some(vec![PathBuf::from("E:/projects/cut.mdp")]),
            ..FakePlatform::default()
        };

        let action =
            resolve_app_shell_action(app_shell_open_project_dialog_action(), &platform, None);

        assert_eq!(
            action,
            Some(Action::OpenProject(PathBuf::from("E:/projects/cut.mdp")))
        );
    }

    #[test]
    fn resolve_app_shell_open_recent_project_returns_open_action() {
        let platform = FakePlatform::default();
        let project_file = PathBuf::from("E:/projects/recent.mdp");

        let action = resolve_app_shell_action(
            app_shell_open_recent_project_action(AppShellOpenRecentProjectPayload {
                project_file: project_file.clone(),
            }),
            &platform,
            None,
        );

        assert_eq!(action, Some(Action::OpenProject(project_file)));
    }

    #[test]
    fn resolve_app_shell_recover_project_returns_project_recovery_action() {
        let platform = FakePlatform::default();
        let payload = ProjectRecoverFromAutosavePayload {
            project_file: PathBuf::from("E:/projects/recover.mdp"),
            autosave_file: PathBuf::from("E:/runtime/autosave/project.autosave.mdp"),
        };

        let action = resolve_app_shell_action(
            app_shell_recover_project_action(payload.clone()),
            &platform,
            None,
        )
        .expect("recover action");

        let Action::Custom { namespace, name, payload: actual } = action else {
            panic!("expected project custom action");
        };
        assert_eq!(namespace, PROJECT_NAMESPACE);
        assert_eq!(name, PROJECT_RECOVER_FROM_AUTOSAVE);
        assert_eq!(
            serde_json::from_value::<ProjectRecoverFromAutosavePayload>(actual)
                .expect("recovery payload"),
            payload
        );
    }

    #[test]
    fn resolve_app_shell_new_project_dialog_returns_create_project_action() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/projects/My Cut.mdp")),
            ..FakePlatform::default()
        };

        let action =
            resolve_app_shell_action(app_shell_new_project_dialog_action(), &platform, None)
                .expect("new project action");

        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected custom project action");
        };
        assert_eq!(namespace, PROJECT_NAMESPACE);
        assert_eq!(name, PROJECT_CREATE_WITH_SETTINGS);
        let payload: ProjectCreateWithSettingsPayload =
            serde_json::from_value(payload).expect("project create payload");
        assert_eq!(
            payload.project_file,
            PathBuf::from("E:/projects/My Cut.mdp")
        );
        assert_eq!(payload.name, "My Cut");
        assert!(payload.sequence_settings.validate().is_ok());
    }

    #[test]
    fn app_root_handles_new_project_dialog_draft_and_confirm() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/projects/Rough Cut.mdp")),
            ..FakePlatform::default()
        };
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(
            root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None),
            None
        );
        assert!(root.modal.as_ref().and_then(ShellModal::as_new_project).is_some());
        assert_eq!(root.child_count(), 3);

        assert_eq!(
            root.handle_shell_action(
                app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::Name(
                    "Rough Cut".into(),
                )),
                &platform,
                None
            ),
            None
        );

        let action = root
            .handle_shell_action(
                app_shell_confirm_new_project_dialog_action(),
                &platform,
                None,
            )
            .expect("confirm action");

        assert!(root.modal.is_none());
        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected project create action");
        };
        assert_eq!(namespace, PROJECT_NAMESPACE);
        assert_eq!(name, PROJECT_CREATE_WITH_SETTINGS);
        let payload: ProjectCreateWithSettingsPayload =
            serde_json::from_value(payload).expect("project create payload");
        assert_eq!(payload.name, "Rough Cut");
        assert_eq!(
            payload.project_file,
            PathBuf::from("E:/projects/Rough Cut.mdp")
        );
    }

    #[test]
    fn app_root_applies_new_project_setting_updates_to_payload() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/projects/UHD.mdp")),
            ..FakePlatform::default()
        };
        let mut root = SelfHostedAppRoot::demo();

        root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None);
        root.handle_shell_action(
            app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::Resolution(
                Resolution::UHD4K,
            )),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::FrameRate(
                Rational::FPS_23976,
            )),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_new_project_draft_changed_action(
                NewProjectDraftUpdatePayload::AudioSampleRate(96_000),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::ProxyEnabled(
                false,
            )),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_new_project_draft_changed_action(
                NewProjectDraftUpdatePayload::PreviewCacheEnabled(false),
            ),
            &platform,
            None,
        );

        let action = root
            .handle_shell_action(
                app_shell_confirm_new_project_dialog_action(),
                &platform,
                None,
            )
            .expect("confirm action");
        let Action::Custom { payload, .. } = action else {
            panic!("expected project create action");
        };
        let payload: ProjectCreateWithSettingsPayload =
            serde_json::from_value(payload).expect("project create payload");

        assert_eq!(payload.sequence_settings.resolution, Resolution::UHD4K);
        assert_eq!(payload.sequence_settings.frame_rate, Rational::FPS_23976);
        assert_eq!(payload.sequence_settings.audio_sample_rate, 96_000);
        assert!(!payload.project_settings.proxy_enabled);
        assert!(!payload.sequence_settings.preview.cache_enabled);
    }

    #[test]
    fn app_root_cancels_new_project_dialog_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();

        root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None);

        assert_eq!(
            root.handle_shell_action(
                app_shell_cancel_new_project_dialog_action(),
                &platform,
                None
            ),
            None
        );
        assert!(root.modal.is_none());
    }

    #[test]
    fn app_root_handles_about_action_as_shell_modal() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(app_shell_about_action(), &platform, None);

        assert_eq!(action, None);
        assert!(root.modal.as_ref().and_then(ShellModal::as_about).is_some());
        assert_eq!(root.child_count(), 3);
    }

    #[test]
    fn app_root_handles_preferences_action_as_shell_modal() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(app_shell_preferences_action(), &platform, None);

        assert_eq!(action, None);
        assert!(root.modal.as_ref().and_then(ShellModal::as_preferences).is_some());
        assert_eq!(root.child_count(), 3);
    }

    #[test]
    fn app_root_switches_preferences_tab_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();

        root.handle_shell_action(app_shell_preferences_action(), &platform, None);
        let action = root.handle_shell_action(
            app_shell_preferences_tab_changed_action(PreferencesTabPayload::Shortcuts),
            &platform,
            None,
        );

        assert_eq!(action, None);
        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_preferences)
            .expect("preferences dialog");
        assert_eq!(dialog.active_tab(), PreferencesDialogTab::Shortcuts);
    }

    #[test]
    fn app_root_refresh_updates_open_preferences_model() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.handle_shell_action(app_shell_preferences_action(), &platform, None);

        let mut state = AppState::new();
        state.current_project_path = Some(PathBuf::from("E:/projects/live.mdp"));
        state.sequence = Some(mondrian_timeline::sequence::Sequence::new("Live"));
        state.auto_proxy_enabled = true;
        root.refresh_from_app_state(&state);

        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_preferences)
            .expect("preferences dialog");
        assert!(dialog.model().project_status.contains("live.mdp"));
        assert!(dialog.model().sequence_summary.contains("Live"));
        assert_eq!(dialog.model().proxy_mode, "Enabled");
    }

    #[test]
    fn app_root_closes_shell_modal_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();

        root.handle_shell_action(app_shell_about_action(), &platform, None);

        let action = root.handle_shell_action(app_shell_close_modal_action(), &platform, None);

        assert_eq!(action, None);
        assert!(root.modal.is_none());
    }

    #[test]
    fn app_root_modal_blocks_unhandled_keyboard_events() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.handle_shell_action(app_shell_about_action(), &platform, None);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        let result = root.event(
            &UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert!(root.modal.as_ref().and_then(ShellModal::as_about).is_some());
    }

    #[test]
    fn resolve_app_shell_import_dialog_returns_import_action() {
        let paths = vec![
            PathBuf::from("E:/media/a.mov"),
            PathBuf::from("E:/media/b.wav"),
        ];
        let platform = FakePlatform {
            open_paths: Some(paths.clone()),
            ..FakePlatform::default()
        };

        let action =
            resolve_app_shell_action(app_shell_import_media_dialog_action(), &platform, None);

        assert_eq!(action, Some(Action::ImportMedia(paths)));
    }

    #[test]
    fn resolve_app_shell_import_dialog_with_folder_returns_asset_import_action() {
        let paths = vec![
            PathBuf::from("E:/media/a.mov"),
            PathBuf::from("E:/media/b.wav"),
        ];
        let platform = FakePlatform {
            open_paths: Some(paths.clone()),
            ..FakePlatform::default()
        };

        let action = resolve_app_shell_action(
            app_shell_import_media_dialog_action_with_target(ImportMediaDialogPayload {
                folder_id: Some("rushes".to_owned()),
            }),
            &platform,
            None,
        );

        let Some(Action::Custom { namespace, name, payload }) = action else {
            panic!("expected assets import action");
        };
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_IMPORT_FILES);
        let payload: AssetsImportFilesPayload =
            serde_json::from_value(payload).expect("assets import payload");
        assert_eq!(payload.paths, paths);
        assert_eq!(payload.folder_id.as_deref(), Some("rushes"));
    }

    #[test]
    fn try_resolve_app_shell_reveal_file_manager_invokes_platform_only() {
        let platform = FakePlatform::default();
        let target = PathBuf::from("E:/media/a.mp4");

        let action = try_resolve_app_shell_action(
            app_shell_reveal_in_file_manager_action(AppShellRevealInFileManagerPayload {
                path: target.clone(),
            }),
            &platform,
            None,
        )
        .expect("resolve reveal action");

        assert_eq!(action, None);
        assert_eq!(
            platform.revealed_paths.lock().expect("revealed path lock").as_slice(),
            &[target]
        );
    }

    #[test]
    fn resolve_app_shell_save_as_dialog_returns_save_action() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/projects/out.mdp")),
            ..FakePlatform::default()
        };

        let action = resolve_app_shell_action(
            app_shell_save_project_as_dialog_action(),
            &platform,
            Some(Path::new("E:/projects/current.mdp")),
        );

        assert_eq!(
            action,
            Some(Action::SaveProjectAs(PathBuf::from("E:/projects/out.mdp")))
        );
    }

    #[test]
    fn resolve_app_shell_export_output_dialog_returns_draft_update() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/renders/deliverable.mp4")),
            ..FakePlatform::default()
        };

        let action = resolve_app_shell_action(
            app_shell_export_output_dialog_action(ExportOutputDialogPayload {
                default_file_name: "rough-cut.mp4".to_owned(),
                extension: ".MP4".to_owned(),
            }),
            &platform,
            None,
        )
        .expect("export output action");

        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected export draft action");
        };
        assert_eq!(namespace, EXPORT_NAMESPACE);
        assert_eq!(name, EXPORT_SET_DRAFT);
        let payload: ExportDraftUpdatePayload =
            serde_json::from_value(payload).expect("export draft payload");
        assert_eq!(
            payload,
            ExportDraftUpdatePayload::OutputPath(
                PathBuf::from("E:/renders/deliverable.mp4").display().to_string()
            )
        );
    }

    #[test]
    fn resolve_app_shell_dialog_cancel_returns_none() {
        let platform = FakePlatform::default();

        let action =
            resolve_app_shell_action(app_shell_open_project_dialog_action(), &platform, None);

        assert_eq!(action, None);
    }

    #[test]
    fn resolve_app_shell_non_dialog_action_passes_through() {
        let platform = FakePlatform::default();

        let action = resolve_app_shell_action(Action::SaveProject, &platform, None);

        assert_eq!(action, Some(Action::SaveProject));
    }

    #[test]
    fn try_resolve_app_shell_unknown_action_returns_protocol_error() {
        let platform = FakePlatform::default();

        let err = try_resolve_app_shell_action(
            Action::Custom {
                namespace: APP_SHELL_NAMESPACE.into(),
                name: "missing_command".into(),
                payload: serde_json::Value::Null,
            },
            &platform,
            None,
        )
        .expect_err("unknown app-shell command should fail");

        match err {
            MondrianError::WorkflowStepFailed { step_id, reason } => {
                assert_eq!(step_id, "app_shell_action.missing_command");
                assert!(reason.contains("unknown self-hosted app-shell action"));
            }
            other => panic!("expected app-shell workflow error, got {other:?}"),
        }
    }

    #[test]
    fn app_root_layout_reserves_title_bar_height_for_dock() {
        let mut root = SelfHostedAppRoot::demo();

        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let zones = root.dock().collect_grab_zones();

        assert_eq!(
            root.title_bar.bounds(),
            Rect::new(0.0, 0.0, 1280.0, TITLE_BAR_HEIGHT)
        );
        assert_eq!(zones[0].0.y, TITLE_BAR_HEIGHT);
        assert_eq!(zones[0].0.height, 720.0 - TITLE_BAR_HEIGHT);
        assert_eq!(root.child_count(), 2);
    }

    #[test]
    fn app_root_child_order_matches_bottom_to_top_z_order() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(root.child(0).map(Widget::id), Some(root.dock.id()));
        assert_eq!(root.child(1).map(Widget::id), Some(root.title_bar.id()));

        let platform = FakePlatform::default();
        let action = root.handle_shell_action(app_shell_about_action(), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.child_count(), 3);
        assert_eq!(
            root.child(2).map(Widget::id),
            root.modal.as_ref().map(Widget::id)
        );
    }

    #[test]
    fn app_root_paints_in_bottom_to_top_z_order() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let mut encoder = PaintOrderRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 1280.0, 720.0),
        };

        root.paint(&mut ctx);

        let first_menu_text = encoder
            .texts
            .iter()
            .position(|text| text == "File")
            .expect("menu bar should paint");
        assert!(
            first_menu_text > 0,
            "dock content must paint before menu chrome: {:?}",
            encoder.texts
        );

        let platform = FakePlatform::default();
        root.handle_shell_action(app_shell_about_action(), &platform, None);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let mut encoder = PaintOrderRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 1280.0, 720.0),
        };

        root.paint(&mut ctx);

        let menu_text = encoder
            .texts
            .iter()
            .position(|text| text == "File")
            .expect("menu bar should paint");
        let modal_text = encoder
            .texts
            .iter()
            .rposition(|text| text == "Mondrian")
            .expect("modal title should paint");
        assert!(menu_text < modal_text, "modal must paint above menu chrome");
    }

    #[test]
    fn app_root_modal_overlay_wins_over_open_menu_overlay() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = |_| {};
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        click_menu(
            root.title_bar.menu_bar_mut(),
            &mut ctx,
            Point::new(118.0, 14.0),
        );
        let platform = FakePlatform::default();
        root.handle_shell_action(app_shell_about_action(), &platform, None);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let modal_id = root.modal.as_ref().map(Widget::id).expect("active modal");
        let tree = WidgetTreeView::new(&mut root);

        let hit = hit_test_deepest(&tree, Point::new(50.0, 50.0));

        assert_eq!(hit, Some(modal_id));
    }

    #[test]
    fn app_root_builds_from_app_state_snapshot() {
        let state = AppState::new();
        let mut root = SelfHostedAppRoot::from_app_state(&state);

        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(
            root.title_bar.bounds(),
            Rect::new(0.0, 0.0, 1280.0, TITLE_BAR_HEIGHT)
        );
        assert_eq!(
            root.title_bar.menu_bar().bounds().height,
            crate::self_hosted::menu_bar::MENU_BAR_HEIGHT
        );
        assert!(!root.dock().collect_grab_zones().is_empty());
    }

    #[test]
    fn set_models_preserves_user_splitter_ratio() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        drag_root_splitter_to(&mut root, 620.0);

        let dragged_ratio = root.dock().ratio();
        assert!(dragged_ratio > 0.4);

        root.set_models(SelfHostedPanelModels::demo());

        assert!((root.dock().ratio() - dragged_ratio).abs() < f32::EPSILON);
    }

    #[test]
    fn set_models_preserves_asset_grid_filter_and_selection() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(with_asset_grid_mut_for_title(
            root.dock_mut(),
            "Assets",
            &mut |grid| {
                grid.set_filter_query("audio");
                grid.set_selected(Some(1));
            },
        ));

        root.set_models(SelfHostedPanelModels::demo());

        let state = asset_grid_state_for_title(&root, "Assets").expect("assets state");
        assert_eq!(state.filter_query, "audio");
        assert_eq!(state.selected_item_id.as_deref(), Some("demo-audio"));
        assert_eq!(state.selected_index, Some(1));
    }

    #[test]
    fn set_models_preserves_grouped_panel_active_tab_and_visible_list_state() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(Action::FocusPanel(PanelKind::Effects), &platform, None);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(1)
        );
        assert!(with_panel_list_mut_for_title(
            root.dock_mut(),
            "Effects",
            &mut |list| {
                list.set_filter_query("blur");
            },
        ));

        root.set_models(SelfHostedPanelModels::demo());

        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(1)
        );
        let state = panel_list_state_for_title(&root, "Effects").expect("effects state");
        assert_eq!(state.filter_query, "blur");
    }

    #[test]
    fn set_models_preserves_panel_scroll_position() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 480.0));
        assert!(with_scroll_view_mut_for_panel(
            root.dock_mut(),
            PanelKind::Inspector,
            &mut |scroll| {
                scroll.set_scroll_offset(Vec2::new(0.0, 96.0));
            },
        ));
        let before = scroll_state_for_panel(root.dock(), PanelKind::Inspector)
            .expect("inspector should have a scroll view");
        assert!(
            before.scroll_offset.y > 0.0,
            "test fixture must overflow vertically"
        );

        root.set_models(SelfHostedPanelModels::demo());

        let after = scroll_state_for_panel(root.dock(), PanelKind::Inspector)
            .expect("inspector should keep a scroll view");
        assert_eq!(after.scroll_offset, before.scroll_offset);
    }

    #[test]
    fn refresh_from_app_state_preserves_user_splitter_ratio() {
        let state = AppState::new();
        let mut root = SelfHostedAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        drag_root_splitter_to(&mut root, 620.0);
        let dragged_ratio = root.dock().ratio();

        root.refresh_from_app_state(&state);

        assert!((root.dock().ratio() - dragged_ratio).abs() < f32::EPSILON);
    }

    #[test]
    fn refresh_from_app_state_preserves_workspace_preset() {
        let state = AppState::new();
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Compositing),
            &platform,
            None,
        );

        root.refresh_from_app_state(&state);

        assert_eq!(root.workspace_preset(), WorkspacePreset::Compositing);
        assert!((root.dock().ratio() - 0.35).abs() < f32::EPSILON);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::NodeGraph),
            Some(0)
        );
    }
}
