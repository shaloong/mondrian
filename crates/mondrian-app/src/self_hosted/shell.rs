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
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use std::path::Path;

use crate::app::ui_actions::{
    app_shell_about_action, app_shell_import_media_dialog_action,
    app_shell_new_project_dialog_action, app_shell_open_project_dialog_action,
    app_shell_quit_action, app_shell_save_project_as_dialog_action, export_set_draft_action,
    project_create_with_settings_action, ExportDraftUpdatePayload, ExportOutputDialogPayload,
    NewProjectDraftUpdatePayload, APP_SHELL_ABOUT, APP_SHELL_CANCEL_NEW_PROJECT_DIALOG,
    APP_SHELL_CLOSE_MODAL, APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_EXPORT_OUTPUT_DIALOG,
    APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG,
    APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, APP_SHELL_OPEN_PROJECT_DIALOG,
    APP_SHELL_SAVE_PROJECT_AS_DIALOG,
};
use crate::app::AppState;
use crate::self_hosted::modal::ShellModal;
use crate::self_hosted::new_project_dialog::{
    default_project_file_name, SelfHostedNewProjectDraft,
};
use crate::self_hosted::panels::{build_dock_tree_for_preset, SelfHostedPanelModels};
use mondrian_core::{MondrianError, Result};

/// Height reserved for the self-hosted top menu bar.
pub const MENU_BAR_HEIGHT: f32 = 28.0;

/// Default file extension for Mondrian project containers.
pub const PROJECT_FILE_EXTENSION: &str = "mdp";

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
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
        {
            let Some(paths) = platform.open_file_dialog("Import Media", &media_import_filters())
            else {
                return Ok(None);
            };
            Ok((!paths.is_empty()).then_some(Action::ImportMedia(paths)))
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

/// Default Mondrian menu structure for self-hosted shells.
pub fn default_menu_items() -> Vec<(&'static str, Vec<MenuItem>)> {
    vec![
        (
            "File",
            vec![
                MenuItem::new("New Project...", app_shell_new_project_dialog_action()),
                MenuItem::new("Open Project...", app_shell_open_project_dialog_action()),
                MenuItem::new("Import Media...", app_shell_import_media_dialog_action()),
                MenuItem::new("Save", Action::SaveProject),
                MenuItem::new("Save As...", app_shell_save_project_as_dialog_action()),
                MenuItem::separator(),
                MenuItem::new("Close Project", Action::CloseProject),
                MenuItem::new("Quit", app_shell_quit_action()),
            ],
        ),
        (
            "Edit",
            vec![
                MenuItem::new("Undo", Action::Undo),
                MenuItem::new("Redo", Action::Redo),
                MenuItem::new("Cut", Action::Cut),
                MenuItem::new("Copy", Action::Copy),
                MenuItem::new("Paste", Action::Paste),
            ],
        ),
        (
            "View",
            vec![
                MenuItem::new("Viewer", Action::FocusPanel(PanelKind::Viewer)),
                MenuItem::new("Timeline", Action::FocusPanel(PanelKind::Timeline)),
                MenuItem::new("Inspector", Action::FocusPanel(PanelKind::Inspector)),
                MenuItem::separator(),
                MenuItem::new("Assets", Action::FocusPanel(PanelKind::Assets)),
                MenuItem::new("Effects", Action::FocusPanel(PanelKind::Effects)),
                MenuItem::new("Project", Action::FocusPanel(PanelKind::Project)),
                MenuItem::new("Console", Action::FocusPanel(PanelKind::Console)),
                MenuItem::new("Node Graph", Action::FocusPanel(PanelKind::NodeGraph)),
                MenuItem::new("Export", Action::FocusPanel(PanelKind::Export)),
                MenuItem::separator(),
                MenuItem::new("Toggle Fullscreen", Action::ToggleFullscreen),
            ],
        ),
        (
            "Workspace",
            vec![
                MenuItem::new("Editing", Action::SwitchWorkspace(WorkspacePreset::Editing)),
                MenuItem::new("Color", Action::SwitchWorkspace(WorkspacePreset::Color)),
                MenuItem::new("Audio", Action::SwitchWorkspace(WorkspacePreset::Audio)),
                MenuItem::new(
                    "Compositing",
                    Action::SwitchWorkspace(WorkspacePreset::Compositing),
                ),
                MenuItem::new("Export", Action::SwitchWorkspace(WorkspacePreset::Export)),
            ],
        ),
        (
            "Help",
            vec![MenuItem::new("About Mondrian", app_shell_about_action())],
        ),
    ]
}

/// Horizontal menu bar wrapping dropdown widgets.
pub struct MenuBar {
    id: WidgetId,
    menus: Vec<Dropdown>,
    bounds: Rect,
}

impl Default for MenuBar {
    fn default() -> Self {
        Self::new(default_menu_items())
    }
}

impl MenuBar {
    /// Build a menu bar from explicit menu definitions.
    pub fn new(items: Vec<(&'static str, Vec<MenuItem>)>) -> Self {
        let menus = items.into_iter().map(|(label, items)| Dropdown::new(label, items)).collect();
        Self { id: WidgetId::new(), menus, bounds: Rect::ZERO }
    }

    fn open_menu_index(&self) -> Option<usize> {
        self.menus.iter().position(Dropdown::is_open)
    }

    fn trigger_index_at(&self, position: Point) -> Option<usize> {
        self.menus.iter().position(|menu| menu.trigger_contains(position))
    }

    fn close_other_menus(&mut self, target: usize, ctx: &mut EventContext) {
        for (index, menu) in self.menus.iter_mut().enumerate() {
            if index != target {
                menu.close_menu(ctx);
            }
        }
    }

    fn switch_open_menu_to(&mut self, target: usize, ctx: &mut EventContext) {
        self.close_other_menus(target, ctx);
        self.menus[target].open_menu(ctx);
    }
}

impl Widget for MenuBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        Size::new(600.0, MENU_BAR_HEIGHT)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let mut x = bounds.x;
        for menu in &mut self.menus {
            menu.layout(Rect::new(x, bounds.y, 100.0, MENU_BAR_HEIGHT));
            x += 100.0;
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let UiEvent::MouseMove { position, .. } = event {
            if let (Some(open), Some(target)) =
                (self.open_menu_index(), self.trigger_index_at(*position))
            {
                if open != target {
                    self.switch_open_menu_to(target, ctx);
                    return EventResult::Handled;
                }
            }
        }

        if let UiEvent::MouseDown { position, button: MouseButton::Left, .. } = event {
            if let (Some(open), Some(target)) =
                (self.open_menu_index(), self.trigger_index_at(*position))
            {
                if open != target {
                    self.close_other_menus(target, ctx);
                    return self.menus[target].event(event, ctx);
                }
            }
        }

        for menu in &mut self.menus {
            if menu.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let bar_bg = Rect::new(
            self.bounds.x,
            self.bounds.y,
            self.bounds.width,
            MENU_BAR_HEIGHT,
        );
        ctx.encoder.draw_rect(bar_bg, ctx.theme.colors.card, 0.0);
        for menu in &self.menus {
            menu.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        self.menus.len()
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        self.menus.get(index).map(|menu| menu as &dyn Widget)
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        self.menus.get_mut(index).map(|menu| menu as &mut dyn Widget)
    }
}

/// Root widget for the self-hosted editor window.
pub struct SelfHostedAppRoot {
    id: WidgetId,
    menu_bar: MenuBar,
    dock: DockSplitter,
    models: SelfHostedPanelModels,
    workspace_preset: WorkspacePreset,
    modal: Option<ShellModal>,
    bounds: Rect,
}

impl SelfHostedAppRoot {
    /// Build a root widget from the current application state snapshot.
    pub fn from_app_state(state: &AppState) -> Self {
        Self::from_models(SelfHostedPanelModels::from_app_state(state))
    }

    /// Build a root widget from app-facing panel models.
    pub fn from_models(models: SelfHostedPanelModels) -> Self {
        Self::new(MenuBar::default(), models, WorkspacePreset::Editing)
    }

    /// Build a root widget using test-only demo fixtures.
    #[cfg(test)]
    pub fn demo() -> Self {
        Self::new(
            MenuBar::default(),
            SelfHostedPanelModels::demo(),
            WorkspacePreset::Editing,
        )
    }

    /// Build a root widget from explicit shell parts.
    pub fn new(
        menu_bar: MenuBar,
        models: SelfHostedPanelModels,
        workspace_preset: WorkspacePreset,
    ) -> Self {
        let dock = build_dock_tree_for_preset(models.clone(), workspace_preset);
        Self {
            id: WidgetId::new(),
            menu_bar,
            dock,
            models,
            workspace_preset,
            modal: None,
            bounds: Rect::ZERO,
        }
    }

    /// Current built-in workspace preset used by the dock tree.
    pub fn workspace_preset(&self) -> WorkspacePreset {
        self.workspace_preset
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
        self.models = models;
        self.dock = build_dock_tree_for_preset(self.models.clone(), self.workspace_preset);
        self.dock.restore_layout(&layout);
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    /// Refresh panel contents from the current application state snapshot.
    pub fn refresh_from_app_state(&mut self, state: &AppState) {
        self.set_models(SelfHostedPanelModels::from_app_state(state));
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
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_CLOSE_MODAL =>
            {
                self.modal = None;
                Ok(None)
            }
            action => try_resolve_app_shell_action(action, platform, current_project_path),
        }
    }
}

fn dock_panel_locations(panel: PanelKind) -> Vec<(PanelKind, usize)> {
    match panel {
        PanelKind::Assets => vec![(PanelKind::Assets, 0)],
        PanelKind::Effects => vec![(PanelKind::Assets, 1), (PanelKind::Effects, 0)],
        PanelKind::Project => vec![(PanelKind::Console, 0), (PanelKind::Project, 0)],
        PanelKind::Console => vec![(PanelKind::Console, 1), (PanelKind::Console, 0)],
        PanelKind::Export => vec![(PanelKind::Console, 2), (PanelKind::Export, 0)],
        PanelKind::Viewer | PanelKind::Timeline | PanelKind::Inspector | PanelKind::NodeGraph => {
            vec![(panel, 0)]
        }
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

impl Widget for SelfHostedAppRoot {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(800.0, 600.0))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.menu_bar
            .layout(Rect::new(bounds.x, bounds.y, bounds.width, MENU_BAR_HEIGHT));
        self.dock.layout(Rect::new(
            bounds.x,
            bounds.y + MENU_BAR_HEIGHT,
            bounds.width,
            (bounds.height - MENU_BAR_HEIGHT).max(0.0),
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
        if self.menu_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.dock.event(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.menu_bar.paint(ctx);
        self.dock.paint(ctx);
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
            1 => Some(&self.menu_bar),
            2 => self.modal.as_ref().map(|modal| modal as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.dock),
            1 => Some(&mut self.menu_bar),
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
        app_shell_new_project_dialog_action, app_shell_new_project_draft_changed_action,
        app_shell_open_project_dialog_action, app_shell_save_project_as_dialog_action,
        ExportDraftUpdatePayload, ExportOutputDialogPayload, NewProjectDraftUpdatePayload,
        ProjectCreateWithSettingsPayload, EXPORT_NAMESPACE, EXPORT_SET_DRAFT,
        PROJECT_CREATE_WITH_SETTINGS, PROJECT_NAMESPACE,
    };
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::{Rational, Resolution};
    use mondrian_timeline::sequence::PreviewRenderFormat;
    use mondrian_ui_core::EventRequests;
    use mondrian_ui_core::Widget;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    #[derive(Debug, Default)]
    struct FakePlatform {
        open_paths: Option<Vec<PathBuf>>,
        save_path: Option<PathBuf>,
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

        fn reveal_in_file_manager(&self, _path: &Path) {}

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

    #[test]
    fn default_menu_bar_exposes_primary_menu_groups() {
        let menu = MenuBar::default();

        assert_eq!(menu.child_count(), 5);
    }

    #[test]
    fn default_menu_items_expose_all_panels_and_builtin_workspaces() {
        let menu_items = default_menu_items();
        let view_items = menu_items
            .iter()
            .find_map(|(label, items)| (*label == "View").then_some(items))
            .expect("view menu");
        let workspace_items = menu_items
            .iter()
            .find_map(|(label, items)| (*label == "Workspace").then_some(items))
            .expect("workspace menu");

        for panel in PanelKind::ALL {
            assert!(
                view_items.iter().any(|item| item.action == Action::FocusPanel(panel)),
                "missing panel menu item for {panel:?}"
            );
        }
        for preset in [
            WorkspacePreset::Editing,
            WorkspacePreset::Color,
            WorkspacePreset::Audio,
            WorkspacePreset::Compositing,
            WorkspacePreset::Export,
        ] {
            assert!(
                workspace_items
                    .iter()
                    .any(|item| item.action == Action::SwitchWorkspace(preset)),
                "missing workspace menu item for {preset:?}"
            );
        }
    }

    #[test]
    fn menu_bar_switches_open_menu_on_trigger_click() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(0.0, 0.0, 500.0, MENU_BAR_HEIGHT));
        let dispatched = Rc::new(RefCell::new(Vec::new()));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = {
            let dispatched = Rc::clone(&dispatched);
            move |action| dispatched.borrow_mut().push(action)
        };
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        click_menu(&mut menu, &mut ctx, Point::new(10.0, 10.0));
        click_menu(&mut menu, &mut ctx, Point::new(410.0, 10.0));
        click_menu(&mut menu, &mut ctx, Point::new(410.0, 42.0));

        assert_eq!(dispatched.borrow().len(), 1);
        let action = dispatched.borrow()[0].clone();
        match &action {
            Action::Custom { namespace, name, .. } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_ABOUT);
            }
            other => panic!("expected about action after menu switch, got {other:?}"),
        }
    }

    #[test]
    fn menu_bar_switches_open_menu_on_trigger_hover() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(0.0, 0.0, 500.0, MENU_BAR_HEIGHT));
        let dispatched = Rc::new(RefCell::new(Vec::new()));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let dispatch = {
            let dispatched = Rc::clone(&dispatched);
            move |action| dispatched.borrow_mut().push(action)
        };
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );

        click_menu(&mut menu, &mut ctx, Point::new(10.0, 10.0));

        assert_eq!(
            menu.event(
                &UiEvent::MouseMove {
                    position: Point::new(410.0, 10.0),
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        click_menu(&mut menu, &mut ctx, Point::new(410.0, 42.0));

        assert_eq!(dispatched.borrow().len(), 1);
        let action = dispatched.borrow()[0].clone();
        match &action {
            Action::Custom { namespace, name, .. } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_ABOUT);
            }
            other => panic!("expected about action after hover menu switch, got {other:?}"),
        }
    }

    #[test]
    fn default_menu_items_use_stable_app_shell_about_action() {
        let menu_items = default_menu_items();
        let help_items = menu_items
            .iter()
            .find_map(|(label, items)| (*label == "Help").then_some(items))
            .expect("help menu");
        let about = help_items
            .iter()
            .find(|item| item.label == "About Mondrian")
            .expect("about item");

        match &about.action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, APP_SHELL_ABOUT);
                assert!(payload.is_null());
            }
            other => panic!("expected app-shell about action, got {other:?}"),
        }
    }

    #[test]
    fn default_menu_items_separate_close_project_from_quit() {
        let menu_items = default_menu_items();
        let file_items = menu_items
            .iter()
            .find_map(|(label, items)| (*label == "File").then_some(items))
            .expect("file menu");
        let close_project = file_items
            .iter()
            .find(|item| item.label == "Close Project")
            .expect("close project item");
        let quit = file_items.iter().find(|item| item.label == "Quit").expect("quit item");

        assert_eq!(close_project.action, Action::CloseProject);
        match &quit.action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, crate::app::ui_actions::APP_SHELL_QUIT);
                assert!(payload.is_null());
            }
            other => panic!("expected app-shell quit action, got {other:?}"),
        }
    }

    #[test]
    fn default_menu_items_expose_window_fullscreen_command() {
        let menu_items = default_menu_items();
        let view_items = menu_items
            .iter()
            .find_map(|(label, items)| (*label == "View").then_some(items))
            .expect("view menu");
        let fullscreen = view_items
            .iter()
            .find(|item| item.label == "Toggle Fullscreen")
            .expect("fullscreen item");

        assert_eq!(fullscreen.action, Action::ToggleFullscreen);
    }

    #[test]
    fn app_root_focus_panel_activates_grouped_export_tab_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action =
            root.handle_shell_action(Action::FocusPanel(PanelKind::Export), &platform, None);

        assert_eq!(action, None);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Console),
            Some(2)
        );
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
            save_path: None,
        };

        let action =
            resolve_app_shell_action(app_shell_open_project_dialog_action(), &platform, None);

        assert_eq!(
            action,
            Some(Action::OpenProject(PathBuf::from("E:/projects/cut.mdp")))
        );
    }

    #[test]
    fn resolve_app_shell_new_project_dialog_returns_create_project_action() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/projects/My Cut.mdp")),
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
        let platform = FakePlatform { open_paths: Some(paths.clone()), save_path: None };

        let action =
            resolve_app_shell_action(app_shell_import_media_dialog_action(), &platform, None);

        assert_eq!(action, Some(Action::ImportMedia(paths)));
    }

    #[test]
    fn resolve_app_shell_save_as_dialog_returns_save_action() {
        let platform = FakePlatform {
            open_paths: None,
            save_path: Some(PathBuf::from("E:/projects/out.mdp")),
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
    fn app_root_layout_reserves_top_menu_height_for_dock() {
        let mut root = SelfHostedAppRoot::demo();

        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let zones = root.dock().collect_grab_zones();

        assert_eq!(root.menu_bar.bounds, Rect::new(0.0, 0.0, 1280.0, 28.0));
        assert_eq!(zones[0].0.y, MENU_BAR_HEIGHT);
        assert_eq!(zones[0].0.height, 720.0 - MENU_BAR_HEIGHT);
        assert_eq!(root.child_count(), 2);
    }

    #[test]
    fn app_root_child_order_matches_bottom_to_top_z_order() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(root.child(0).map(Widget::id), Some(root.dock.id()));
        assert_eq!(root.child(1).map(Widget::id), Some(root.menu_bar.id()));

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
    fn app_root_builds_from_app_state_snapshot() {
        let state = AppState::new();
        let mut root = SelfHostedAppRoot::from_app_state(&state);

        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(root.menu_bar.bounds, Rect::new(0.0, 0.0, 1280.0, 28.0));
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
