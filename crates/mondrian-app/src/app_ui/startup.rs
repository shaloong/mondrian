//! App UI startup surface.
//!
//! The startup surface is separate from the editor workspace. It owns only
//! launch-time presentation and emits shell actions; project lifecycle work
//! stays in `AppUiHost` / `AppState`.

#[cfg(test)]
use mondrian_core::ProjectId;
use mondrian_core::{Color, MondrianError, Result};
use mondrian_editor_state::Action;
use mondrian_platform::PlatformService;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_widgets::{RasterImage, VectorIcon};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::app::ui_actions::{
    app_shell_new_project_dialog_action, app_shell_open_project_dialog_action,
    app_shell_open_recent_project_action, app_shell_quit_action, app_shell_recover_project_action,
    app_shell_recovery_dialog_action, app_shell_window_drag_action,
    project_create_with_settings_action, AppShellOpenRecentProjectPayload,
    NewProjectDraftUpdatePayload, ProjectRecoverFromAutosavePayload,
    APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_CONFIRM_RECOVERY_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, APP_SHELL_RECOVERY_DIALOG,
    APP_SHELL_SELECT_CUSTOM_OCIO_CONFIG,
};
use crate::app::CrashRecoveryCandidate;
use crate::app_ui::icons::AppIcon;
use crate::app_ui::modal::ShellModal;
use crate::app_ui::new_project_dialog::{default_project_file_name, AppUiNewProjectDraft};
use crate::app_ui::recovery_dialog::RecoveryConfirmationModel;
use crate::app_ui::shell::project_file_filters;

/// Startup window logical size used by the app UI product entrypoint.
pub const STARTUP_WINDOW_WIDTH: f32 = 784.0;
/// Startup window logical size used by the app UI product entrypoint.
pub const STARTUP_WINDOW_HEIGHT: f32 = 464.0;

const LEFT_WIDTH: f32 = 300.0;
const CONTENT_PAD_X: f32 = 28.0;
const CONTENT_PAD_Y: f32 = 46.0;
const CLOSE_SIZE: f32 = 28.0;
const CLOSE_MARGIN: f32 = 10.0;
const ACTION_BUTTON_HEIGHT: f32 = 32.0;
const ACTION_BUTTON_WIDTH: f32 = 118.0;
const ACTION_BUTTON_GAP: f32 = 10.0;
const RECENT_ROW_HEIGHT: f32 = 52.0;
const RECENT_ROW_GAP: f32 = 10.0;
const SECTION_LABEL_TO_ROW_GAP: f32 = 30.0;
const MAX_VISIBLE_RECENT_PROJECTS: usize = 5;
const MAX_VISIBLE_RECENT_PROJECTS_WITH_RECOVERY: usize = 2;
const MAX_VISIBLE_RECOVERY_PROJECTS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupHit {
    NewProject,
    OpenProject,
    RecentProject(usize),
    RecoveryProject(usize),
    Close,
    DragSurface,
}

/// One project row shown on the app UI startup surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupRecentProject {
    /// Project file opened when the row is activated.
    pub project_file: PathBuf,
    /// Primary row label.
    pub title: String,
    /// Secondary row label, typically the parent directory.
    pub subtitle: String,
}

/// One autosave recovery row shown on the app UI startup surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupRecoveryProject {
    /// Exact recovery candidate revalidated when the row is activated.
    pub candidate: CrashRecoveryCandidate,
    /// Primary row label.
    pub title: String,
    /// Secondary row label with age/snapshot metadata.
    pub detail: String,
}

/// Startup screen shown before a project is opened.
pub struct AppUiStartupScreen {
    id: WidgetId,
    bounds: Rect,
    panel_rect: Rect,
    left_rect: Rect,
    right_rect: Rect,
    new_project_rect: Rect,
    open_project_rect: Rect,
    recovery_rects: Vec<Rect>,
    recovery_projects: Vec<StartupRecoveryProject>,
    recent_rects: Vec<Rect>,
    recent_projects: Vec<StartupRecentProject>,
    modal: Option<ShellModal>,
    close_rect: Rect,
    hover: Option<StartupHit>,
    pressed: Option<StartupHit>,
}

impl AppUiStartupScreen {
    /// Build the default startup surface.
    pub fn new() -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            panel_rect: Rect::ZERO,
            left_rect: Rect::ZERO,
            right_rect: Rect::ZERO,
            new_project_rect: Rect::ZERO,
            open_project_rect: Rect::ZERO,
            recovery_rects: Vec::new(),
            recovery_projects: Vec::new(),
            recent_rects: Vec::new(),
            recent_projects: Vec::new(),
            modal: None,
            close_rect: Rect::ZERO,
            hover: None,
            pressed: None,
        }
    }

    /// Replace startup autosave recovery rows.
    pub fn set_recovery_projects(&mut self, recovery_projects: Vec<StartupRecoveryProject>) {
        self.recovery_projects = recovery_projects;
        self.recovery_rects.clear();
        self.hover = None;
        self.pressed = None;
    }

    /// Number of autosave recovery rows currently shown.
    pub fn recovery_project_count(&self) -> usize {
        self.recovery_projects.len()
    }

    /// Whether a shell-local startup modal is currently open.
    pub fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    /// Apply a startup-local shell action and return an editor action when the
    /// modal flow completes.
    pub fn try_handle_shell_action(
        &mut self,
        action: Action,
        platform: &dyn PlatformService,
    ) -> Result<Option<Action>> {
        match action {
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
                let update: NewProjectDraftUpdatePayload = serde_json::from_value(payload)
                    .map_err(|err| {
                        startup_shell_action_error(APP_SHELL_NEW_PROJECT_DRAFT_CHANGED, err)
                    })?;
                if let Some(dialog) = self.modal.as_mut().and_then(ShellModal::as_new_project_mut) {
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
                }
                Ok(None)
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_RECOVERY_DIALOG =>
            {
                let payload: ProjectRecoverFromAutosavePayload = serde_json::from_value(payload)
                    .map_err(|err| startup_shell_action_error(APP_SHELL_RECOVERY_DIALOG, err))?;
                self.modal = Some(ShellModal::recovery(
                    RecoveryConfirmationModel::from_candidate(payload.candidate),
                ));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                Ok(None)
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && (name == APP_SHELL_CANCEL_NEW_PROJECT_DIALOG
                        || name == APP_SHELL_CLOSE_MODAL) =>
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
                        "Create Mondrian Project",
                        &default_project_file_name(&draft.name),
                        &project_file_filters(),
                    )
                    .map_err(|error| MondrianError::WorkflowStepFailed {
                        step_id: "startup.create_project_dialog".to_owned(),
                        reason: error.to_string(),
                    })?
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
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CONFIRM_RECOVERY_DIALOG =>
            {
                let Some(candidate) = self
                    .modal
                    .as_ref()
                    .and_then(ShellModal::as_recovery)
                    .map(|dialog| dialog.candidate().clone())
                else {
                    return Ok(None);
                };
                self.modal = None;
                Ok(Some(app_shell_recover_project_action(
                    ProjectRecoverFromAutosavePayload { candidate },
                )))
            }
            action => Ok(Some(action)),
        }
    }

    /// Replace startup recent-project rows.
    pub fn set_recent_projects(&mut self, recent_projects: Vec<StartupRecentProject>) {
        self.recent_projects = recent_projects;
        self.recent_rects.clear();
        self.hover = None;
        self.pressed = None;
    }

    /// Number of recent project rows currently shown.
    pub fn recent_project_count(&self) -> usize {
        self.recent_projects.len()
    }

    fn hit_region(&self, point: Point) -> Option<StartupHit> {
        if self.close_rect.contains(point) {
            Some(StartupHit::Close)
        } else if self.new_project_rect.contains(point) {
            Some(StartupHit::NewProject)
        } else if self.open_project_rect.contains(point) {
            Some(StartupHit::OpenProject)
        } else if let Some((index, _)) =
            self.recovery_rects.iter().enumerate().find(|(_, rect)| rect.contains(point))
        {
            Some(StartupHit::RecoveryProject(index))
        } else if let Some((index, _)) =
            self.recent_rects.iter().enumerate().find(|(_, rect)| rect.contains(point))
        {
            Some(StartupHit::RecentProject(index))
        } else if self.panel_rect.contains(point) {
            Some(StartupHit::DragSurface)
        } else {
            None
        }
    }

    fn dispatch_hit(&self, hit: StartupHit, ctx: &mut EventContext) {
        let action = match hit {
            StartupHit::NewProject => app_shell_new_project_dialog_action(),
            StartupHit::OpenProject => app_shell_open_project_dialog_action(),
            StartupHit::RecentProject(index) => {
                let Some(project) = self.recent_projects.get(index) else {
                    return;
                };
                app_shell_open_recent_project_action(AppShellOpenRecentProjectPayload {
                    project_file: project.project_file.clone(),
                })
            }
            StartupHit::RecoveryProject(index) => {
                let Some(project) = self.recovery_projects.get(index) else {
                    return;
                };
                app_shell_recovery_dialog_action(ProjectRecoverFromAutosavePayload {
                    candidate: project.candidate.clone(),
                })
            }
            StartupHit::Close => app_shell_quit_action(),
            StartupHit::DragSurface => app_shell_window_drag_action(),
        };
        (ctx.dispatch)(action);
    }

    fn button_color(&self, hit: StartupHit, primary: bool, ctx: &PaintContext) -> Color {
        let colors = &ctx.theme.colors;
        if self.pressed == Some(hit) {
            return colors.muted;
        }
        if primary {
            if self.hover == Some(hit) {
                colors.foreground
            } else {
                startup_alpha(colors.foreground, 0.88)
            }
        } else if self.hover == Some(hit) {
            colors.muted
        } else {
            colors.card
        }
    }
}

impl Default for AppUiStartupScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for AppUiStartupScreen {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.panel_rect = bounds;

        let left_width = LEFT_WIDTH.min(self.panel_rect.width * 0.45);
        self.left_rect = Rect::new(
            self.panel_rect.x,
            self.panel_rect.y,
            left_width,
            self.panel_rect.height,
        );
        self.right_rect = Rect::new(
            self.left_rect.x + self.left_rect.width,
            self.panel_rect.y,
            (self.panel_rect.width - self.left_rect.width).max(0.0),
            self.panel_rect.height,
        );

        self.close_rect = Rect::new(
            self.panel_rect.x + self.panel_rect.width - CLOSE_MARGIN - CLOSE_SIZE,
            self.panel_rect.y + CLOSE_MARGIN,
            CLOSE_SIZE,
            CLOSE_SIZE,
        );

        let content_x = self.right_rect.x + CONTENT_PAD_X;
        let content_width = (self.right_rect.width - CONTENT_PAD_X * 2.0).max(0.0);
        let action_y = self.right_rect.y + CONTENT_PAD_Y - 3.0;
        let available_button_width = ((content_width - ACTION_BUTTON_GAP) * 0.5).max(0.0);
        let button_width = ACTION_BUTTON_WIDTH.min(available_button_width);
        let button_group_width = button_width * 2.0 + ACTION_BUTTON_GAP;
        let button_x = content_x + (content_width - button_group_width).max(0.0);
        self.new_project_rect = Rect::new(content_x, action_y, button_width, ACTION_BUTTON_HEIGHT);
        self.new_project_rect.x = button_x;
        self.open_project_rect = Rect::new(
            button_x + button_width + ACTION_BUTTON_GAP,
            action_y,
            button_width,
            ACTION_BUTTON_HEIGHT,
        );

        let mut section_y = self.right_rect.y + CONTENT_PAD_Y + 72.0;
        self.recovery_rects.clear();
        let recovery_count = self.recovery_projects.len().min(MAX_VISIBLE_RECOVERY_PROJECTS);
        if recovery_count > 0 {
            let first_row_y = section_y + SECTION_LABEL_TO_ROW_GAP;
            for index in 0..recovery_count {
                self.recovery_rects.push(Rect::new(
                    content_x,
                    first_row_y + index as f32 * (RECENT_ROW_HEIGHT + RECENT_ROW_GAP),
                    content_width,
                    RECENT_ROW_HEIGHT,
                ));
            }
            section_y =
                first_row_y + recovery_count as f32 * (RECENT_ROW_HEIGHT + RECENT_ROW_GAP) + 8.0;
        }

        self.recent_rects.clear();
        let max_recent = if recovery_count > 0 {
            MAX_VISIBLE_RECENT_PROJECTS_WITH_RECOVERY
        } else {
            MAX_VISIBLE_RECENT_PROJECTS
        };
        let first_row_y = section_y + SECTION_LABEL_TO_ROW_GAP;
        let row_count = self.recent_projects.len().min(max_recent);
        for index in 0..row_count {
            self.recent_rects.push(Rect::new(
                content_x,
                first_row_y + index as f32 * (RECENT_ROW_HEIGHT + RECENT_ROW_GAP),
                content_width,
                RECENT_ROW_HEIGHT,
            ));
        }
        if let Some(modal) = &mut self.modal {
            modal.layout(bounds);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(modal) = &mut self.modal
            && modal.event(event, ctx) == EventResult::Handled
        {
            return EventResult::Handled;
        }
        match event {
            UiEvent::MouseMove { position, .. } => {
                self.hover = self.hit_region(*position);
                EventResult::Ignored
            }
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                let Some(hit) = self.hit_region(*position) else {
                    self.pressed = None;
                    return EventResult::Ignored;
                };
                if hit == StartupHit::DragSurface {
                    self.dispatch_hit(hit, ctx);
                }
                self.pressed = Some(hit);
                EventResult::Handled
            }
            UiEvent::MouseUp { position, button: MouseButton::Left, .. } => {
                let pressed = self.pressed.take();
                if let Some(hit) = pressed {
                    if hit != StartupHit::DragSurface && self.hit_region(*position) == Some(hit) {
                        self.dispatch_hit(hit, ctx);
                    }
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::FocusLost => {
                self.hover = None;
                self.pressed = None;
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                self.dispatch_hit(StartupHit::Close, ctx);
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let typography = &ctx.theme.typography;
        let radius = spacing.radius_lg;

        ctx.encoder.draw_rect(self.panel_rect, colors.popover, radius);
        ctx.push_clip(self.panel_rect);
        paint_startup_banner(ctx, self.left_rect);
        ctx.pop_clip();

        let content_x = self.right_rect.x + CONTENT_PAD_X;
        let content_y = self.right_rect.y + CONTENT_PAD_Y;
        let content_width = self.right_rect.width - CONTENT_PAD_X * 2.0;
        ctx.encoder.draw_text(
            "开始工作",
            typography.heading_h2.font_size,
            Point::new(content_x, content_y),
            colors.popover_foreground,
        );

        self.paint_button(
            ctx,
            self.new_project_rect,
            "新建项目",
            true,
            Some(AppIcon::PlusFilled),
            StartupHit::NewProject,
        );
        self.paint_button(
            ctx,
            self.open_project_rect,
            "打开项目",
            false,
            Some(AppIcon::FolderOpenFilled),
            StartupHit::OpenProject,
        );

        let mut section_y = self.right_rect.y + CONTENT_PAD_Y + 72.0;
        if !self.recovery_projects.is_empty() {
            ctx.encoder.draw_text(
                "可恢复项目",
                typography.large.font_size,
                Point::new(content_x, section_y),
                colors.popover_foreground,
            );
            for (index, rect) in self.recovery_rects.iter().enumerate() {
                let hit = StartupHit::RecoveryProject(index);
                let fill = if self.pressed == Some(hit) {
                    colors.muted
                } else if self.hover == Some(hit) {
                    colors.card.lerp(colors.foreground, 0.06)
                } else {
                    colors.card
                };
                ctx.encoder.draw_rect(*rect, fill, spacing.radius_md);
                if let Some(project) = self.recovery_projects.get(index) {
                    ctx.encoder.draw_text_box(
                        &project.title,
                        typography.body.font_size,
                        Point::new(rect.x + 14.0, rect.y + 15.0),
                        rect.width - 28.0,
                        colors.card_foreground,
                    );
                    ctx.encoder.draw_text_box(
                        &project.detail,
                        typography.small.font_size,
                        Point::new(rect.x + 14.0, rect.y + 32.0),
                        rect.width - 28.0,
                        colors.muted_foreground,
                    );
                }
            }
            section_y = self
                .recovery_rects
                .last()
                .map(|rect| rect.y + rect.height + 16.0)
                .unwrap_or(section_y + 24.0);
        }

        let recent_y = section_y;
        ctx.encoder.draw_text(
            "最近项目",
            typography.large.font_size,
            Point::new(content_x, recent_y),
            colors.popover_foreground,
        );
        if self.recent_projects.is_empty() {
            let recent_rect = Rect::new(
                content_x,
                recent_y + SECTION_LABEL_TO_ROW_GAP,
                content_width,
                RECENT_ROW_HEIGHT,
            );
            ctx.encoder.draw_rect(recent_rect, colors.card, spacing.radius_md);
            ctx.encoder.draw_text(
                "暂无最近项目",
                typography.body.font_size,
                Point::new(recent_rect.x + 14.0, recent_rect.y + 20.0),
                colors.muted_foreground,
            );
        } else {
            for (index, rect) in self.recent_rects.iter().enumerate() {
                let hit = StartupHit::RecentProject(index);
                let fill = if self.pressed == Some(hit) {
                    colors.muted
                } else if self.hover == Some(hit) {
                    colors.card.lerp(colors.foreground, 0.06)
                } else {
                    colors.card
                };
                ctx.encoder.draw_rect(*rect, fill, spacing.radius_md);
                if let Some(project) = self.recent_projects.get(index) {
                    let title_font = typography.body.font_size;
                    let detail_font = typography.small.font_size;
                    let text_x = rect.x + 14.0;
                    let text_right = rect.x + rect.width - 14.0;
                    let title = startup_ellipsize(&project.title, title_font, text_right - text_x);
                    ctx.encoder.draw_text(
                        &title,
                        typography.body.font_size,
                        Point::new(text_x, rect.y + 12.0),
                        colors.card_foreground,
                    );
                    let icon_size = 12.0;
                    let icon_rect = Rect::new(text_x, rect.y + 31.0, icon_size, icon_size);
                    paint_startup_icon(
                        ctx,
                        AppIcon::Clock,
                        icon_rect,
                        startup_alpha(colors.muted_foreground, 0.92),
                    );
                    let detail_x = icon_rect.x + icon_rect.width + 6.0;
                    let detail =
                        startup_ellipsize(&project.subtitle, detail_font, text_right - detail_x);
                    ctx.encoder.draw_text(
                        &detail,
                        detail_font,
                        Point::new(detail_x, rect.y + 29.0),
                        colors.muted_foreground,
                    );
                }
            }
        }

        let close_bg = if self.hover == Some(StartupHit::Close) {
            colors.muted
        } else {
            Color::TRANSPARENT
        };
        ctx.encoder.draw_rect(self.close_rect, close_bg, spacing.radius_sm);
        let inset = 8.0;
        let a = Point::new(self.close_rect.x + inset, self.close_rect.y + inset);
        let b = Point::new(
            self.close_rect.x + self.close_rect.width - inset,
            self.close_rect.y + self.close_rect.height - inset,
        );
        let c = Point::new(
            self.close_rect.x + inset,
            self.close_rect.y + self.close_rect.height - inset,
        );
        let d = Point::new(
            self.close_rect.x + self.close_rect.width - inset,
            self.close_rect.y + inset,
        );
        ctx.encoder.draw_line(a, b, spacing.border_emphasis, colors.popover_foreground);
        ctx.encoder.draw_line(c, d, spacing.border_emphasis, colors.popover_foreground);
        if let Some(modal) = &self.modal {
            modal.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.panel_rect.contains(point)
    }

    fn child_count(&self) -> usize {
        usize::from(self.modal.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => self.modal.as_ref().map(|modal| modal as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => self.modal.as_mut().map(|modal| modal as &mut dyn Widget),
            _ => None,
        }
    }
}

impl AppUiStartupScreen {
    fn paint_button(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        label: &str,
        primary: bool,
        icon: Option<AppIcon>,
        hit: StartupHit,
    ) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let fill = self.button_color(hit, primary, ctx);
        let text = if primary {
            colors.background
        } else {
            colors.card_foreground
        };
        ctx.encoder.draw_rect(rect, fill, spacing.radius_md);
        let font_size = ctx.theme.typography.button.font_size;
        let icon_size = if icon.is_some() { 13.0 } else { 0.0 };
        let icon_gap = if icon.is_some() { 6.0 } else { 0.0 };
        let text_width = startup_text_width(label, font_size);
        let content_width = icon_size + icon_gap + text_width;
        let content_x = rect.x + (rect.width - content_width).max(0.0) * 0.5;
        let text_y =
            rect.y + (rect.height - ctx.theme.typography.button.line_height).max(0.0) * 0.5;
        if let Some(icon) = icon {
            let icon_rect = Rect::new(
                content_x,
                rect.y + (rect.height - icon_size).max(0.0) * 0.5,
                icon_size,
                icon_size,
            );
            paint_startup_icon(ctx, icon, icon_rect, text);
        }
        ctx.encoder.draw_text(
            label,
            font_size,
            Point::new(content_x + icon_size + icon_gap, text_y),
            text,
        );
    }
}

fn paint_startup_icon(ctx: &mut PaintContext, icon: AppIcon, rect: Rect, color: Color) {
    if let Some(icon) = startup_vector_icon(icon) {
        icon.paint(ctx, rect, color);
    }
}

fn startup_vector_icon(icon: AppIcon) -> Option<VectorIcon> {
    icon.vector_icon().ok()
}

fn paint_startup_banner(ctx: &mut PaintContext, rect: Rect) {
    let colors = &ctx.theme.colors;
    if let Some(image) = startup_banner_image() {
        ctx.push_clip(rect);
        let image_rect = cover_image_rect(rect, image.width, image.height);
        ctx.encoder.draw_raster_image(
            &image.key,
            image_rect,
            image.width,
            image.height,
            image.color_space,
            image.rgba.clone(),
            Color::WHITE,
        );
        ctx.pop_clip();
    } else {
        ctx.encoder.draw_gradient_rect(
            rect,
            [
                colors.surface.lerp(colors.background, 0.18),
                colors.card.lerp(colors.surface_2, 0.24),
                colors.background.lerp(colors.surface, 0.18),
                colors.card.lerp(colors.foreground, 0.06),
            ],
            0.0,
        );
    }
}

fn startup_text_width(text: &str, font_size: f32) -> f32 {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_whitespace() {
                font_size * 0.32
            } else if ch.is_ascii() {
                font_size * 0.56
            } else {
                font_size
            }
        })
        .sum()
}

fn startup_ellipsize(text: &str, font_size: f32, max_width: f32) -> String {
    if max_width <= 0.0 || startup_text_width(text, font_size) <= max_width {
        return text.to_owned();
    }
    let ellipsis = "...";
    let ellipsis_width = startup_text_width(ellipsis, font_size);
    if ellipsis_width >= max_width {
        return ellipsis.to_owned();
    }
    let mut output = String::new();
    for ch in text.chars() {
        output.push(ch);
        if startup_text_width(&output, font_size) + ellipsis_width > max_width {
            output.pop();
            break;
        }
    }
    output.push_str(ellipsis);
    output
}

fn cover_image_rect(bounds: Rect, image_width: u32, image_height: u32) -> Rect {
    let image_width = image_width.max(1) as f32;
    let image_height = image_height.max(1) as f32;
    let scale = (bounds.width / image_width).max(bounds.height / image_height);
    let width = image_width * scale;
    let height = image_height * scale;
    Rect::new(
        bounds.x + (bounds.width - width) * 0.5,
        bounds.y + (bounds.height - height) * 0.5,
        width,
        height,
    )
}

fn startup_banner_image() -> Option<&'static RasterImage> {
    static IMAGE: OnceLock<Option<RasterImage>> = OnceLock::new();
    IMAGE
        .get_or_init(|| {
            decode_startup_png("startup.banner", include_bytes!("../../assets/banner.png"))
        })
        .as_ref()
}

fn decode_startup_png(key: &str, bytes: &[u8]) -> Option<RasterImage> {
    let image = image::load_from_memory(bytes).ok()?.into_rgba8();
    RasterImage::new(
        key,
        image.width(),
        image.height(),
        mondrian_ui_core::RasterImageColorSpace::Srgb,
        image.into_raw(),
    )
}

fn startup_alpha(mut color: Color, alpha: f32) -> Color {
    color.a *= alpha.clamp(0.0, 1.0);
    color
}

fn startup_shell_action_error(name: &str, err: serde_json::Error) -> MondrianError {
    MondrianError::WorkflowStepFailed {
        step_id: format!("startup_shell_action.{name}"),
        reason: format!("invalid action payload: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_OPEN_PROJECT_DIALOG,
        APP_SHELL_OPEN_RECENT_PROJECT, APP_SHELL_QUIT, APP_SHELL_RECOVER_PROJECT,
        APP_SHELL_WINDOW_DRAG,
    };
    use crate::app_ui::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::Action;
    use mondrian_platform::{
        ClipboardError, FileDialogError, FileDialogOutcome, FileFilter, FileRevealError,
        PlatformService,
    };
    use mondrian_ui_core::widget::{DrawCommandEncoder, EventRequests};
    use std::cell::RefCell;
    use std::path::Path;
    use std::sync::Arc;

    struct SaveProjectPlatform {
        project_file: PathBuf,
        open_file: Option<PathBuf>,
    }

    impl PlatformService for SaveProjectPlatform {
        fn clipboard_copy(&self, _text: &str) -> Result<(), ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn clipboard_paste(&self) -> Result<Option<String>, ClipboardError> {
            Err(ClipboardError::Unavailable)
        }

        fn open_file_dialog(
            &self,
            _title: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<Vec<PathBuf>>, FileDialogError> {
            Ok(match self.open_file.clone() {
                Some(path) => FileDialogOutcome::Selected(vec![path]),
                None => FileDialogOutcome::Cancelled,
            })
        }

        fn save_file_dialog(
            &self,
            _title: &str,
            _default_name: &str,
            _filters: &[FileFilter],
        ) -> Result<FileDialogOutcome<PathBuf>, FileDialogError> {
            Ok(FileDialogOutcome::Selected(self.project_file.clone()))
        }

        fn reveal_in_file_manager(&self, _path: &Path) -> Result<(), FileRevealError> {
            Ok(())
        }
    }

    fn action_name(action: &Action) -> (&str, &str) {
        match action {
            Action::Custom { namespace, name, .. } => (namespace.as_str(), name.as_str()),
            other => panic!("expected custom action, got {other:?}"),
        }
    }

    fn dispatch_click(screen: &mut AppUiStartupScreen, point: Point) -> Vec<Action> {
        let actions = RefCell::new(Vec::<Action>::new());
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
        assert_eq!(
            screen.event(
                &UiEvent::MouseDown {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            screen.event(
                &UiEvent::MouseUp {
                    position: point,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        actions.into_inner()
    }

    #[derive(Default)]
    struct StartupPaintRecorder {
        rects: usize,
        texts: Vec<String>,
        raster_images: Vec<(String, Rect, u32, u32)>,
        clips: Vec<Rect>,
        clip_pops: usize,
    }

    impl DrawCommandEncoder for StartupPaintRecorder {
        fn push_clip(&mut self, bounds: Rect) {
            self.clips.push(bounds);
        }

        fn pop_clip(&mut self) {
            self.clip_pops += 1;
        }

        fn draw_rect(&mut self, _bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects += 1;
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {}

        fn draw_text(&mut self, text: &str, _font_size: f32, _position: Point, _color: Color) {
            self.texts.push(text.to_owned());
        }

        fn draw_text_box(
            &mut self,
            text: &str,
            _font_size: f32,
            _position: Point,
            _max_width: f32,
            _color: Color,
        ) {
            self.texts.push(text.to_owned());
        }

        fn draw_raster_image(
            &mut self,
            key: &str,
            bounds: Rect,
            width: u32,
            height: u32,
            _color_space: mondrian_ui_core::RasterImageColorSpace,
            _rgba: Arc<[u8]>,
            _tint: Color,
        ) {
            self.raster_images.push((key.to_owned(), bounds, width, height));
        }

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    #[test]
    fn startup_screen_measures_to_product_startup_window_size() {
        let screen = AppUiStartupScreen::new();

        assert_eq!(
            screen.measure(LayoutConstraint::LOOSE),
            Size::new(STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT)
        );
    }

    #[test]
    fn startup_panel_fills_native_window_without_transparent_outer_margin() {
        let mut screen = AppUiStartupScreen::new();
        let bounds = Rect::new(0.0, 0.0, STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT);

        screen.layout(bounds);

        assert_eq!(screen.panel_rect, bounds);
        assert_eq!(screen.left_rect.x, bounds.x);
        assert_eq!(screen.left_rect.y, bounds.y);
        assert_eq!(screen.right_rect.y, bounds.y);
        assert_eq!(
            screen.right_rect.x + screen.right_rect.width,
            bounds.x + bounds.width
        );
        assert_eq!(screen.left_rect.height, bounds.height);
        assert_eq!(screen.right_rect.height, bounds.height);
    }

    #[test]
    fn startup_screen_uses_embedded_banner_without_text_overlay() {
        let mut screen = AppUiStartupScreen::new();
        screen.layout(Rect::new(
            0.0,
            0.0,
            STARTUP_WINDOW_WIDTH,
            STARTUP_WINDOW_HEIGHT,
        ));
        let theme = mondrian_ui_theme::ThemePreset::Dark.build();
        let mut encoder = StartupPaintRecorder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT),
        };

        screen.paint(&mut ctx);

        assert!(
            encoder.raster_images.iter().any(|(key, _, _, _)| key == "startup.banner"),
            "startup banner should use the embedded raster asset"
        );
        assert!(
            !encoder.raster_images.iter().any(|(key, _, _, _)| key == "startup.app-icon"),
            "startup left half should not layer a separate app icon over the banner"
        );
        assert_eq!(encoder.clip_pops, encoder.clips.len());
        assert!(!encoder.texts.iter().any(|text| text == "Mondrian"));
        assert!(!encoder.texts.iter().any(|text| text == "自研 UI"));
    }

    #[test]
    fn startup_screen_dispatches_project_actions() {
        let mut screen = AppUiStartupScreen::new();
        screen.layout(Rect::new(
            0.0,
            0.0,
            STARTUP_WINDOW_WIDTH,
            STARTUP_WINDOW_HEIGHT,
        ));

        let new_point = screen.new_project_rect.center();
        let new_actions = dispatch_click(&mut screen, new_point);
        assert_eq!(new_actions.len(), 1);
        assert_eq!(
            action_name(&new_actions[0]),
            (APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG)
        );

        let open_point = screen.open_project_rect.center();
        let open_actions = dispatch_click(&mut screen, open_point);
        assert_eq!(open_actions.len(), 1);
        assert_eq!(
            action_name(&open_actions[0]),
            (APP_SHELL_NAMESPACE, APP_SHELL_OPEN_PROJECT_DIALOG)
        );
    }

    #[test]
    fn startup_screen_dispatches_window_commands() {
        let mut screen = AppUiStartupScreen::new();
        screen.layout(Rect::new(
            0.0,
            0.0,
            STARTUP_WINDOW_WIDTH,
            STARTUP_WINDOW_HEIGHT,
        ));

        let close_point = screen.close_rect.center();
        let close_actions = dispatch_click(&mut screen, close_point);
        assert_eq!(close_actions.len(), 1);
        assert_eq!(
            action_name(&close_actions[0]),
            (APP_SHELL_NAMESPACE, APP_SHELL_QUIT)
        );

        let drag_point = screen.left_rect.center();
        let drag_actions = dispatch_click(&mut screen, drag_point);
        assert_eq!(drag_actions.len(), 1);
        assert_eq!(
            action_name(&drag_actions[0]),
            (APP_SHELL_NAMESPACE, APP_SHELL_WINDOW_DRAG)
        );
    }

    #[test]
    fn startup_screen_dispatches_recent_project_action() {
        let mut screen = AppUiStartupScreen::new();
        let project_file = PathBuf::from("E:/projects/recent.mdp");
        screen.set_recent_projects(vec![StartupRecentProject {
            project_file: project_file.clone(),
            title: "recent".to_owned(),
            subtitle: "E:/projects".to_owned(),
        }]);
        screen.layout(Rect::new(
            0.0,
            0.0,
            STARTUP_WINDOW_WIDTH,
            STARTUP_WINDOW_HEIGHT,
        ));

        let recent_point = screen.recent_rects[0].center();
        let recent_actions = dispatch_click(&mut screen, recent_point);

        assert_eq!(recent_actions.len(), 1);
        let Action::Custom { namespace, name, payload } = &recent_actions[0] else {
            panic!("expected recent custom action");
        };
        assert_eq!(namespace, APP_SHELL_NAMESPACE);
        assert_eq!(name, APP_SHELL_OPEN_RECENT_PROJECT);
        let payload: AppShellOpenRecentProjectPayload =
            serde_json::from_value(payload.clone()).expect("recent payload");
        assert_eq!(payload.project_file, project_file);
    }

    #[test]
    fn startup_screen_requires_recovery_inspection_before_project_action() {
        let mut screen = AppUiStartupScreen::new();
        let project_file = PathBuf::from("E:/projects/recover.mdp");
        let autosave_file = PathBuf::from("E:/runtime/autosave/project.autosave.mdp");
        let candidate = CrashRecoveryCandidate {
            project_id: ProjectId::new(),
            runtime_root: PathBuf::from("E:/runtime"),
            project_file: project_file.clone(),
            canonical_target: crate::app::RecoveryCanonicalTargetEvidence::Missing,
            autosave_file: autosave_file.clone(),
            author_generation: 5,
            asset_library_revision: 2,
            document_revision: 9,
            archive_sha256: "a".repeat(64),
            saved_at_unix_ms: 17,
            total_snapshots: 2,
        };
        screen.set_recovery_projects(vec![StartupRecoveryProject {
            candidate: candidate.clone(),
            title: "recover".to_owned(),
            detail: "刚刚，共 2 个恢复点".to_owned(),
        }]);
        screen.layout(Rect::new(
            0.0,
            0.0,
            STARTUP_WINDOW_WIDTH,
            STARTUP_WINDOW_HEIGHT,
        ));

        let recovery_point = screen.recovery_rects[0].center();
        let recovery_actions = dispatch_click(&mut screen, recovery_point);

        assert_eq!(recovery_actions.len(), 1);
        let Action::Custom { namespace, name, payload } = &recovery_actions[0] else {
            panic!("expected recovery custom action");
        };
        assert_eq!(namespace, APP_SHELL_NAMESPACE);
        assert_eq!(name, APP_SHELL_RECOVERY_DIALOG);
        let payload: ProjectRecoverFromAutosavePayload =
            serde_json::from_value(payload.clone()).expect("recovery payload");
        assert_eq!(payload.candidate, candidate);

        let platform = SaveProjectPlatform {
            project_file: PathBuf::from("E:/unused.mdp"),
            open_file: None,
        };
        assert!(screen
            .try_handle_shell_action(recovery_actions[0].clone(), &platform)
            .expect("open recovery inspection")
            .is_none());
        assert!(screen.has_modal());

        let resolved = screen
            .try_handle_shell_action(
                crate::app::ui_actions::app_shell_confirm_recovery_dialog_action(),
                &platform,
            )
            .expect("confirm recovery inspection")
            .expect("recovery project action");
        let Action::Custom { namespace, name, payload } = resolved else {
            panic!("expected confirmed recovery custom action");
        };
        assert_eq!(namespace, APP_SHELL_NAMESPACE);
        assert_eq!(name, APP_SHELL_RECOVER_PROJECT);
        let payload: ProjectRecoverFromAutosavePayload =
            serde_json::from_value(payload).expect("confirmed recovery payload");
        assert_eq!(payload.candidate, candidate);
        assert!(!screen.has_modal());
    }

    #[test]
    fn startup_screen_handles_new_project_modal_shell_actions() {
        let mut screen = AppUiStartupScreen::new();
        let platform = SaveProjectPlatform {
            project_file: PathBuf::from("E:/projects/modal-create.mdp"),
            open_file: None,
        };
        screen.layout(Rect::new(
            0.0,
            0.0,
            STARTUP_WINDOW_WIDTH,
            STARTUP_WINDOW_HEIGHT,
        ));

        let opened = screen
            .try_handle_shell_action(app_shell_new_project_dialog_action(), &platform)
            .expect("open new project modal");
        assert_eq!(opened, None);
        assert!(screen.has_modal());
        assert_eq!(screen.child_count(), 1);

        let confirmed = screen
            .try_handle_shell_action(
                crate::app::ui_actions::app_shell_confirm_new_project_dialog_action(),
                &platform,
            )
            .expect("confirm new project modal")
            .expect("confirm should produce project action");

        assert!(!screen.has_modal());
        let Action::Custom { namespace, name, payload } = confirmed else {
            panic!("expected project create action");
        };
        assert_eq!(namespace, crate::app::ui_actions::PROJECT_NAMESPACE);
        assert_eq!(name, crate::app::ui_actions::PROJECT_CREATE_WITH_SETTINGS);
        let payload: crate::app::ui_actions::ProjectCreateWithSettingsPayload =
            serde_json::from_value(payload).expect("project payload");
        assert_eq!(
            payload.project_file,
            PathBuf::from("E:/projects/modal-create.mdp")
        );
    }

    #[test]
    fn startup_new_project_modal_routes_custom_ocio_selection() {
        let config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../mondrian-core/assets/ocio/mondrian_default_ocio_v2.ocio");
        let platform = SaveProjectPlatform {
            project_file: PathBuf::from("E:/projects/custom.mdp"),
            open_file: Some(config_path),
        };
        let mut screen = AppUiStartupScreen::new();

        screen
            .try_handle_shell_action(app_shell_new_project_dialog_action(), &platform)
            .expect("open new-project modal");
        screen
            .try_handle_shell_action(
                crate::app::ui_actions::app_shell_select_custom_ocio_config_action(),
                &platform,
            )
            .expect("select Custom OCIO");

        let identity = screen
            .modal
            .as_ref()
            .and_then(ShellModal::as_new_project)
            .expect("new-project modal")
            .draft()
            .color_environment
            .engine()
            .custom_ocio_identity()
            .expect("pinned Custom OCIO identity");
        assert_eq!(
            identity
                .output(mondrian_core::ColorSpace::Rec709)
                .expect("Rec.709 output binding")
                .display(),
            "Rec.1886 Rec.709 - Display"
        );
        assert!(!identity.dependency_manifest_sha256().is_empty());
    }
}
