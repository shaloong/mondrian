//! Reusable widget shell for app UI Mondrian windows.
//!
//! Developer binaries own native window setup and event-loop plumbing. This
//! module owns the reusable root widget composition above the dock/panel layer.

use crate::app::AppMessage;
use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::{Action, AuthoringSessionId};
use mondrian_export::queue::{ExportProgressDetail, ExportProgressPhase, JobStatus};
use mondrian_platform::{FileFilter, PlatformService};
use mondrian_timeline::Sequence;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{
    AccessibilityNode, AccessibilityRole, AccessibilityState, EventContext, PaintContext,
};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_theme::ThemePreset;
use mondrian_ui_widgets::dock_panel::DockPanel;
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::{
    AssetGrid, AssetGridState, PanelList, PanelListState, ScrollView, ScrollViewState,
    TimelineView, TimelineViewState, VideoScopesSurface, ViewerPresentationGeometry, ViewerSurface,
    WaveformDisplay,
};
use std::path::Path;

use crate::app::product_action::{
    AudioInstallClapLibraryPayload, AudioInstallVst3PluginPayload, AudioProductAction,
    ProductAction, VisualEffectInstallOpenFxBundlePayload, VisualEffectProductAction,
};
use crate::app::ui_actions::{
    app_shell_export_portable_package_action,
    app_shell_preferences_display_management_changed_action, assets_import_files_action,
    assets_relink_asset_action, assets_set_interpretation_action, export_edit_draft_action,
    project_create_with_settings_action, project_recover_from_autosave_action,
    project_update_color_environment_action, sequence_update_settings_action,
    AppShellCopySystemInfoPayload, AppShellInterpretAssetDialogPayload,
    AppShellOpenRecentProjectPayload, AppShellRelinkAssetDialogPayload,
    AppShellRelocatePanelPayload, AppShellRevealInFileManagerPayload, AssetsImportFilesPayload,
    AssetsRelinkAssetPayload, DockDropAreaPayload, ExportDraftEdit, ExportOutputDialogPayload,
    ImportMediaDialogPayload, InterpretAssetDraftUpdatePayload, NewProjectDraftUpdatePayload,
    PreferencesTabPayload, ProjectRecoverFromAutosavePayload, ProjectSettingsDraftUpdatePayload,
    ProjectUpdateColorEnvironmentPayload, SequenceSettingsDraftUpdatePayload,
    SequenceSettingsTabPayload, ViewerSetZoomScalePayload, APP_SHELL_ABOUT,
    APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_INTERPRET_ASSET_DIALOG, APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG,
    APP_SHELL_CONFIRM_PROJECT_SETTINGS, APP_SHELL_CONFIRM_SEQUENCE_SETTINGS,
    APP_SHELL_COPY_SYSTEM_INFO, APP_SHELL_EXPORT_OUTPUT_DIALOG,
    APP_SHELL_EXPORT_PORTABLE_PACKAGE_DIALOG, APP_SHELL_IMPORT_MEDIA_DIALOG,
    APP_SHELL_INSTALL_CLAP_LIBRARY_DIALOG, APP_SHELL_INSTALL_OPENFX_BUNDLE_DIALOG,
    APP_SHELL_INSTALL_VST3_BINARY_DIALOG, APP_SHELL_INSTALL_VST3_BUNDLE_DIALOG,
    APP_SHELL_INTERPRET_ASSET_DIALOG, APP_SHELL_INTERPRET_ASSET_DRAFT_CHANGED, APP_SHELL_NAMESPACE,
    APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_NEW_PROJECT_DRAFT_CHANGED,
    APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_OPEN_RECENT_PROJECT, APP_SHELL_PREFERENCES,
    APP_SHELL_PREFERENCES_SELECT_DISPLAY_ICC_PROFILE, APP_SHELL_PREFERENCES_TAB_CHANGED,
    APP_SHELL_PROJECT_SETTINGS, APP_SHELL_PROJECT_SETTINGS_DRAFT_CHANGED,
    APP_SHELL_RECOVER_PROJECT, APP_SHELL_RELINK_ASSET_DIALOG, APP_SHELL_RELOCATE_PANEL,
    APP_SHELL_REVEAL_IN_FILE_MANAGER, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
    APP_SHELL_SELECT_CUSTOM_OCIO_CONFIG, APP_SHELL_SEQUENCE_SETTINGS,
    APP_SHELL_SEQUENCE_SETTINGS_DRAFT_CHANGED, APP_SHELL_SEQUENCE_SETTINGS_TAB_CHANGED,
    VIEWER_CYCLE_ZOOM, VIEWER_NAMESPACE, VIEWER_SET_ZOOM_SCALE,
};
use crate::app::waveform_service::AudioWaveformSource;
use crate::app::{AppState, StatusLogEntry};
use crate::app_ui::audio_device_catalog::AudioOutputDeviceCatalogState;
use crate::app_ui::interpret_asset_dialog::AppUiInterpretAssetDraft;
#[cfg(test)]
use crate::app_ui::localization::AppUiLocale;
use crate::app_ui::localization::{export_progress_phase_message_id, Localizer};
use crate::app_ui::menu_bar::MenuBar;
use crate::app_ui::modal::ShellModal;
use crate::app_ui::new_project_dialog::{default_project_file_name, AppUiNewProjectDraft};
use crate::app_ui::notifications::NotificationOverlay;
use crate::app_ui::panels::{
    build_dock_tree_for_preset, build_dock_tree_from_layout, AppUiPanelModels,
    AssetThumbnailSource, ScopesPanelModel, ViewerPanelModel, ViewerPreviewSource,
};
use crate::app_ui::pending_close_dialog::PendingCloseDialogAction;
use crate::app_ui::preferences_dialog::{AppUiPreferencesModel, PreferencesDialogTab};
use crate::app_ui::preferences_store::AppUiPreferences;
use crate::app_ui::project_settings_dialog::AppUiProjectSettingsDraft;
use crate::app_ui::sequence_settings_dialog::AppUiSequenceSettingsDraft;
use crate::app_ui::title_bar::{TitleBar, TITLE_BAR_HEIGHT};
use crate::app_ui::workspace_layout::{AppUiWorkspaceLayout, DockDropArea};
use mondrian_core::{ColorSpace, MondrianError, Result, WorkingColorSpace};

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
pub fn project_file_filters(localizer: &Localizer) -> Vec<FileFilter> {
    vec![FileFilter::new(
        localizer.text("file-filter-project"),
        vec![PROJECT_FILE_EXTENSION],
    )]
}

/// File dialog filter for the portable package directory destination.
pub fn portable_package_filters(localizer: &Localizer) -> Vec<FileFilter> {
    vec![FileFilter::new(
        localizer.text("file-filter-package"),
        vec!["mdpkg"],
    )]
}

/// File dialog filters for media import commands.
pub fn media_import_filters(localizer: &Localizer) -> Vec<FileFilter> {
    vec![
        FileFilter::new(
            localizer.text("file-filter-video"),
            vec!["mp4", "mov", "mkv", "webm", "avi"],
        ),
        FileFilter::new(
            localizer.text("file-filter-image"),
            vec!["dng", "dpx", "exr", "png", "jpg", "jpeg", "tif", "tiff"],
        ),
        FileFilter::new(
            localizer.text("file-filter-audio"),
            vec!["mp3", "wav", "aac", "flac", "m4a"],
        ),
    ]
}

/// Native binary extensions accepted by the CLAP discovery worker.
pub fn clap_library_filters(localizer: &Localizer) -> Vec<FileFilter> {
    vec![FileFilter::new(
        localizer.text("file-filter-clap"),
        vec!["clap", "dll"],
    )]
}

/// Native binary extensions accepted by the VST3 discovery worker.
pub fn vst3_binary_filters(localizer: &Localizer) -> Vec<FileFilter> {
    let extensions = match std::env::consts::OS {
        "windows" => vec!["vst3", "dll"],
        "linux" => vec!["so", "vst3"],
        "macos" => vec!["dylib", "vst3"],
        _ => vec!["vst3"],
    };
    vec![FileFilter::new(
        localizer.text("file-filter-vst3"),
        extensions,
    )]
}

/// File dialog filter for monitor calibration profiles.
pub fn display_icc_profile_filters(localizer: &Localizer) -> Vec<FileFilter> {
    vec![FileFilter::new(
        localizer.text("file-filter-icc"),
        vec!["icc", "icm"],
    )]
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusBarModel {
    message: String,
    is_error: bool,
    is_busy: bool,
    context: String,
}

impl Default for StatusBarModel {
    fn default() -> Self {
        Self {
            message: "就绪".to_owned(),
            is_error: false,
            is_busy: false,
            context: String::new(),
        }
    }
}

const STATUS_HISTORY_PAGE_SIZE: usize = 8;

struct StatusBar {
    id: WidgetId,
    bounds: Rect,
    model: StatusBarModel,
    history: Vec<StatusLogEntry>,
    localizer: Localizer,
    history_open: bool,
    history_offset: usize,
    notifications: NotificationOverlay,
}

impl StatusBar {
    fn set_model(&mut self, model: StatusBarModel) {
        self.model = model;
    }

    fn history_entry_text(&self, entry: &StatusLogEntry) -> String {
        self.localizer.message(&entry.message)
    }

    fn sync_history(&mut self, history: &[StatusLogEntry]) {
        if self.history != history {
            self.history = history.to_vec();
            self.history_offset = self
                .history_offset
                .min(self.history.len().saturating_sub(STATUS_HISTORY_PAGE_SIZE));
        }
    }

    fn accessible_history_name(&self) -> String {
        if !self.history_open {
            return self.localizer.format_text(
                "status-history-accessibility",
                "status",
                &self.model.message,
            );
        }
        let mut message = AppMessage::new(if self.history.is_empty() {
            "status-history-accessibility-empty"
        } else {
            "status-history-accessibility-open"
        })
        .with_text("status", &self.model.message);
        if !self.history.is_empty() {
            let entries = self
                .history
                .iter()
                .rev()
                .skip(self.history_offset)
                .take(STATUS_HISTORY_PAGE_SIZE)
                .map(|entry| {
                    let body = self.history_entry_text(entry);
                    if entry.is_error {
                        self.localizer.format_text("status-history-entry-error", "message", &body)
                    } else {
                        body
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            message = message
                .with_count("first", self.history_offset + 1)
                .with_count(
                    "last",
                    (self.history_offset + STATUS_HISTORY_PAGE_SIZE).min(self.history.len()),
                )
                .with_count("total", self.history.len())
                .with_text("entries", entries);
        }
        self.localizer.message(&message)
    }

    fn expire_notifications(&mut self, now: std::time::Instant) -> bool {
        self.notifications.expire(now)
    }

    fn next_notification_deadline(&self) -> Option<std::time::Instant> {
        self.notifications.next_deadline()
    }

    fn history_rect(&self) -> Rect {
        let rows = self.history.len().clamp(1, STATUS_HISTORY_PAGE_SIZE);
        let height = 34.0 + rows as f32 * 26.0;
        let width = (self.bounds.width - 24.0).clamp(0.0, 520.0);
        Rect::new(
            self.bounds.x + 12.0,
            (self.bounds.y - height - 6.0).max(0.0),
            width,
            height,
        )
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

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.history_open
            && self.notifications.event(event, self.bounds) == EventResult::Handled
        {
            return EventResult::Handled;
        }
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                ctx.focus.request_focus(self.id);
                self.history_open = !self.history_open;
                self.history_offset = 0;
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, modifiers }
                if *modifiers == Modifiers::none()
                    && ctx.focus.focused_widget() == Some(self.id) =>
            {
                self.history_open = !self.history_open;
                self.history_offset = 0;
                ctx.request_repaint();
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers }
                if self.history_open
                    && ctx.focus.focused_widget() == Some(self.id)
                    && *modifiers == Modifiers::none()
                    && matches!(
                        key,
                        KeyCode::Up
                            | KeyCode::Down
                            | KeyCode::PageUp
                            | KeyCode::PageDown
                            | KeyCode::Home
                            | KeyCode::End
                    ) =>
            {
                let previous = self.history_offset;
                let maximum = self.history.len().saturating_sub(STATUS_HISTORY_PAGE_SIZE);
                self.history_offset = match key {
                    KeyCode::Down => previous.saturating_add(1).min(maximum),
                    KeyCode::Up => previous.saturating_sub(1),
                    KeyCode::PageDown => {
                        previous.saturating_add(STATUS_HISTORY_PAGE_SIZE).min(maximum)
                    }
                    KeyCode::PageUp => previous.saturating_sub(STATUS_HISTORY_PAGE_SIZE),
                    KeyCode::Home => 0,
                    KeyCode::End => maximum,
                    _ => previous,
                };
                if self.history_offset != previous {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::MouseDown { position, .. }
                if self.history_open && self.history_rect().contains(*position) =>
            {
                EventResult::Handled
            }
            UiEvent::MouseUp { position, .. } | UiEvent::MouseMove { position, .. }
                if self.history_open && self.history_rect().contains(*position) =>
            {
                EventResult::Handled
            }
            UiEvent::MouseDown { .. } if self.history_open => {
                self.history_open = false;
                ctx.request_repaint();
                EventResult::Ignored
            }
            UiEvent::MouseWheel { position, delta, .. }
                if self.history_open && self.history_rect().contains(*position) =>
            {
                let previous = self.history_offset;
                let max_offset = self.history.len().saturating_sub(STATUS_HISTORY_PAGE_SIZE);
                if *delta > 0.0 {
                    self.history_offset = self.history_offset.saturating_add(1).min(max_offset);
                } else if *delta < 0.0 {
                    self.history_offset = self.history_offset.saturating_sub(1);
                }
                if self.history_offset != previous {
                    ctx.request_repaint();
                }
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } | UiEvent::FocusLost
                if self.history_open =>
            {
                self.history_open = false;
                ctx.request_repaint();
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
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
        if self.history_open {
            let panel = self.history_rect();
            if panel.width < 48.0 {
                return;
            }
            let padding_x = ctx.theme.spacing.panel_inner_margin.0;
            let available_width = (panel.width - padding_x * 2.0).max(0.0);
            ctx.push_clip(panel);
            ctx.encoder.draw_rect(panel, colors.popover, ctx.theme.spacing.radius_md);
            ctx.encoder.draw_text(
                &elide_text_to_width(
                    &self.localizer.text("status-history-title"),
                    font_size,
                    available_width,
                ),
                font_size,
                Point::new(panel.x + padding_x, panel.y + ctx.theme.spacing.md),
                colors.popover_foreground,
            );
            if self.history.is_empty() {
                ctx.encoder.draw_text(
                    &elide_text_to_width(
                        &self.localizer.text("status-history-empty"),
                        font_size,
                        available_width,
                    ),
                    font_size,
                    Point::new(panel.x + padding_x, panel.y + 36.0),
                    colors.muted_foreground,
                );
            } else {
                for (index, entry) in self
                    .history
                    .iter()
                    .rev()
                    .skip(self.history_offset)
                    .take(STATUS_HISTORY_PAGE_SIZE)
                    .enumerate()
                {
                    let text = elide_text_to_width(
                        &self.history_entry_text(entry),
                        font_size,
                        available_width,
                    );
                    ctx.encoder.draw_text(
                        &text,
                        font_size,
                        Point::new(panel.x + padding_x, panel.y + 36.0 + index as f32 * 26.0),
                        if entry.is_error {
                            colors.destructive_foreground
                        } else {
                            colors.popover_foreground
                        },
                    );
                }
            }
            ctx.pop_clip();
        }
        if !self.history_open {
            self.notifications.paint(ctx, self.bounds);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
            || self.history_open && self.history_rect().contains(point)
            || !self.history_open && self.notifications.hit_test(point, self.bounds)
    }

    fn can_focus(&self) -> bool {
        true
    }

    fn accessibility(&self) -> Option<AccessibilityNode> {
        Some(
            AccessibilityNode::new(self.id, AccessibilityRole::Button)
                .with_name(self.accessible_history_name())
                .with_state(AccessibilityState {
                    focusable: true,
                    pressed: Some(self.history_open),
                    ..AccessibilityState::default()
                }),
        )
    }

    fn accessibility_bounds(&self) -> Option<Rect> {
        Some(self.bounds)
    }
}

pub(super) fn elide_text_to_width(text: &str, font_size: f32, max_width: f32) -> String {
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

fn status_bar_model(state: &AppState, localizer: &Localizer) -> StatusBarModel {
    let jobs = state.export_jobs_snapshot();
    let active_jobs = jobs.iter().filter(|job| !job.status.is_terminal()).collect::<Vec<_>>();

    let (message, is_error, is_busy) = if let Some((message, true)) = state.status_hint() {
        (localizer.message(message), true, false)
    } else if let Some(job) = active_jobs.first() {
        let message = active_export_status_text(
            &job.status,
            &job.progress.detail,
            active_jobs.len(),
            localizer,
        );
        (message, false, true)
    } else if let Some((message, is_error)) = state.status_hint() {
        (localizer.message(message), *is_error, false)
    } else {
        (localizer.text("status-ready"), false, false)
    };
    let context = state
        .active_sequence()
        .map(|sequence| sequence.name.clone())
        .or_else(|| {
            state
                .current_project_path()
                .and_then(|path| path.file_stem())
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| localizer.text("status-no-project"));

    StatusBarModel { message, is_error, is_busy, context }
}

fn active_export_status_text(
    status: &JobStatus,
    detail: &ExportProgressDetail,
    count: usize,
    localizer: &Localizer,
) -> String {
    let mut args = fluent_bundle::FluentArgs::new();
    args.set("count", count as f64);
    let message = match status {
        JobStatus::Pending => "status-export-pending",
        JobStatus::Running { phase } => match detail {
            ExportProgressDetail::Frames { completed, total }
                if *phase == ExportProgressPhase::Rendering =>
            {
                args.set("completed", completed.to_string());
                args.set("total", total.to_string());
                "status-export-frames"
            }
            _ => {
                args.set(
                    "phase",
                    localizer.text(export_progress_phase_message_id(*phase)),
                );
                "status-export-phase"
            }
        },
        JobStatus::Cancelling { .. } => "status-export-cancelling",
        _ => "status-export-active",
    };
    localizer.format(message, Some(&args))
}

/// File dialog filter for timeline export output commands.
pub fn export_output_filters(extension: &str, localizer: &Localizer) -> Vec<FileFilter> {
    let extension = normalized_export_extension(extension);
    if extension.is_empty() {
        vec![FileFilter::new(
            localizer.text("file-filter-media"),
            vec!["mp4", "mov", "mkv", "gif", "mxf", "webm"],
        )]
    } else {
        vec![FileFilter::new(
            localizer.text("file-filter-export"),
            vec![extension],
        )]
    }
}

fn normalized_export_extension(extension: &str) -> String {
    extension.trim().trim_start_matches('.').trim().to_ascii_lowercase()
}

#[cfg(test)]
fn resolve_app_shell_action(
    action: Action,
    platform: &dyn PlatformService,
    current_project_path: Option<&Path>,
) -> Option<Action> {
    let localizer = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
    match try_resolve_app_shell_action(action, platform, current_project_path, &localizer) {
        Ok(action) => action,
        Err(err) => {
            tracing::warn!("app-shell action failed: {err}");
            None
        }
    }
}

/// Resolve a shell request into a concrete editor action using the active formatter.
/// Native file dialogs stay behind [`PlatformService`].
pub fn try_resolve_app_shell_action(
    action: Action,
    platform: &dyn PlatformService,
    current_project_path: Option<&Path>,
    localizer: &Localizer,
) -> Result<Option<Action>> {
    match action {
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_NEW_PROJECT_DIALOG =>
        {
            let path = platform
                .save_file_dialog(
                    &localizer.text("file-dialog-create-project"),
                    &format!(
                        "{}.{PROJECT_FILE_EXTENSION}",
                        localizer.text("file-default-untitled")
                    ),
                    &project_file_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection();
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
            let Some(paths) = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-open-project"),
                    &project_file_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
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
            let Some(paths) = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-import-media"),
                    &media_import_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
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
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_INSTALL_CLAP_LIBRARY_DIALOG =>
        {
            let selected = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-install-clap"),
                    &clap_library_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
                .and_then(|paths| paths.into_iter().next());
            Ok(selected.map(|path| {
                ProductAction::Audio(AudioProductAction::InstallClapLibrary(
                    AudioInstallClapLibraryPayload { path },
                ))
                .into_external_action()
            }))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_INSTALL_VST3_BINARY_DIALOG =>
        {
            let selected = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-install-vst3"),
                    &vst3_binary_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
                .and_then(|paths| paths.into_iter().next());
            Ok(selected.map(|path| {
                ProductAction::Audio(AudioProductAction::InstallVst3Plugin(
                    AudioInstallVst3PluginPayload { path },
                ))
                .into_external_action()
            }))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_INSTALL_VST3_BUNDLE_DIALOG =>
        {
            let selected = platform
                .open_folder_dialog(&localizer.text("file-dialog-install-vst3-bundle"))
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection();
            Ok(selected.map(|path| {
                ProductAction::Audio(AudioProductAction::InstallVst3Plugin(
                    AudioInstallVst3PluginPayload { path },
                ))
                .into_external_action()
            }))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_INSTALL_OPENFX_BUNDLE_DIALOG =>
        {
            let selected = platform
                .open_folder_dialog(&localizer.text("file-dialog-install-openfx-bundle"))
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection();
            Ok(selected.map(|path| {
                ProductAction::VisualEffect(VisualEffectProductAction::InstallOpenFxBundle(
                    VisualEffectInstallOpenFxBundlePayload { path },
                ))
                .into_external_action()
            }))
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_REVEAL_IN_FILE_MANAGER =>
        {
            let payload: AppShellRevealInFileManagerPayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            platform
                .reveal_in_file_manager(&payload.path)
                .map_err(|error| native_shell_error(&name, error))?;
            Ok(None)
        }
        Action::Custom { namespace, name, payload }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_RELINK_ASSET_DIALOG =>
        {
            let payload: AppShellRelinkAssetDialogPayload = serde_json::from_value(payload)
                .map_err(|err| app_shell_action_error(&name, err))?;
            let Some(paths) = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-relink-media"),
                    &media_import_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
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
                .unwrap_or_else(|| {
                    format!(
                        "{}.{PROJECT_FILE_EXTENSION}",
                        localizer.text("file-default-untitled")
                    )
                });
            Ok(platform
                .save_file_dialog(
                    &localizer.text("file-dialog-save-as-project"),
                    &default_name,
                    &project_file_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
                .map(Action::SaveProjectAs))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && name == APP_SHELL_EXPORT_PORTABLE_PACKAGE_DIALOG =>
        {
            let stem = current_project_path
                .and_then(Path::file_stem)
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| localizer.text("file-default-untitled-project"));
            let default_name = format!("{stem}.mdpkg");
            Ok(platform
                .save_file_dialog(
                    &localizer.text("file-dialog-package-project"),
                    &default_name,
                    &portable_package_filters(localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
                .map(app_shell_export_portable_package_action))
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
                    &localizer.text("file-dialog-export-output"),
                    &default_name,
                    &export_output_filters(&extension, localizer),
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
                .map(|path| {
                    export_edit_draft_action(ExportDraftEdit::OutputPath(
                        path.display().to_string(),
                    ))
                }))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && name == crate::app::ui_actions::APP_SHELL_IMPORT_EXPORT_ANCILLARY_DIALOG =>
        {
            let Some(paths) = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-import-ancillary"),
                    &[FileFilter::new(
                        localizer.text("file-filter-ancillary"),
                        vec!["json", "mdanc", "scc", "cdp"],
                    )],
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
            else {
                return Ok(None);
            };
            Ok(paths
                .into_iter()
                .next()
                .map(|path| export_edit_draft_action(ExportDraftEdit::ImportAncillary(path))))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE
                && name == crate::app::ui_actions::APP_SHELL_IMPORT_EXPORT_PSE_DIALOG =>
        {
            let Some(paths) = platform
                .open_file_dialog(
                    &localizer.text("file-dialog-import-pse"),
                    &[FileFilter::new(
                        localizer.text("file-filter-pse"),
                        vec!["json"],
                    )],
                )
                .map_err(|error| native_shell_error(&name, error))?
                .into_selection()
            else {
                return Ok(None);
            };
            Ok(paths
                .into_iter()
                .next()
                .map(|path| export_edit_draft_action(ExportDraftEdit::ImportRegulatoryPse(path))))
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

fn native_shell_error(name: &str, error: impl std::fmt::Display) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: format!("app_shell_action.{name}"),
        reason: error.to_string(),
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
        .current_project_path()
        .and_then(|path| path.file_stem())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Untitled");
    let sequence = state.active_sequence().map(|sequence| sequence.name.as_str());
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

    fn label(self, fit_label: &str) -> String {
        match self {
            Self::Fit => fit_label.to_owned(),
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
    apply_viewer_zoom_mode_to_model(&mut models.viewer, mode);
}

fn apply_viewer_canvas_background(
    models: &mut AppUiPanelModels,
    background: mondrian_ui_widgets::ViewerCanvasBackground,
) {
    models.viewer.canvas_background = background;
}

fn apply_video_scopes_preferences(
    models: &mut AppUiPanelModels,
    state: &AppState,
    preferences: &AppUiPreferences,
) {
    let program_output = state
        .active_sequence()
        .map(|sequence| sequence.settings.color.program_output.color_space)
        .unwrap_or(state.new_sequence_defaults().color.program_output.color_space);
    let signal_color_space = match preferences.video_scopes.tap {
        mondrian_core::ProgramScopesTap::ProgramOutput => program_output,
        mondrian_core::ProgramScopesTap::MonitorOutput => preferences
            .display_management
            .resolve_output_color_space(state.project_color_environment().engine(), program_output)
            .unwrap_or(program_output),
    };
    models.scopes.settings = preferences.video_scopes;
    models.scopes.signal_color_space = signal_color_space;
}

fn project_sequence_color_contracts(state: &AppState) -> Vec<(WorkingColorSpace, ColorSpace)> {
    let defaults = state.new_sequence_defaults();
    let mut contracts = vec![(
        defaults.color.working_color_space,
        defaults.color.program_output.color_space,
    )];
    for sequence in state.sequences() {
        let contract = (
            sequence.settings.color.working_color_space,
            sequence.settings.color.program_output.color_space,
        );
        if !contracts.contains(&contract) {
            contracts.push(contract);
        }
    }
    contracts
}

fn apply_viewer_zoom_mode_to_model(model: &mut ViewerPanelModel, mode: ViewerZoomMode) {
    model.zoom_label = mode.label(&model.fit_label);
    model.zoom_scale = mode.scale();
}

/// Root widget for the app UI editor window.
pub struct AppUiAppRoot {
    native_visual_gui: std::sync::Arc<crate::app::preview_native_gui::PreviewNativeGuiRegistry>,
    id: WidgetId,
    title_bar: TitleBar,
    dock: DockSplitter,
    status_bar: StatusBar,
    notification_session_id: Option<AuthoringSessionId>,
    models: AppUiPanelModels,
    asset_folder_id: Option<String>,
    preferences_model: AppUiPreferencesModel,
    localizer: Localizer,
    audio_output_device_catalog: AudioOutputDeviceCatalogState,
    workspace_preset: WorkspacePreset,
    custom_workspace_layout: Option<AppUiWorkspaceLayout>,
    viewer_zoom_mode: ViewerZoomMode,
    active_sequence: Option<Sequence>,
    project_color_environment: mondrian_core::ProjectColorEnvironment,
    new_sequence_defaults: mondrian_timeline::SequenceSettings,
    project_sequence_color_contracts: Vec<(WorkingColorSpace, ColorSpace)>,
    modal: Option<ShellModal>,
    bounds: Rect,
}

fn resource_asset_catalog_revision(
    state: &AppState,
) -> Result<Option<crate::app_ui::resource_dialog::ResourceAssetCatalogRevision>> {
    state
        .asset_library_handle()
        .map(|library| {
            Ok(
                crate::app_ui::resource_dialog::ResourceAssetCatalogRevision {
                    library: std::sync::Arc::downgrade(&library),
                    revision: library.database_revision()?,
                },
            )
        })
        .transpose()
}

fn resource_asset_choices(
    state: &AppState,
) -> Result<Vec<crate::app_ui::resource_dialog::ResourceAssetChoice>> {
    let mut assets = state
        .asset_library()
        .map(|library| library.list_assets())
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .map(
            |asset| crate::app_ui::resource_dialog::ResourceAssetChoice {
                id: asset.id,
                label: asset.file_path().map_or_else(
                    || asset.name.clone(),
                    |path| format!("{} · {}", asset.name, path.display()),
                ),
            },
        )
        .collect::<Vec<_>>();
    assets.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
    Ok(assets)
}

impl AppUiAppRoot {
    /// Route typed resource drafts before ordinary author actions.
    pub(crate) fn try_handle_resource_action(
        &mut self,
        action: &Action,
        state: &AppState,
        platform: &dyn PlatformService,
    ) -> Result<crate::app_ui::resource_dialog::ResourceActionOutcome> {
        use crate::app_ui::resource_dialog::{
            ResourceActionOutcome, ResourceDialog, ResourceDraft, ResourceEditorAction,
            RESOURCE_EDITOR,
        };
        let Action::Custom { namespace, name, payload } = action else {
            return Ok(ResourceActionOutcome::Unhandled);
        };
        if namespace != APP_SHELL_NAMESPACE || name != RESOURCE_EDITOR {
            return Ok(ResourceActionOutcome::Unhandled);
        }
        let event: ResourceEditorAction = serde_json::from_value(payload.clone())
            .map_err(|error| app_shell_action_error(name, error))?;
        // A failed presentation read must not prevent editing or cancelling a
        // local draft. Open and changed-asset Confirm still require fresh reads.
        if let Err(error) = self.synchronize_resource_draft(state) {
            tracing::warn!(%error, "failed to refresh resource asset catalog before draft event");
        }
        let refreshed = matches!(
            &event,
            ResourceEditorAction::Open { .. } | ResourceEditorAction::Confirm { .. }
        )
        .then(|| {
            crate::app_ui::panels::InspectorPanelModel::from_app_state(state, &self.localizer)
        });
        let model = refreshed.as_ref().unwrap_or(&self.models.inspector);
        if let ResourceEditorAction::Open { clip_id, target } = event {
            let Some(property) = target.property(clip_id, model) else {
                return Ok(ResourceActionOutcome::Handled(None));
            };
            let mondrian_core::PropertyValue::Resource(reference) = &property.value else {
                return Ok(ResourceActionOutcome::Handled(None));
            };
            let draft = ResourceDraft::new(
                clip_id,
                target,
                reference.clone(),
                property.schema.clone(),
                state.authoring_session_id(),
                state.active_sequence_id(),
                state.current_project_path().map(Path::to_path_buf),
            );
            let (catalog_revision, assets) =
                if draft.allows_kind(crate::app_ui::resource_dialog::ResourceKind::ProjectAsset) {
                    (
                        resource_asset_catalog_revision(state)?,
                        resource_asset_choices(state)?,
                    )
                } else {
                    (None, Vec::new())
                };
            self.show_modal(ShellModal::Resource(Box::new(ResourceDialog::new(
                draft,
                property.label.clone(),
                assets,
                catalog_revision,
                self.localizer.locale(),
            ))));
            self.layout(self.bounds);
            return Ok(ResourceActionOutcome::Handled(None));
        }
        let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_resource_mut) else {
            return Ok(ResourceActionOutcome::Handled(None));
        };
        if event.id() != Some(dialog.draft.id) {
            return Ok(ResourceActionOutcome::Handled(None));
        }
        if dialog.draft.session != state.authoring_session_id()
            || dialog.draft.sequence != state.active_sequence_id()
            || !dialog.draft.matches_state(state)
            || !dialog.draft.matches_model(model)
        {
            self.modal = None;
            return Ok(ResourceActionOutcome::Handled(None));
        }
        match event {
            ResourceEditorAction::Update { update, .. } => dialog.apply_update(update),
            ResourceEditorAction::Browse { folder, .. } => {
                if !dialog.draft.allows_browse(folder) || dialog.is_composing() {
                    return Ok(ResourceActionOutcome::Handled(None));
                }
                dialog.clear_inspection();
                let filters = dialog
                    .draft
                    .schema
                    .resource
                    .as_ref()
                    .filter(|contract| !contract.file_extensions.is_empty())
                    .map_or_else(Vec::new, |contract| {
                        vec![FileFilter::new(
                            self.localizer.text("resource-file-only"),
                            contract.file_extensions.clone(),
                        )]
                    });
                let selection = if folder {
                    platform
                        .open_folder_dialog(&self.localizer.text("resource-browse-folder"))
                        .map(|outcome| outcome.into_selection())
                } else {
                    platform
                        .open_file_dialog(
                            &self.localizer.text(dialog.draft.file_browse_message()),
                            &filters,
                        )
                        .map(|outcome| {
                            outcome.into_selection().and_then(|mut paths| {
                                if paths.len() == 1 {
                                    paths.pop()
                                } else {
                                    if !paths.is_empty() {
                                        dialog.set_error("resource-select-one-file");
                                    }
                                    None
                                }
                            })
                        })
                };
                match selection {
                    Ok(Some(path)) => dialog.selected_path(path),
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(%error, "resource picker failed");
                        dialog.set_error("resource-browse-failed");
                    }
                }
            }
            ResourceEditorAction::Confirm { .. } => {
                if dialog.is_composing() || dialog.pending_inspection.is_some() {
                    return Ok(ResourceActionOutcome::Handled(None));
                }
                let value = match dialog.draft.confirmed_value() {
                    Ok(Some(value)) => value,
                    Ok(None) => {
                        self.modal = None;
                        return Ok(ResourceActionOutcome::Handled(None));
                    }
                    Err(key) => {
                        dialog.set_error(key);
                        return Ok(ResourceActionOutcome::Handled(None));
                    }
                };
                if let mondrian_core::ParameterResourceReference::ExternalFile { path } = &value
                    && dialog
                        .draft
                        .schema
                        .resource
                        .as_ref()
                        .and_then(|contract| contract.path)
                        .is_some_and(|contract| {
                            contract.selection != mondrian_core::ResourcePathSelection::Reference
                        })
                {
                    let path = path
                        .native_path()
                        .map_err(|error| mondrian_core::MondrianError::WorkflowStepFailed {
                            step_id: "resource_inspection".into(),
                            reason: error.to_string(),
                        })?
                        .to_path_buf();
                    let ticket = crate::resource_inspection::InspectionTicket::new();
                    dialog.begin_inspection(ticket);
                    return Ok(ResourceActionOutcome::Inspect(
                        crate::app_ui::resource_dialog::ResourceInspectionRequest { ticket, path },
                    ));
                }
                if let mondrian_core::ParameterResourceReference::ProjectAsset { asset_id } = value
                {
                    let current = state
                        .asset_library()
                        .map(|library| library.get_asset(asset_id))
                        .transpose()?
                        .flatten();
                    if current.as_ref().is_none_or(|asset| asset.membership.is_retired()) {
                        dialog.set_error("resource-asset-missing");
                        return Ok(ResourceActionOutcome::Handled(None));
                    }
                }
                let author = dialog.draft.author_action(value);
                self.modal = None;
                return Ok(ResourceActionOutcome::Handled(author));
            }
            ResourceEditorAction::Cancel { .. } => self.modal = None,
            ResourceEditorAction::Open { .. } => {}
        }
        Ok(ResourceActionOutcome::Handled(None))
    }

    pub(crate) fn resource_inspection_ticket(
        &self,
        state: &AppState,
    ) -> Option<crate::resource_inspection::InspectionTicket> {
        self.modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .filter(|dialog| {
                dialog.draft.matches_state(state) && !state.project_lifecycle_blocks_actions()
            })
            .and_then(|dialog| dialog.pending_inspection)
    }

    pub(crate) fn finish_resource_inspection(
        &mut self,
        ticket: crate::resource_inspection::InspectionTicket,
        outcome: crate::resource_inspection::PathInspectionOutcome,
        state: &AppState,
    ) -> Option<Action> {
        let dialog = self.modal.as_mut().and_then(ShellModal::as_resource_mut)?;
        if dialog.pending_inspection != Some(ticket) {
            return None;
        }
        dialog.clear_inspection();
        if !dialog.draft.matches_state(state) || state.project_lifecycle_blocks_actions() {
            self.modal = None;
            return None;
        }
        let value = match dialog.draft.confirmed_value() {
            Ok(Some(value)) => value,
            Ok(None) => {
                self.modal = None;
                return None;
            }
            Err(key) => {
                dialog.set_error(key);
                return None;
            }
        };
        let contract = dialog.draft.schema.resource.as_ref().and_then(|contract| contract.path)?;
        if let Err(key) = outcome.validate(contract) {
            dialog.set_error(key);
            return None;
        }
        if dialog.is_composing() {
            dialog.set_error("resource-path-confirm-again");
            return None;
        }
        let action = dialog.draft.author_action(value);
        if action.as_ref().is_some_and(|action| {
            !crate::app_ui::action_availability::app_state_action_enabled(action, state)
        }) {
            dialog.set_error("resource-owner-unavailable");
            return None;
        }
        self.modal = None;
        action
    }

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
        waveform_source: Option<AudioWaveformSource>,
    ) -> Self {
        let viewer_zoom_mode = ViewerZoomMode::Fit;
        let locale = preferences.locale_preference.resolve(sys_locale::get_locale().as_deref());
        let localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        let mut models =
            AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_preview_and_locale(
                state,
                None,
                thumbnails,
                preview,
                Some(&localizer),
            );
        models.timeline.waveform_display = preferences.waveform_display;
        models.timeline.waveform_source = waveform_source;
        apply_video_scopes_preferences(&mut models, state, preferences);
        apply_viewer_canvas_background(&mut models, preferences.viewer_canvas_background);
        apply_viewer_zoom_mode(&mut models, viewer_zoom_mode);
        let mut preferences_model =
            AppUiPreferencesModel::from_app_state_with_shortcut_overrides_and_audio_output(
                state,
                preferences.workspace_preset,
                preferences.theme_preference,
                preferences.theme_preference.resolve(ThemePreset::Dark),
                &preferences.shortcut_overrides,
                preferences.waveform_display,
                preferences.viewer_canvas_background,
                preferences.audio_output_device.clone(),
                AudioOutputDeviceCatalogState::Loading,
            );
        preferences_model.set_locale_preference(
            preferences.locale_preference,
            sys_locale::get_locale().as_deref(),
        );
        let mut menu_bar =
            MenuBar::for_app_state_with_shortcut_overrides(state, &preferences.shortcut_overrides);
        menu_bar.set_locale(preferences_model.locale);
        let status_model = status_bar_model(
            state,
            &Localizer::new(preferences_model.locale).expect("bundled UI catalogs must be valid"),
        );
        let mut root = Self::new_with_preferences(
            TitleBar::new(window_title_for_app_state(state), menu_bar),
            models,
            preferences_model,
            preferences.workspace_preset,
            preferences.custom_workspace_layout.clone(),
            status_model,
        );
        root.status_bar.sync_history(state.status_log());
        root.status_bar.notifications =
            NotificationOverlay::from_existing(&state.notifications, root.preferences_model.locale);
        root.notification_session_id = state.authoring_session_id();
        root.active_sequence = state.active_sequence().cloned();
        root.project_color_environment = state.project_color_environment().clone();
        root.new_sequence_defaults = state.new_sequence_defaults().clone();
        root.project_sequence_color_contracts = project_sequence_color_contracts(state);
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
            StatusBarModel::default(),
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
        let native_visual_gui = std::sync::Arc::new(
            crate::app::preview_native_gui::PreviewNativeGuiRegistry::default(),
        );
        bind_native_visual_gui_models(&mut models, &native_visual_gui);
        let dock = build_dock_tree_for_workspace(
            models.clone(),
            workspace_preset,
            custom_workspace_layout.as_ref(),
        );
        let audio_output_device_catalog = preferences_model.audio_output_device_catalog.clone();
        let localizer =
            Localizer::new(preferences_model.locale).expect("bundled UI catalogs must be valid");
        let mut root = Self {
            native_visual_gui,
            id: WidgetId::new(),
            title_bar,
            dock,
            status_bar: StatusBar {
                id: WidgetId::new(),
                bounds: Rect::ZERO,
                model: status_bar_model,
                history: Vec::new(),
                localizer: Localizer::new(preferences_model.locale)
                    .expect("bundled UI catalogs must be valid"),
                history_open: false,
                history_offset: 0,
                notifications: NotificationOverlay::from_existing(
                    &Default::default(),
                    preferences_model.locale,
                ),
            },
            notification_session_id: None,
            models,
            asset_folder_id: None,
            preferences_model,
            localizer,
            audio_output_device_catalog,
            workspace_preset,
            custom_workspace_layout,
            viewer_zoom_mode,
            active_sequence: None,
            project_color_environment: mondrian_core::ProjectColorEnvironment::default(),
            new_sequence_defaults: mondrian_timeline::SequenceSettings::default(),
            project_sequence_color_contracts: vec![(
                WorkingColorSpace::LinearRec2020,
                ColorSpace::Rec709,
            )],
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
        if self.workspace_preset == WorkspacePreset::Custom
            && let Some(previous) = self.custom_workspace_layout.as_ref()
        {
            return Some(layout.with_panel_metadata_from(previous));
        }
        Some(layout)
    }

    pub(crate) fn native_visual_gui_registry(
        &self,
    ) -> std::sync::Arc<crate::app::preview_native_gui::PreviewNativeGuiRegistry> {
        std::sync::Arc::clone(&self.native_visual_gui)
    }

    /// Whether `panel` is the active visible tab, without allocating a layout snapshot.
    pub fn is_panel_active(&self, panel: PanelKind) -> bool {
        is_panel_active_in_widget(&self.dock, panel)
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

    /// Admit an opaque delivery into the one existing modal/focus owner.
    /// Other modals keep their lifetime; unavailable native UI returns Failed.
    pub(crate) fn show_native_visual_question(
        &mut self,
        presentation: crate::app::preview_native_gui::NativeVisualQuestionPresentation,
    ) -> bool {
        self.retire_stale_native_visual_question();
        if self.modal.is_some() || !presentation.is_current() {
            presentation.dismiss();
            return false;
        }
        self.show_modal(ShellModal::native_visual_question(
            presentation,
            self.preferences_model.locale,
        ));
        true
    }

    pub(crate) fn retire_stale_native_visual_question(&mut self) -> bool {
        if self
            .modal
            .as_ref()
            .and_then(ShellModal::as_native_visual_question)
            .is_some_and(|dialog| !dialog.is_current())
        {
            self.modal = None;
            return true;
        }
        false
    }

    #[cfg(feature = "validation")]
    pub(crate) fn validation_native_question_controls(
        &self,
    ) -> Option<super::native_question_validation::NativeQuestionValidationControls> {
        self.modal
            .as_ref()
            .and_then(ShellModal::as_native_visual_question)
            .and_then(|dialog| dialog.validation_controls())
    }

    pub(crate) fn painted_native_visual_question(
        &self,
    ) -> Option<crate::app::preview_native_gui::NativeVisualQuestionPresentation> {
        self.modal
            .as_ref()
            .and_then(ShellModal::as_native_visual_question)
            .and_then(|dialog| dialog.painted_delivery())
    }

    fn show_modal(&mut self, modal: ShellModal) {
        cancel_keyboard_activations(self);
        self.modal = Some(modal);
    }

    /// Show the pending-close confirmation modal.
    pub fn show_pending_close_dialog(&mut self, action: PendingCloseDialogAction) {
        self.show_modal(ShellModal::pending_close(
            action,
            self.preferences_model.locale,
        ));
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

    /// Stable titlebar identity exposed to Host refresh-domain tests.
    #[cfg(test)]
    pub(crate) fn title_bar_id_for_test(&self) -> WidgetId {
        self.title_bar.id()
    }

    /// Validate the draft owner and reconcile changed library presentation before input deferral.
    pub(crate) fn synchronize_resource_draft(&mut self, state: &AppState) -> Result<()> {
        if self
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .is_some_and(|dialog| !dialog.draft.matches_state(state))
        {
            self.modal = None;
        }
        let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_resource_mut) else {
            return Ok(());
        };
        if !dialog
            .draft
            .allows_kind(crate::app_ui::resource_dialog::ResourceKind::ProjectAsset)
        {
            return Ok(());
        }
        let revision = resource_asset_catalog_revision(state)?;
        if dialog.asset_catalog_revision != revision {
            // Capture the revision before reading rows. A concurrent later commit
            // then causes another refresh, rather than marking older rows current.
            dialog.replace_asset_catalog(revision, resource_asset_choices(state)?);
        }
        Ok(())
    }

    /// Replace panel contents from a fresh model snapshot while preserving the
    /// root widget id and menu state.
    pub fn set_models(&mut self, mut models: AppUiPanelModels) {
        bind_native_visual_gui_models(&mut models, &self.native_visual_gui);
        if self
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .is_some_and(|dialog| !dialog.draft.matches_model(&models.inspector))
        {
            self.modal = None;
        }

        let curve_states = if self.models.author_control_scope.is_some()
            && self.models.author_control_scope == models.author_control_scope
        {
            collect_curve_view_state(&self.dock)
        } else {
            Vec::new()
        };
        let control_states = if self.models.author_control_scope.is_some()
            && self.models.author_control_scope == models.author_control_scope
        {
            collect_control_inspection_state(&self.dock)
        } else {
            Vec::new()
        };
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
        restore_curve_view_state(&mut self.dock, &curve_states);
        restore_control_inspection_state(&mut self.dock, &control_states);
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
            version: AppUiPreferences::default().version,
            theme_preference: self.preferences_model.theme_preference,
            locale_preference: self.preferences_model.locale_preference,
            workspace_preset: self.workspace_preset,
            recent_projects: Vec::new(),
            installed_audio_plugins: Vec::new(),
            installed_openfx_bundles: Vec::new(),
            shortcut_overrides: Vec::new(),
            custom_workspace_layout: self.custom_workspace_layout.clone(),
            waveform_display: WaveformDisplay::BottomAligned,
            viewer_canvas_background: self.models.viewer.canvas_background,
            video_scopes: self.models.scopes.settings,
            audio_output_device: Default::default(),
            reference_output: Default::default(),
            display_management: self.preferences_model.display_management.clone(),
        };
        self.refresh_from_app_state_with_preferences(state, &preferences);
    }

    /// Refresh only playback-frame dependent UI state.
    pub fn refresh_playback_frame_from_app_state(
        &mut self,
        state: &AppState,
        preview: Option<&dyn ViewerPreviewSource>,
    ) -> bool {
        let frame = state.current_frame().max(0);
        self.status_bar.set_model(status_bar_model(state, &self.localizer));
        self.status_bar.sync_history(state.status_log());
        self.sync_notifications(state, self.preferences_model.locale);
        let mut viewer = ViewerPanelModel::from_app_state_with_preview_and_locale(
            state,
            preview,
            Some(&self.localizer),
        );
        if preview.is_none() && viewer.enabled && self.models.viewer.enabled {
            viewer.retain_presentation_from(&self.models.viewer, state, Some(&self.localizer));
        }
        apply_viewer_zoom_mode_to_model(&mut viewer, self.viewer_zoom_mode);
        let preview_waiting = viewer.preview_waiting;
        self.models.scopes = ScopesPanelModel::from_viewer_with_settings(
            &viewer,
            self.models.scopes.settings,
            self.models.scopes.signal_color_space,
        );
        self.models.viewer = viewer;
        self.models.timeline.playhead_frame = frame;
        update_viewer_widgets(&mut self.dock, &self.models.viewer);
        update_scopes_widgets(&mut self.dock, &self.models.scopes);
        update_timeline_playhead_widgets(&mut self.dock, frame);
        preview_waiting
    }

    /// Refresh a user transport intent without claiming the retained output is exact.
    pub fn refresh_transport_intent_from_app_state(&mut self, state: &AppState) {
        self.refresh_playback_frame_from_app_state(state, None);
        self.models
            .viewer
            .mark_presentation_pending_after_transport_intent(state, Some(&self.localizer));
        update_viewer_widgets(&mut self.dock, &self.models.viewer);
    }

    /// Return the payload-free Viewer lifecycle meaning used by playback.
    pub(crate) fn viewer_playback_feedback(
        &self,
    ) -> crate::app_ui::playback_feedback::ViewerPlaybackFeedback {
        crate::app_ui::playback_feedback::ViewerPlaybackFeedback::from_viewer_model(
            &self.models.viewer,
        )
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
        waveform_source: Option<AudioWaveformSource>,
    ) {
        self.title_bar.set_title(window_title_for_app_state(state));
        self.title_bar
            .menu_bar_mut()
            .refresh_for_app_state_with_shortcut_overrides(state, &preferences.shortcut_overrides);
        self.status_bar.sync_history(state.status_log());
        self.active_sequence = state.active_sequence().cloned();
        if let Err(error) = self.synchronize_resource_draft(state) {
            tracing::warn!(%error, "failed to refresh resource asset catalog");
        }

        self.project_color_environment = state.project_color_environment().clone();
        self.new_sequence_defaults = state.new_sequence_defaults().clone();
        self.project_sequence_color_contracts = project_sequence_color_contracts(state);
        let locale = if preferences.locale_preference == self.preferences_model.locale_preference {
            self.preferences_model.locale
        } else {
            preferences.locale_preference.resolve(sys_locale::get_locale().as_deref())
        };
        let localizer = Localizer::new(locale).expect("bundled UI catalogs must be valid");
        self.status_bar.set_model(status_bar_model(state, &localizer));
        if self.status_bar.localizer.locale() != locale {
            self.status_bar.localizer =
                Localizer::new(locale).expect("bundled UI catalogs must be valid");
        }
        let mut models =
            AppUiPanelModels::from_app_state_with_asset_folder_thumbnails_preview_and_locale(
                state,
                self.asset_folder_id.as_deref(),
                thumbnails,
                preview,
                Some(&localizer),
            );
        models.timeline.waveform_display = preferences.waveform_display;
        models.timeline.waveform_source = waveform_source;
        apply_video_scopes_preferences(&mut models, state, preferences);
        apply_viewer_canvas_background(&mut models, preferences.viewer_canvas_background);
        apply_viewer_zoom_mode(&mut models, self.viewer_zoom_mode);
        self.localizer = localizer;
        self.set_models(models);
        if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_resource_mut)
            && let Some(property) =
                dialog.draft.target.property(dialog.draft.clip_id, &self.models.inspector)
        {
            dialog.set_locale(locale, property.label.clone());
        }
        let preferences_model =
            AppUiPreferencesModel::from_app_state_with_shortcut_overrides_and_audio_output(
                state,
                self.workspace_preset,
                preferences.theme_preference,
                preferences.theme_preference.resolve(ThemePreset::Dark),
                &preferences.shortcut_overrides,
                preferences.waveform_display,
                preferences.viewer_canvas_background,
                preferences.audio_output_device.clone(),
                self.audio_output_device_catalog.clone(),
            );
        let mut preferences_model = preferences_model;
        if preferences.locale_preference == self.preferences_model.locale_preference {
            preferences_model.locale_preference = self.preferences_model.locale_preference;
            preferences_model.locale = self.preferences_model.locale;
        } else {
            preferences_model.set_locale_preference(
                preferences.locale_preference,
                sys_locale::get_locale().as_deref(),
            );
        }
        preferences_model
            .set_display_output_snapshot(self.preferences_model.display_output_snapshot.clone());
        self.preferences_model = preferences_model.clone();
        self.title_bar.menu_bar_mut().set_locale(preferences_model.locale);
        self.sync_notifications(state, preferences_model.locale);
        if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_preferences_mut) {
            dialog.set_model(preferences_model);
        }
        self.refresh_shell_menu_checked_state();
    }

    /// Remove expired transient messages without changing authoring state.
    pub(crate) fn expire_notifications(&mut self, now: std::time::Instant) -> bool {
        self.status_bar.expire_notifications(now)
    }

    fn sync_notifications(
        &mut self,
        state: &AppState,
        locale: crate::app_ui::localization::AppUiLocale,
    ) {
        if self.notification_session_id != state.authoring_session_id() {
            if self.modal.as_ref().and_then(ShellModal::as_resource).is_some() {
                self.modal = None;
            }

            self.notification_session_id = state.authoring_session_id();
            self.status_bar.notifications =
                NotificationOverlay::from_existing(&state.notifications, locale);
        } else {
            self.status_bar.notifications.sync(
                &state.notifications,
                locale,
                std::time::Instant::now(),
            );
        }
    }

    /// Earliest deadline at which a toast must be removed from the surface.
    pub(crate) fn next_notification_deadline(&self) -> Option<std::time::Instant> {
        self.status_bar.next_notification_deadline()
    }

    /// Publish one device-catalog observation without rebuilding editor panels.
    pub fn set_audio_output_device_catalog(&mut self, catalog: AudioOutputDeviceCatalogState) {
        if self.audio_output_device_catalog == catalog {
            return;
        }
        self.audio_output_device_catalog = catalog.clone();
        self.preferences_model.set_audio_output_device_catalog(catalog);
        if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_preferences_mut) {
            dialog.set_model(self.preferences_model.clone());
        }
    }

    /// Publish the latest structured display diagnosis into Preferences.
    pub fn set_display_output_snapshot(
        &mut self,
        snapshot: Option<mondrian_core::display_contract::DisplayOutputSnapshot>,
    ) {
        if self.preferences_model.display_output_snapshot == snapshot {
            return;
        }
        self.preferences_model.set_display_output_snapshot(snapshot);
        if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_preferences_mut) {
            dialog.set_model(self.preferences_model.clone());
        }
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
                self.show_modal(ShellModal::new_project_with_locale(
                    AppUiNewProjectDraft::for_locale(self.preferences_model.locale),
                    self.preferences_model.locale,
                ));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_NEW_PROJECT_DRAFT_CHANGED =>
            {
                if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_new_project_mut)
                    && let Ok(update) =
                        serde_json::from_value::<NewProjectDraftUpdatePayload>(payload)
                {
                    dialog.apply_update(update);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_SELECT_CUSTOM_OCIO_CONFIG =>
            {
                if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_new_project_mut) {
                    dialog.choose_custom_ocio(platform);
                } else if let Some(dialog) =
                    self.modal.as_mut().and_then(ShellModal::as_project_settings_mut)
                {
                    dialog.choose_custom_ocio(platform);
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
                let Some(path) = platform
                    .save_file_dialog(
                        &self.localizer.text("file-dialog-create-project"),
                        &default_project_file_name(&draft.display_name()),
                        &project_file_filters(&self.localizer),
                    )
                    .map_err(|error| native_shell_error(&name, error))?
                    .into_selection()
                else {
                    return Ok(None);
                };
                self.modal = None;
                Ok(Some(project_create_with_settings_action(
                    draft.into_payload(path),
                )))
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PROJECT_SETTINGS =>
            {
                self.show_modal(ShellModal::project_settings_with_locale(
                    AppUiProjectSettingsDraft::new(
                        self.project_color_environment.engine().clone(),
                        self.project_sequence_color_contracts.clone(),
                    ),
                    self.preferences_model.locale,
                ));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_PROJECT_SETTINGS_DRAFT_CHANGED =>
            {
                let update: ProjectSettingsDraftUpdatePayload = serde_json::from_value(payload)
                    .map_err(|err| app_shell_action_error(&name, err))?;
                if let Some(dialog) =
                    self.modal.as_mut().and_then(ShellModal::as_project_settings_mut)
                {
                    dialog.apply_update(update);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CONFIRM_PROJECT_SETTINGS =>
            {
                let Some(engine) = self
                    .modal
                    .as_ref()
                    .and_then(ShellModal::as_project_settings)
                    .map(|dialog| dialog.draft().engine.clone())
                else {
                    return Ok(None);
                };
                self.modal = None;
                Ok(Some(project_update_color_environment_action(
                    ProjectUpdateColorEnvironmentPayload {
                        color_environment: mondrian_core::ProjectColorEnvironment::new(engine),
                    },
                )))
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_ABOUT =>
            {
                self.show_modal(ShellModal::about(self.localizer.locale()));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_PREFERENCES =>
            {
                self.show_modal(ShellModal::preferences(self.preferences_model()));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_PREFERENCES_SELECT_DISPLAY_ICC_PROFILE =>
            {
                let Some(path) = platform
                    .open_file_dialog(
                        &self.localizer.text("file-dialog-select-icc"),
                        &display_icc_profile_filters(&self.localizer),
                    )
                    .map_err(|error| native_shell_error(&name, error))?
                    .into_selection()
                    .and_then(|paths| paths.into_iter().next())
                else {
                    return Ok(None);
                };
                let policy = self
                    .preferences_model
                    .display_management
                    .with_calibration(mondrian_core::DisplayCalibrationPolicy::IccProfilePath(
                        path.display().to_string(),
                    ))
                    .map_err(|error| MondrianError::WorkflowStepFailed {
                        step_id: "display_management.select_icc_profile".to_owned(),
                        reason: error.to_string(),
                    })?;
                Ok(Some(
                    app_shell_preferences_display_management_changed_action(policy),
                ))
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_INTERPRET_ASSET_DIALOG =>
            {
                let payload: AppShellInterpretAssetDialogPayload = serde_json::from_value(payload)
                    .map_err(|err| app_shell_action_error(&name, err))?;
                let draft = AppUiInterpretAssetDraft::new(
                    payload.asset_id,
                    payload.asset_name,
                    payload.interpretation,
                    payload.auto_interpretation,
                )
                .with_input_diagnostics(payload.video_signal, payload.input_pipeline);
                self.show_modal(ShellModal::interpret_asset(draft, self.localizer.locale()));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_INTERPRET_ASSET_DRAFT_CHANGED =>
            {
                let update: InterpretAssetDraftUpdatePayload = serde_json::from_value(payload)
                    .map_err(|err| app_shell_action_error(&name, err))?;
                if let Some(dialog) =
                    self.modal.as_mut().and_then(ShellModal::as_interpret_asset_mut)
                {
                    dialog.apply_update(update);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CONFIRM_INTERPRET_ASSET_DIALOG =>
            {
                let Some(draft) = self
                    .modal
                    .as_ref()
                    .and_then(ShellModal::as_interpret_asset)
                    .map(|dialog| dialog.draft().clone())
                else {
                    return Ok(None);
                };
                self.modal = None;
                Ok(Some(assets_set_interpretation_action(draft.into_payload())))
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SEQUENCE_SETTINGS =>
            {
                let draft = self.active_sequence.as_ref().map(|sequence| {
                    AppUiSequenceSettingsDraft::from_sequence_in_environment(
                        sequence,
                        &self.project_color_environment,
                    )
                });
                if let Some(draft) = draft {
                    self.show_modal(ShellModal::sequence_settings_with_locale(
                        draft,
                        self.preferences_model.locale,
                    ));
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
                    && let Ok(update) =
                        serde_json::from_value::<SequenceSettingsDraftUpdatePayload>(payload)
                {
                    dialog.apply_update(update);
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
                if let Err(error) = draft.validate() {
                    if let Some(dialog) =
                        self.modal.as_mut().and_then(ShellModal::as_sequence_settings_mut)
                    {
                        dialog.set_validation_error(&error);
                    }
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
                    self.show_modal(ShellModal::preferences_with_tab(
                        self.preferences_model(),
                        tab,
                    ));
                    if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                        self.layout(self.bounds);
                    }
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_COPY_SYSTEM_INFO =>
            {
                if let Ok(p) = serde_json::from_value::<AppShellCopySystemInfoPayload>(payload) {
                    let _ = platform.clipboard_copy(&p.text);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_CLOSE_MODAL =>
            {
                self.modal = None;
                Ok(None)
            }
            action => try_resolve_app_shell_action(
                action,
                platform,
                current_project_path,
                &self.localizer,
            ),
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
    if preset == WorkspacePreset::Custom
        && let Some(layout) = custom_layout
        && let Some(dock) = build_dock_tree_from_layout(models.clone(), layout)
    {
        return dock;
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
        PanelKind::Inspector => vec![(PanelKind::Inspector, 0), (PanelKind::Mixer, 1)],
        PanelKind::Viewer
        | PanelKind::Scopes
        | PanelKind::Timeline
        | PanelKind::Mixer
        | PanelKind::NodeGraph
        | PanelKind::Export => vec![(panel, 0)],
    }
}

fn preferred_workspace_for_panel(panel: PanelKind) -> WorkspacePreset {
    match panel {
        PanelKind::Export => WorkspacePreset::Export,
        PanelKind::NodeGraph => WorkspacePreset::Compositing,
        PanelKind::Scopes => WorkspacePreset::Color,
        PanelKind::Mixer => WorkspacePreset::Audio,
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
    if let Some(panel) = widget.as_any_mut().and_then(|any| any.downcast_mut::<DockPanel>())
        && panel.kind() == owner
    {
        if active_index >= panel.tab_count() {
            return false;
        }
        panel.set_active_index(active_index);
        return true;
    }

    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index)
            && activate_panel_in_widget(child, owner, active_index)
        {
            return true;
        }
    }
    false
}

fn is_panel_active_in_widget(widget: &dyn Widget, target: PanelKind) -> bool {
    if let Some(panel) = widget.as_any().and_then(|any| any.downcast_ref::<DockPanel>()) {
        return panel.active_panel_kind() == target;
    }
    (0..widget.child_count()).any(|index| {
        widget
            .child(index)
            .is_some_and(|child| is_panel_active_in_widget(child, target))
    })
}

fn update_timeline_playhead_widgets(widget: &mut dyn Widget, frame: i64) -> bool {
    if let Some(timeline) = widget.as_any_mut().and_then(|any| any.downcast_mut::<TimelineView>()) {
        timeline.set_playhead_frame(frame);
        return true;
    }

    let mut updated = false;
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            updated |= update_timeline_playhead_widgets(child, frame);
        }
    }
    updated
}

fn update_viewer_widgets(widget: &mut dyn Widget, model: &ViewerPanelModel) -> bool {
    if let Some(viewer) = widget.as_any_mut().and_then(|any| any.downcast_mut::<ViewerSurface>()) {
        viewer.set_playback_frame_content_state(
            model.status.clone(),
            model.status_tone,
            model.position_label.clone(),
            model.playing,
            model.frame_content.clone(),
            model.empty_message.clone(),
        );
        viewer.set_canvas_background(model.canvas_background);
        return true;
    }

    let mut updated = false;
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            updated |= update_viewer_widgets(child, model);
        }
    }
    updated
}

fn update_scopes_widgets(widget: &mut dyn Widget, model: &ScopesPanelModel) -> bool {
    if let Some(scopes) =
        widget.as_any_mut().and_then(|any| any.downcast_mut::<VideoScopesSurface>())
    {
        scopes.set_textures(model.textures.clone());
        scopes.set_settings(model.settings, model.signal_color_space);
        return true;
    }
    (0..widget.child_count()).fold(false, |updated, index| {
        updated | widget.child_mut(index).is_some_and(|child| update_scopes_widgets(child, model))
    })
}

pub(crate) fn viewer_presentation_geometry(
    widget: &dyn Widget,
    pixels_per_point: f32,
) -> Option<ViewerPresentationGeometry> {
    if let Some(viewer) = widget.as_any().and_then(|any| any.downcast_ref::<ViewerSurface>()) {
        return viewer.presentation_geometry(pixels_per_point);
    }
    for index in 0..widget.child_count() {
        if let Some(geometry) = widget
            .child(index)
            .and_then(|child| viewer_presentation_geometry(child, pixels_per_point))
        {
            return Some(geometry);
        }
    }
    None
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
    if let Some(panel) = widget.as_any_mut().and_then(|any| any.downcast_mut::<DockPanel>())
        && let Some(state) = states.iter().find(|state| state.owner == panel.kind())
    {
        panel.set_active_index(state.active_index);
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_dock_panel_state(child, states);
        }
    }
}

fn cancel_keyboard_activations(widget: &mut dyn Widget) {
    widget.cancel_keyboard_activation();
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            cancel_keyboard_activations(child);
        }
    }
}

fn collect_control_inspection_state(
    widget: &dyn Widget,
) -> Vec<(PanelKind, mondrian_ui_core::ControlInspectionState)> {
    fn visit(
        widget: &dyn Widget,
        panel: Option<PanelKind>,
        states: &mut Vec<(PanelKind, mondrian_ui_core::ControlInspectionState)>,
    ) {
        let panel = widget.panel_kind().or(panel);
        if let Some(panel) = panel
            && let Some(state) = widget.control_inspection_state()
        {
            states.push((panel, state));
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                visit(child, panel, states);
            }
        }
    }
    let mut states = Vec::new();
    visit(widget, None, &mut states);
    states
}

fn restore_control_inspection_state(
    widget: &mut dyn Widget,
    states: &[(PanelKind, mondrian_ui_core::ControlInspectionState)],
) {
    fn counts(
        states: &[(PanelKind, mondrian_ui_core::ControlInspectionState)],
    ) -> std::collections::HashMap<(PanelKind, &str), usize> {
        let mut counts = std::collections::HashMap::new();
        for (panel, state) in states {
            *counts.entry((*panel, state.key.as_str())).or_insert(0) += 1;
        }
        counts
    }
    let replacements = collect_control_inspection_state(widget);
    let previous_counts = counts(states);
    let replacement_counts = counts(&replacements);
    let unambiguous = states
        .iter()
        .filter_map(|(panel, state)| {
            let key = (*panel, state.key.as_str());
            (previous_counts.get(&key) == Some(&1) && replacement_counts.get(&key) == Some(&1))
                .then_some((key, state))
        })
        .collect::<std::collections::HashMap<_, _>>();
    fn visit(
        widget: &mut dyn Widget,
        panel: Option<PanelKind>,
        states: &std::collections::HashMap<
            (PanelKind, &str),
            &mondrian_ui_core::ControlInspectionState,
        >,
    ) {
        let panel = widget.panel_kind().or(panel);
        if let Some(panel) = panel
            && let Some(current) = widget.control_inspection_state()
            && let Some(state) = states.get(&(panel, current.key.as_str()))
        {
            widget.restore_control_inspection_state(state);
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index) {
                visit(child, panel, states);
            }
        }
    }
    visit(widget, None, &unambiguous);
}

fn collect_curve_view_state(
    widget: &dyn Widget,
) -> Vec<(PanelKind, mondrian_ui_widgets::CurveEditorViewState)> {
    fn visit(
        widget: &dyn Widget,
        panel: Option<PanelKind>,
        states: &mut Vec<(PanelKind, mondrian_ui_widgets::CurveEditorViewState)>,
    ) {
        let panel = widget.panel_kind().or(panel);
        if let Some(panel) = panel
            && let Some(curve) = widget
                .as_any()
                .and_then(|any| any.downcast_ref::<mondrian_ui_widgets::CurveEditor>())
            && let Some(state) = curve.view_state()
        {
            states.push((panel, state));
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                visit(child, panel, states);
            }
        }
    }
    let mut states = Vec::new();
    visit(widget, None, &mut states);
    states
}

fn restore_curve_view_state(
    widget: &mut dyn Widget,
    states: &[(PanelKind, mondrian_ui_widgets::CurveEditorViewState)],
) {
    fn visit(
        widget: &mut dyn Widget,
        panel: Option<PanelKind>,
        states: &[(PanelKind, mondrian_ui_widgets::CurveEditorViewState)],
        consumed: &mut std::collections::HashSet<usize>,
    ) {
        let panel = widget.panel_kind().or(panel);
        if let Some(panel) = panel
            && let Some(curve) = widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<mondrian_ui_widgets::CurveEditor>())
        {
            for (index, (owner, state)) in states.iter().enumerate() {
                if *owner == panel && !consumed.contains(&index) && curve.restore_view_state(state)
                {
                    consumed.insert(index);
                    break;
                }
            }
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index) {
                visit(child, panel, states, consumed);
            }
        }
    }
    visit(widget, None, states, &mut std::collections::HashSet::new());
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
    if let Some(grid) = widget.as_any().and_then(|any| any.downcast_ref::<AssetGrid>())
        && let Some(owner) = owner
    {
        states.push(AssetGridLocalState {
            owner,
            ordinal: states.iter().filter(|state| state.owner == owner).count(),
            state: grid.state(),
        });
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
    if let Some(owner) = owner
        && let Some(grid) = widget.as_any_mut().and_then(|any| any.downcast_mut::<AssetGrid>())
    {
        let ordinal =
            restored.iter().filter(|(restored_owner, _)| *restored_owner == owner).count();
        if let Some(state) =
            states.iter().find(|state| state.owner == owner && state.ordinal == ordinal)
        {
            grid.restore_state(&state.state);
        }
        restored.push((owner, ordinal));
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
    if let Some(list) = widget.as_any().and_then(|any| any.downcast_ref::<PanelList>())
        && let Some(owner) = owner
    {
        states.push(PanelListLocalState {
            owner,
            ordinal: states.iter().filter(|state| state.owner == owner).count(),
            state: list.state(),
        });
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
    if let Some(owner) = owner
        && let Some(list) = widget.as_any_mut().and_then(|any| any.downcast_mut::<PanelList>())
    {
        let ordinal =
            restored.iter().filter(|(restored_owner, _)| *restored_owner == owner).count();
        if let Some(state) =
            states.iter().find(|state| state.owner == owner && state.ordinal == ordinal)
        {
            list.restore_state(&state.state);
        }
        restored.push((owner, ordinal));
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
        if let Some(child) = widget.child(index)
            && let Some(state) = collect_timeline_view_state(child)
        {
            return Some(state);
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
    if let Some(owner) = owner
        && let Some(scroll) = widget.as_any_mut().and_then(|any| any.downcast_mut::<ScrollView>())
    {
        let ordinal =
            restored.iter().filter(|(restored_owner, _)| *restored_owner == owner).count();
        if let Some(state) =
            states.iter().find(|state| state.owner == owner && state.ordinal == ordinal)
        {
            scroll.restore_state(&state.state);
        }
        restored.push((owner, ordinal));
    }
    for index in 0..widget.child_count() {
        if let Some(child) = widget.child_mut(index) {
            restore_panel_scroll_state_into(child, owner, states, restored);
        }
    }
}

impl Widget for AppUiAppRoot {
    fn keyboard_focus_scope(&self) -> Option<WidgetId> {
        self.modal.as_ref().map(Widget::id)
    }

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
        if self.status_bar.event(event, ctx) == EventResult::Handled {
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

fn bind_native_visual_gui_models(
    models: &mut AppUiPanelModels,
    registry: &crate::app::preview_native_gui::PreviewNativeGuiRegistry,
) {
    let views = models
        .inspector
        .effects
        .iter()
        .filter_map(|effect| effect.native_author_view.clone())
        .collect::<Vec<_>>();
    registry.reconcile_views(&views);
    for effect in &mut models.inspector.effects {
        effect.native_gui_controller =
            effect.native_author_view.as_ref().and_then(|view| registry.controller(view));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        app_shell_about_action, app_shell_cancel_new_project_dialog_action,
        app_shell_close_modal_action, app_shell_confirm_interpret_asset_dialog_action,
        app_shell_confirm_new_project_dialog_action, app_shell_confirm_project_settings_action,
        app_shell_confirm_sequence_settings_action, app_shell_export_output_dialog_action,
        app_shell_export_portable_package_dialog_action, app_shell_import_media_dialog_action,
        app_shell_import_media_dialog_action_with_target,
        app_shell_install_clap_library_dialog_action,
        app_shell_install_openfx_bundle_dialog_action, app_shell_install_vst3_binary_dialog_action,
        app_shell_install_vst3_bundle_dialog_action, app_shell_interpret_asset_dialog_action,
        app_shell_interpret_asset_draft_changed_action, app_shell_new_project_dialog_action,
        app_shell_new_project_draft_changed_action, app_shell_open_project_dialog_action,
        app_shell_open_recent_project_action, app_shell_preferences_action,
        app_shell_preferences_tab_changed_action, app_shell_project_settings_action,
        app_shell_project_settings_draft_changed_action, app_shell_recover_project_action,
        app_shell_relink_asset_dialog_action, app_shell_relocate_panel_action,
        app_shell_reveal_in_file_manager_action, app_shell_save_project_as_dialog_action,
        app_shell_select_custom_ocio_config_action, app_shell_sequence_settings_action,
        app_shell_sequence_settings_draft_changed_action,
        app_shell_sequence_settings_tab_changed_action, viewer_cycle_zoom_action,
        viewer_set_zoom_scale_action, AppShellInterpretAssetDialogPayload,
        AppShellOpenRecentProjectPayload, AppShellRelinkAssetDialogPayload,
        AppShellRelocatePanelPayload, AppShellRevealInFileManagerPayload, AssetsImportFilesPayload,
        AssetsRelinkAssetPayload, AssetsSetInterpretationPayload, DockDropAreaPayload,
        ExportDraftEdit, ExportOutputDialogPayload, ImportMediaDialogPayload,
        InterpretAssetDraftUpdatePayload, NewProjectDraftUpdatePayload, PreferencesTabPayload,
        ProjectCreateWithSettingsPayload, ProjectRecoverFromAutosavePayload,
        ProjectSettingsDraftUpdatePayload, ProjectUpdateColorEnvironmentPayload,
        SequenceSettingsDraftUpdatePayload, SequenceSettingsTabPayload,
        SequenceUpdateSettingsPayload, ViewerSetZoomScalePayload, ASSET_IMPORT_FILES,
        ASSET_NAMESPACE, ASSET_RELINK, ASSET_SET_INTERPRETATION, EXPORT_EDIT_DRAFT,
        EXPORT_NAMESPACE, PROJECT_CREATE_WITH_SETTINGS, PROJECT_NAMESPACE,
        PROJECT_RECOVER_FROM_AUTOSAVE, PROJECT_UPDATE_COLOR_ENVIRONMENT, SEQUENCE_NAMESPACE,
        SEQUENCE_UPDATE_SETTINGS,
    };
    use crate::app::CrashRecoveryCandidate;
    use crate::app_ui::panels::ViewerPreviewState;
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use crate::app_ui::window_controls::WindowControl;
    use glam::Vec2;
    use mondrian_core::timeline_data::{AssetMediaInterpretation, MediaColorInterpretation};
    use mondrian_core::types::AssetId;
    use mondrian_core::{
        ColorSpace, ProjectId, Rational, Resolution, SmpteCountingMode, TimelineDisplayFormat,
        VideoContentLightMetadata, VideoMasteringDisplayMetadata, WorkingColorSpace,
    };
    use mondrian_platform::{ClipboardError, FileDialogError, FileDialogOutcome, FileRevealError};
    use mondrian_timeline::sequence::{
        AudioChannelLayout, AudioDisplayFormat, ColorWorkflow, DeliveryBitDepth, EditingMode,
        FieldOrder, MissingColorMetadataPolicy, PixelAspectRatio, PreviewRenderFormat, Sequence,
        StaticHdrMetadataPolicy, VideoRange,
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
        open_folder: Option<PathBuf>,
        save_path: Option<PathBuf>,
        revealed_paths: Mutex<Vec<PathBuf>>,
        dialog_titles: Mutex<Vec<String>>,
        dialog_default_names: Mutex<Vec<String>>,
        dialog_filters: Mutex<Vec<Vec<FileFilter>>>,
        dialog_error: bool,
    }

    impl PlatformService for FakePlatform {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn open_file_dialog(
            &self,
            title: &str,
            filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            self.dialog_titles.lock().expect("dialog titles lock").push(title.to_owned());
            self.dialog_filters.lock().expect("filter lock").push(filters.to_vec());
            if self.dialog_error {
                return Err(FileDialogError::Unavailable);
            }
            Ok(match self.open_paths.clone() {
                Some(paths) => FileDialogOutcome::Selected(paths),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn open_folder_dialog(
            &self,
            title: &str,
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            self.dialog_titles.lock().expect("dialog titles lock").push(title.to_owned());
            if self.dialog_error {
                return Err(FileDialogError::Unavailable);
            }
            Ok(match self.open_folder.clone() {
                Some(path) => FileDialogOutcome::Selected(path),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn save_file_dialog(
            &self,
            title: &str,
            default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            self.dialog_titles.lock().expect("dialog titles lock").push(title.to_owned());
            self.dialog_default_names
                .lock()
                .expect("dialog default names lock")
                .push(default_name.to_owned());
            Ok(match self.save_path.clone() {
                Some(path) => FileDialogOutcome::Selected(path),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn reveal_in_file_manager(&self, path: &Path) -> Result<(), FileRevealError> {
            self.revealed_paths.lock().expect("revealed path lock").push(path.to_path_buf());
            Ok(())
        }
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
        if let Some(panel) = widget.as_any().and_then(|any| any.downcast_ref::<DockPanel>())
            && panel.kind() == kind
        {
            return Some(panel.active_index());
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index)
                && let Some(active) = active_index_for_dock_panel(child, kind)
            {
                return Some(active);
            }
        }
        None
    }

    fn panel_list_state_for_title(widget: &dyn Widget, title: &str) -> Option<PanelListState> {
        if let Some(list) = widget.as_any().and_then(|any| any.downcast_ref::<PanelList>())
            && list.title() == title
        {
            return Some(list.state());
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index)
                && let Some(state) = panel_list_state_for_title(child, title)
            {
                return Some(state);
            }
        }
        None
    }

    fn menu_checked_for_action(root: &AppUiAppRoot, action: &Action) -> Option<bool> {
        root.title_bar.menu_bar().checked_for_action(action)
    }

    fn asset_grid_state_for_title(widget: &dyn Widget, title: &str) -> Option<AssetGridState> {
        if let Some(grid) = widget.as_any().and_then(|any| any.downcast_ref::<AssetGrid>())
            && grid.title() == title
        {
            return Some(grid.state());
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index)
                && let Some(state) = asset_grid_state_for_title(child, title)
            {
                return Some(state);
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
            if let Some(child) = widget.child(index)
                && let Some(state) = timeline_view_state(child)
            {
                return Some(state);
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
            if let Some(child) = widget.child_mut(index)
                && with_timeline_view_mut(child, update)
            {
                return true;
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
            && let Some(grid) = widget.as_any_mut().and_then(|any| any.downcast_mut::<AssetGrid>())
        {
            update(grid);
            return true;
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index)
                && with_asset_grid_mut_for_title(child, title, update)
            {
                return true;
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
            && let Some(list) = widget.as_any_mut().and_then(|any| any.downcast_mut::<PanelList>())
        {
            update(list);
            return true;
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index)
                && with_panel_list_mut_for_title(child, title, update)
            {
                return true;
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
            && let Some(scroll) =
                widget.as_any_mut().and_then(|any| any.downcast_mut::<ScrollView>())
        {
            update(scroll);
            return true;
        }
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child_mut(index)
                && with_scroll_view_mut_for_panel_inner(child, owner, panel, update)
            {
                return true;
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
    fn app_root_reports_live_scopes_visibility_without_layout_snapshot() {
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(!root.is_panel_active(PanelKind::Scopes));

        root.handle_shell_action(
            Action::SwitchWorkspace(WorkspacePreset::Color),
            &platform,
            None,
        );
        assert!(root.is_panel_active(PanelKind::Scopes));

        root.handle_shell_action(Action::TogglePanel(PanelKind::Scopes), &platform, None);
        assert!(!root.is_panel_active(PanelKind::Scopes));
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
        assert!(root.is_panel_active(PanelKind::Effects));
        assert_eq!(active_index_for_dock_panel(&root, PanelKind::Effects), None);
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
        let localizer = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let filters = media_import_filters(&localizer);

        assert!(filters.iter().any(
            |filter| filter.name == "视频" && filter.extensions.iter().any(|ext| ext == "mp4")
        ));
        assert!(filters.iter().any(
            |filter| filter.name == "音频" && filter.extensions.iter().any(|ext| ext == "wav")
        ));
    }

    #[test]
    fn native_file_dialogs_use_selected_locale_without_changing_cancel_semantics() {
        let platform = FakePlatform::default();
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        let opened = try_resolve_app_shell_action(
            app_shell_open_project_dialog_action(),
            &platform,
            None,
            &english,
        )
        .expect("English open-project dialog");
        let packaged = try_resolve_app_shell_action(
            crate::app::ui_actions::app_shell_export_portable_package_dialog_action(),
            &platform,
            None,
            &english,
        )
        .expect("English package dialog");
        assert_eq!(opened, None);
        assert_eq!(packaged, None);
        assert_eq!(
            platform.dialog_titles.lock().expect("dialog titles lock").as_slice(),
            &["Open Mondrian project", "Package Mondrian project"]
        );
        assert_eq!(
            platform
                .dialog_default_names
                .lock()
                .expect("dialog default names lock")
                .as_slice(),
            &["Untitled Project.mdpkg"]
        );
        assert_eq!(project_file_filters(&english)[0].name, "Mondrian project");
    }

    #[test]
    fn project_file_filters_cover_mondrian_project_extension() {
        let localizer = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let filters = project_file_filters(&localizer);

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "Mondrian 项目");
        assert!(filters[0]
            .extensions
            .iter()
            .any(|extension| extension == PROJECT_FILE_EXTENSION));
    }

    #[test]
    fn portable_package_dialog_uses_distinct_directory_extension() {
        let localizer = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let filters = portable_package_filters(&localizer);
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].extensions, vec!["mdpkg"]);
    }

    #[test]
    fn export_output_filters_normalize_requested_extension() {
        let localizer = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let filters = export_output_filters(".MP4 ", &localizer);

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "导出");
        assert_eq!(filters[0].extensions, vec!["mp4"]);
    }

    #[test]
    fn export_output_filters_fall_back_for_empty_extension() {
        let localizer = Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog");
        let filters = export_output_filters(" . ", &localizer);

        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].name, "媒体");
        assert!(filters[0].extensions.iter().any(|extension| extension == "mp4"));
        assert!(filters[0].extensions.iter().any(|extension| extension == "gif"));
    }

    #[test]
    fn packaging_phase_has_a_product_status_message() {
        assert_eq!(
            export_progress_phase_message_id(ExportProgressPhase::Packaging),
            "export-job-packaging"
        );
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
            candidate: CrashRecoveryCandidate {
                project_id: ProjectId::new(),
                runtime_root: PathBuf::from("E:/runtime"),
                project_file: PathBuf::from("E:/projects/recover.mdp"),
                canonical_target: crate::app::RecoveryCanonicalTargetEvidence::Missing,
                autosave_file: PathBuf::from("E:/runtime/autosave/project.autosave.mdp"),
                author_generation: 4,
                asset_library_revision: 2,
                document_revision: 8,
                archive_sha256: "b".repeat(64),
                saved_at_unix_ms: 19,
                total_snapshots: 1,
            },
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
        assert_eq!(
            payload.sequence_settings.color.program_output.workflow,
            ColorWorkflow::SceneReferred
        );
        let context = payload
            .sequence_settings
            .root_program_color_context(&payload.color_environment)
            .expect("valid context");
        assert_eq!(
            context.engine(),
            &mondrian_core::ColorEngine::mondrian_standard()
        );
        assert_eq!(
            context.output_transform(),
            &mondrian_core::OutputTransformIntent::mondrian_standard()
        );
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
    fn app_root_applies_new_project_color_engine_to_payload() {
        let platform = FakePlatform {
            save_path: Some(PathBuf::from("E:/projects/ACES.mdp")),
            ..FakePlatform::default()
        };
        let mut root = AppUiAppRoot::demo();
        let engine = mondrian_core::ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        };

        root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None);
        root.handle_shell_action(
            app_shell_new_project_draft_changed_action(NewProjectDraftUpdatePayload::ColorEngine(
                engine.clone(),
            )),
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

        assert_eq!(payload.color_environment.engine(), &engine);
    }

    #[test]
    fn app_root_pins_selected_custom_ocio_before_updating_new_project() {
        let config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../mondrian-core/assets/ocio/mondrian_default_ocio_v2.ocio");
        let platform = FakePlatform {
            open_paths: Some(vec![config_path.clone()]),
            ..FakePlatform::default()
        };
        let mut root = AppUiAppRoot::demo();

        root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None);
        root.handle_shell_action(
            app_shell_select_custom_ocio_config_action(),
            &platform,
            None,
        );

        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_new_project)
            .expect("new-project dialog");
        let identity = dialog
            .draft()
            .color_environment
            .engine()
            .custom_ocio_identity()
            .expect("complete Custom OCIO identity");
        assert_eq!(
            identity.source(),
            &mondrian_core::OcioConfigSource::Path {
                path: config_path.canonicalize().expect("fixture config path"),
            }
        );
        let output = identity
            .output(mondrian_core::ColorSpace::Rec709)
            .expect("Rec.709 output binding");
        assert_eq!(output.display(), "Rec.1886 Rec.709 - Display");
        assert_eq!(output.view(), "ACES 2.0 - SDR 100 nits (Rec.709)");
        assert!(!output.display_color_space().is_empty());
        assert!(!identity.config_sha256().is_empty());
        assert!(!identity.dependency_manifest_sha256().is_empty());
        assert!(dialog.error_text().is_empty());
    }

    #[test]
    fn app_root_rejects_invalid_custom_ocio_without_partial_mode() {
        let platform = FakePlatform {
            open_paths: Some(vec![PathBuf::from("E:/missing/config.ocio")]),
            ..FakePlatform::default()
        };
        let mut root = AppUiAppRoot::demo();

        root.handle_shell_action(app_shell_new_project_dialog_action(), &platform, None);
        root.handle_shell_action(
            app_shell_select_custom_ocio_config_action(),
            &platform,
            None,
        );

        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_new_project)
            .expect("new-project dialog remains open");
        assert_eq!(
            dialog.draft().color_environment.engine(),
            &mondrian_core::ColorEngine::mondrian_standard()
        );
        assert!(!dialog.error_text().is_empty());
    }

    #[test]
    fn app_root_project_settings_commits_complete_color_engine_action() {
        let platform = FakePlatform::default();
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Program")));
        let mut root = AppUiAppRoot::from_app_state(&state);
        let engine = mondrian_core::ColorEngine::Aces {
            preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
        };

        root.handle_shell_action(app_shell_project_settings_action(), &platform, None);
        assert!(root.modal.as_ref().and_then(ShellModal::as_project_settings).is_some());
        root.handle_shell_action(
            app_shell_project_settings_draft_changed_action(
                ProjectSettingsDraftUpdatePayload::ColorEngine(engine.clone()),
            ),
            &platform,
            None,
        );
        let action = root
            .handle_shell_action(app_shell_confirm_project_settings_action(), &platform, None)
            .expect("project color environment action");

        let Action::Custom { namespace, name, payload } = action else {
            panic!("expected project custom action");
        };
        assert_eq!(namespace, PROJECT_NAMESPACE);
        assert_eq!(name, PROJECT_UPDATE_COLOR_ENVIRONMENT);
        let payload: ProjectUpdateColorEnvironmentPayload =
            serde_json::from_value(payload).expect("project color environment payload");
        assert_eq!(payload.color_environment.engine(), &engine);
        assert!(root.modal.is_none());
    }

    #[test]
    fn app_root_project_settings_pins_selected_custom_ocio() {
        let config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../mondrian-core/assets/ocio/mondrian_default_ocio_v2.ocio");
        let platform = FakePlatform {
            open_paths: Some(vec![config_path]),
            ..FakePlatform::default()
        };
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Program")));
        let mut root = AppUiAppRoot::from_app_state(&state);

        root.handle_shell_action(app_shell_project_settings_action(), &platform, None);
        root.handle_shell_action(
            app_shell_select_custom_ocio_config_action(),
            &platform,
            None,
        );

        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_project_settings)
            .expect("project-settings dialog");
        let identity = dialog
            .draft()
            .engine
            .custom_ocio_identity()
            .expect("pinned Custom OCIO identity");
        assert_eq!(identity.working_space(), "Linear Rec.2020");
        assert!(!identity.config_sha256().is_empty());
        assert!(!identity.dependency_manifest_sha256().is_empty());
        assert!(dialog.error_text().is_empty());
    }

    #[test]
    fn project_settings_custom_ocio_pins_template_and_all_sequence_outputs() {
        let config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../mondrian-core/assets/ocio/mondrian_default_ocio_v2.ocio");
        let platform = FakePlatform {
            open_paths: Some(vec![config_path]),
            ..FakePlatform::default()
        };
        let mut state = AppState::new();
        let rec709 = Sequence::new("SDR Program");
        let mut pq = Sequence::new("HDR Program");
        pq.settings.color.program_output.color_space = ColorSpace::Rec2100Pq;
        state.test_set_sequences(vec![rec709, pq]);
        let mut root = AppUiAppRoot::from_app_state(&state);

        root.handle_shell_action(app_shell_project_settings_action(), &platform, None);
        root.handle_shell_action(
            app_shell_select_custom_ocio_config_action(),
            &platform,
            None,
        );

        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_project_settings)
            .expect("project-settings dialog");
        let identity = dialog
            .draft()
            .engine
            .custom_ocio_identity()
            .expect("complete Custom OCIO identity");
        assert!(identity.output(ColorSpace::Rec709).is_some());
        assert!(identity.output(ColorSpace::Rec2100Pq).is_some());
        assert_eq!(identity.outputs().len(), 2);
        assert!(dialog.error_text().is_empty());
    }

    #[test]
    fn project_settings_custom_ocio_rejects_multiple_project_working_spaces() {
        let config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../mondrian-core/assets/ocio/mondrian_default_ocio_v2.ocio");
        let platform = FakePlatform {
            open_paths: Some(vec![config_path]),
            ..FakePlatform::default()
        };
        let mut state = AppState::new();
        *state.test_project_color_environment_mut() =
            mondrian_core::ProjectColorEnvironment::new(mondrian_core::ColorEngine::Aces {
                preset: mondrian_core::AcesConfigPreset::StudioV4Aces2Ocio25,
            });
        let mut aces_cg = Sequence::new("ACEScg Program");
        aces_cg.settings.color.working_color_space = WorkingColorSpace::AcesCg;
        state.test_set_sequences(vec![aces_cg]);
        let mut root = AppUiAppRoot::from_app_state(&state);

        root.handle_shell_action(app_shell_project_settings_action(), &platform, None);
        root.handle_shell_action(
            app_shell_select_custom_ocio_config_action(),
            &platform,
            None,
        );

        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_project_settings)
            .expect("project-settings dialog");
        assert!(matches!(
            dialog.draft().engine,
            mondrian_core::ColorEngine::Aces { .. }
        ));
        assert!(dialog.error_text().contains("exactly one working space"));
    }

    #[test]
    fn app_root_handles_sequence_settings_draft_and_confirm() {
        let platform = FakePlatform::default();
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Scene 01");
        sequence.settings.delivery.hdr_mastering_display =
            Some(VideoMasteringDisplayMetadata::rec2100_1000_nit_reference());
        sequence.settings.delivery.hdr_content_light =
            Some(VideoContentLightMetadata::rec2100_1000_nit_reference());
        let sequence_id = sequence.id;
        state.test_set_active_sequence(sequence_id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
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
                SequenceSettingsDraftUpdatePayload::FieldOrder(FieldOrder::Progressive),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::TimelineDisplayFormat(
                    TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame),
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
                SequenceSettingsDraftUpdatePayload::WorkingColorSpace(
                    WorkingColorSpace::LinearRec2020,
                ),
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
                SequenceSettingsDraftUpdatePayload::ColorWorkflow(ColorWorkflow::SceneReferred),
            ),
            &platform,
            None,
        );
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::MissingColorMetadataPolicy(
                    MissingColorMetadataPolicy::AssumeRec709,
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
                SequenceSettingsDraftUpdatePayload::DeliveryBitDepth(DeliveryBitDepth::Ten),
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
                SequenceSettingsDraftUpdatePayload::WriteStaticHdrMetadata(true),
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
                    AudioChannelLayout::Surround51Side,
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
        assert_eq!(payload.settings.field_order, FieldOrder::Progressive);
        assert_eq!(
            payload.settings.timeline_display.format,
            TimelineDisplayFormat::Timecode(SmpteCountingMode::DropFrame)
        );
        assert_eq!(payload.settings.timeline_display.timecode_start_frame, 120);
        assert_eq!(
            payload.settings.color.working_color_space,
            WorkingColorSpace::LinearRec2020
        );
        assert!(!payload.settings.color.input.auto_tone_map_media);
        assert_eq!(
            payload.settings.color.program_output.workflow,
            ColorWorkflow::SceneReferred
        );
        assert_eq!(
            payload.settings.color.input.missing_metadata_policy,
            MissingColorMetadataPolicy::AssumeRec709
        );
        assert_eq!(
            payload.settings.color.program_output.color_space,
            ColorSpace::Rec2100Pq
        );
        assert_eq!(payload.settings.delivery.video_range, VideoRange::Legal);
        assert_eq!(payload.settings.delivery.bit_depth, DeliveryBitDepth::Ten);
        assert_eq!(
            payload.settings.delivery.static_hdr_metadata_policy,
            StaticHdrMetadataPolicy::WriteAuthored
        );
        assert_eq!(payload.settings.audio_sample_rate, 96_000);
        assert_eq!(
            payload.settings.audio_channel_layout,
            AudioChannelLayout::Surround51Side
        );
        assert_eq!(payload.settings.audio_channel_layout.channel_count(), 6);
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
    fn invalid_sequence_name_keeps_modal_and_explains_rejection() {
        let platform = FakePlatform::default();
        let mut state = AppState::new();
        let sequence = Sequence::new("Scene 01");
        state.test_set_active_sequence(sequence.id);
        state.test_set_sequence(Some(sequence.clone()));
        state.test_add_sequence(sequence);
        let preferences = AppUiPreferences {
            locale_preference: crate::app_ui::localization::AppUiLocalePreference::ZhCn,
            ..AppUiPreferences::default()
        };
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);

        root.handle_shell_action(app_shell_sequence_settings_action(), &platform, None);
        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::Name(String::new()),
            ),
            &platform,
            None,
        );
        assert_eq!(
            root.handle_shell_action(
                app_shell_confirm_sequence_settings_action(),
                &platform,
                None,
            ),
            None
        );
        let dialog = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_sequence_settings)
            .expect("modal remains open");
        assert_eq!(dialog.error_text(), "序列名称不能为空");

        root.handle_shell_action(
            app_shell_sequence_settings_draft_changed_action(
                SequenceSettingsDraftUpdatePayload::Name("Scene 02".to_owned()),
            ),
            &platform,
            None,
        );
        assert_eq!(
            root.modal
                .as_ref()
                .and_then(ShellModal::as_sequence_settings)
                .expect("modal")
                .error_text(),
            ""
        );
        assert!(root
            .handle_shell_action(
                app_shell_confirm_sequence_settings_action(),
                &platform,
                None
            )
            .is_some());
        assert!(root.modal.is_none());
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
        state.test_set_project_path(PathBuf::from("E:/projects/live.mdp"));
        state.test_set_sequence(Some(mondrian_timeline::sequence::Sequence::new("Live")));
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
    fn app_root_refresh_preserves_machine_locale_choice() {
        let state = AppState::new();
        let preferences = AppUiPreferences {
            locale_preference: crate::app_ui::localization::AppUiLocalePreference::EnUs,
            ..AppUiPreferences::default()
        };
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
        let platform = FakePlatform::default();
        assert_eq!(
            root.try_handle_shell_action(app_shell_open_project_dialog_action(), &platform, None)
                .expect("English project dialog"),
            None
        );
        assert_eq!(
            platform.dialog_titles.lock().expect("dialog titles lock").as_slice(),
            &["Open Mondrian project"]
        );
        assert_eq!(
            root.preferences_model.locale,
            crate::app_ui::localization::AppUiLocale::EnUs
        );
        assert_eq!(
            root.models.effects.filter_placeholder.as_deref(),
            Some("Search effects")
        );
        assert_eq!(root.models.assets.subtitle, "Project library");
        assert_eq!(root.models.viewer.title, "Viewer");
        assert_eq!(root.models.viewer.status, "No sequence");
        assert_eq!(root.models.viewer.fit_label, "Fit");
        root.refresh_playback_frame_from_app_state(&state, None);
        assert_eq!(root.models.viewer.status, "No sequence");

        root.refresh_from_app_state(&state);

        assert_eq!(
            root.preferences_model.locale_preference,
            crate::app_ui::localization::AppUiLocalePreference::EnUs
        );
        assert_eq!(
            root.preferences_model.locale,
            crate::app_ui::localization::AppUiLocale::EnUs
        );
        assert!(root.models.effects.items.iter().any(|item| item.title == "Color"));
        assert_eq!(
            root.models.assets.filter_placeholder.as_deref(),
            Some("Search assets")
        );

        let chinese = AppUiPreferences {
            locale_preference: crate::app_ui::localization::AppUiLocalePreference::ZhCn,
            ..AppUiPreferences::default()
        };
        root.refresh_from_app_state_with_preferences(&state, &chinese);
        assert_eq!(
            root.models.effects.filter_placeholder.as_deref(),
            Some("搜索效果")
        );
        assert!(root.models.effects.items.iter().any(|item| item.title == "颜色"));
        assert_eq!(root.models.assets.subtitle, "项目素材库");
        assert_eq!(root.models.viewer.title, "预览");
        assert_eq!(root.models.viewer.status, "没有序列");
        assert_eq!(root.models.viewer.fit_label, "适合");
    }

    #[test]
    fn model_only_shell_does_not_construct_an_app_owner() {
        let state = AppState::new();
        let models = AppUiPanelModels::from_app_state(&state);
        let before = crate::app::test_app_state_construction_count();
        let root = AppUiAppRoot::from_models(models);
        assert_eq!(crate::app::test_app_state_construction_count(), before);
        assert!(!root.status_bar.model.is_busy);
        assert!(!root.status_bar.model.is_error);
    }

    #[test]
    fn export_status_uses_selected_locale_and_preserves_exact_frame_counts() {
        let english = Localizer::new(AppUiLocale::EnUs).expect("English");
        let chinese = Localizer::new(AppUiLocale::ZhCn).expect("Chinese");
        for (phase, text) in [
            (ExportProgressPhase::Preparing, "Preparing"),
            (ExportProgressPhase::Rendering, "Rendering"),
            (ExportProgressPhase::Encoding, "Encoding"),
            (ExportProgressPhase::Packaging, "Packaging"),
            (ExportProgressPhase::Validating, "Validating"),
            (ExportProgressPhase::Publishing, "Publishing"),
        ] {
            let status = JobStatus::Running { phase };
            let formatted =
                active_export_status_text(&status, &ExportProgressDetail::None, 2, &english);
            assert!(formatted.contains(text), "{formatted}");
            assert!(formatted.contains('2'), "{formatted}");
            assert_ne!(
                formatted,
                active_export_status_text(&status, &ExportProgressDetail::None, 2, &chinese)
            );
        }
        let completed = u64::MAX - 1;
        let total = u64::MAX;
        let detail = ExportProgressDetail::Frames { completed, total };
        let rendered = active_export_status_text(
            &JobStatus::Running { phase: ExportProgressPhase::Rendering },
            &detail,
            2,
            &english,
        );
        assert!(rendered.contains(&completed.to_string()), "{rendered}");
        assert!(rendered.contains(&total.to_string()), "{rendered}");
        let encoding = active_export_status_text(
            &JobStatus::Running { phase: ExportProgressPhase::Encoding },
            &detail,
            2,
            &english,
        );
        assert!(encoding.contains("Encoding"));
        assert!(
            !encoding.contains(&completed.to_string()),
            "stale frame units must not describe encoding"
        );
        assert!(active_export_status_text(
            &JobStatus::Pending,
            &ExportProgressDetail::None,
            2,
            &english
        )
        .contains("Export queued"));
        assert!(active_export_status_text(
            &JobStatus::Cancelling { phase: ExportProgressPhase::Rendering },
            &detail,
            2,
            &english
        )
        .contains("Cancelling export"));
    }

    #[test]
    fn status_locale_refresh_preserves_history_viewport_and_widget_identity() {
        use crate::app_ui::localization::AppUiLocalePreference;
        let mut state = AppState::new();
        for index in 0..12 {
            state.set_status_hint(
                AppMessage::new("status-clap-installed").with_count("count", index),
                false,
            );
        }
        state.clear_status_hint();
        let mut preferences = AppUiPreferences {
            locale_preference: AppUiLocalePreference::ZhCn,
            ..Default::default()
        };
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
        root.status_bar.history_open = true;
        root.status_bar.history_offset = 3;
        let widget_id = root.status_bar.id;
        let history = root.status_bar.history.clone();
        for (preference, locale) in [
            (AppUiLocalePreference::EnUs, AppUiLocale::EnUs),
            (AppUiLocalePreference::Pseudo, AppUiLocale::Pseudo),
            (AppUiLocalePreference::ZhCn, AppUiLocale::ZhCn),
        ] {
            preferences.locale_preference = preference;
            root.refresh_from_app_state_with_preferences(&state, &preferences);
            assert_eq!(root.status_bar.id, widget_id);
            assert_eq!(root.status_bar.history, history);
            let entry = &root.status_bar.history[5];
            let body = root.status_bar.history_entry_text(entry);
            assert_eq!(body, root.localizer.message(&entry.message));
            let initial =
                Localizer::new(AppUiLocale::ZhCn).expect("Chinese").message(&entry.message);
            if locale != AppUiLocale::ZhCn {
                assert_ne!(body, initial);
            }
            assert!(root.status_bar.history_open);
            assert_eq!(root.status_bar.history_offset, 3);
            assert_eq!(root.status_bar.localizer.locale(), locale);
            assert_eq!(
                root.status_bar.model.message,
                root.localizer.text("status-ready")
            );
            let name = root
                .status_bar
                .accessibility()
                .expect("status button")
                .name
                .expect("accessible name");
            assert!(name.contains(&root.status_bar.model.message));
            if locale == AppUiLocale::EnUs {
                assert!(name.contains("Recent notifications"));
                assert_eq!(root.status_bar.model.message, "Ready");
            }
            root.refresh_playback_frame_from_app_state(&state, None);
            assert_eq!(
                root.status_bar.model.message,
                root.localizer.text("status-ready")
            );
        }
    }

    #[test]
    fn locale_switch_reformats_current_error_and_retained_success_without_author_edits() {
        use crate::app_ui::localization::AppUiLocalePreference;
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Cut 01")));
        let path = "E:/项目/Mixed Case/{clip}.mdp";
        state.set_status_hint(
            AppMessage::new("status-packaged").with_text("path", path),
            false,
        );
        state.set_status_hint(
            AppMessage::new("status-save-failed").with_text("error", "disk full"),
            true,
        );
        let generation = state.project_author_generation();
        let hint = state.status_hint().cloned();
        let history = state.status_log().to_vec();
        let mut preferences = AppUiPreferences::default();
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
        root.status_bar.history_open = true;
        let widget_id = root.status_bar.id;
        for (preference, locale) in [
            (AppUiLocalePreference::EnUs, AppUiLocale::EnUs),
            (AppUiLocalePreference::Pseudo, AppUiLocale::Pseudo),
            (AppUiLocalePreference::ZhCn, AppUiLocale::ZhCn),
        ] {
            preferences.locale_preference = preference;
            root.refresh_from_app_state_with_preferences(&state, &preferences);
            let localizer = Localizer::new(locale).expect("catalog");
            let current = &hint.as_ref().expect("error fact").0;
            assert_eq!(root.status_bar.model.message, localizer.message(current));
            assert!(root.status_bar.model.is_error);
            assert!(!root.status_bar.model.is_busy);
            let first = &root.status_bar.history[0];
            assert_eq!(
                root.status_bar.history_entry_text(first),
                localizer.message(&first.message)
            );
            assert!(root.status_bar.history_entry_text(first).contains(path));
            if locale == AppUiLocale::EnUs {
                assert!(root.status_bar.model.message.contains("Could not save project"));
                assert!(root.status_bar.history_entry_text(first).contains("Project packaged"));
            }
            assert_eq!(state.project_author_generation(), generation);
            assert_eq!(state.status_hint(), hint.as_ref());
            assert_eq!(state.status_log(), history);
            assert_eq!(root.status_bar.history, history);
            assert_eq!(root.status_bar.id, widget_id);
            assert!(root.status_bar.history_open);
            root.refresh_playback_frame_from_app_state(&state, None);
            assert_eq!(root.status_bar.model.message, localizer.message(current));
        }
    }

    #[test]
    fn status_bar_model_prefers_active_sequence_context() {
        let mut state = AppState::new();
        state.test_set_project_path(PathBuf::from("E:/projects/rough-cut.mdp"));
        state.test_set_sequence(Some(mondrian_timeline::sequence::Sequence::new("Cut 01")));
        state.set_status_hint(
            AppMessage::new("status-opaque-detail").with_text("detail", "Project saved"),
            false,
        );

        let model = status_bar_model(
            &state,
            &Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog"),
        );

        assert_eq!(
            model.message,
            crate::app_ui::localization::chinese_product_message(
                &state.status_hint().expect("status").0
            )
        );
        assert!(!model.is_error);
        assert!(!model.is_busy);
        assert_eq!(model.context, "Cut 01");
    }

    #[test]
    fn app_root_playback_frame_refresh_updates_timeline_without_full_model_rebuild() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("edit")));
        state.set_playback_frame_running(12);
        let mut root = AppUiAppRoot::from_app_state(&state);

        state.set_playback_frame_running(48);
        let preview_waiting = root.refresh_playback_frame_from_app_state(&state, None);

        assert_eq!(root.models.timeline.playhead_frame, 48);
        assert_eq!(root.models.viewer.position_label, "F48");
        assert!(root.models.viewer.playing);
        assert!(!preview_waiting);
    }

    #[test]
    fn app_state_refresh_preserves_titlebar_window_control_hover() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("edit")));
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let close = root.title_bar.control_bounds(WindowControl::Close).center();
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
            root.title_bar.event(
                &UiEvent::MouseMove { position: close, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(root.title_bar.hovered_control(), Some(WindowControl::Close));

        for _ in 0..100 {
            root.refresh_from_app_state(&state);
            assert_eq!(
                root.title_bar.hovered_control(),
                Some(WindowControl::Close),
                "model refresh must not replace titlebar interaction state"
            );
        }
    }

    #[test]
    fn app_state_refresh_preserves_titlebar_window_control_press_and_capture_identity() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("edit")));
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let titlebar_id = root.title_bar.id();
        let close = root.title_bar.control_bounds(WindowControl::Close).center();
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
            root.title_bar.event(
                &UiEvent::MouseDown {
                    position: close,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(root.title_bar.pressed_control(), Some(WindowControl::Close));
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Capture(
                titlebar_id
            ))
        );

        root.refresh_from_app_state(&state);

        assert_eq!(root.title_bar.id(), titlebar_id);
        assert_eq!(root.title_bar.hovered_control(), Some(WindowControl::Close));
        assert_eq!(root.title_bar.pressed_control(), Some(WindowControl::Close));
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Capture(
                titlebar_id
            ))
        );
    }

    #[test]
    fn app_root_playback_frame_refresh_reports_preview_waiting() {
        struct LoadingPreview;

        impl ViewerPreviewSource for LoadingPreview {
            fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
                ViewerPreviewState::Loading
            }
        }

        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("edit")));
        state.set_playback_frame_running(12);
        let mut root = AppUiAppRoot::from_app_state(&state);

        let preview_waiting =
            root.refresh_playback_frame_from_app_state(&state, Some(&LoadingPreview));

        assert!(preview_waiting);
        assert!(root.models.viewer.preview_waiting);
    }

    #[test]
    fn narrow_preview_refresh_installs_and_retires_scope_textures_in_the_live_dock() {
        struct ReadyPreview;
        impl ViewerPreviewSource for ReadyPreview {
            fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
                ViewerPreviewState::Ready(mondrian_ui_widgets::ViewerFrameContent::ExternalTexture(
                    mondrian_ui_widgets::ViewerExternalTextureFrame::new("current", 1920, 1080)
                        .expect("valid GPU frame"),
                ))
            }
        }
        struct LoadingPreview;
        impl ViewerPreviewSource for LoadingPreview {
            fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
                ViewerPreviewState::Loading
            }
        }
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("scopes")));
        let preferences = AppUiPreferences {
            workspace_preset: WorkspacePreset::Color,
            ..AppUiPreferences::default()
        };
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        root.layout(bounds);
        assert!(root.is_panel_active(PanelKind::Scopes));
        let theme = ThemePreset::Dark.build();
        for (preview, waiting) in [
            (&ReadyPreview as &dyn ViewerPreviewSource, false),
            (&LoadingPreview as &dyn ViewerPreviewSource, true),
            (&ReadyPreview as &dyn ViewerPreviewSource, false),
        ] {
            root.refresh_playback_frame_from_app_state(&state, Some(preview));
            let mut encoder = PaintOrderRecorder::default();
            root.paint(&mut PaintContext {
                pixels_per_point: 1.0,
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: bounds,
            });
            assert_eq!(
                encoder.texts.iter().any(|text| text == "等待 Scopes tap"),
                waiting,
                "live scope widget must follow the admitted preview snapshot",
            );
        }
    }

    #[test]
    fn transport_only_refresh_preserves_the_installed_viewer_output() {
        struct ReadyPreview;

        impl ViewerPreviewSource for ReadyPreview {
            fn viewer_preview_for_state(&self, _state: &AppState) -> ViewerPreviewState {
                ViewerPreviewState::Ready(mondrian_ui_widgets::ViewerFrameContent::Raster(
                    mondrian_ui_widgets::ViewerFrameImage::new(
                        "transport-stable-frame",
                        1,
                        1,
                        mondrian_ui_core::RasterImageColorSpace::Srgb,
                        vec![20, 40, 60, 255],
                    )
                    .expect("valid frame"),
                ))
            }
        }

        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("edit")));
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.refresh_playback_frame_from_app_state(&state, Some(&ReadyPreview));

        for transition in 0..100 {
            if transition % 2 == 0 {
                state.play().expect("play");
            } else {
                state.pause().expect("pause");
            }
            root.refresh_transport_intent_from_app_state(&state);

            assert_eq!(root.models.viewer.playing, state.is_playing());
            assert!(root.models.viewer.preview_waiting);
            let Some(mondrian_ui_widgets::ViewerFrameContent::Raster(installed)) =
                root.models.viewer.frame_content.as_ref()
            else {
                panic!("transport refresh must retain the installed raster");
            };
            assert_eq!(installed.key, "transport-stable-frame");
            assert!(root.models.viewer.empty_message.is_none());
        }
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
    fn status_history_opens_scrolls_and_closes_without_blocking_editing() {
        let mut state = AppState::new();
        for index in 0..12 {
            state.set_status_hint(
                AppMessage::new("status-opaque-detail")
                    .with_text("detail", format!("Activity {index}")),
                index == 11,
            );
        }
        let mut root = AppUiAppRoot::from_app_state(&state);
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

        assert_eq!(root.status_bar.history.len(), 12);
        let bar_center = root.status_bar.bounds.center();
        assert_eq!(
            root.event(
                &UiEvent::MouseDown {
                    position: bar_center,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(root.status_bar.history_open);
        let history_center = root.status_bar.history_rect().center();
        assert_eq!(
            root.event(
                &UiEvent::MouseWheel {
                    delta: 1.0,
                    position: history_center,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(root.status_bar.history_offset, 1);
        assert_eq!(
            root.event(
                &UiEvent::MouseUp {
                    position: history_center,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            root.event(
                &UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert!(!root.status_bar.history_open);
    }

    #[test]
    fn status_history_actual_router_navigates_keyboard_pages_and_reads_full_localized_entries() {
        use crate::app_ui::localization::AppUiLocalePreference;
        use mondrian_ui_core::FocusManager;
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        for locale_preference in [
            AppUiLocalePreference::EnUs,
            AppUiLocalePreference::ZhCn,
            AppUiLocalePreference::Pseudo,
        ] {
            let mut state = AppState::new();
            for index in 0..20 {
                state.set_status_hint(
                    AppMessage::new("status-opaque-detail").with_text(
                        "detail",
                        format!("Activity {index:02} · E:/项目/完整消息🙂.mdp"),
                    ),
                    index == 0,
                );
            }
            let mut root = AppUiAppRoot::from_app_state_with_preferences(
                &state,
                &AppUiPreferences { locale_preference, ..Default::default() },
            );
            root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
            let id = root.status_bar.id;
            let history = root.status_bar.history.clone();
            let generation = state.project_author_generation();
            let actions = std::cell::RefCell::new(Vec::new());
            let dispatch = |action| actions.borrow_mut().push(action);
            let mut router = EventRouter::new(root.id());
            router.focus_manager_mut().set_focused_widget(Some(id), None);
            for (key, expected) in [
                (KeyCode::Enter, 0),
                (KeyCode::Down, 1),
                (KeyCode::PageDown, 9),
                (KeyCode::End, 12),
                (KeyCode::Down, 12),
                (KeyCode::Up, 11),
                (KeyCode::PageUp, 3),
                (KeyCode::Home, 0),
                (KeyCode::Up, 0),
            ] {
                router.route(
                    UiEvent::KeyDown { key, modifiers: Modifiers::none() },
                    &mut WidgetTreeView::new(&mut root),
                    &dispatch,
                );
                assert!(root.status_bar.history_open);
                assert_eq!(root.status_bar.history_offset, expected, "{key:?}");
                assert_eq!(router.focus_manager().focused_widget(), Some(id));
                let name =
                    root.status_bar.accessibility().expect("history access").name.expect("name");
                for entry in history.iter().rev().skip(expected).take(8) {
                    let body = root.localizer.message(&entry.message);
                    assert!(
                        name.contains(&body),
                        "accessible history retains full entry: {name}"
                    );
                }
                assert!(name.contains("E:/项目/完整消息🙂.mdp"));
                if expected == 12 {
                    let severity = match locale_preference {
                        AppUiLocalePreference::EnUs => "Error:",
                        AppUiLocalePreference::ZhCn => "错误：",
                        AppUiLocalePreference::Pseudo => "错·误·",
                        _ => panic!("explicit test locale"),
                    };
                    assert!(
                        name.contains(severity),
                        "error severity is accessible: {name}"
                    );
                }
                assert_eq!(root.status_bar.history, history);
                assert!(
                    actions.borrow().is_empty(),
                    "history inspection never dispatches editor actions"
                );
                assert_eq!(state.project_author_generation(), generation);
            }
            router.route(
                UiEvent::KeyDown { key: KeyCode::PageUp, modifiers: Modifiers::ctrl() },
                &mut WidgetTreeView::new(&mut root),
                &dispatch,
            );
            assert_eq!(
                root.status_bar.history_offset, 0,
                "modified navigation is not history input"
            );
            router.route(
                UiEvent::FocusLost,
                &mut WidgetTreeView::new(&mut root),
                &dispatch,
            );
            assert!(!root.status_bar.history_open, "unfocused popup closes");
        }
    }

    #[test]
    fn status_history_keyboard_handles_empty_short_and_replaced_history() {
        use crate::app_ui::localization::AppUiLocalePreference;
        use mondrian_ui_core::FocusManager;
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let preferences = AppUiPreferences {
            locale_preference: AppUiLocalePreference::EnUs,
            ..Default::default()
        };
        for (count, oldest_offset) in [(0, 0), (1, 0), (9, 1)] {
            let mut state = AppState::new();
            for index in 0..count {
                state.set_status_hint(
                    AppMessage::new("status-opaque-detail")
                        .with_text("detail", format!("Entry {index}")),
                    false,
                );
            }
            let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
            root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
            let id = root.status_bar.id;
            let mut router = EventRouter::new(root.id());
            router.focus_manager_mut().set_focused_widget(Some(id), None);
            let dispatch = |_| panic!("notification history cannot author");
            for key in [
                KeyCode::Space,
                KeyCode::End,
                KeyCode::PageDown,
                KeyCode::Down,
            ] {
                router.route(
                    UiEvent::KeyDown { key, modifiers: Modifiers::none() },
                    &mut WidgetTreeView::new(&mut root),
                    &dispatch,
                );
            }
            assert!(root.status_bar.history_open);
            assert_eq!(root.status_bar.history_offset, oldest_offset);
            assert_eq!(router.focus_manager().focused_widget(), Some(id));
            let name = root.status_bar.accessibility().expect("history access").name.expect("name");
            if count == 0 {
                assert!(name.contains("No notifications"));
            } else {
                assert!(name.contains("Entry 0"), "oldest entry is inspectable");
            }
            let empty_state = AppState::new();
            root.refresh_from_app_state_with_preferences(&empty_state, &preferences);
            assert_eq!(root.status_bar.id, id);
            assert_eq!(router.focus_manager().focused_widget(), Some(id));
            assert_eq!(root.status_bar.history_offset, 0);
            assert!(root.status_bar.history.is_empty());
            assert!(root
                .status_bar
                .accessibility()
                .expect("history access")
                .name
                .expect("name")
                .contains("No notifications"));
            router.route(
                UiEvent::KeyDown { key: KeyCode::Escape, modifiers: Modifiers::none() },
                &mut WidgetTreeView::new(&mut root),
                &dispatch,
            );
            assert!(!root.status_bar.history_open);
        }
    }

    #[test]
    fn status_history_paint_clips_localized_text_using_theme_padding_without_truncating_accessibility(
    ) {
        use crate::app_ui::localization::AppUiLocalePreference;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let detail = "E:/项目/需要保留的完整诊断🙂/".repeat(12);
        for populated in [false, true] {
            let mut state = AppState::new();
            if populated {
                state.set_status_hint(
                    AppMessage::new("status-opaque-detail").with_text("detail", &detail),
                    true,
                );
            }
            let mut root = AppUiAppRoot::from_app_state_with_preferences(
                &state,
                &AppUiPreferences {
                    locale_preference: AppUiLocalePreference::Pseudo,
                    ..Default::default()
                },
            );
            root.layout(Rect::new(0.0, 0.0, 220.0, 720.0));
            root.status_bar.history_open = true;
            root.status_bar.model.context.clear();
            let mut theme = ThemePreset::Dark.build();
            theme.spacing.panel_inner_margin.0 = 36.0;
            let available = root.status_bar.history_rect().width - 72.0;
            let mut encoder = PaintOrderRecorder::default();
            root.status_bar.paint(&mut PaintContext {
                encoder: &mut encoder,
                theme: &theme,
                clip_rect: root.bounds,
                pixels_per_point: 1.0,
            });
            assert_eq!(
                encoder.texts.len(),
                3,
                "status, history title, history body"
            );
            for text in encoder.texts.iter().skip(1) {
                assert!(
                    estimate_text_width(text, theme.typography.metadata.font_size) <= available
                );
            }
            assert!(
                encoder.texts[1].ends_with("..."),
                "expanded title elides within themed content"
            );
            if populated {
                assert!(encoder.texts[2].ends_with("..."));
                assert!(root
                    .status_bar
                    .accessibility()
                    .expect("access")
                    .name
                    .expect("name")
                    .contains(&detail));
            }
        }
    }

    #[test]
    fn root_projects_only_new_terminal_notifications_and_expires_them() {
        let mut state = AppState::new();
        state.notifications.publish(
            "save:before-root",
            crate::app::notifications::AppNotificationSeverity::Success,
            AppMessage::new("notification-save-complete"),
        );
        let mut root = AppUiAppRoot::from_app_state(&state);
        assert!(root.next_notification_deadline().is_none());

        state.notifications.publish(
            "save:after-root",
            crate::app::notifications::AppNotificationSeverity::Success,
            AppMessage::new("notification-save-complete"),
        );
        root.refresh_from_app_state(&state);
        let deadline = root.next_notification_deadline().expect("visible terminal toast");
        assert!(root.expire_notifications(deadline));
        assert!(root.next_notification_deadline().is_none());
    }

    #[test]
    fn authoring_session_switch_discards_stale_toast() {
        let mut state = AppState::new();
        state.test_set_project_path(PathBuf::from("E:/projects/notification-first.mdp"));
        let mut root = AppUiAppRoot::from_app_state(&state);
        state.notifications.publish(
            "save:first-session",
            crate::app::notifications::AppNotificationSeverity::Success,
            AppMessage::new("notification-save-complete"),
        );
        root.refresh_from_app_state(&state);
        assert!(root.next_notification_deadline().is_some());

        state.test_set_project_path(PathBuf::from("E:/projects/notification-second.mdp"));
        root.refresh_from_app_state(&state);
        assert!(root.next_notification_deadline().is_none());
    }

    #[test]
    fn app_root_refresh_updates_status_bar_paint_model() {
        let mut root = AppUiAppRoot::demo();
        let mut state = AppState::new();
        state.test_set_sequence(Some(mondrian_timeline::Sequence::new("rough-cut")));
        state.test_set_project_path(PathBuf::from("E:/projects/rough-cut.mdp"));
        state.set_status_hint(
            AppMessage::new("status-opaque-detail").with_text("detail", "Import failed"),
            true,
        );

        root.refresh_from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let mut encoder = PaintOrderRecorder::default();
        let mut ctx = PaintContext {
            pixels_per_point: 1.0,
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
    fn app_root_interpret_asset_dialog_confirms_asset_interpretation_action() {
        let platform = FakePlatform::default();
        let asset_id = AssetId::new();
        let mut root = AppUiAppRoot::demo();

        root.handle_shell_action(
            app_shell_interpret_asset_dialog_action(AppShellInterpretAssetDialogPayload {
                asset_id,
                asset_name: "Shot A.mov".to_owned(),
                interpretation: AssetMediaInterpretation::default(),
                auto_interpretation: None,
                video_signal: None,
                input_pipeline: None,
            }),
            &platform,
            None,
        );
        assert!(root.modal.as_ref().and_then(ShellModal::as_interpret_asset).is_some());
        root.handle_shell_action(
            app_shell_interpret_asset_draft_changed_action(InterpretAssetDraftUpdatePayload {
                interpretation: AssetMediaInterpretation {
                    color: MediaColorInterpretation::Override {
                        color_space: ColorSpace::SonySLog3SGamut3Cine,
                    },
                    ..AssetMediaInterpretation::default()
                },
            }),
            &platform,
            None,
        );

        let action = root.handle_shell_action(
            app_shell_confirm_interpret_asset_dialog_action(),
            &platform,
            None,
        );

        let Some(Action::Custom { namespace, name, payload }) = action else {
            panic!("expected asset interpretation action");
        };
        assert_eq!(namespace, ASSET_NAMESPACE);
        assert_eq!(name, ASSET_SET_INTERPRETATION);
        let payload: AssetsSetInterpretationPayload =
            serde_json::from_value(payload).expect("interpretation payload");
        assert_eq!(payload.asset_id, asset_id);
        assert_eq!(
            payload.interpretation.color.override_color_space(),
            Some(ColorSpace::SonySLog3SGamut3Cine)
        );
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
    fn app_root_modal_router_contains_tabs_and_restores_background_focus() {
        use mondrian_ui_core::{FocusManager, TreeWalker, WidgetTree};
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let platform = FakePlatform::default();
        let mut root = AppUiAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let background = {
            let tree = WidgetTreeView::new(&mut root);
            TreeWalker::focus_next(&tree, WidgetId::default()).expect("background control")
        };
        let mut router = EventRouter::new(root.id());
        router.focus_manager_mut().set_focused_widget(Some(background), None);
        root.handle_shell_action(app_shell_about_action(), &platform, None);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let modal = root.keyboard_focus_scope().expect("modal scope");
        let actions = std::cell::RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        {
            let mut tree = WidgetTreeView::new(&mut root);
            router.route(
                UiEvent::TextInput("背景不能收到".to_owned()),
                &mut tree,
                &dispatch,
            );
            let first = router.focus_manager().focused_widget().expect("modal initial focus");
            assert_ne!(first, background);
            for modifiers in [Modifiers::none(), Modifiers::shift()] {
                for _ in 0..24 {
                    router.route(
                        UiEvent::KeyDown { key: KeyCode::Tab, modifiers },
                        &mut tree,
                        &dispatch,
                    );
                    let mut current = router.focus_manager().focused_widget();
                    while current.is_some() && current != Some(modal) {
                        current = current.and_then(|id| tree.parent_id(id));
                    }
                    assert_eq!(current, Some(modal));
                }
            }
        }
        assert!(
            actions.borrow().is_empty(),
            "typing and traversing must not author background actions"
        );
        root.modal = None;
        {
            let mut tree = WidgetTreeView::new(&mut root);
            router.route(
                UiEvent::KeyUp { key: KeyCode::Tab, modifiers: Modifiers::none() },
                &mut tree,
                &dispatch,
            );
        }
        assert_eq!(router.focus_manager().focused_widget(), Some(background));
    }

    fn async_resource_fixture() -> (
        AppState,
        mondrian_core::ClipId,
        crate::app_ui::resource_dialog::InspectorPropertyTarget,
    ) {
        use mondrian_core::{
            ParameterResourceContract, ParameterResourceReference, PropertyDescriptor,
            PropertyValue, ResourcePathKind, ResourcePathSelection, TimelineTime,
        };
        use mondrian_effects::*;
        let kind = EffectType::Plugin(format!("test.resource.async.{}", uuid::Uuid::new_v4()));
        let builder = EffectPluginDefinitionBuilder::new(
            kind.key(),
            "Async resource",
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_branching_graph(|_, _, graph| {
            graph.apply(EffectRenderOp::Sharpen { amount: 0.0 });
        })
        .property(
            PropertyDescriptor::new(
                "path",
                "Path",
                PropertyValue::Resource(ParameterResourceReference::Unbound),
            )
            .with_parameter_id(kind.parameter_id("path").expect("parameter"))
            .with_resource_contract(ParameterResourceContract::external_path(
                ResourcePathKind::File,
                ResourcePathSelection::ExistingOrNew,
            )),
        );
        register_effect_definition(builder.build()).expect("definition");
        let effect = instantiate_effect_node(kind).expect("effect");
        let target = crate::app_ui::resource_dialog::InspectorPropertyTarget::Effect {
            effect_id: effect.id,
            parameter: effect.properties.property("path").expect("path").address(),
        };
        let mut clip = mondrian_timeline::Clip::new_basic_title(
            "Title",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("clip");
        let id = clip.id;
        clip.add_effect_node(effect);
        let mut sequence = mondrian_timeline::Sequence::new("Async resource");
        sequence.video_tracks[0].add_clip(clip).expect("clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.select_clip_by_id(id).expect("selection");
        (state, id, target)
    }

    #[test]
    fn relative_resource_inspection_and_authoring_use_the_project_directory() {
        use crate::app_ui::panels::InspectorPanelModel;
        use crate::app_ui::resource_dialog::{
            ResourceActionOutcome, ResourceEditorAction, ResourceKind, ResourceUpdate,
        };
        use crate::resource_inspection::PathInspectionOutcome;
        use mondrian_core::{ParameterResourceReference, PropertyValue};
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, clip_id, target) = async_resource_fixture();
        let project_directory = state
            .current_project_path()
            .expect("saved authoring owner")
            .parent()
            .expect("project directory")
            .to_path_buf();
        let relative = PathBuf::from("relative-resources").join("output.cube");
        let expected = project_directory.join(&relative);
        let mut root = AppUiAppRoot::from_app_state(&state);
        let platform = FakePlatform::default();
        root.try_handle_resource_action(
            &ResourceEditorAction::Open { clip_id, target: target.clone() }.action(),
            &state,
            &platform,
        )
        .expect("open");
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        for update in [
            ResourceUpdate::Kind(ResourceKind::ExternalFile),
            ResourceUpdate::Text(relative.to_string_lossy().into_owned()),
        ] {
            root.try_handle_resource_action(
                &ResourceEditorAction::Update { id, update }.action(),
                &state,
                &platform,
            )
            .expect("update");
        }
        let ResourceActionOutcome::Inspect(request) = root
            .try_handle_resource_action(
                &ResourceEditorAction::Confirm { id }.action(),
                &state,
                &platform,
            )
            .expect("confirm")
        else {
            panic!("new external path requires inspection");
        };
        assert_eq!(
            request.path, expected,
            "inspection must not depend on child CWD"
        );
        assert!(!state.can_undo_action(), "inspection is not an edit");
        let action = root
            .finish_resource_inspection(request.ticket, PathInspectionOutcome::Missing, &state)
            .expect("accepted new path");
        state.dispatch_action(action).expect("one author transaction");
        let property = target
            .property(
                clip_id,
                &InspectorPanelModel::from_app_state(&state, &root.localizer),
            )
            .expect("resource property")
            .value
            .clone();
        assert_eq!(
            property,
            PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                path: expected.into()
            }),
            "saved author value must refer to the same inspected resource",
        );
        state.dispatch_action(Action::Undo).expect("Undo");
        assert!(
            !state.can_undo_action(),
            "one confirmation creates one history entry"
        );
        state.dispatch_action(Action::Redo).expect("Redo");
        assert_eq!(
            target
                .property(
                    clip_id,
                    &InspectorPanelModel::from_app_state(&state, &root.localizer)
                )
                .expect("resource after Redo")
                .value,
            property,
        );
        let direct_relative = PathBuf::from("direct-author").join("resource.cube");
        state
            .dispatch_action(target.write_action(clip_id, |parameter| {
                mondrian_core::automation::ParameterValueWrite::Value {
                    parameter,
                    value: PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                        path: direct_relative.clone().into(),
                    }),
                }
            }))
            .expect("direct typed author action uses the same owner");
        assert_eq!(
            target
                .property(
                    clip_id,
                    &InspectorPanelModel::from_app_state(&state, &root.localizer)
                )
                .expect("direct resource")
                .value,
            PropertyValue::Resource(ParameterResourceReference::ExternalFile {
                path: project_directory.join(direct_relative).into(),
            }),
        );
    }

    #[test]
    fn resource_inspection_does_not_commit_against_a_new_save_as_directory() {
        use crate::app_ui::resource_dialog::{
            ResourceActionOutcome, ResourceEditorAction, ResourceKind, ResourceUpdate,
        };
        use crate::resource_inspection::PathInspectionOutcome;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, clip_id, target) = async_resource_fixture();
        let session = state.authoring_session_id();
        let mut root = AppUiAppRoot::from_app_state(&state);
        let platform = FakePlatform::default();
        root.try_handle_resource_action(
            &ResourceEditorAction::Open { clip_id, target }.action(),
            &state,
            &platform,
        )
        .expect("open");
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        for update in [
            ResourceUpdate::Kind(ResourceKind::ExternalFile),
            ResourceUpdate::Text("pending-resource.cube".into()),
        ] {
            root.try_handle_resource_action(
                &ResourceEditorAction::Update { id, update }.action(),
                &state,
                &platform,
            )
            .expect("update");
        }
        let ResourceActionOutcome::Inspect(request) = root
            .try_handle_resource_action(
                &ResourceEditorAction::Confirm { id }.action(),
                &state,
                &platform,
            )
            .expect("inspect")
        else {
            panic!("expected inspection");
        };
        let destination = tempfile::tempdir().expect("new project directory");
        let project_file = destination.path().join("saved-as.mdp");
        let save = state.request_project_save_as(project_file.clone()).expect("Save As");
        state.wait_for_persistence_request(save).expect("durable Save As");
        assert_eq!(state.current_project_path(), Some(project_file.as_path()));
        assert_eq!(
            state.authoring_session_id(),
            session,
            "Save As preserves Session identity"
        );
        assert!(root.resource_inspection_ticket(&state).is_none());
        assert!(root
            .finish_resource_inspection(request.ticket, PathInspectionOutcome::Missing, &state)
            .is_none());
        assert!(root.modal.is_none());
        assert!(
            !state.can_undo_action(),
            "stale inspection creates no author history"
        );
        state.close_project().expect("release saved project and destination");
    }

    #[test]
    fn pending_resource_modal_routes_tabs_after_apply_is_disabled_and_restores_focus() {
        use crate::app_ui::resource_dialog::{
            ResourceActionOutcome, ResourceEditorAction, ResourceKind, ResourceUpdate,
        };
        use crate::resource_inspection::PathInspectionOutcome;
        use mondrian_ui_core::{FocusManager, TreeWalker, WidgetTree};
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (state, clip_id, target) = async_resource_fixture();
        let revision = state.active_sequence().expect("sequence").revision;
        let mut root = AppUiAppRoot::from_app_state(&state);
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        root.layout(bounds);
        let background = {
            let tree = WidgetTreeView::new(&mut root);
            TreeWalker::focus_next(&tree, WidgetId::default()).expect("background focus")
        };
        let mut router = EventRouter::new(root.id());
        router.focus_manager_mut().set_focused_widget(Some(background), None);
        let platform = FakePlatform::default();
        root.try_handle_resource_action(
            &ResourceEditorAction::Open { clip_id, target }.action(),
            &state,
            &platform,
        )
        .expect("open resource");
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("dialog").draft.id;
        for update in [
            ResourceUpdate::Kind(ResourceKind::ExternalFile),
            ResourceUpdate::Text("pending-output.cube".into()),
        ] {
            root.try_handle_resource_action(
                &ResourceEditorAction::Update { id, update }.action(),
                &state,
                &platform,
            )
            .expect("update draft");
        }
        root.layout(bounds);
        let scope = root.keyboard_focus_scope().expect("modal scope");
        let apply = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("dialog")
            .child(8)
            .expect("apply")
            .id();
        let actions = std::cell::RefCell::new(Vec::new());
        let dispatch = |action| actions.borrow_mut().push(action);
        // Reach Apply through actual traversal so the router suspends background focus.
        for _ in 0..24 {
            router.route(
                UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
                &mut WidgetTreeView::new(&mut root),
                &dispatch,
            );
            if router.focus_manager().focused_widget() == Some(apply) {
                break;
            }
        }
        assert_eq!(router.focus_manager().focused_widget(), Some(apply));
        let ResourceActionOutcome::Inspect(request) = root
            .try_handle_resource_action(
                &ResourceEditorAction::Confirm { id }.action(),
                &state,
                &platform,
            )
            .expect("confirm")
        else {
            panic!("inspection required");
        };
        assert!(!root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("pending dialog")
            .child(8)
            .expect("disabled apply")
            .can_focus());
        for modifiers in [Modifiers::none(), Modifiers::shift()] {
            for _ in 0..24 {
                let mut tree = WidgetTreeView::new(&mut root);
                assert_eq!(
                    router.route(
                        UiEvent::KeyDown { key: KeyCode::Tab, modifiers },
                        &mut tree,
                        &dispatch,
                    ),
                    EventResult::Handled
                );
                let focused = router.focus_manager().focused_widget().expect("modal focus");
                assert_ne!(
                    focused, apply,
                    "pending confirmation must leave the focus order"
                );
                assert!(tree.get(focused).expect("live focus").can_focus());
                let mut ancestor = Some(focused);
                while ancestor.is_some() && ancestor != Some(scope) {
                    ancestor = ancestor.and_then(|widget| tree.parent_id(widget));
                }
                assert_eq!(ancestor, Some(scope), "pending Tab escaped the modal");
            }
        }
        assert!(
            actions.borrow().is_empty(),
            "traversal must not dispatch resource edits"
        );
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            revision
        );
        assert!(!state.can_undo_action());
        assert!(root
            .finish_resource_inspection(request.ticket, PathInspectionOutcome::TimedOut, &state,)
            .is_none());
        assert!(root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("retryable dialog")
            .child(8)
            .expect("apply")
            .can_focus());
        for _ in 0..24 {
            router.route(
                UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::shift() },
                &mut WidgetTreeView::new(&mut root),
                &dispatch,
            );
            if router.focus_manager().focused_widget() == Some(apply) {
                break;
            }
        }
        assert_eq!(
            router.focus_manager().focused_widget(),
            Some(apply),
            "timeout must permit retry"
        );
        root.try_handle_resource_action(
            &ResourceEditorAction::Cancel { id }.action(),
            &state,
            &platform,
        )
        .expect("cancel pending draft");
        router.route(
            UiEvent::KeyUp { key: KeyCode::Tab, modifiers: Modifiers::none() },
            &mut WidgetTreeView::new(&mut root),
            &dispatch,
        );
        assert_eq!(router.focus_manager().focused_widget(), Some(background));
        assert!(!state.can_undo_action());
        assert!(actions.borrow().is_empty());
    }

    #[test]
    fn resource_inspection_rejects_edited_cancelled_locked_and_replaced_drafts() {
        use crate::app_ui::resource_dialog::{
            ResourceActionOutcome, ResourceEditorAction, ResourceKind, ResourceUpdate,
        };
        use crate::resource_inspection::PathInspectionOutcome;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (mut state, clip_id, target) = async_resource_fixture();
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let platform = FakePlatform::default();
        let open = ResourceEditorAction::Open { clip_id, target };
        let prepare = |root: &mut AppUiAppRoot, state: &AppState| {
            root.try_handle_resource_action(&open.clone().action(), state, &platform)
                .expect("open");
            let id =
                root.modal.as_ref().and_then(ShellModal::as_resource).expect("dialog").draft.id;
            for update in [
                ResourceUpdate::Kind(ResourceKind::ExternalFile),
                ResourceUpdate::Text("未创建/输出🙂.cube".to_owned()),
            ] {
                root.try_handle_resource_action(
                    &ResourceEditorAction::Update { id, update }.action(),
                    state,
                    &platform,
                )
                .expect("update");
            }
            id
        };
        let confirm = |root: &mut AppUiAppRoot, state: &AppState, id| match root
            .try_handle_resource_action(
                &ResourceEditorAction::Confirm { id }.action(),
                state,
                &platform,
            )
            .expect("confirm")
        {
            ResourceActionOutcome::Inspect(request) => request.ticket,
            _ => panic!("new path requires inspection"),
        };
        let id = prepare(&mut root, &state);
        let first = confirm(&mut root, &state, id);
        assert!(!root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("dialog")
            .child(8)
            .expect("apply")
            .can_focus());
        assert!(root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("dialog")
            .child(7)
            .expect("cancel")
            .can_focus());
        assert!(!state.can_undo_action());
        assert!(matches!(
            root.try_handle_resource_action(
                &ResourceEditorAction::Confirm { id }.action(),
                &state,
                &platform
            )
            .expect("duplicate"),
            ResourceActionOutcome::Handled(None)
        ));
        for text in ["another.cube", "未创建/输出🙂.cube"] {
            root.try_handle_resource_action(
                &ResourceEditorAction::Update { id, update: ResourceUpdate::Text(text.into()) }
                    .action(),
                &state,
                &platform,
            )
            .expect("edit");
        }
        assert!(root
            .finish_resource_inspection(first, PathInspectionOutcome::File, &state)
            .is_none());
        assert!(root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("dialog")
            .child(8)
            .expect("apply")
            .can_focus());
        assert!(root.modal.is_some());
        let browsed = confirm(&mut root, &state, id);
        root.try_handle_resource_action(
            &ResourceEditorAction::Browse { id, folder: false }.action(),
            &state,
            &platform,
        )
        .expect("cancelled picker");
        let dialog = root.modal.as_ref().and_then(ShellModal::as_resource).expect("retained draft");
        assert!(dialog.pending_inspection.is_none());
        assert!(dialog.child(8).expect("apply").can_focus());
        assert!(!state.can_undo_action());
        assert!(root
            .finish_resource_inspection(browsed, PathInspectionOutcome::File, &state)
            .is_none());
        let second = confirm(&mut root, &state, id);
        assert!(root
            .finish_resource_inspection(second, PathInspectionOutcome::TimedOut, &state)
            .is_none());
        assert!(root.modal.is_some());
        assert!(!state.can_undo_action());
        let third = confirm(&mut root, &state, id);
        let action = root
            .finish_resource_inspection(third, PathInspectionOutcome::Missing, &state)
            .expect("one commit");
        state.dispatch_action(action).expect("author commit");
        assert!(root.modal.is_none());
        assert!(state.undo_timeline().expect("undo"));
        assert!(!state.can_undo_action());
        let id = prepare(&mut root, &state);
        let cancelled = confirm(&mut root, &state, id);
        root.try_handle_resource_action(
            &ResourceEditorAction::Cancel { id }.action(),
            &state,
            &platform,
        )
        .expect("cancel");
        let id = prepare(&mut root, &state);
        assert!(root
            .finish_resource_inspection(cancelled, PathInspectionOutcome::File, &state)
            .is_none());
        let locked = confirm(&mut root, &state, id);
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        assert_eq!(root.resource_inspection_ticket(&state), None);
        assert!(root
            .finish_resource_inspection(locked, PathInspectionOutcome::File, &state)
            .is_none());
        assert!(root.modal.is_none());
        assert!(!state.can_undo_action());
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked =
            false;
        let id = prepare(&mut root, &state);
        let replaced = confirm(&mut root, &state, id);
        state.clear_selection();
        assert!(root
            .finish_resource_inspection(replaced, PathInspectionOutcome::File, &state)
            .is_none());
        assert!(root.modal.is_none());
        assert!(!state.can_undo_action());
    }

    #[test]
    #[ignore = "requires a rebuilt MONDRIAN_RESOURCE_INSPECTION_HELPER product executable"]
    fn resource_inspection_actual_host_commits_only_after_the_owned_helper_finishes() {
        use crate::app_ui::action_queue::PendingUiActions;
        use crate::app_ui::host::AppUiHost;
        use crate::app_ui::resource_dialog::{ResourceEditorAction, ResourceKind, ResourceUpdate};
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let (state, clip_id, target) = async_resource_fixture();
        let temp = tempfile::tempdir().expect("workspace");
        let destination = temp.path().join("新目录").join("输出🙂.cube");
        let helper =
            std::env::var_os("MONDRIAN_RESOURCE_INSPECTION_HELPER").expect("rebuilt helper");
        let mut host = AppUiHost::new_with_preferences_path(
            state,
            AppUiPreferences::default(),
            temp.path().join("prefs.json"),
        );
        host.set_resource_inspection_helper_for_test(helper.into());
        let pending = PendingUiActions::default();
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        let platform = FakePlatform::default();
        pending.push(ResourceEditorAction::Open { clip_id, target }.action());
        host.drain_pending_actions(&pending, bounds, &platform);
        let id = host
            .root()
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("dialog")
            .draft
            .id;
        for update in [
            ResourceUpdate::Kind(ResourceKind::ExternalFile),
            ResourceUpdate::Text(destination.to_str().expect("path").into()),
        ] {
            pending.push(ResourceEditorAction::Update { id, update }.action());
        }
        pending.push(ResourceEditorAction::Confirm { id }.action());
        host.drain_pending_actions(&pending, bounds, &platform);
        assert!(!host.app_state().can_undo_action());
        assert!(host.root().modal.is_some());
        assert!(host.resource_inspection_next_wake_delay().is_some());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while host.root().modal.is_some() {
            host.poll_background_tasks(bounds);
            assert!(
                std::time::Instant::now() < deadline,
                "resource helper did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(host.app_state().can_undo_action());
        assert_eq!(host.resource_inspection_next_wake_delay(), None);
        assert!(!destination.parent().expect("parent").exists());
        drop(host);
    }

    #[test]
    fn resource_catalog_refreshes_during_ime_without_replacing_draft_controls() {
        use crate::app_ui::action_queue::PendingUiActions;
        use crate::app_ui::host::AppUiHost;
        use crate::app_ui::resource_dialog::{
            InspectorPropertyTarget, ResourceEditorAction, ResourceUpdate,
        };
        use mondrian_core::automation::PropertyDescriptor;
        use mondrian_core::{ParameterResourceReference, PropertyValue, TimelineTime};
        use mondrian_effects::{
            instantiate_effect_node, register_effect_definition, EffectColorDomainContract,
            EffectDeterminism, EffectExecutionContract, EffectExecutionModes, EffectGraphTopology,
            EffectPluginDefinitionBuilder, EffectRenderOp, EffectResourceLifetime,
            EffectRoiPropagation, EffectStateModel, EffectTemporalInputExtent, EffectType,
        };
        use mondrian_ui_core::{FocusManager, TreeWalker};
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let mut state = AppState::new();
        state.test_set_sequence(Some(mondrian_timeline::Sequence::new("Catalog acceptance")));
        let selected = state
            .asset_library()
            .expect("library")
            .create_solid_color_asset(Some("Film selected"))
            .expect("selected asset");
        let other = state
            .asset_library()
            .expect("library")
            .create_solid_color_asset(Some("Film other"))
            .expect("other asset");
        let kind = EffectType::Plugin("test.resource.live-catalog".to_owned());
        let builder = EffectPluginDefinitionBuilder::new(
            kind.key(),
            "Live resource catalog",
            EffectColorDomainContract::SCENE_LINEAR,
        )
        .with_execution_contract(EffectExecutionContract {
            execution_modes: EffectExecutionModes::CPU_F32,
            determinism: EffectDeterminism::Deterministic,
            state_model: EffectStateModel::Stateless,
            temporal_input: EffectTemporalInputExtent::CURRENT_FRAME,
            roi_propagation: EffectRoiPropagation::UnknownRequiresFullFrame,
            resource_lifetime: EffectResourceLifetime::Frame,
            topology: EffectGraphTopology::LinearChain,
        })
        .with_branching_graph(|_, _, graph| {
            graph.apply(EffectRenderOp::Sharpen { amount: 0.0 });
        })
        .property(
            PropertyDescriptor::new(
                "asset",
                "Asset",
                PropertyValue::Resource(ParameterResourceReference::ProjectAsset {
                    asset_id: selected,
                }),
            )
            .with_parameter_id(kind.parameter_id("asset").expect("parameter")),
        );
        register_effect_definition(builder.build()).expect("definition");
        let effect = instantiate_effect_node(kind).expect("effect");
        let target = InspectorPropertyTarget::Effect {
            effect_id: effect.id,
            parameter: effect.properties.property("asset").expect("resource").address(),
        };
        let mut clip = mondrian_timeline::Clip::new_basic_title(
            "Title",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("clip");
        let clip_id = clip.id;
        clip.add_effect_node(effect);
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0]
            .add_clip(clip)
            .expect("add clip");
        state.select_clip_by_id(clip_id).expect("select clip");
        let preferences =
            std::env::temp_dir().join(format!("catalog-{}.json", uuid::Uuid::new_v4()));
        let mut host =
            AppUiHost::new_with_preferences_path(state, AppUiPreferences::default(), preferences);
        let bounds = Rect::new(0.0, 0.0, 1280.0, 720.0);
        TreeWalker::layout(host.active_root_mut(), bounds);
        let pending = PendingUiActions::default();
        let platform = FakePlatform::default();
        pending.push(ResourceEditorAction::Open { clip_id, target }.action());
        host.drain_pending_actions(&pending, bounds, &platform);
        let (draft_id, input, point) = {
            let dialog =
                host.root().modal.as_ref().and_then(ShellModal::as_resource).expect("dialog");
            let input = dialog.child(2).expect("search input");
            let point = (0..720)
                .step_by(4)
                .find_map(|y| {
                    (0..1280).step_by(4).find_map(|x| {
                        let point = Point::new(x as f32, y as f32);
                        input.hit_test(point).then_some(point)
                    })
                })
                .expect("input point");
            (dialog.draft.id, input.id(), point)
        };
        let history = host.app_state().authoring_history().expect("history").diagnostics();
        let mut router = EventRouter::new(host.active_root().id());
        {
            let mut tree = WidgetTreeView::new(host.active_root_mut());
            let dispatch = |action| pending.push(action);
            for event in [
                UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::MouseUp {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                UiEvent::TextInput("Film".to_owned()),
                UiEvent::ImePreedit("候选".to_owned()),
            ] {
                router.route(event, &mut tree, &dispatch);
            }
        }
        host.drain_pending_actions(&pending, bounds, &platform);
        let choices = host
            .root()
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("dialog")
            .child(3)
            .expect("choices")
            .id();
        let added = {
            let state = host.app_state();
            let library = state.asset_library().expect("library");
            library.rename_asset(selected, "Film renamed").expect("rename");
            library.retire_assets(&[other]).expect("retire other");
            library.create_solid_color_asset(Some("Film new")).expect("new asset")
        };
        host.mark_dirty();
        host.refresh_if_dirty(bounds);
        let dialog = host
            .root()
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("retained dialog");
        assert_eq!(dialog.draft.id, draft_id);
        assert_eq!(dialog.child(2).expect("search").id(), input);
        assert_eq!(router.focus_manager().focused_widget(), Some(input));
        assert!(dialog.is_composing(), "library refresh must preserve IME");
        assert_eq!(dialog.child(3).expect("choices").id(), choices);
        assert_eq!(
            dialog.child(3).expect("choices").accessibility().expect("choice caption").name,
            Some("Film renamed".to_owned()),
            "live chooser must reflect authoritative asset rename"
        );
        assert_eq!(
            host.app_state().authoring_history().expect("history").diagnostics(),
            history
        );
        {
            let mut tree = WidgetTreeView::new(host.active_root_mut());
            let dispatch = |action| pending.push(action);
            for event in [
                UiEvent::ImeCancel,
                UiEvent::KeyDown { key: KeyCode::Tab, modifiers: Modifiers::none() },
                UiEvent::KeyDown { key: KeyCode::Space, modifiers: Modifiers::none() },
                UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            ] {
                router.route(event, &mut tree, &dispatch);
            }
        }
        let actions = pending.take_all();
        assert_eq!(
            actions.len(),
            1,
            "keyboard selection emits one local draft update"
        );
        let Action::Custom { payload, .. } = &actions[0] else {
            panic!("resource action")
        };
        let ResourceEditorAction::Update { id, update: ResourceUpdate::Asset(chosen) } =
            serde_json::from_value(payload.clone()).expect("selection")
        else {
            panic!("asset update")
        };
        assert_eq!(id, draft_id);
        assert_eq!(
            chosen, added,
            "fresh imported asset must be the first matching choice"
        );
        pending.push(actions[0].clone());
        host.drain_pending_actions(&pending, bounds, &platform);
        // An unchanged revision must not reset an open menu's keyboard row.
        {
            let mut tree = WidgetTreeView::new(host.active_root_mut());
            let dispatch = |action| pending.push(action);
            for key in [KeyCode::Space, KeyCode::Down] {
                router.route(
                    UiEvent::KeyDown { key, modifiers: Modifiers::none() },
                    &mut tree,
                    &dispatch,
                );
            }
        }
        host.mark_dirty();
        host.refresh_if_dirty(bounds);
        {
            let mut tree = WidgetTreeView::new(host.active_root_mut());
            router.route(
                UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                &mut tree,
                &|action| pending.push(action),
            );
        }
        let actions = pending.take_all();
        assert_eq!(
            actions.len(),
            1,
            "unchanged library must preserve the highlighted row"
        );
        let Action::Custom { payload, .. } = &actions[0] else {
            panic!("resource action")
        };
        let ResourceEditorAction::Update { update: ResourceUpdate::Asset(chosen), .. } =
            serde_json::from_value(payload.clone()).expect("selection")
        else {
            panic!("asset update")
        };
        assert_eq!(chosen, selected);
        // Delayed selection of a retired row cannot author a new binding.
        pending.push(
            ResourceEditorAction::Update { id: draft_id, update: ResourceUpdate::Asset(other) }
                .action(),
        );
        pending.push(ResourceEditorAction::Confirm { id: draft_id }.action());
        host.drain_pending_actions(&pending, bounds, &platform);
        assert!(host.root().modal.as_ref().and_then(ShellModal::as_resource).is_some());
        assert_eq!(
            host.app_state().authoring_history().expect("history").diagnostics(),
            history
        );
    }

    #[test]
    #[ignore = "requires rebuilt product and SessionProbe in compound_author mode"]
    fn native_compound_author_edits_undo_and_archive_preserve_actual_pixels() {
        use crate::app::ui_actions::{
            visual_effect_set_parameter_value_action, VisualEffectSetParameterValuePayload,
        };
        use mondrian_core::automation::ParameterValueWrite;
        use mondrian_core::{PropertyValue, TimelineTime};
        use mondrian_effects::{EffectFrameContext, LutPreparationCache, PreparedEffectProgram};
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        assert_eq!(
            std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
            "compound_author"
        );
        let helper =
            PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").expect("product"));
        let binary =
            PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
        let inspection = crate::openfx_adapter::inspect_openfx_binary(&helper, &binary)
            .expect("inspect actual SDK binary");
        let kind = crate::openfx_effect::register_selected_openfx_filter(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
        )
        .expect("describe and register compound controls");
        let effect =
            mondrian_effects::instantiate_effect_node(kind).expect("native compound effect");
        let defaults = [
            (
                "point2",
                PropertyValue::DoubleVec2([0.12500000000000003, 10.000000000000002]),
            ),
            (
                "point3",
                PropertyValue::DoubleVec3([
                    -0.5000000000000001,
                    1.0000000000000002,
                    2.0000000000000004,
                ]),
            ),
            (
                "rgb",
                PropertyValue::DoubleRgb([
                    0.10000000000000002,
                    0.20000000000000004,
                    0.30000000000000004,
                ]),
            ),
            (
                "rgba",
                PropertyValue::DoubleColor([
                    0.12500000000000003,
                    0.25000000000000006,
                    0.5000000000000001,
                    0.7500000000000001,
                ]),
            ),
            ("integer2", PropertyValue::IntVec2([-123456789, 123456789])),
            ("integer3", PropertyValue::IntVec3([i32::MIN, 0, i32::MAX])),
        ];
        let effect_id = effect.id;
        let addresses = defaults
            .iter()
            .map(|(name, expected)| {
                let property = effect
                    .properties
                    .iter()
                    .find(|(_, property)| property.descriptor.display_name == *name)
                    .expect("typed native author property")
                    .1;
                assert_eq!(property.static_value(), expected);
                property.address()
            })
            .collect::<Vec<_>>();
        let mut clip = mondrian_timeline::Clip::new_basic_title(
            "Tuple",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("clip");
        let clip_id = clip.id;
        clip.add_effect_node(effect);
        let mut sequence = mondrian_timeline::Sequence::new("Compound author workflow");
        sequence.video_tracks[0].add_clip(clip).expect("insert clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        let execute = |state: &AppState| {
            let clip = state.active_sequence().expect("sequence").find_clip(clip_id).expect("clip");
            let range = mondrian_core::TimelineTimeRange::new(
                TimelineTime::ZERO,
                TimelineTime::new(1, 1).expect("end"),
            )
            .expect("range");
            let context = EffectFrameContext::new(
                mondrian_core::Rational::FPS_24,
                mondrian_core::SampleAspectRatio::SQUARE,
                range,
            )
            .expect("context");
            let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
                &clip.effects,
                &[],
                &[],
                &[],
                mondrian_core::WorkingColorSpace::LinearRec709,
                context,
                &LutPreparationCache::uncached(),
            )
            .expect("compile native compound graph");
            let graph = program.evaluate(TimelineTime::ZERO).expect("evaluate graph");
            let input = [[0.1, 0.2, 0.3, 0.05]; 4];
            let output = mondrian_effects::apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(2, 2),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    &input,
                ),
                &graph,
                1415,
            )
            .expect("execute actual supervised product SDK pixels");
            let mut multiplier = 1.0;
            for (index, (name, default)) in defaults.iter().enumerate() {
                let actual = clip.effects[0]
                    .properties
                    .iter()
                    .find(|(_, property)| property.descriptor.display_name == *name)
                    .expect("author property")
                    .1
                    .static_value()
                    .to_channel_values();
                for (channel, (actual, original)) in
                    actual.iter().zip(default.to_channel_values()).enumerate()
                {
                    multiplier +=
                        (actual - original) * (index + 1) as f64 * (channel + 1) as f64 / 128.0;
                }
            }
            for (actual, original) in output.iter().zip(&input) {
                for channel in 0..4 {
                    assert!(
                        (actual[channel] - original[channel] * multiplier as f32).abs() < 1e-6,
                        "each precise author component must reach SDK pixel execution"
                    );
                }
            }
            output
        };
        let initial_pixels = execute(&state);
        assert_eq!(
            initial_pixels.format().alpha(),
            mondrian_core::ColorFrameAlpha::StraightCoverage
        );
        assert_eq!(initial_pixels.pixels(), &[[0.1, 0.2, 0.3, 0.05]; 4]);
        for (index, (_, original)) in defaults.iter().enumerate() {
            let before = execute(&state);
            let mut channels = original.to_channel_values();
            let last = channels.len() - 1;
            channels[last] += if index < 4 { 0.125 } else { -1.0 };
            let changed =
                PropertyValue::from_channel_values(original.value_type(), &channels, original);
            state
                .dispatch_action(visual_effect_set_parameter_value_action(
                    VisualEffectSetParameterValuePayload {
                        clip_id,
                        effect_id,
                        write: ParameterValueWrite::Value {
                            parameter: addresses[index].clone(),
                            value: changed.clone(),
                        },
                    },
                ))
                .expect("commit precise native author edit");
            let after = execute(&state);
            assert_ne!(after, before, "every tuple kind must change native output");
            assert!(state.undo_timeline().expect("undo tuple"));
            assert_eq!(execute(&state), before);
            assert!(state.redo_timeline().expect("redo tuple"));
            assert_eq!(execute(&state), after);
            let clip = state.active_sequence().expect("sequence").find_clip(clip_id).expect("clip");
            assert_eq!(
                clip.effects[0]
                    .properties
                    .iter()
                    .find(|(_, property)| property.descriptor.display_name == defaults[index].0)
                    .expect("edited tuple")
                    .1
                    .static_value(),
                &changed
            );
        }
        let before_save = execute(&state);
        let properties = state
            .active_sequence()
            .expect("sequence")
            .find_clip(clip_id)
            .expect("clip")
            .effects[0]
            .properties
            .clone();
        let workspace = tempfile::tempdir().expect("project folder");
        let project = workspace.path().join("复合参数🙂.mdp");
        state
            .save_project_file_as(project.clone())
            .expect("durably save actual project");
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(project).expect("reopen actual container");
        let clip = reopened
            .active_sequence()
            .expect("sequence")
            .find_clip(clip_id)
            .expect("reopened clip");
        assert_eq!(clip.effects[0].id, effect_id);
        assert_eq!(clip.effects[0].properties, properties);
        assert_eq!(execute(&reopened), before_save);
        assert_eq!(
            reopened.asset_library().expect("library").list_assets().expect("assets").len(),
            0
        );
        drop(reopened);
    }

    #[test]
    #[ignore = "set MONDRIAN_OPENFX_PRODUCT_BINARY, SESSION_PROBE_BINARY and probe mode strings_paths"]
    fn native_path_resource_draft_cancel_undo_save_reopen_preserves_output_pixels() {
        use crate::app_ui::resource_dialog::{
            InspectorPropertyTarget, ResourceActionOutcome, ResourceEditorAction, ResourceKind,
            ResourceUpdate,
        };
        use mondrian_core::{ParameterResourceReference, PropertyValue, TimelineTime};
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        assert_eq!(
            std::env::var("MONDRIAN_OPENFX_SESSION_PROBE_MODE").expect("mode"),
            "strings_paths"
        );
        let helper = PathBuf::from(
            std::env::var_os("MONDRIAN_OPENFX_PRODUCT_BINARY").expect("product worker"),
        );
        let binary =
            PathBuf::from(std::env::var_os("MONDRIAN_OPENFX_SESSION_PROBE_BINARY").expect("probe"));
        let inspection =
            crate::openfx_adapter::inspect_openfx_binary(&helper, &binary).expect("inspect");
        let kind = crate::openfx_effect::register_selected_openfx_filter(
            &helper,
            &inspection,
            "uk.co.thefoundry.BasicGainPlugin",
        )
        .expect("register native path controls");
        let temp = tempfile::tempdir().expect("workspace");
        let destination = temp.path().join("尚未创建").join("输出🙂.cube");
        let existing = temp.path().join("已有🙂.cube");
        std::fs::write(&existing, b"existing input").expect("existing file");
        let effect = mondrian_effects::instantiate_effect_node(kind).expect("native effect");
        for (name, kind, selection) in [
            (
                "existingFile",
                mondrian_core::ResourcePathKind::File,
                mondrian_core::ResourcePathSelection::Existing,
            ),
            (
                "newFile",
                mondrian_core::ResourcePathKind::File,
                mondrian_core::ResourcePathSelection::ExistingOrNew,
            ),
            (
                "existingDirectory",
                mondrian_core::ResourcePathKind::Directory,
                mondrian_core::ResourcePathSelection::Existing,
            ),
            (
                "newDirectory",
                mondrian_core::ResourcePathKind::Directory,
                mondrian_core::ResourcePathSelection::ExistingOrNew,
            ),
        ] {
            let schema = &effect
                .properties
                .iter()
                .find(|(_, p)| p.descriptor.display_name == name)
                .expect("native path")
                .1
                .descriptor
                .schema;
            let contract = schema.resource.as_ref().expect("Resource");
            assert_eq!(
                contract.allowed_kinds,
                vec![mondrian_core::ParameterResourceKind::ExternalFile]
            );
            let path = contract.path.expect("path");
            assert_eq!(path.kind, kind);
            assert_eq!(path.selection, selection);
            assert_eq!(
                path.max_utf8_bytes,
                Some(crate::openfx_host::OPENFX_MAX_STRING_BYTES)
            );
        }
        let effect_id = effect.id;
        let parameter = effect
            .properties
            .iter()
            .find(|(_, property)| property.descriptor.display_name == "newFile")
            .expect("destination")
            .1
            .address();
        let original = effect
            .properties
            .property_by_address(&parameter)
            .expect("path")
            .1
            .static_value()
            .clone();
        let mut clip = mondrian_timeline::Clip::new_basic_title(
            "Title",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("clip");
        let clip_id = clip.id;
        clip.add_effect_node(effect);
        let mut sequence = mondrian_timeline::Sequence::new("Native paths");
        sequence.video_tracks[0].add_clip(clip).expect("add");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.select_clip_by_id(clip_id).expect("select");
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let open = ResourceEditorAction::Open {
            clip_id,
            target: InspectorPropertyTarget::Effect { effect_id, parameter: parameter.clone() },
        };
        let route = |root: &mut AppUiAppRoot,
                     state: &AppState,
                     event: ResourceEditorAction,
                     platform: &FakePlatform| {
            match root
                .try_handle_resource_action(&event.action(), state, platform)
                .expect("route")
            {
                ResourceActionOutcome::Handled(action) => action,
                ResourceActionOutcome::Inspect(request) => {
                    let mut inspector =
                        crate::resource_inspection::PathInspectionService::new(helper.clone());
                    inspector.submit(request.ticket, request.path);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                    loop {
                        if let Some((ticket, outcome)) = inspector.poll() {
                            break root.finish_resource_inspection(ticket, outcome, state);
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "actual resource inspector did not finish"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                }
                ResourceActionOutcome::Unhandled => panic!("typed path event"),
            }
        };
        let picker = FakePlatform {
            save_path: Some(temp.path().join("wrong-save-picker.cube")),
            open_paths: Some(vec![existing.clone()]),
            ..FakePlatform::default()
        };
        let revision = state.active_sequence().expect("sequence").revision;
        assert!(route(&mut root, &state, open.clone(), &picker).is_none());
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Browse { id, folder: false },
            &picker
        )
        .is_none());
        assert!(picker.dialog_default_names.lock().expect("names").is_empty());
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Browse { id, folder: false },
            &FakePlatform::default()
        )
        .is_none());
        assert_eq!(
            root.modal
                .as_ref()
                .and_then(ShellModal::as_resource)
                .expect("draft")
                .draft
                .value()
                .expect("selected"),
            ParameterResourceReference::ExternalFile { path: existing.clone().into() }
        );
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Text(destination.to_str().expect("UTF-8").to_owned())
            },
            &picker
        )
        .is_none());
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Cancel { id },
            &picker
        )
        .is_none());
        assert!(!state.can_undo_action());
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            revision
        );
        assert!(!destination.exists());
        assert!(!destination.parent().expect("parent").exists());
        assert!(route(&mut root, &state, open, &picker).is_none());
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Kind(ResourceKind::ExternalFile)
            },
            &picker
        )
        .is_none());
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Browse { id, folder: false },
            &picker
        )
        .is_none());
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Text(destination.to_str().expect("UTF-8").to_owned())
            },
            &picker
        )
        .is_none());
        let action = route(
            &mut root,
            &state,
            ResourceEditorAction::Confirm { id },
            &picker,
        )
        .expect("one author action");
        state.dispatch_action(action).expect("atomic commit");
        let resource = |state: &AppState| {
            state
                .active_sequence()
                .expect("sequence")
                .find_clip(clip_id)
                .expect("clip")
                .effects
                .iter()
                .find(|effect| effect.id == effect_id)
                .expect("effect")
                .properties
                .property_by_address(&parameter)
                .expect("path")
                .1
                .static_value()
                .clone()
        };
        let bound = PropertyValue::Resource(ParameterResourceReference::ExternalFile {
            path: destination.clone().into(),
        });
        assert_eq!(resource(&state), bound);
        assert!(state.undo_timeline().expect("undo"));
        assert_eq!(resource(&state), original);
        assert!(!state.can_undo_action());
        assert!(state.redo_timeline().expect("redo"));
        assert_eq!(resource(&state), bound);
        let execute = |state: &AppState| {
            use mondrian_core::{
                Rational, SampleAspectRatio, TimelineTimeRange, WorkingColorSpace,
            };
            use mondrian_effects::{
                EffectFrameContext, LutPreparationCache, PreparedEffectProgram,
            };
            let clip = state.active_sequence().expect("sequence").find_clip(clip_id).expect("clip");
            let bytes: usize = clip.effects[0]
                .properties
                .iter()
                .map(|(_, p)| match p.static_value() {
                    PropertyValue::Text(text) => text.len(),
                    PropertyValue::Resource(ParameterResourceReference::ExternalFile { path }) => {
                        path.utf8_text().expect("UTF-8").len()
                    }
                    _ => 0,
                })
                .sum();
            let range =
                TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::new(1, 1).expect("end"))
                    .expect("range");
            let context =
                EffectFrameContext::new(Rational::FPS_24, SampleAspectRatio::SQUARE, range)
                    .expect("context");
            let program = PreparedEffectProgram::prepare_hierarchical_with_frame_context(
                &clip.effects,
                &[],
                &[],
                &[],
                WorkingColorSpace::LinearRec709,
                context,
                &LutPreparationCache::uncached(),
            )
            .expect("compile native paths");
            let graph = program.evaluate(TimelineTime::ZERO).expect("evaluate");
            let input = vec![[0.1, 0.2, 0.3, 0.05]; 4];
            let pixels = mondrian_effects::apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(2, 2),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    &input,
                ),
                &graph,
                1193,
            )
            .expect("actual native pixels");
            let gain = 1.0 + (bytes % 11) as f32 / 10.0;
            for (actual, original) in pixels.iter().zip(&input) {
                for channel in 0..4 {
                    assert!((actual[channel] - original[channel] * gain).abs() < 1e-6);
                }
            }
            pixels
        };
        let before = execute(&state);
        assert_eq!(
            state.asset_library().expect("library").list_assets().expect("assets").len(),
            0
        );
        let project = temp.path().join("验收.mdp");
        state.save_project_file_as(project.clone()).expect("durable save");
        drop(root);
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(project).expect("reopen");
        assert_eq!(resource(&reopened), bound);
        assert_eq!(execute(&reopened), before);
        assert_eq!(
            std::fs::read(&existing).expect("existing input"),
            b"existing input"
        );
        reopened.select_clip_by_id(clip_id).expect("select reopened clip");
        let mut root = AppUiAppRoot::from_app_state(&reopened);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let open = ResourceEditorAction::Open {
            clip_id,
            target: InspectorPropertyTarget::Effect { effect_id, parameter: parameter.clone() },
        };
        assert!(route(&mut root, &reopened, open, &picker).is_none());
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        assert!(route(
            &mut root,
            &reopened,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Kind(ResourceKind::Unbound)
            },
            &picker
        )
        .is_none());
        let action = route(
            &mut root,
            &reopened,
            ResourceEditorAction::Confirm { id },
            &picker,
        )
        .expect("clear author action");
        reopened.dispatch_action(action).expect("clear");
        assert_eq!(
            resource(&reopened),
            PropertyValue::Resource(ParameterResourceReference::Unbound)
        );
        execute(&reopened);
        assert!(reopened.undo_timeline().expect("undo clear"));
        assert_eq!(resource(&reopened), bound);
        assert_eq!(execute(&reopened), before);
        assert!(!destination.exists());
        assert!(!destination.parent().expect("parent").exists());
    }

    #[test]
    fn resource_lut_draft_commit_undo_save_reopen_executes_real_cube() {
        use crate::app_ui::resource_dialog::{
            InspectorPropertyTarget, ResourceActionOutcome, ResourceEditorAction, ResourceUpdate,
        };
        use mondrian_core::{
            ParameterResourceReference, PropertyValue, TimelineTime, WorkingColorSpace,
        };
        use mondrian_effects::EffectType;
        use mondrian_ui_core::{FocusManager, TreeWalker, WidgetTree};
        use mondrian_ui_events::EventRouter;
        let _guard = crate::app_ui::test_utils::theme_test_guard();
        let directory = std::env::temp_dir().join(format!(
            "mondrian-resource-workflow-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).expect("test directory");
        let cube = directory.join("蓝色 映射.cube");
        std::fs::write(
            &cube,
            "LUT_3D_SIZE 2\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n",
        )
        .expect("real cube file");
        let mut sequence = mondrian_timeline::Sequence::new("Resource acceptance");
        let mut clip = mondrian_timeline::Clip::new_basic_title(
            "Title",
            "Arial",
            TimelineTime::ZERO,
            TimelineTime::new(1, 1).expect("duration"),
        )
        .expect("generated clip");
        let clip_id = clip.id;
        let mut effect = mondrian_effects::instantiate_effect_node(EffectType::Lut3D).expect("LUT");
        let effect_id = effect.id;
        effect
            .set_static_value_by_parameter(
                &EffectType::Lut3D.parameter_id("processing_space").expect("space"),
                PropertyValue::Enum("scene_linear".to_owned()),
            )
            .expect("explicit color space");
        let path = EffectType::Lut3D.property_path("path");
        let parameter = effect.properties.property(&path).expect("path property").address();
        clip.add_effect_node(effect);
        sequence.video_tracks[0].add_clip(clip).expect("add generated clip");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        state.select_clip_by_id(clip_id).expect("select clip");
        let mut root = AppUiAppRoot::from_app_state(&state);
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let background = {
            let tree = WidgetTreeView::new(&mut root);
            TreeWalker::focus_next(&tree, WidgetId::default()).expect("background focus")
        };
        let mut router = EventRouter::new(root.id());
        router.focus_manager_mut().set_focused_widget(Some(background), None);
        let target = InspectorPropertyTarget::Effect { effect_id, parameter: parameter.clone() };
        let platform = FakePlatform {
            open_paths: Some(vec![cube.clone()]),
            ..FakePlatform::default()
        };
        let open = ResourceEditorAction::Open { clip_id, target: target.clone() };
        let route =
            |root: &mut AppUiAppRoot, state: &AppState, event: ResourceEditorAction| match root
                .try_handle_resource_action(&event.action(), state, &platform)
                .expect("shell resource event")
            {
                ResourceActionOutcome::Handled(action) => action,
                ResourceActionOutcome::Inspect(_) => {
                    panic!("reference-only LUT paths do not inspect the filesystem")
                }
                ResourceActionOutcome::Unhandled => panic!("typed resource event must be handled"),
            };
        let revision = state.active_sequence().expect("sequence").revision;
        assert!(route(&mut root, &state, open.clone()).is_none());
        let cancelled =
            root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id: cancelled,
                update: ResourceUpdate::Kind(
                    crate::app_ui::resource_dialog::ResourceKind::ExternalFile
                )
            }
        )
        .is_none());
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Browse { id: cancelled, folder: false }
        )
        .is_none());
        let scope = root.keyboard_focus_scope().expect("resource modal scope");
        let keyboard_actions = std::cell::RefCell::new(Vec::new());
        let dispatch = |action| keyboard_actions.borrow_mut().push(action);
        {
            let mut tree = WidgetTreeView::new(&mut root);
            let mut visited = std::collections::HashSet::new();
            for modifiers in [Modifiers::none(), Modifiers::shift()] {
                for _ in 0..24 {
                    assert_eq!(
                        router.route(
                            UiEvent::KeyDown { key: KeyCode::Tab, modifiers },
                            &mut tree,
                            &dispatch,
                        ),
                        EventResult::Handled
                    );
                    let focused = router.focus_manager().focused_widget().expect("modal focus");
                    visited.insert(focused);
                    let mut ancestor = Some(focused);
                    while ancestor.is_some() && ancestor != Some(scope) {
                        ancestor = ancestor.and_then(|id| tree.parent_id(id));
                    }
                    assert_eq!(ancestor, Some(scope), "resource Tab escaped the modal");
                    assert_ne!(focused, background);
                }
            }
            assert!(
                visited.len() >= 4,
                "Tab must reach input, browse and confirmation controls"
            );
        }
        assert!(
            keyboard_actions.borrow().is_empty(),
            "Tab must not confirm or edit a resource"
        );
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Cancel { id: cancelled }
        )
        .is_none());
        {
            let mut tree = WidgetTreeView::new(&mut root);
            router.route(
                UiEvent::KeyUp { key: KeyCode::Tab, modifiers: Modifiers::none() },
                &mut tree,
                &dispatch,
            );
        }
        assert_eq!(router.focus_manager().focused_widget(), Some(background));
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            revision
        );
        assert!(!state.can_undo_action());
        assert!(route(&mut root, &state, open.clone()).is_none());
        let unchanged =
            root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Confirm { id: unchanged }
        )
        .is_none());
        assert!(!state.can_undo_action());
        assert!(route(&mut root, &state, open.clone()).is_none());
        let id = root.modal.as_ref().and_then(ShellModal::as_resource).expect("draft").draft.id;
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Cancel { id: cancelled }
        )
        .is_none());
        assert!(
            root.modal.is_some(),
            "stale cancellation cannot close a newer draft"
        );
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Kind(
                    crate::app_ui::resource_dialog::ResourceKind::ExternalFile
                )
            }
        )
        .is_none());
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Browse { id, folder: false }
        )
        .is_none());
        let selected = root
            .modal
            .as_ref()
            .and_then(ShellModal::as_resource)
            .expect("draft")
            .draft
            .value()
            .expect("selected cube");
        for picker in [
            FakePlatform::default(),
            FakePlatform { dialog_error: true, ..FakePlatform::default() },
            FakePlatform {
                open_paths: Some(vec![cube.clone(), cube.clone()]),
                ..FakePlatform::default()
            },
        ] {
            assert!(matches!(
                root.try_handle_resource_action(
                    &ResourceEditorAction::Browse { id, folder: false }.action(),
                    &state,
                    &picker
                )
                .expect("picker result"),
                ResourceActionOutcome::Handled(None)
            ));
            assert_eq!(
                root.modal
                    .as_ref()
                    .and_then(ShellModal::as_resource)
                    .expect("preserved draft")
                    .draft
                    .value()
                    .expect("preserved value"),
                selected
            );
            let filters = picker.dialog_filters.lock().expect("filters");
            assert_eq!(filters[0][0].extensions, vec!["cube".to_owned()]);
        }
        let folder_picker = FakePlatform {
            open_folder: Some(directory.clone()),
            ..FakePlatform::default()
        };
        assert!(matches!(
            root.try_handle_resource_action(
                &ResourceEditorAction::Browse { id, folder: true }.action(),
                &state,
                &folder_picker
            )
            .expect("disallowed folder"),
            ResourceActionOutcome::Handled(None)
        ));
        assert!(
            folder_picker.dialog_titles.lock().expect("titles").is_empty(),
            "File-only schema must not open a folder picker"
        );
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Text("wrong.txt".to_owned())
            }
        )
        .is_none());
        assert!(route(&mut root, &state, ResourceEditorAction::Confirm { id }).is_none());
        assert!(
            root.modal.is_some(),
            "invalid confirmation retains the recoverable draft"
        );
        assert!(route(
            &mut root,
            &state,
            ResourceEditorAction::Update {
                id,
                update: ResourceUpdate::Text(cube.to_string_lossy().into_owned())
            }
        )
        .is_none());
        assert_eq!(
            state.active_sequence().expect("sequence").revision,
            revision
        );
        let action = route(&mut root, &state, ResourceEditorAction::Confirm { id })
            .expect("one author action");
        assert!(root.modal.is_none());
        state.dispatch_action(action).expect("atomic author commit");
        let resource = |state: &AppState| {
            state
                .active_sequence()
                .expect("sequence")
                .find_clip(clip_id)
                .expect("clip")
                .effects
                .iter()
                .find(|effect| effect.id == effect_id)
                .expect("effect")
                .properties
                .property_by_address(&parameter)
                .expect("path")
                .1
                .static_value()
                .clone()
        };
        let bound = PropertyValue::Resource(ParameterResourceReference::ExternalFile {
            path: cube.clone().into(),
        });
        assert_eq!(resource(&state), bound);
        assert!(state.undo_timeline().expect("undo binding"));
        assert_eq!(
            resource(&state),
            PropertyValue::Resource(ParameterResourceReference::Unbound)
        );
        assert!(
            !state.can_undo_action(),
            "the confirmed binding is exactly one history entry"
        );
        assert!(state.redo_timeline().expect("redo binding"));
        assert_eq!(resource(&state), bound);
        let execute_lut = |state: &AppState| {
            let sequence = state.active_sequence().expect("sequence");
            let clip = sequence.find_clip(clip_id).expect("clip");
            let graph = mondrian_effects::compile_clip_effect_graph(
                &clip.effects,
                &[],
                TimelineTime::ZERO,
                WorkingColorSpace::LinearRec709,
            )
            .expect("compile authored LUT");
            mondrian_effects::apply_compiled_effect_graph_rgba_f32(
                mondrian_effects::EffectFloatFrameView::new(
                    mondrian_effects::EffectFrameExtent::new(3, 2),
                    mondrian_effects::EffectValueFormat::new(
                        mondrian_effects::EffectWorkingPrecision::Float32,
                        mondrian_effects::EffectColorDomain::SceneLinearRgb,
                        mondrian_core::ColorFrameAlpha::StraightCoverage,
                    ),
                    &[
                        [1.0, 0.0, 0.0, 0.375],
                        [0.125, 0.25, 0.75, 1.0],
                        [0.75, 0.5, 0.125, 0.0],
                        [0.125, 0.25, 0.75, 1.0 / 65_535.0],
                        [1.0, 1.0, 1.0, 1.0],
                        [0.5, 0.5, 0.5, 0.25],
                    ],
                ),
                &graph,
                0,
            )
            .expect("execute real LUT pixels")
        };
        let before_save = execute_lut(&state);
        assert_eq!(
            before_save.format().alpha(),
            mondrian_core::ColorFrameAlpha::StraightCoverage
        );
        assert_eq!(
            before_save.pixels(),
            &[
                [0.0, 0.0, 1.0, 0.375],
                [0.75, 0.25, 0.125, 1.0],
                [0.75, 0.5, 0.125, 0.0],
                [0.75, 0.25, 0.125, 1.0 / 65_535.0],
                [1.0, 1.0, 1.0, 1.0],
                [0.5, 0.5, 0.5, 0.25],
            ],
            "LUT must transform every positive-alpha sample, preserve alpha and retain zero-alpha hidden RGB"
        );
        assert_eq!(
            state.asset_library().expect("library").list_assets().expect("assets").len(),
            0,
            "binding a LUT must not import library media"
        );
        assert!(state.undo_timeline().expect("Undo before save"));
        let assert_unbound_lut = |state: &AppState| {
            let clip = state.active_sequence().expect("sequence").find_clip(clip_id).expect("clip");
            let error = mondrian_effects::compile_clip_effect_graph(
                &clip.effects,
                &[],
                TimelineTime::ZERO,
                WorkingColorSpace::LinearRec709,
            )
            .expect_err("an unbound LUT must not reuse the previous compiled resource");
            assert!(
                matches!(error, mondrian_effects::EffectGraphBuildError::ResourceUnavailable {
                effect_id: owner, parameter_id: ref parameter, recovery: mondrian_effects::EffectResourceRecovery::AuthorEdit, ..
            } if owner == effect_id && parameter == &EffectType::Lut3D.parameter_id("path").expect("path parameter"))
            );
        };
        assert_unbound_lut(&state);
        let undone_project = directory.join("撤销状态.mdp");
        state
            .save_project_file_as(undone_project.clone())
            .expect("save undone author state");
        assert!(
            state.can_redo_action(),
            "saving must preserve the live Redo branch"
        );
        let mut undone_reopened = AppState::new();
        undone_reopened
            .open_project_file(undone_project)
            .expect("reopen undone snapshot");
        assert_eq!(
            resource(&undone_reopened),
            PropertyValue::Resource(ParameterResourceReference::Unbound)
        );
        assert_unbound_lut(&undone_reopened);
        assert!(!undone_reopened.can_undo_action());
        assert!(
            !undone_reopened.can_redo_action(),
            "a fresh Session must not resurrect old history"
        );
        drop(undone_reopened);
        assert!(state.redo_timeline().expect("Redo after durable save"));
        assert_eq!(
            execute_lut(&state),
            before_save,
            "Redo must restore the resource and exact pixels after saving an undone snapshot"
        );
        root.refresh_from_app_state(&state);
        assert!(route(&mut root, &state, open).is_none());
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked = true;
        root.refresh_from_app_state(&state);
        assert!(root.modal.is_none(), "locking the owner cancels the draft");
        state.active_sequence_mut_uncommitted().expect("sequence").video_tracks[0].is_locked =
            false;
        let project = directory.join("验收.mdp");
        state.save_project_file_as(project.clone()).expect("durable save");
        drop(root);
        drop(state);
        let mut reopened = AppState::new();
        reopened.open_project_file(project).expect("reopen actual container");
        assert_eq!(resource(&reopened), bound);
        assert_eq!(
            execute_lut(&reopened),
            before_save,
            "durable reopen must preserve exact output"
        );
        let clip = reopened
            .active_sequence()
            .expect("reopened sequence")
            .find_clip(clip_id)
            .expect("persisted clip");
        let graph = mondrian_effects::compile_clip_effect_graph(
            &clip.effects,
            &[],
            TimelineTime::ZERO,
            WorkingColorSpace::LinearRec709,
        )
        .expect("compile persisted LUT");
        let output = mondrian_effects::apply_compiled_effect_graph_rgba_f32(
            mondrian_effects::EffectFloatFrameView::new(
                mondrian_effects::EffectFrameExtent::new(1, 1),
                mondrian_effects::EffectValueFormat::new(
                    mondrian_effects::EffectWorkingPrecision::Float32,
                    mondrian_effects::EffectColorDomain::SceneLinearRgb,
                    mondrian_core::ColorFrameAlpha::StraightCoverage,
                ),
                &[[1.0, 0.0, 0.0, 0.375]],
            ),
            &graph,
            0,
        )
        .expect("execute compiled LUT");
        assert_eq!(
            output.pixels(),
            vec![[0.0, 0.0, 1.0, 0.375]],
            "real cube must swap red and blue without changing alpha"
        );
        drop(reopened);
        std::fs::remove_dir_all(&directory).expect("remove test-owned directory");
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
    fn clap_install_picker_cancels_cleanly_and_uses_first_selected_library() {
        let request = app_shell_install_clap_library_dialog_action();
        assert!(
            resolve_app_shell_action(request.clone(), &FakePlatform::default(), None).is_none()
        );
        let path = PathBuf::from("E:/plugins/gain.clap");
        let platform = FakePlatform {
            open_paths: Some(vec![path.clone(), PathBuf::from("E:/plugins/other.clap")]),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(request, &platform, None).expect("selected library");
        assert_eq!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(
                AudioProductAction::InstallClapLibrary(AudioInstallClapLibraryPayload { path })
            ))
        );
    }

    #[test]
    fn vst3_install_picker_cancels_cleanly_and_uses_first_selected_binary() {
        let request = app_shell_install_vst3_binary_dialog_action();
        assert!(
            resolve_app_shell_action(request.clone(), &FakePlatform::default(), None).is_none()
        );
        let path = PathBuf::from("E:/plugins/gain.vst3");
        let platform = FakePlatform {
            open_paths: Some(vec![path.clone(), PathBuf::from("E:/plugins/other.vst3")]),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(request, &platform, None).expect("selected binary");
        assert_eq!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::InstallVst3Plugin(
                AudioInstallVst3PluginPayload { path }
            )))
        );
    }

    #[test]
    fn vst3_bundle_picker_cancels_cleanly_and_returns_selected_directory() {
        let request = app_shell_install_vst3_bundle_dialog_action();
        assert!(
            resolve_app_shell_action(request.clone(), &FakePlatform::default(), None).is_none()
        );
        let path = PathBuf::from("E:/plugins/Gain.vst3");
        let platform = FakePlatform {
            open_folder: Some(path.clone()),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(request, &platform, None).expect("selected bundle");
        assert_eq!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::InstallVst3Plugin(
                AudioInstallVst3PluginPayload { path }
            )))
        );
    }

    #[test]
    fn openfx_bundle_picker_cancels_without_installing_and_keeps_selected_directory() {
        let request = app_shell_install_openfx_bundle_dialog_action();
        assert!(
            resolve_app_shell_action(request.clone(), &FakePlatform::default(), None).is_none()
        );
        let path = PathBuf::from("E:/plugins/Basic.ofx.bundle");
        let platform = FakePlatform {
            open_folder: Some(path.clone()),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(request, &platform, None).expect("selected bundle");
        assert_eq!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::VisualEffect(
                VisualEffectProductAction::InstallOpenFxBundle(
                    VisualEffectInstallOpenFxBundlePayload { path },
                ),
            ))
        );
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
        assert_eq!(namespace, ASSET_NAMESPACE);
        assert_eq!(name, ASSET_IMPORT_FILES);
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
        assert_eq!(namespace, ASSET_NAMESPACE);
        assert_eq!(name, ASSET_RELINK);
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
            &Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog"),
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
    fn resolve_app_shell_package_dialog_returns_export_action() {
        let platform = FakePlatform {
            save_path: Some(PathBuf::from("E:/projects/share.mdpkg")),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(
            app_shell_export_portable_package_dialog_action(),
            &platform,
            Some(Path::new("E:/projects/current.mdp")),
        );
        assert_eq!(
            action,
            Some(app_shell_export_portable_package_action(PathBuf::from(
                "E:/projects/share.mdpkg"
            )))
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
        assert_eq!(name, EXPORT_EDIT_DRAFT);
        let payload: ExportDraftEdit =
            serde_json::from_value(payload).expect("export draft payload");
        assert_eq!(
            payload,
            ExportDraftEdit::OutputPath(
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
    fn ancillary_dialog_yields_typed_import_and_cancellation_keeps_the_draft() {
        let path = PathBuf::from("E:/broadcast/canonical.json");
        let platform = FakePlatform {
            open_paths: Some(vec![path.clone()]),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(
            crate::app::ui_actions::app_shell_import_export_ancillary_dialog_action(),
            &platform,
            None,
        )
        .expect("selection");
        assert_eq!(
            action,
            export_edit_draft_action(ExportDraftEdit::ImportAncillary(path))
        );
        assert_eq!(
            resolve_app_shell_action(
                crate::app::ui_actions::app_shell_import_export_ancillary_dialog_action(),
                &FakePlatform::default(),
                None
            ),
            None
        );
    }

    #[test]
    fn regulatory_pse_dialog_yields_typed_import_and_retains_cancellation() {
        let path = PathBuf::from("E:/broadcast/pse.json");
        let platform = FakePlatform {
            open_paths: Some(vec![path.clone()]),
            ..FakePlatform::default()
        };
        let action = resolve_app_shell_action(
            crate::app::ui_actions::app_shell_import_export_pse_dialog_action(),
            &platform,
            None,
        )
        .expect("selection");
        assert_eq!(
            action,
            export_edit_draft_action(ExportDraftEdit::ImportRegulatoryPse(path))
        );
        assert_eq!(
            resolve_app_shell_action(
                crate::app::ui_actions::app_shell_import_export_pse_dialog_action(),
                &FakePlatform::default(),
                None
            ),
            None
        );
    }

    #[test]
    fn try_resolve_app_shell_dialog_unavailable_is_not_user_cancel() {
        let error = try_resolve_app_shell_action(
            app_shell_open_project_dialog_action(),
            &mondrian_platform::NoopPlatformService,
            None,
            &Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog"),
        )
        .expect_err("unavailable native dialog must remain an actionable failure");

        assert!(error.to_string().contains("native file dialog is unavailable"));
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
            &Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog"),
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
            pixels_per_point: 1.0,
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
            pixels_per_point: 1.0,
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
                grid.event(&UiEvent::focus_gained_keyboard(), &mut ctx);
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
        state.test_set_sequence(Some(Sequence::new("edit")));
        let platform = FakePlatform::default();
        let preferences = AppUiPreferences {
            locale_preference: crate::app_ui::localization::AppUiLocalePreference::ZhCn,
            ..AppUiPreferences::default()
        };
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
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
        state.test_set_sequence(Some(Sequence::new("edit")));
        let platform = FakePlatform::default();
        let preferences = AppUiPreferences {
            locale_preference: crate::app_ui::localization::AppUiLocalePreference::ZhCn,
            ..AppUiPreferences::default()
        };
        let mut root = AppUiAppRoot::from_app_state_with_preferences(&state, &preferences);
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
