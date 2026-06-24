//! Reusable widget shell for app UI Mondrian windows.
//!
//! Developer binaries own native window setup and event-loop plumbing. This
//! module owns the reusable root widget composition above the dock/panel layer.

use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_export::queue::JobStatus;
use mondrian_platform::{FileFilter, PlatformService};
use mondrian_timeline::Sequence;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_widgets::dock_panel::DockPanel;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::{
    AssetGrid, AssetGridState, PanelList, PanelListState, ScrollView, ScrollViewState,
    TimelineView, TimelineViewState,
};
use std::path::Path;

use crate::app::ui_actions::{
    assets_import_files_action, assets_relink_asset_action, export_set_draft_action,
    project_create_with_settings_action, project_recover_from_autosave_action,
    sequence_update_settings_action, AppShellOpenRecentProjectPayload,
    AppShellRelinkAssetDialogPayload, AppShellRelocatePanelPayload,
    AppShellRevealInFileManagerPayload, AssetsImportFilesPayload, AssetsRelinkAssetPayload,
    DockDropAreaPayload, ExportDraftUpdatePayload, ExportOutputDialogPayload,
    ImportMediaDialogPayload, NewProjectDraftUpdatePayload, PreferencesTabPayload,
    ProjectRecoverFromAutosavePayload, SequenceSettingsDraftUpdatePayload,
    SequenceSettingsTabPayload, ViewerSetZoomScalePayload, APP_SHELL_ABOUT,
    APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_CONFIRM_SEQUENCE_SETTINGS,
    APP_SHELL_EXPORT_OUTPUT_DIALOG, APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_NEW_PROJECT_DRAFT_CHANGED,
    APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_OPEN_RECENT_PROJECT, APP_SHELL_PREFERENCES,
    APP_SHELL_PREFERENCES_TAB_CHANGED, APP_SHELL_RECOVER_PROJECT, APP_SHELL_RELINK_ASSET_DIALOG,
    APP_SHELL_RELOCATE_PANEL, APP_SHELL_REVEAL_IN_FILE_MANAGER, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
    APP_SHELL_SEQUENCE_SETTINGS, APP_SHELL_SEQUENCE_SETTINGS_DRAFT_CHANGED,
    APP_SHELL_SEQUENCE_SETTINGS_TAB_CHANGED, VIEWER_CYCLE_ZOOM, VIEWER_NAMESPACE,
    VIEWER_SET_ZOOM_SCALE,
};
use crate::app::AppState;
use crate::app_ui::menu_bar::MenuBar;
use crate::app_ui::modal::ShellModal;
use crate::app_ui::new_project_dialog::{default_project_file_name, AppUiNewProjectDraft};
use crate::app_ui::panels::{
    build_dock_tree_for_preset, build_dock_tree_from_layout, AppUiPanelModels,
    AssetThumbnailSource, ViewerPreviewSource,
};
use crate::app_ui::pending_close_dialog::PendingCloseDialogAction;
use crate::app_ui::preferences_dialog::{AppUiPreferencesModel, PreferencesDialogTab};
use crate::app_ui::preferences_store::AppUiPreferences;
use crate::app_ui::sequence_settings_dialog::AppUiSequenceSettingsDraft;
use crate::app_ui::title_bar::{TitleBar, TITLE_BAR_HEIGHT};
use crate::app_ui::workspace_layout::{AppUiWorkspaceLayout, DockDropArea};
use mondrian_core::{MondrianError, Result};

/// Default file extension for Mondrian project containers.
pub const PROJECT_FILE_EXTENSION: &str = "mdp";
const STATUS_BAR_HEIGHT: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DockPanelState {
    owner: PanelKind,
    active_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct AssetGridLocalState {
    owner: PanelKind,
    ordinal: usize,
    state: AssetGridState,
}

#[derive(Debug, Clone, PartialEq)]
struct PanelListLocalState {
    owner: PanelKind,
    ordinal: usize,
    state: PanelListState,
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
        "Mondrian 项目",
        vec![PROJECT_FILE_EXTENSION],
    )]
}

/// File dialog filters for media import commands.
pub fn media_import_filters() -> Vec<FileFilter> {
    vec![
        FileFilter::new("视频", vec!["mp4", "mov", "mkv", "webm", "avi"]),
        FileFilter::new("音频", vec!["mp3", "wav", "aac", "flac", "m4a"]),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusBarModel {
    message: String,
    is_error: bool,
    is_busy: bool,
    context: String,
}

struct StatusBar {
    id: WidgetId,
    bounds: Rect,
    model: StatusBarModel,
}

impl StatusBar {
    fn set_model(&mut self, model: StatusBarModel) {
        self.model = model;
    }
}

impl Widget for StatusBar {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(240.0, STATUS_BAR_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, _event: &UiEvent, _ctx: &mut EventContext) -> EventResult {
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let font_size = ctx.theme.typography.metadata.font_size;
        let padding = 10.0;
        let text_gap = 16.0;
        ctx.push_clip(self.bounds);
        ctx.encoder.draw_rect(self.bounds, colors.background, 0.0);
        ctx.encoder.draw_line(
            Point::new(self.bounds.x, self.bounds.y),
            Point::new(self.bounds.x + self.bounds.width, self.bounds.y),
            1.0,
            {
                let mut border = colors.border;
                border.a *= 0.72;
                border
            },
        );

        let message_color = if self.model.is_error {
            colors.destructive_foreground
        } else if self.model.is_busy {
            colors.primary
        } else {
            colors.muted_foreground
        };
        let text_y = self.bounds.y + ((self.bounds.height - font_size) * 0.5).max(0.0);

        let content_width = (self.bounds.width - padding * 2.0).max(0.0);
        let context_text = if self.model.context.is_empty() {
            String::new()
        } else {
            elide_text_to_width(&self.model.context, font_size, content_width * 0.38)
        };
        let context_width = estimate_text_width(&context_text, font_size);
        let message_max_width = if context_text.is_empty() {
            content_width
        } else {
            (content_width - context_width - text_gap).max(0.0)
        };
        let message_text = elide_text_to_width(&self.model.message, font_size, message_max_width);
        if !message_text.is_empty() {
            ctx.encoder.draw_text(
                &message_text,
                font_size,
                Point::new(self.bounds.x + padding, text_y),
                message_color,
            );
        }

        if !context_text.is_empty() {
            let context_x =
                (self.bounds.x + self.bounds.width - context_width - padding).max(self.bounds.x);
            ctx.encoder.draw_text(
                &context_text,
                font_size,
                Point::new(context_x, text_y),
                colors.muted_foreground,
            );
        }
        ctx.pop_clip();
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

fn elide_text_to_width(text: &str, font_size: f32, max_width: f32) -> String {
    if text.is_empty() || max_width <= 0.0 {
        return String::new();
    }
    if estimate_text_width(text, font_size) <= max_width {
        return text.to_owned();
    }

    let suffix = "...";
    if estimate_text_width(suffix, font_size) > max_width {
        return String::new();
    }

    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        let candidate = format!("{out}{suffix}");
        if estimate_text_width(&candidate, font_size) > max_width {
            out.pop();
            break;
        }
    }
    format!("{out}{suffix}")
}

fn status_bar_model(state: &AppState) -> StatusBarModel {
    let jobs = state.render_queue.list_jobs();
    let active_jobs = jobs
        .iter()
        .filter(|job| {
            matches!(
                job.status,
                JobStatus::Pending | JobStatus::Rendering { .. } | JobStatus::Encoding
            )
        })
        .collect::<Vec<_>>();

    let (message, is_error, is_busy) = if let Some(job) = active_jobs.first() {
        let message = match &job.status {
            JobStatus::Pending => format!("导出队列处理中（{}）", active_jobs.len()),
            JobStatus::Rendering { frame, total_frames } => format!(
                "正在导出帧 {}/{}（队列 {}）",
                frame,
                total_frames,
                active_jobs.len()
            ),
            JobStatus::Encoding => format!("正在编码（队列 {}）", active_jobs.len()),
            _ => "导出处理中".to_owned(),
        };
        (message, false, true)
    } else if state.is_playing() && state.is_playback_buffering() {
        ("预览缓冲中...".to_owned(), false, true)
    } else if let Some((message, is_error)) = &state.status_hint {
        (message.clone(), *is_error, false)
    } else {
        ("就绪".to_owned(), false, false)
    };

    let context = state
        .sequence
        .as_ref()
        .map(|sequence| sequence.name.clone())
        .or_else(|| {
            state
                .current_project_path
                .as_ref()
                .and_then(|path| path.file_stem())
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "没有项目".to_owned());

    StatusBarModel { message, is_error, is_busy, context }
}

/// File dialog filter for timeline export output commands.
pub fn export_output_filters(extension: &str) -> Vec<FileFilter> {
    let extension = normalized_export_extension(extension);
    if extension.is_empty() {
        vec![FileFilter::new(
            "媒体",
            vec!["mp4", "mov", "mkv", "gif", "mxf", "webm"],
        )]
    } else {
        vec![FileFilter::new("导出", vec![extension])]
    }
}

fn normalized_export_extension(extension: &str) -> String {
    extension.trim().trim_start_matches('.').trim().to_ascii_lowercase()
}

/// Resolve an app-shell action into a concrete editor action.
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
            tracing::warn!("app-shell action failed: {err}");
            None
        }
    }
}

/// Resolve an app-shell action and report protocol errors.
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
                "创建 Mondrian 项目",
                &format!("未命名.{PROJECT_FILE_EXTENSION}"),
                &project_file_filters(),
            );
            let Some(path) = path else {
                return Ok(None);
            };
            let draft = AppUiNewProjectDraft::from_project_path(&path);
            Ok(Some(project_create_with_settings_action(
                draft.into_payload(path),
            )))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_OPEN_PROJECT_DIALOG =>
        {
            let Some(paths) =
                platform.open_file_dialog("打开 Mondrian 项目", &project_file_filters())
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
            let Some(paths) = platform.open_file_dialog("导入媒体", &media_import_filters())
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
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_RELINK_ASSET_DIALOG =>
        {
            let payload: AppShellRelinkAssetDialogPayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            let Some(paths) = platform.open_file_dialog("重新链接媒体", &media_import_filters())
            else {
                return Ok(None);
            };
            Ok(paths.into_iter().next().map(|path| {
                assets_relink_asset_action(AssetsRelinkAssetPayload {
                    asset_id: payload.asset_id,
                    path,
                })
            }))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            let default_name = current_project_path
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("未命名.{PROJECT_FILE_EXTENSION}"));
            Ok(platform
                .save_file_dialog("另存 Mondrian 项目", &default_name, &project_file_filters())
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
                    "选择导出输出",
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
        reason: format!("unknown app-shell action: {name}"),
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
            format!("{project} · {sequence} — Mondrian")
        }
        _ => format!("{project} — Mondrian"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewerZoomMode {
    Fit,
    Fixed(u16),
}

impl ViewerZoomMode {
    fn next(self) -> Self {
        match self {
            Self::Fit => Self::Fixed(50),
            Self::Fixed(50) => Self::Fixed(100),
            Self::Fixed(100) => Self::Fixed(200),
            Self::Fixed(200) => Self::Fit,
            Self::Fixed(_) => Self::Fit,
        }
    }

    fn label(self) -> String {
        match self {
            Self::Fit => "适合".to_owned(),
            Self::Fixed(percent) => format!("{percent}%"),
        }
    }

    fn scale(self) -> Option<f32> {
        match self {
            Self::Fit => None,
            Self::Fixed(percent) => Some(percent as f32 / 100.0),
        }
    }

    fn from_scale(scale: Option<f32>) -> Self {
        let Some(scale) = scale.filter(|scale| scale.is_finite() && *scale > 0.0) else {
            return Self::Fit;
        };
        Self::Fixed((scale * 100.0).round().clamp(1.0, 3200.0) as u16)
    }
}

fn apply_viewer_zoom_mode(models: &mut AppUiPanelModels, mode: ViewerZoomMode) {
    models.viewer.zoom_label = mode.label();
    models.viewer.zoom_scale = mode.scale();
}

/// Root widget for the app UI editor window.
pub struct AppUiAppRoot {
    id: WidgetId,
    title_bar: TitleBar,
    dock: DockSplitter,
    status_bar: StatusBar,
    models: AppUiPanelModels,
    asset_folder_id: Option<String>,
    preferences_model: AppUiPreferencesModel,
    workspace_preset: WorkspacePreset,
    custom_workspace_layout: Option<AppUiWorkspaceLayout>,
    viewer_zoom_mode: ViewerZoomMode,
    active_sequence: Option<Sequence>,
    modal: Option<ShellModal>,
    bounds: Rect,
}

impl AppUiAppRoot {
    /// Build a root widget from the current application state snapshot.
    pub fn from_app_state(state: &AppState) -> Self {
        Self::from_app_state_with_preferences(state, &AppUiPreferences::default())
    }

    /// Build a root widget from the current app state and app UI shell
    /// preferences.
    pub fn from_app_state_with_preferences(
        state: &AppState,
        preferences: &AppUiPreferences,
    ) -> Self {
        Self::from_app_state_with_preferences_and_thumbnails(state, preferences, None)
    }

    /// Build a root widget from app state, preferences, and an optional asset
    /// thumbnail source.
    pub fn from_app_state_with_preferences_and_thumbnails(
        state: &AppState,
        preferences: &AppUiPreferences,
        thumbnails: Option<&dyn AssetThumbnailSource>,
    ) -> Self {
        Self::from_app_state_with_preferences_thumbnails_and_preview(
            state,
            preferences,
            thumbnails,
            None,
        )
    }

    /// Build a root widget from app state, preferences, optional thumbnails,
    /// and an optional viewer preview source.
    pub fn from_app_state_with_preferences_thumbnails_and_preview(
        state: &AppState,
        preferences: &AppUiPreferences,
        thumbnails: Option<&dyn AssetThumbnailSource>,
        preview: Option<&dyn ViewerPreviewSource>,
    ) -> Self {
        let viewer_zoom_mode = ViewerZoomMode::Fit;
        let mut models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
            state, None, thumbnails, preview,
        );
        apply_viewer_zoom_mode(&mut models, viewer_zoom_mode);
        let mut root = Self::new_with_preferences(
            TitleBar::new(
                window_title_for_app_state(state),
                MenuBar::for_app_state_with_shortcut_overrides(
                    state,
                    &preferences.shortcut_overrides,
                ),
            ),
            models,
            AppUiPreferencesModel::from_app_state_with_shortcut_overrides(
                state,
                preferences.workspace_preset,
                preferences.theme_preset,
                &preferences.shortcut_overrides,
            ),
            preferences.workspace_preset,
            preferences.custom_workspace_layout.clone(),
            status_bar_model(state),
        );
        root.active_sequence = state.sequence.clone();
        root
    }

    /// Build a root widget from app-facing panel models.
    pub fn from_models(models: AppUiPanelModels) -> Self {
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
            AppUiPanelModels::demo(),
            WorkspacePreset::Editing,
        )
    }

    /// Build a root widget from explicit shell parts.
    pub fn new(
        title_bar: TitleBar,
        models: AppUiPanelModels,
        workspace_preset: WorkspacePreset,
    ) -> Self {
        Self::new_with_preferences(
            title_bar,
            models,
            AppUiPreferencesModel::default(),
            workspace_preset,
            None,
            status_bar_model(&AppState::new()),
        )
    }

    fn new_with_preferences(
        title_bar: TitleBar,
        mut models: AppUiPanelModels,
        preferences_model: AppUiPreferencesModel,
        workspace_preset: WorkspacePreset,
        custom_workspace_layout: Option<AppUiWorkspaceLayout>,
        status_bar_model: StatusBarModel,
    ) -> Self {
        let viewer_zoom_mode = ViewerZoomMode::Fit;
        apply_viewer_zoom_mode(&mut models, viewer_zoom_mode);
        let dock = build_dock_tree_for_workspace(
            models.clone(),
            workspace_preset,
            custom_workspace_layout.as_ref(),
        );
        let mut root = Self {
            id: WidgetId::new(),
            title_bar,
            dock,
            status_bar: StatusBar {
                id: WidgetId::new(),
                bounds: Rect::ZERO,
                model: status_bar_model,
            },
            models,
            asset_folder_id: None,
            preferences_model,
            workspace_preset,
            custom_workspace_layout,
            viewer_zoom_mode,
            active_sequence: None,
            modal: None,
            bounds: Rect::ZERO,
        };
        root.refresh_shell_menu_checked_state();
        root
    }

    /// Current built-in workspace preset used by the dock tree.
    pub fn workspace_preset(&self) -> WorkspacePreset {
        self.workspace_preset
    }

    /// Persistable custom layout currently associated with the root.
    pub fn custom_workspace_layout(&self) -> Option<&AppUiWorkspaceLayout> {
        self.custom_workspace_layout.as_ref()
    }

    /// Capture the live dock tree as a persistable workspace layout.
    pub fn workspace_layout(&self) -> Option<AppUiWorkspaceLayout> {
        let layout = AppUiWorkspaceLayout::from_dock(&self.dock)?;
        if self.workspace_preset == WorkspacePreset::Custom {
            if let Some(previous) = self.custom_workspace_layout.as_ref() {
                return Some(layout.with_panel_metadata_from(previous));
            }
        }
        Some(layout)
    }

    fn refresh_shell_menu_checked_state(&mut self) {
        let layout = self.workspace_layout();
        self.title_bar
            .refresh_shell_menu_checked_state(self.workspace_preset, layout.as_ref());
    }

    /// Promote modified built-in layouts to Custom and refresh the custom
    /// layout snapshot. Returns true when the shell preference snapshot changed.
    pub fn sync_custom_workspace_layout_from_dock(&mut self) -> bool {
        let Some(layout) = self.workspace_layout() else {
            return false;
        };
        let promoted = self.workspace_preset != WorkspacePreset::Custom
            && !self.current_split_layout_matches_builtin_preset();
        if promoted {
            self.workspace_preset = WorkspacePreset::Custom;
            self.preferences_model.workspace = WorkspacePreset::Custom.display_name().to_owned();
        }

        if self.workspace_preset != WorkspacePreset::Custom {
            return false;
        }

        let changed = promoted || self.custom_workspace_layout.as_ref() != Some(&layout);
        if changed {
            self.custom_workspace_layout = Some(layout);
            self.refresh_shell_menu_checked_state();
        }

        changed
    }

    /// Currently shown asset-library folder, or root when absent.
    pub fn asset_folder_id(&self) -> Option<&str> {
        self.asset_folder_id.as_deref()
    }

    /// Replace the shell-local asset-library browser folder.
    pub fn set_asset_folder_id(&mut self, folder_id: Option<String>) {
        self.asset_folder_id = folder_id;
    }

    /// Show the pending-close confirmation modal.
    pub fn show_pending_close_dialog(&mut self, action: PendingCloseDialogAction) {
        self.modal = Some(ShellModal::pending_close(action));
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
    }

    /// Close the pending-close confirmation modal when it is active.
    pub fn close_pending_close_dialog(&mut self) {
        if self.modal.as_ref().and_then(ShellModal::as_pending_close).is_some() {
            self.modal = None;
        }
    }

    /// Whether the pending-close confirmation modal is currently active.
    pub fn has_pending_close_dialog(&self) -> bool {
        self.modal.as_ref().and_then(ShellModal::as_pending_close).is_some()
    }

    /// Access the inner dock splitter for shell-owned grab zone cursor queries.
    pub fn dock(&self) -> &DockSplitter {
        &self.dock
    }

    /// Access the inner dock splitter for shell-owned layout state migration.
    pub fn dock_mut(&mut self) -> &mut DockSplitter {
        &mut self.dock
    }

    /// Menu bar bounds exposed for root-level event tests.
    #[cfg(test)]
    pub(crate) fn menu_bar_bounds_for_test(&self) -> Rect {
        self.title_bar.menu_bar().bounds()
    }

    /// Replace panel contents from a fresh model snapshot while preserving the
    /// root widget id and menu state.
    pub fn set_models(&mut self, models: AppUiPanelModels) {
        let layout = self.dock.layout_snapshot();
        let dock_panel_state = collect_dock_panel_state(&self.dock);
        let asset_grid_state = collect_asset_grid_state(&self.dock);
        let panel_list_state = collect_panel_list_state(&self.dock);
        let timeline_state = collect_timeline_view_state(&self.dock);
        let panel_scroll_state = collect_panel_scroll_state(&self.dock);
        self.models = models;
        apply_viewer_zoom_mode(&mut self.models, self.viewer_zoom_mode);
        self.dock = build_dock_tree_for_workspace(
            self.models.clone(),
            self.workspace_preset,
            self.custom_workspace_layout.as_ref(),
        );
        self.dock.restore_layout(&layout);
        restore_dock_panel_state(&mut self.dock, &dock_panel_state);
        restore_asset_grid_state(&mut self.dock, &asset_grid_state);
        restore_panel_list_state(&mut self.dock, &panel_list_state);
        restore_timeline_view_state(&mut self.dock, &timeline_state);
        restore_panel_scroll_state(&mut self.dock, &panel_scroll_state);
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
            restore_dock_panel_state(&mut self.dock, &dock_panel_state);
            restore_asset_grid_state(&mut self.dock, &asset_grid_state);
            restore_panel_list_state(&mut self.dock, &panel_list_state);
            restore_timeline_view_state(&mut self.dock, &timeline_state);
            restore_panel_scroll_state(&mut self.dock, &panel_scroll_state);
        }
    }

    /// Refresh panel contents from the current application state snapshot.
    pub fn refresh_from_app_state(&mut self, state: &AppState) {
        let preferences = AppUiPreferences {
            version: 1,
            theme_preset: self.preferences_model.theme_preset,
            workspace_preset: self.workspace_preset,
            recent_projects: Vec::new(),
            shortcut_overrides: Vec::new(),
            custom_workspace_layout: self.custom_workspace_layout.clone(),
        };
        self.refresh_from_app_state_with_preferences(state, &preferences);
    }

    /// Refresh panel contents and preferences from a full app UI state
    /// snapshot.
    pub fn refresh_from_app_state_with_preferences(
        &mut self,
        state: &AppState,
        preferences: &AppUiPreferences,
    ) {
        self.refresh_from_app_state_with_preferences_and_thumbnails(state, preferences, None);
    }

    /// Refresh panel contents and preferences with an optional asset thumbnail
    /// source.
    pub fn refresh_from_app_state_with_preferences_and_thumbnails(
        &mut self,
        state: &AppState,
        preferences: &AppUiPreferences,
        thumbnails: Option<&dyn AssetThumbnailSource>,
    ) {
        self.refresh_from_app_state_with_preferences_thumbnails_and_preview(
            state,
            preferences,
            thumbnails,
            None,
        );
    }

    /// Refresh panel contents and preferences with optional asset thumbnail and
    /// viewer preview sources.
    pub fn refresh_from_app_state_with_preferences_thumbnails_and_preview(
        &mut self,
        state: &AppState,
        preferences: &AppUiPreferences,
        thumbnails: Option<&dyn AssetThumbnailSource>,
        preview: Option<&dyn ViewerPreviewSource>,
    ) {
        self.title_bar = TitleBar::new(
            window_title_for_app_state(state),
            MenuBar::for_app_state_with_shortcut_overrides(state, &preferences.shortcut_overrides),
        );
        self.status_bar.set_model(status_bar_model(state));
        self.active_sequence = state.sequence.clone();
        let mut models = AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_and_preview(
            state,
            self.asset_folder_id.as_deref(),
            thumbnails,
            preview,
        );
        apply_viewer_zoom_mode(&mut models, self.viewer_zoom_mode);
        self.set_models(models);
        let preferences_model = AppUiPreferencesModel::from_app_state_with_shortcut_overrides(
            state,
            self.workspace_preset,
            preferences.theme_preset,
            &preferences.shortcut_overrides,
        );
        self.preferences_model = preferences_model.clone();
        if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_preferences_mut) {
            dialog.set_model(preferences_model);
        }
        self.refresh_shell_menu_checked_state();
    }

    /// Activate a dock panel or grouped tab in the default app UI layout.
    pub fn activate_panel(&mut self, panel: PanelKind) -> bool {
        let activated = dock_panel_locations(panel).into_iter().any(|(owner, active_index)| {
            activate_panel_in_widget(&mut self.dock, owner, active_index)
        });
        if activated && self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
        activated
    }

    /// Activate a panel, switching to a built-in workspace when the current
    /// dock tree does not contain that panel.
    pub fn focus_panel(&mut self, panel: PanelKind) {
        if self.activate_panel(panel) {
            return;
        }
        self.switch_workspace(preferred_workspace_for_panel(panel));
        self.activate_panel(panel);
    }

    /// Toggle a direct dock panel leaf or a visible grouped tab.
    pub fn toggle_panel(&mut self, panel: PanelKind) {
        if self.hide_panel(panel) {
            return;
        }
        self.focus_panel(panel);
    }

    /// Relocate one docked panel tab relative to another visible panel group.
    pub fn relocate_panel(
        &mut self,
        panel: PanelKind,
        target: PanelKind,
        area: DockDropArea,
    ) -> bool {
        let Some(layout) = self.workspace_layout() else {
            return false;
        };
        let Some(next_layout) = layout.relocate_panel(panel, target, area) else {
            return false;
        };
        if self.workspace_layout().as_ref() == Some(&next_layout) {
            return false;
        }
        let Some(dock) = build_dock_tree_from_layout(self.models.clone(), &next_layout) else {
            return false;
        };

        self.workspace_preset = WorkspacePreset::Custom;
        self.preferences_model.workspace = WorkspacePreset::Custom.display_name().to_owned();
        self.custom_workspace_layout = Some(next_layout);
        self.dock = dock;
        self.refresh_shell_menu_checked_state();
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
        true
    }

    /// Relocate one docked panel tab to an explicit target tab-bar insertion index.
    pub fn relocate_panel_to_tab_index(
        &mut self,
        panel: PanelKind,
        target: PanelKind,
        insert_index: usize,
    ) -> bool {
        let Some(layout) = self.workspace_layout() else {
            return false;
        };
        let Some(next_layout) = layout.relocate_panel_to_tab_index(panel, target, insert_index)
        else {
            return false;
        };
        if self.workspace_layout().as_ref() == Some(&next_layout) {
            return false;
        }
        let Some(dock) = build_dock_tree_from_layout(self.models.clone(), &next_layout) else {
            return false;
        };

        self.workspace_preset = WorkspacePreset::Custom;
        self.preferences_model.workspace = WorkspacePreset::Custom.display_name().to_owned();
        self.custom_workspace_layout = Some(next_layout);
        self.dock = dock;
        self.refresh_shell_menu_checked_state();
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
        true
    }

    fn hide_panel(&mut self, panel: PanelKind) -> bool {
        let Some(layout) = self.workspace_layout() else {
            return false;
        };
        if !layout.contains_panel(panel) {
            return false;
        }
        let Some(next_layout) = layout.without_panel(panel) else {
            return false;
        };
        if !next_layout.is_split_root() {
            return false;
        }
        let Some(dock) = build_dock_tree_from_layout(self.models.clone(), &next_layout) else {
            return false;
        };

        self.workspace_preset = WorkspacePreset::Custom;
        self.preferences_model.workspace = WorkspacePreset::Custom.display_name().to_owned();
        self.custom_workspace_layout = Some(next_layout);
        self.dock = dock;
        self.refresh_shell_menu_checked_state();
        if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
            self.layout(self.bounds);
        }
        true
    }

    /// Switch to a built-in workspace preset and rebuild the dock tree from the
    /// current shell models.
    pub fn switch_workspace(&mut self, preset: WorkspacePreset) {
        self.workspace_preset = preset;
        self.preferences_model.workspace = preset.display_name().to_owned();
        self.dock = build_dock_tree_for_workspace(
            self.models.clone(),
            preset,
            self.custom_workspace_layout.as_ref(),
        );
        self.refresh_shell_menu_checked_state();
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
                tracing::warn!("app UI shell action failed: {err}");
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
            Action::FocusPanel(panel) => {
                self.focus_panel(panel);
                Ok(None)
            }
            Action::TogglePanel(panel) => {
                self.toggle_panel(panel);
                Ok(None)
            }
            Action::SwitchWorkspace(preset) => {
                self.switch_workspace(preset);
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_RELOCATE_PANEL =>
            {
                if let Ok(payload) = serde_json::from_value::<AppShellRelocatePanelPayload>(payload)
                {
                    if let Some(tab_index) = payload.tab_index {
                        self.relocate_panel_to_tab_index(payload.panel, payload.target, tab_index);
                    } else {
                        self.relocate_panel(
                            payload.panel,
                            payload.target,
                            dock_drop_area(payload.area),
                        );
                    }
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == VIEWER_NAMESPACE && name == VIEWER_CYCLE_ZOOM =>
            {
                self.viewer_zoom_mode = self.viewer_zoom_mode.next();
                let mut models = self.models.clone();
                apply_viewer_zoom_mode(&mut models, self.viewer_zoom_mode);
                self.set_models(models);
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == VIEWER_NAMESPACE && name == VIEWER_SET_ZOOM_SCALE =>
            {
                if let Ok(payload) = serde_json::from_value::<ViewerSetZoomScalePayload>(payload) {
                    self.viewer_zoom_mode = ViewerZoomMode::from_scale(payload.scale);
                    let mut models = self.models.clone();
                    apply_viewer_zoom_mode(&mut models, self.viewer_zoom_mode);
                    self.set_models(models);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_NEW_PROJECT_DIALOG =>
            {
                self.modal = Some(ShellModal::new_project(AppUiNewProjectDraft::default()));
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
                    "创建 Mondrian 项目",
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
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SEQUENCE_SETTINGS =>
            {
                let draft =
                    self.active_sequence.as_ref().map(AppUiSequenceSettingsDraft::from_sequence);
                if let Some(draft) = draft {
                    self.modal = Some(ShellModal::sequence_settings(draft));
                    if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                        self.layout(self.bounds);
                    }
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_SEQUENCE_SETTINGS_DRAFT_CHANGED =>
            {
                if let Some(dialog) =
                    self.modal.as_mut().and_then(ShellModal::as_sequence_settings_mut)
                {
                    if let Ok(update) =
                        serde_json::from_value::<SequenceSettingsDraftUpdatePayload>(payload)
                    {
                        dialog.apply_update(update);
                    }
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_SEQUENCE_SETTINGS_TAB_CHANGED =>
            {
                let tab: SequenceSettingsTabPayload = serde_json::from_value(payload)
                    .map_err(|err| app_shell_action_error(&name, err))?;
                if let Some(dialog) =
                    self.modal.as_mut().and_then(ShellModal::as_sequence_settings_mut)
                {
                    dialog.set_active_tab(tab);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CONFIRM_SEQUENCE_SETTINGS =>
            {
                let Some(draft) = self
                    .modal
                    .as_ref()
                    .and_then(ShellModal::as_sequence_settings)
                    .map(|dialog| dialog.draft().clone())
                else {
                    return Ok(None);
                };
                if draft.validate().is_err() {
                    return Ok(None);
                }
                self.modal = None;
                Ok(Some(sequence_update_settings_action(draft.into_payload())))
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

    fn preferences_model(&self) -> AppUiPreferencesModel {
        self.preferences_model.clone()
    }

    fn current_split_layout_matches_builtin_preset(&self) -> bool {
        if self.workspace_preset == WorkspacePreset::Custom {
            return true;
        }
        let default = build_dock_tree_for_preset(self.models.clone(), self.workspace_preset);
        self.dock.layout_snapshot() == default.layout_snapshot()
    }
}

fn build_dock_tree_for_workspace(
    models: AppUiPanelModels,
    preset: WorkspacePreset,
    custom_layout: Option<&AppUiWorkspaceLayout>,
) -> DockSplitter {
    if preset == WorkspacePreset::Custom {
        if let Some(layout) = custom_layout {
            if let Some(dock) = build_dock_tree_from_layout(models.clone(), layout) {
                return dock;
            }
        }
    }
    build_dock_tree_for_preset(models, preset)
}

fn dock_drop_area(area: DockDropAreaPayload) -> DockDropArea {
    match area {
        DockDropAreaPayload::Center => DockDropArea::Center,
        DockDropAreaPayload::Left => DockDropArea::Left,
        DockDropAreaPayload::Right => DockDropArea::Right,
        DockDropAreaPayload::Top => DockDropArea::Top,
        DockDropAreaPayload::Bottom => DockDropArea::Bottom,
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

fn preferred_workspace_for_panel(panel: PanelKind) -> WorkspacePreset {
    match panel {
        PanelKind::Export => WorkspacePreset::Export,
        PanelKind::NodeGraph => WorkspacePreset::Compositing,
        PanelKind::Viewer
        | PanelKind::Timeline
        | PanelKind::Assets
        | PanelKind::Inspector
        | PanelKind::Effects => WorkspacePreset::Editing,
    }
}

fn activate_panel_in_widget(
    widget: &mut dyn Widget,
    owner: PanelKind,
    active_index: usize,
) -> bool {
    if let Some(panel) = widget.as_any_mut().and_then(|any| any.downcast_mut::<DockPanel>()) {
        if panel.kind() == owner {
            if active_index >= panel.tab_count() {
                return false;
            }
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

fn collect_asset_grid_state(widget: &dyn Widget) -> Vec<AssetGridLocalState> {
    let mut states = Vec::new();
    collect_asset_grid_state_into(widget, None, &mut states);
    states
}

fn collect_asset_grid_state_into(
    widget: &dyn Widget,
    owner: Option<PanelKind>,
    states: &mut Vec<AssetGridLocalState>,
) {
    let owner = widget.panel_kind().or(owner);
    if let Some(grid) = widget.as_any().and_then(|any| any.downcast_ref::<AssetGrid>()) {
        if let Some(owner) = owner {
            states.push(AssetGridLocalState {
                owner,
                ordinal: states.iter().filter(|state| state.owner == owner).count(),
                state: grid.state(),
            });
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            collect_asset_grid_state_into(child, owner, states);
        }
    }
}

fn restore_asset_grid_state(widget: &mut dyn Widget, states: &[AssetGridLocalState]) {
    let mut restored = Vec::new();
    restore_asset_grid_state_into(widget, None, states, &mut restored);
}

fn restore_asset_grid_state_into(
    widget: &mut dyn Widget,
    owner: Option<PanelKind>,
    states: &[AssetGridLocalState],
    restored: &mut Vec<(PanelKind, usize)>,
) {
    let owner = widget.panel_kind().or(owner);
    if let Some(owner) = owner {
        if let Some(grid) = widget.as_any_mut().and_then(|any| any.downcast_mut::<AssetGrid>()) {
            let ordinal =
                restored.iter().filter(|(restored_owner, _)| *restored_owner == owner).count();
            if let Some(state) =
                states.iter().find(|state| state.owner == owner && state.ordinal == ordinal)
            {
                grid.restore_state(&state.state);
            }
            restored.push((owner, ordinal));
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_asset_grid_state_into(child, owner, states, restored);
        }
    }
}

fn collect_panel_list_state(widget: &dyn Widget) -> Vec<PanelListLocalState> {
    let mut states = Vec::new();
    collect_panel_list_state_into(widget, None, &mut states);
    states
}

fn collect_panel_list_state_into(
    widget: &dyn Widget,
    owner: Option<PanelKind>,
    states: &mut Vec<PanelListLocalState>,
) {
    let owner = widget.panel_kind().or(owner);
    if let Some(list) = widget.as_any().and_then(|any| any.downcast_ref::<PanelList>()) {
        if let Some(owner) = owner {
            states.push(PanelListLocalState {
                owner,
                ordinal: states.iter().filter(|state| state.owner == owner).count(),
                state: list.state(),
            });
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            collect_panel_list_state_into(child, owner, states);
        }
    }
}

fn restore_panel_list_state(widget: &mut dyn Widget, states: &[PanelListLocalState]) {
    let mut restored = Vec::new();
    restore_panel_list_state_into(widget, None, states, &mut restored);
}

fn restore_panel_list_state_into(
    widget: &mut dyn Widget,
    owner: Option<PanelKind>,
    states: &[PanelListLocalState],
    restored: &mut Vec<(PanelKind, usize)>,
) {
    let owner = widget.panel_kind().or(owner);
    if let Some(owner) = owner {
        if let Some(list) = widget.as_any_mut().and_then(|any| any.downcast_mut::<PanelList>()) {
            let ordinal =
                restored.iter().filter(|(restored_owner, _)| *restored_owner == owner).count();
            if let Some(state) =
                states.iter().find(|state| state.owner == owner && state.ordinal == ordinal)
            {
                list.restore_state(&state.state);
            }
            restored.push((owner, ordinal));
        }
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_panel_list_state_into(child, owner, states, restored);
        }
    }
}

fn collect_timeline_view_state(widget: &dyn Widget) -> Option<TimelineViewState> {
    if let Some(timeline) = widget.as_any().and_then(|any| any.downcast_ref::<TimelineView>()) {
        return Some(timeline.state());
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child(index) {
            if let Some(state) = collect_timeline_view_state(child) {
                return Some(state);
            }
        }
    }
    None
}

fn restore_timeline_view_state(widget: &mut dyn Widget, state: &Option<TimelineViewState>) {
    if let (Some(timeline), Some(state)) = (
        widget.as_any_mut().and_then(|any| any.downcast_mut::<TimelineView>()),
        state.as_ref(),
    ) {
        timeline.restore_state(state);
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_timeline_view_state(child, state);
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

impl Widget for AppUiAppRoot {
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
            (bounds.height - TITLE_BAR_HEIGHT - STATUS_BAR_HEIGHT).max(0.0),
        ));
        self.status_bar.layout(Rect::new(
            bounds.x,
            (bounds.y + bounds.height - STATUS_BAR_HEIGHT).max(bounds.y + TITLE_BAR_HEIGHT),
            bounds.width,
            STATUS_BAR_HEIGHT.min((bounds.height - TITLE_BAR_HEIGHT).max(0.0)),
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
        self.status_bar.paint(ctx);
        self.title_bar.paint(ctx);
        if let Some(modal) = &self.modal {
            modal.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        3 + usize::from(self.modal.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.dock),
            1 => Some(&self.status_bar),
            2 => Some(&self.title_bar),
            3 => self.modal.as_ref().map(|modal| modal as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.dock),
            1 => Some(&mut self.status_bar),
            2 => Some(&mut self.title_bar),
            3 => self.modal.as_mut().map(|modal| modal as &mut dyn Widget),
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
        app_shell_confirm_sequence_settings_action, app_shell_export_output_dialog_action,
        app_shell_import_media_dialog_action, app_shell_import_media_dialog_action_with_target,
        app_shell_new_project_dialog_action, app_shell_new_project_draft_changed_action,
        app_shell_open_project_dialog_action, app_shell_open_recent_project_action,
        app_shell_preferences_action, app_shell_preferences_tab_changed_action,
        app_shell_recover_project_action, app_shell_relink_asset_dialog_action,
        app_shell_relocate_panel_action, app_shell_reveal_in_file_manager_action,
        app_shell_save_project_as_dialog_action, app_shell_sequence_settings_action,
        app_shell_sequence_settings_draft_changed_action,
        app_shell_sequence_settings_tab_changed_action, viewer_cycle_zoom_action,
        viewer_set_zoom_scale_action, AppShellOpenRecentProjectPayload,
        AppShellRelinkAssetDialogPayload, AppShellRelocatePanelPayload,
        AppShellRevealInFileManagerPayload, AssetsImportFilesPayload, AssetsRelinkAssetPayload,
        DockDropAreaPayload, ExportDraftUpdatePayload, ExportOutputDialogPayload,
        ImportMediaDialogPayload, NewProjectDraftUpdatePayload, PreferencesTabPayload,
        ProjectCreateWithSettingsPayload, ProjectRecoverFromAutosavePayload,
        SequenceSettingsDraftUpdatePayload, SequenceSettingsTabPayload,
        SequenceUpdateSettingsPayload, ViewerSetZoomScalePayload, ASSETS_IMPORT_FILES,
        ASSETS_NAMESPACE, ASSETS_RELINK_ASSET, EXPORT_NAMESPACE, EXPORT_SET_DRAFT,
        PROJECT_CREATE_WITH_SETTINGS, PROJECT_NAMESPACE, PROJECT_RECOVER_FROM_AUTOSAVE,
        SEQUENCE_NAMESPACE, SEQUENCE_UPDATE_SETTINGS,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use glam::Vec2;
    use mondrian_core::types::AssetId;
    use mondrian_core::{ColorSpace, Rational, Resolution};
    use mondrian_timeline::sequence::{
        AudioChannelLayout, AudioDisplayFormat, ColorWorkflow, EditingMode, ExportBitDepth,
        FieldOrder, MissingColorMetadataPolicy, NestedColorProcessing, PixelAspectRatio,
        PreviewRenderFormat, Sequence, VideoDisplayFormat, VideoRange,
    };
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

    fn drag_root_splitter_to(root: &mut AppUiAppRoot, x: f32) {
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

    fn menu_checked_for_action(root: &AppUiAppRoot, action: &Action) -> Option<bool> {
        root.title_bar.menu_bar().checked_for_action(action)
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

    fn widget_tree_accepts_text_input(widget: &dyn Widget) -> bool {
        widget.accepts_text_input()
            || (0..widget.child_count())
                .any(|index| widget.child(index).is_some_and(widget_tree_accepts_text_input))
    }

    fn timeline_view_state(widget: &dyn Widget) -> Option<TimelineViewState> {
        if let Some(timeline) = widget.as_any().and_then(|any| any.downcast_ref::<TimelineView>()) {
            return Some(timeline.state());
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                if let Some(state) = timeline_view_state(child) {
                    return Some(state);
                }
            }
        }
        None
    }

    fn with_timeline_view_mut(
        widget: &mut dyn Widget,
        update: &mut dyn FnMut(&mut TimelineView),
    ) -> bool {
        if let Some(timeline) =
            widget.as_any_mut().and_then(|any| any.downcast_mut::<TimelineView>())
        {
            update(timeline);
            return true;
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index) {
                if with_timeline_view_mut(child, update) {
                    return true;
                }
            }
        }
        false
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
    fn app_root_focus_panel_switches_to_workspace_when_panel_is_absent() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action =
            root.handle_shell_action(Action::FocusPanel(PanelKind::Export), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Export);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Export),
            Some(0)
        );
    }

    #[test]
    fn app_root_focus_panel_returns_to_editing_for_timeline_when_absent() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Export),
            &platform,
            None,
        );

        let action =
            root.handle_shell_action(Action::FocusPanel(PanelKind::Timeline), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Editing);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Timeline),
            Some(0)
        );
    }

    #[test]
    fn app_root_toggle_panel_hides_direct_leaf_as_custom_layout() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(root.workspace_layout().expect("layout").contains_panel(PanelKind::Inspector));

        let action =
            root.handle_shell_action(Action::TogglePanel(PanelKind::Inspector), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Custom);
        let layout = root.custom_workspace_layout().expect("custom layout");
        assert!(!layout.contains_panel(PanelKind::Inspector));
        assert!(layout.contains_panel(PanelKind::Viewer));
        assert!(layout.is_split_root());
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Inspector),
            None
        );
    }

    #[test]
    fn app_root_toggle_hidden_panel_restores_preferred_workspace() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(Action::TogglePanel(PanelKind::Inspector), &platform, None);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Inspector),
            None
        );

        let action =
            root.handle_shell_action(Action::TogglePanel(PanelKind::Inspector), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Editing);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Inspector),
            Some(0)
        );
    }

    #[test]
    fn app_root_toggle_panel_hides_grouped_effects_tab_as_custom_layout() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(root.workspace_layout().expect("layout").contains_panel(PanelKind::Effects));

        let action =
            root.handle_shell_action(Action::TogglePanel(PanelKind::Effects), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Custom);
        let layout = root.custom_workspace_layout().expect("custom layout");
        assert!(layout.contains_panel(PanelKind::Assets));
        assert!(!layout.contains_panel(PanelKind::Effects));
        assert_eq!(
            menu_checked_for_action(&root, &Action::TogglePanel(PanelKind::Assets)),
            Some(true)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::TogglePanel(PanelKind::Effects)),
            Some(false)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::SwitchWorkspace(WorkspacePreset::Editing)),
            Some(false)
        );
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(0)
        );
    }

    #[test]
    fn app_root_relocate_panel_action_groups_panel_as_active_tab() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(
            app_shell_relocate_panel_action(AppShellRelocatePanelPayload {
                panel: PanelKind::Inspector,
                target: PanelKind::Assets,
                area: DockDropAreaPayload::Center,
                tab_index: None,
            }),
            &platform,
            None,
        );

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Custom);
        let layout = root.custom_workspace_layout().expect("custom layout");
        assert!(layout.contains_panel(PanelKind::Assets));
        assert!(layout.contains_panel(PanelKind::Inspector));
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(1)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::SwitchWorkspace(WorkspacePreset::Editing)),
            Some(false)
        );
    }

    #[test]
    fn app_root_relocate_panel_action_honors_tab_bar_insert_index() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(
            app_shell_relocate_panel_action(AppShellRelocatePanelPayload {
                panel: PanelKind::Inspector,
                target: PanelKind::Assets,
                area: DockDropAreaPayload::Center,
                tab_index: Some(0),
            }),
            &platform,
            None,
        );

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Custom);
        let layout = root.custom_workspace_layout().expect("custom layout");
        assert_eq!(
            layout,
            &AppUiWorkspaceLayout::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.66,
                first: Box::new(AppUiWorkspaceLayout::Split {
                    direction: SplitDirection::Horizontal,
                    ratio: 0.22,
                    first: Box::new(AppUiWorkspaceLayout::Panel {
                        kind: PanelKind::Inspector,
                        active_index: 0,
                        hidden_tabs: Vec::new(),
                        tabs: vec![PanelKind::Inspector, PanelKind::Assets, PanelKind::Effects,],
                    }),
                    second: Box::new(AppUiWorkspaceLayout::Panel {
                        kind: PanelKind::Viewer,
                        active_index: 0,
                        hidden_tabs: Vec::new(),
                        tabs: vec![PanelKind::Viewer],
                    }),
                }),
                second: Box::new(AppUiWorkspaceLayout::Panel {
                    kind: PanelKind::Timeline,
                    active_index: 0,
                    hidden_tabs: Vec::new(),
                    tabs: vec![PanelKind::Timeline],
                }),
            }
        );
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Inspector),
            Some(0)
        );
    }

    #[test]
    fn app_root_toggle_hidden_grouped_effects_tab_restores_preferred_workspace() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(Action::TogglePanel(PanelKind::Effects), &platform, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Custom);
        assert!(!root
            .custom_workspace_layout()
            .expect("custom layout")
            .contains_panel(PanelKind::Effects));

        let action =
            root.handle_shell_action(Action::TogglePanel(PanelKind::Effects), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Editing);
        assert_eq!(
            menu_checked_for_action(&root, &Action::TogglePanel(PanelKind::Effects)),
            Some(true)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::SwitchWorkspace(WorkspacePreset::Editing)),
            Some(true)
        );
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(1)
        );
    }

    #[test]
    fn app_root_switch_workspace_rebuilds_dock_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Export),
            &platform,
            None,
        );

        assert_eq!(action, None);
        assert_eq!(root.workspace_preset(), WorkspacePreset::Export);
        assert_eq!(
            menu_checked_for_action(&root, &Action::SwitchWorkspace(WorkspacePreset::Export)),
            Some(true)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::SwitchWorkspace(WorkspacePreset::Editing)),
            Some(false)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::TogglePanel(PanelKind::Export)),
            Some(true)
        );
        assert_eq!(
            menu_checked_for_action(&root, &Action::TogglePanel(PanelKind::Timeline)),
            Some(false)
        );
        assert!((root.dock().ratio() - 0.42).abs() < f32::EPSILON);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Export),
            Some(0)
        );
    }

    #[test]
    fn app_root_focus_panel_handles_direct_panels_after_workspace_switch() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
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
    fn every_panel_has_a_workspace_focus_fallback() {
        for panel in PanelKind::ALL {
            let preset = preferred_workspace_for_panel(panel);
            let mut dock = build_dock_tree_for_preset(AppUiPanelModels::demo(), preset);

            assert!(
                dock_panel_locations(panel).into_iter().any(|(owner, active_index)| {
                    activate_panel_in_widget(&mut dock, owner, active_index)
                }),
                "{panel:?} should be activatable in preferred {preset:?} workspace"
            );
        }
    }

    #[test]
    fn media_import_filters_cover_video_and_audio_extensions() {
        let filters = media_import_filters();

        assert!(filters.iter().any(
            |filter| filter.name == "视频" && filter.extensions.iter().any(|ext| ext == "mp4")
        ));
        assert!(filters.iter().any(
            |filter| filter.name == "音频" && filter.extensions.iter().any(|ext| ext == "wav")
        ));
    }

    #[test]
    fn project_file_filters_cover_mondrian_project_extension() {
        let filters = project_file_filters();

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "Mondrian 项目");
        assert!(filters[0]
            .extensions
            .iter()
            .any(|extension| extension == PROJECT_FILE_EXTENSION));
    }

    #[test]
    fn export_output_filters_normalize_requested_extension() {
        let filters = export_output_filters(".MP4 ");

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "导出");
        assert_eq!(filters[0].extensions, vec!["mp4"]);
    }

    #[test]
    fn export_output_filters_fall_back_for_empty_extension() {
        let filters = export_output_filters(" . ");

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "媒体");
        assert!(filters[0].extensions.iter().any(|extension| extension == "mp4"));
        assert!(filters[0].extensions.iter().any(|extension| extension == "gif"));
    }

    #[test]
    fn new_project_draft_uses_path_stem_and_preserves_settings_payload() {
        let path = PathBuf::from("E:/projects/Trailer Cut.mdp");
        let mut draft = AppUiNewProjectDraft::from_project_path(&path);
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
        let mut draft = AppUiNewProjectDraft::default();

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
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(
            root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None),
            None
        );
        assert!(root.modal.as_ref().and_then(ShellModal::as_new_project).is_some());
        assert_eq!(root.child_count(), 4);

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
        let mut root = AppUiAppRoot::demo();

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
    fn app_root_handles_sequence_settings_draft_and_confirm() {
        let platform = FakePlatform::default();
        let mut state = AppState::new();
        let sequence = Sequence::new("Scene 01");
        let sequence_id = sequence.id;
        state.active_sequence_id = Some(sequence_id);
        state.sequence = Some(sequence.clone());
        state.sequences.push(sequence);
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(
            root.handle_shell_action(app_shell_sequence_settings_action(), &platform, None),
            None
        );
        assert!(root.modal.as_ref().and_then(ShellModal::as_sequence_settings).is_some());
        assert_eq!(
            root.handle_shell_action(
                app_shell_sequence_settings_tab_changed_action(SequenceSettingsTabPayload::Color),
                &platform,
                None
            ),
            None
        );

        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::Name("Scene 02".to_owned()),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::EditingMode(EditingMode::Custom),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::Resolution(Resolution::UHD4K),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::ResolutionWidth(2048),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::ResolutionHeight(1152),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::FrameRate(Rational::FPS_2997),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::PixelAspectRatio(
                    PixelAspectRatio::Anamorphic2x,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::FieldOrder(FieldOrder::UpperFirst),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::VideoDisplayFormat(
                    VideoDisplayFormat::Timecode2997DropFrame,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::StartTimecodeFrame(120),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::ColorSpace(ColorSpace::Rec2100Pq),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::OutputColorSpace(ColorSpace::Rec2100Pq),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::ColorWorkflow(ColorWorkflow::Aces),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::MissingColorMetadataPolicy(
                    MissingColorMetadataPolicy::AssumeSequenceWorkingSpace,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::NestedColorProcessing(
                    NestedColorProcessing::ForceParentWorkingSpace,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::VideoRange(VideoRange::Legal),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::ExportBitDepth(ExportBitDepth::Ten),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::AutoToneMapMedia(false),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::PreserveHdrMetadata(true),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::AudioSampleRate(96_000),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::AudioChannelLayout(
                    AudioChannelLayout::Surround51,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::AudioDisplayFormat(
                    AudioDisplayFormat::Milliseconds,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::PreviewRenderFormat(
                    PreviewRenderFormat::ProResProxy,
                ),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::PreviewResolutionScale(0.25),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::PreviewCacheEnabled(false),
            ),
            &platform,
            None,
        );

        let action = root
            .handle_shell_action(
                app_shell_confirm_sequence_settings_action(),
                &platform,
                None,
            )
            .expect("sequence update action");

        assert!(root.modal.is_none());
        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected sequence update action");
        };
        assert_eq!(namespace, SEQUENCE_NAMESPACE);
        assert_eq!(name, SEQUENCE_UPDATE_SETTINGS);
        let payload: SequenceUpdateSettingsPayload =
            serde_json::from_value(payload).expect("sequence settings payload");
        assert_eq!(payload.sequence_id, sequence_id);
        assert_eq!(payload.name, "Scene 02");
        assert_eq!(payload.settings.editing_mode, EditingMode::Custom);
        assert_eq!(
            payload.settings.resolution,
            Resolution { width: 2048, height: 1152 }
        );
        assert_eq!(payload.settings.frame_rate, Rational::FPS_2997);
        assert_eq!(
            payload.settings.pixel_aspect_ratio,
            PixelAspectRatio::Anamorphic2x
        );
        assert_eq!(payload.settings.field_order, FieldOrder::UpperFirst);
        assert_eq!(
            payload.settings.video_display_format,
            VideoDisplayFormat::Timecode2997DropFrame
        );
        assert_eq!(payload.settings.start_timecode_frame, 120);
        assert_eq!(payload.settings.color_space, ColorSpace::Rec2100Pq);
        assert!(!payload.settings.auto_tone_map_media);
        assert_eq!(
            payload.settings.color_management.workflow,
            ColorWorkflow::Aces
        );
        assert_eq!(
            payload.settings.color_management.missing_metadata_policy,
            MissingColorMetadataPolicy::AssumeSequenceWorkingSpace
        );
        assert_eq!(
            payload.settings.color_management.nested_processing,
            NestedColorProcessing::ForceParentWorkingSpace
        );
        assert_eq!(
            payload.settings.color_management.output_color_space,
            ColorSpace::Rec2100Pq
        );
        assert_eq!(
            payload.settings.color_management.video_range,
            VideoRange::Legal
        );
        assert_eq!(
            payload.settings.color_management.export_bit_depth,
            ExportBitDepth::Ten
        );
        assert!(payload.settings.color_management.preserve_hdr_metadata);
        assert_eq!(payload.settings.audio_sample_rate, 96_000);
        assert_eq!(
            payload.settings.audio_channel_layout,
            AudioChannelLayout::Surround51
        );
        assert_eq!(
            payload.settings.audio_channels,
            payload.settings.audio_channel_layout.channels()
        );
        assert_eq!(
            payload.settings.audio_display_format,
            AudioDisplayFormat::Milliseconds
        );
        assert_eq!(
            payload.settings.preview.format,
            PreviewRenderFormat::ProResProxy
        );
        assert_eq!(payload.settings.preview.resolution_scale, 0.25);
        assert!(!payload.settings.preview.cache_enabled);
    }

    #[test]
    fn app_root_ignores_sequence_settings_without_active_sequence() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::from_app_state(&AppState::new());

        let action =
            root.handle_shell_action(app_shell_sequence_settings_action(), &platform, None);

        assert!(action.is_none());
        assert!(root.modal.is_none());
    }

    #[test]
    fn app_root_cancels_new_project_dialog_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();

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
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(app_shell_about_action(), &platform, None);

        assert_eq!(action, None);
        assert!(root.modal.as_ref().and_then(ShellModal::as_about).is_some());
        assert_eq!(root.child_count(), 4);
    }

    #[test]
    fn app_root_handles_preferences_action_as_shell_modal() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let action = root.handle_shell_action(app_shell_preferences_action(), &platform, None);

        assert_eq!(action, None);
        assert!(root.modal.as_ref().and_then(ShellModal::as_preferences).is_some());
        assert_eq!(root.child_count(), 4);
    }

    #[test]
    fn app_root_switches_preferences_tab_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();

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
        let mut root = AppUiAppRoot::demo();
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
        assert_eq!(dialog.model().proxy_mode, "已启用");
    }

    #[test]
    fn status_bar_model_prefers_active_sequence_context() {
        let mut state = AppState::new();
        state.current_project_path = Some(PathBuf::from("E:/projects/rough-cut.mdp"));
        state.sequence = Some(mondrian_timeline::sequence::Sequence::new("Cut 01"));
        state.set_status_hint("Project saved", false);

        let model = status_bar_model(&state);

        assert_eq!(model.message, "Project saved");
        assert!(!model.is_error);
        assert!(!model.is_busy);
        assert_eq!(model.context, "Cut 01");
    }

    #[test]
    fn status_bar_model_prioritizes_preview_buffering_over_hint() {
        let mut state = AppState::new();
        state.set_status_hint("Project saved", false);
        state.set_playback_frame_running(42);
        state.set_playback_buffering(true);

        let model = status_bar_model(&state);

        assert_eq!(model.message, "预览缓冲中...");
        assert!(!model.is_error);
        assert!(model.is_busy);
    }

    #[test]
    fn status_bar_text_elision_respects_available_width() {
        let font_size = 11.0;

        assert_eq!(elide_text_to_width("Saved", font_size, 100.0), "Saved");
        assert_eq!(elide_text_to_width("Saved", font_size, 1.0), "");

        let elided = elide_text_to_width("A very long project status", font_size, 70.0);
        assert!(elided.ends_with("..."));
        assert!(estimate_text_width(&elided, font_size) <= 70.0);
    }

    #[test]
    fn app_root_refresh_updates_status_bar_paint_model() {
        let mut root = AppUiAppRoot::demo();
        let mut state = AppState::new();
        state.current_project_path = Some(PathBuf::from("E:/projects/rough-cut.mdp"));
        state.set_status_hint("Import failed", true);

        root.refresh_from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let mut encoder = PaintOrderRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 1280.0, 720.0),
        };

        root.paint(&mut ctx);

        assert!(encoder.texts.iter().any(|text| text == "Import failed"));
        assert!(encoder.texts.iter().any(|text| text == "rough-cut"));
    }

    #[test]
    fn app_root_closes_shell_modal_without_editor_action() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();

        root.handle_shell_action(app_shell_about_action(), &platform, None);

        let action = root.handle_shell_action(app_shell_close_modal_action(), &platform, None);

        assert_eq!(action, None);
        assert!(root.modal.is_none());
    }

    #[test]
    fn app_root_modal_blocks_unhandled_keyboard_events() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
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
    fn resolve_app_shell_relink_dialog_returns_asset_relink_action() {
        let asset_id = AssetId::new();
        let replacement = PathBuf::from("E:/media/relinked.mov");
        let platform = FakePlatform {
            open_paths: Some(vec![replacement.clone()]),
            ..FakePlatform::default()
        };

        let action = resolve_app_shell_action(
            app_shell_relink_asset_dialog_action(AppShellRelinkAssetDialogPayload { asset_id }),
            &platform,
            None,
        );

        let Some(Action::Custom { namespace, name, payload }) = action else {
            panic!("expected assets relink action");
        };
        assert_eq!(namespace, ASSETS_NAMESPACE);
        assert_eq!(name, ASSETS_RELINK_ASSET);
        let payload: AssetsRelinkAssetPayload =
            serde_json::from_value(payload).expect("assets relink payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(payload.path, replacement);
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
                assert!(reason.contains("unknown app-shell action"));
            }
            other => panic!("expected app-shell workflow error, got {other:?}"),
        }
    }

    #[test]
    fn app_root_layout_reserves_title_bar_height_for_dock() {
        let mut root = AppUiAppRoot::demo();

        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let zones = root.dock().collect_grab_zones();

        assert_eq!(
            root.title_bar.bounds(),
            Rect::new(0.0, 0.0, 1280.0, TITLE_BAR_HEIGHT)
        );
        assert!(
            zones.iter().all(|(zone, _)| zone.y >= TITLE_BAR_HEIGHT - f32::EPSILON),
            "dock splitter handles must stay below title bar: {zones:?}"
        );
        assert!(!root.dock().hit_test(Point::new(12.0, TITLE_BAR_HEIGHT - 1.0)));
        assert!(root.dock().hit_test(Point::new(12.0, TITLE_BAR_HEIGHT + 1.0)));
        assert!(!root.dock().hit_test(Point::new(12.0, 720.0 - STATUS_BAR_HEIGHT + 1.0)));
        assert_eq!(
            root.status_bar.bounds,
            Rect::new(0.0, 696.0, 1280.0, STATUS_BAR_HEIGHT)
        );
        assert_eq!(root.child_count(), 3);
    }

    #[test]
    fn app_root_child_order_matches_bottom_to_top_z_order() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(root.child(0).map(Widget::id), Some(root.dock.id()));
        assert_eq!(root.child(1).map(Widget::id), Some(root.status_bar.id()));
        assert_eq!(root.child(2).map(Widget::id), Some(root.title_bar.id()));

        let platform = FakePlatform::default();
        let action = root.handle_shell_action(app_shell_about_action(), &platform, None);

        assert_eq!(action, None);
        assert_eq!(root.child_count(), 4);
        assert_eq!(
            root.child(3).map(Widget::id),
            root.modal.as_ref().map(Widget::id)
        );
    }

    #[test]
    fn app_root_paints_in_bottom_to_top_z_order() {
        let mut root = AppUiAppRoot::demo();
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
            .position(|text| text == "文件")
            .expect("menu bar should paint");
        let status_text = encoder
            .texts
            .iter()
            .position(|text| text == "就绪")
            .expect("status bar should paint");
        assert!(
            first_menu_text > 0,
            "dock content must paint before menu chrome: {:?}",
            encoder.texts
        );
        assert!(
            status_text < first_menu_text,
            "status bar must paint below title/menu chrome: {:?}",
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
            .position(|text| text == "文件")
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
        let mut root = AppUiAppRoot::demo();
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
        let mut root = AppUiAppRoot::from_app_state(&state);

        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(
            root.title_bar.bounds(),
            Rect::new(0.0, 0.0, 1280.0, TITLE_BAR_HEIGHT)
        );
        assert_eq!(
            root.title_bar.menu_bar().bounds().height,
            crate::app_ui::menu_bar::MENU_BAR_HEIGHT
        );
        assert!(!root.dock().collect_grab_zones().is_empty());
    }

    #[test]
    fn set_models_preserves_user_splitter_ratio() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        drag_root_splitter_to(&mut root, 620.0);

        let dragged_ratio = root.dock().ratio();
        assert!(dragged_ratio > 0.4);

        root.set_models(AppUiPanelModels::demo());

        assert!((root.dock().ratio() - dragged_ratio).abs() < f32::EPSILON);
    }

    #[test]
    fn set_models_preserves_asset_grid_filter_and_selection() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(with_asset_grid_mut_for_title(
            root.dock_mut(),
            "Assets",
            &mut |grid| {
                grid.set_filter_query("audio");
                grid.set_selected(Some(1));
            },
        ));

        root.set_models(AppUiPanelModels::demo());

        let state = asset_grid_state_for_title(&root, "Assets").expect("assets state");
        assert_eq!(state.filter_query, "audio");
        assert_eq!(state.selected_item_id.as_deref(), Some("demo-audio"));
        assert_eq!(state.selected_index, Some(1));
    }

    #[test]
    fn set_models_preserves_asset_grid_hover_by_stable_id() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let dispatch = |_| {};
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        assert!(with_asset_grid_mut_for_title(
            root.dock_mut(),
            "Assets",
            &mut |grid| {
                let point = grid.card_rect_for_index(1).expect("second asset").center();
                assert_eq!(
                    grid.event(
                        &UiEvent::MouseMove { position: point, modifiers: Modifiers::none() },
                        &mut ctx,
                    ),
                    EventResult::Handled
                );
            },
        ));
        let before = asset_grid_state_for_title(&root, "Assets").expect("assets state");
        assert_eq!(before.hovered_item_id.as_deref(), Some("demo-audio"));

        root.set_models(AppUiPanelModels::demo());

        let after = asset_grid_state_for_title(&root, "Assets").expect("assets state");
        assert_eq!(after.hovered_item_id.as_deref(), Some("demo-audio"));
    }

    #[test]
    fn set_models_cancels_asset_grid_inline_rename_without_dispatching() {
        let mut models = AppUiPanelModels::demo();
        models.assets.items[0] = models.assets.items[0].clone().renamable(true);
        let mut root = AppUiAppRoot::from_models(models);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let actions = std::cell::RefCell::new(Vec::<Action>::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();
        let mut ctx = event_ctx(
            &mut focus,
            &mut shortcut,
            &mut tooltip,
            &mut requests,
            &dispatch,
        );
        assert!(with_asset_grid_mut_for_title(
            root.dock_mut(),
            "Assets",
            &mut |grid| {
                grid.event(&UiEvent::FocusGained, &mut ctx);
                grid.event(
                    &UiEvent::MouseDown {
                        position: grid.card_rect_for_index(0).expect("first asset").center(),
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                );
                assert_eq!(
                    grid.event(
                        &UiEvent::KeyDown { key: KeyCode::F2, modifiers: Modifiers::none() },
                        &mut ctx,
                    ),
                    EventResult::Handled
                );
                assert!(grid.accepts_text_input());
            },
        ));
        assert!(widget_tree_accepts_text_input(&root));
        actions.borrow_mut().clear();

        root.set_models(AppUiPanelModels::demo());

        assert!(
            !widget_tree_accepts_text_input(&root),
            "forced model rebuild must discard transient inline editors"
        );
        assert!(
            actions.borrow().is_empty(),
            "discarding a stale inline editor must not commit a rename action"
        );
    }

    #[test]
    fn set_models_preserves_asset_grid_state_when_title_changes() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(with_asset_grid_mut_for_title(
            root.dock_mut(),
            "Assets",
            &mut |grid| {
                grid.set_filter_query("audio");
                grid.set_selected(Some(1));
            },
        ));
        let mut models = AppUiPanelModels::demo();
        models.assets.title = "Media".to_owned();

        root.set_models(models);

        let state = asset_grid_state_for_title(&root, "Media").expect("renamed assets state");
        assert_eq!(state.filter_query, "audio");
        assert_eq!(state.selected_item_id.as_deref(), Some("demo-audio"));
    }

    #[test]
    fn set_models_preserves_grouped_panel_active_tab_and_visible_list_state() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
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

        root.set_models(AppUiPanelModels::demo());

        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::Assets),
            Some(1)
        );
        let state = panel_list_state_for_title(&root, "Effects").expect("effects state");
        assert_eq!(state.filter_query, "blur");
    }

    #[test]
    fn set_models_preserves_panel_list_state_when_title_changes() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(Action::FocusPanel(PanelKind::Effects), &platform, None);
        assert!(with_panel_list_mut_for_title(
            root.dock_mut(),
            "Effects",
            &mut |list| {
                list.set_filter_query("blur");
            },
        ));
        let mut models = AppUiPanelModels::demo();
        models.effects.title = "FX".to_owned();

        root.set_models(models);

        let state = panel_list_state_for_title(&root, "FX").expect("renamed effects state");
        assert_eq!(state.filter_query, "blur");
    }

    #[test]
    fn set_models_preserves_panel_scroll_position() {
        let mut root = AppUiAppRoot::demo();
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

        root.set_models(AppUiPanelModels::demo());

        let after = scroll_state_for_panel(root.dock(), PanelKind::Inspector)
            .expect("inspector should keep a scroll view");
        assert_eq!(after.scroll_offset, before.scroll_offset);
    }

    #[test]
    fn set_models_preserves_assets_panel_scroll_position() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 360.0));
        assert!(with_scroll_view_mut_for_panel(
            root.dock_mut(),
            PanelKind::Assets,
            &mut |scroll| {
                scroll.set_scroll_offset(Vec2::new(0.0, 80.0));
            },
        ));
        let before =
            scroll_state_for_panel(root.dock(), PanelKind::Assets).expect("assets scroll view");
        assert!(
            before.scroll_offset.y > 0.0,
            "test fixture must overflow vertically"
        );

        root.set_models(AppUiPanelModels::demo());

        let after =
            scroll_state_for_panel(root.dock(), PanelKind::Assets).expect("assets scroll view");
        assert_eq!(after.scroll_offset, before.scroll_offset);
    }

    #[test]
    fn set_models_preserves_timeline_tool_zoom_and_scroll_state() {
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 480.0));
        assert!(with_timeline_view_mut(root.dock_mut(), &mut |timeline| {
            timeline.restore_state(&TimelineViewState {
                active_tool: mondrian_ui_widgets::TimelineTool::Blade,
                scroll_x: 96.0,
                scroll_y: 24.0,
                pixels_per_frame: 8.0,
                snapping_enabled: false,
                track_height: 64.0,
            });
        }));
        root.layout(Rect::new(0.0, 0.0, 1280.0, 480.0));
        let before = timeline_view_state(root.dock()).expect("before timeline state");

        root.set_models(AppUiPanelModels::demo());

        let after = timeline_view_state(root.dock()).expect("after timeline state");
        assert_eq!(after.active_tool, before.active_tool);
        assert!((after.pixels_per_frame - before.pixels_per_frame).abs() < f32::EPSILON);
        assert!((after.scroll_x - before.scroll_x).abs() < 0.01);
        assert!((after.scroll_y - before.scroll_y).abs() < 0.01);
        assert_eq!(after.snapping_enabled, before.snapping_enabled);
        assert!((after.track_height - before.track_height).abs() < f32::EPSILON);
    }

    #[test]
    fn refresh_from_app_state_preserves_user_splitter_ratio() {
        let state = AppState::new();
        let mut root = AppUiAppRoot::from_app_state(&state);
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
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Compositing),
            &platform,
            None,
        );

        root.refresh_from_app_state(&state);

        assert_eq!(root.workspace_preset(), WorkspacePreset::Compositing);
        assert!((root.dock().ratio() - 0.42).abs() < f32::EPSILON);
        assert_eq!(
            active_index_for_dock_panel(&root, PanelKind::NodeGraph),
            Some(0)
        );
    }

    #[test]
    fn viewer_zoom_cycle_is_shell_local_and_survives_app_state_refresh() {
        let mut state = AppState::new();
        state.sequence = Some(Sequence::new("edit"));
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        assert_eq!(root.models.viewer.zoom_label, "适合");
        let resolved = root
            .try_handle_shell_action(viewer_cycle_zoom_action(), &platform, None)
            .expect("cycle zoom");

        assert!(resolved.is_none());
        assert_eq!(root.models.viewer.zoom_label, "50%");
        assert_eq!(root.models.viewer.zoom_scale, Some(0.5));

        root.refresh_from_app_state(&state);

        assert_eq!(root.models.viewer.zoom_label, "50%");
        assert_eq!(root.models.viewer.zoom_scale, Some(0.5));
    }

    #[test]
    fn viewer_zoom_set_action_applies_explicit_dropdown_selection() {
        let mut state = AppState::new();
        state.sequence = Some(Sequence::new("edit"));
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));

        let resolved = root
            .try_handle_shell_action(
                viewer_set_zoom_scale_action(ViewerSetZoomScalePayload { scale: Some(1.0) }),
                &platform,
                None,
            )
            .expect("set zoom");

        assert!(resolved.is_none());
        assert_eq!(root.models.viewer.zoom_label, "100%");
        assert_eq!(root.models.viewer.zoom_scale, Some(1.0));

        root.refresh_from_app_state(&state);

        assert_eq!(root.models.viewer.zoom_label, "100%");
        assert_eq!(root.models.viewer.zoom_scale, Some(1.0));

        root.try_handle_shell_action(
            viewer_set_zoom_scale_action(ViewerSetZoomScalePayload { scale: None }),
            &platform,
            None,
        )
        .expect("fit zoom");

        assert_eq!(root.models.viewer.zoom_label, "适合");
        assert_eq!(root.models.viewer.zoom_scale, None);
    }
}
