//! Self-hosted startup surface.
//!
//! The startup surface is separate from the editor workspace. It owns only
//! launch-time presentation and emits shell actions; project lifecycle work
//! stays in `SelfHostedUiHost` / `AppState`.

use mondrian_core::{Color, MondrianError, Result};
use mondrian_editor_state::Action;
use mondrian_platform::PlatformService;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use std::path::PathBuf;

use crate::app::ui_actions::{
    app_shell_new_project_dialog_action, app_shell_open_project_dialog_action,
    app_shell_open_recent_project_action, app_shell_quit_action, app_shell_recover_project_action,
    app_shell_window_drag_action, project_create_with_settings_action,
    AppShellOpenRecentProjectPayload, NewProjectDraftUpdatePayload,
    ProjectRecoverFromAutosavePayload, APP_SHELL_CANCEL_NEW_PROJECT_DIALOG, APP_SHELL_CLOSE_MODAL,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_NEW_PROJECT_DIALOG,
    APP_SHELL_NEW_PROJECT_DRAFT_CHANGED,
};
use crate::self_hosted::modal::ShellModal;
use crate::self_hosted::new_project_dialog::{
    default_project_file_name, SelfHostedNewProjectDraft,
};
use crate::self_hosted::shell::project_file_filters;

/// Startup window logical size used by the self-hosted product entrypoint.
pub const STARTUP_WINDOW_WIDTH: f32 = 820.0;
/// Startup window logical size used by the self-hosted product entrypoint.
pub const STARTUP_WINDOW_HEIGHT: f32 = 500.0;

const OUTER_MARGIN: f32 = 18.0;
const LEFT_WIDTH: f32 = 300.0;
const CONTENT_PAD_X: f32 = 22.0;
const CONTENT_PAD_Y: f32 = 24.0;
const CLOSE_SIZE: f32 = 28.0;
const CLOSE_MARGIN: f32 = 10.0;
const ACTION_BUTTON_HEIGHT: f32 = 34.0;
const ACTION_BUTTON_GAP: f32 = 10.0;
const RECENT_ROW_HEIGHT: f32 = 44.0;
const RECENT_ROW_GAP: f32 = 8.0;
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

/// One project row shown on the self-hosted startup surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupRecentProject {
    /// Project file opened when the row is activated.
    pub project_file: PathBuf,
    /// Primary row label.
    pub title: String,
    /// Secondary row label, typically the parent directory.
    pub subtitle: String,
}

/// One autosave recovery row shown on the self-hosted startup surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupRecoveryProject {
    /// Original project file represented by the autosave snapshot.
    pub project_file: PathBuf,
    /// Autosave snapshot archive to recover.
    pub autosave_file: PathBuf,
    /// Primary row label.
    pub title: String,
    /// Secondary row label with age/snapshot metadata.
    pub detail: String,
}

/// Startup screen shown before a project is opened.
pub struct SelfHostedStartupScreen {
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

impl SelfHostedStartupScreen {
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
                app_shell_recover_project_action(ProjectRecoverFromAutosavePayload {
                    project_file: project.project_file.clone(),
                    autosave_file: project.autosave_file.clone(),
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
                colors.primary.lerp(colors.foreground, 0.10)
            } else {
                colors.primary
            }
        } else if self.hover == Some(hit) {
            colors.muted
        } else {
            colors.card
        }
    }
}

impl Default for SelfHostedStartupScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SelfHostedStartupScreen {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let panel_width = (bounds.width - OUTER_MARGIN * 2.0).max(320.0);
        let panel_height = (bounds.height - OUTER_MARGIN * 2.0).max(260.0);
        self.panel_rect = Rect::new(
            bounds.x + (bounds.width - panel_width) * 0.5,
            bounds.y + (bounds.height - panel_height) * 0.5,
            panel_width,
            panel_height,
        );

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
        let action_y = self.right_rect.y + CONTENT_PAD_Y + 86.0;
        let button_width = ((content_width - ACTION_BUTTON_GAP) * 0.5).max(92.0);
        self.new_project_rect = Rect::new(content_x, action_y, button_width, ACTION_BUTTON_HEIGHT);
        self.open_project_rect = Rect::new(
            content_x + button_width + ACTION_BUTTON_GAP,
            action_y,
            button_width,
            ACTION_BUTTON_HEIGHT,
        );

        let mut section_y = self.new_project_rect.y + ACTION_BUTTON_HEIGHT + 34.0;
        self.recovery_rects.clear();
        let recovery_count = self.recovery_projects.len().min(MAX_VISIBLE_RECOVERY_PROJECTS);
        if recovery_count > 0 {
            let first_row_y = section_y + 24.0;
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
        let first_row_y = section_y + 24.0;
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
        if let Some(modal) = &mut self.modal {
            if modal.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
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

        ctx.encoder.draw_rect(self.panel_rect, colors.border, radius);
        ctx.encoder.draw_gradient_rect(
            self.left_rect,
            [
                colors.primary.lerp(colors.background, 0.15),
                colors.accent.lerp(colors.primary, 0.30),
                colors.background.lerp(colors.primary, 0.22),
                colors.card.lerp(colors.primary, 0.18),
            ],
            radius,
        );
        ctx.encoder.draw_rect(self.right_rect, colors.popover, radius);

        let brand_x = self.left_rect.x + 30.0;
        let brand_y = self.left_rect.y + 54.0;
        let mark = Rect::new(brand_x, brand_y, 54.0, 54.0);
        ctx.encoder.draw_rect(mark, colors.primary_foreground, 14.0);
        ctx.encoder.draw_rect(mark.inset(7.0, 7.0), colors.primary, 9.0);
        ctx.encoder.draw_text(
            "Mondrian",
            typography.heading_h1.font_size,
            Point::new(brand_x, brand_y + 92.0),
            colors.primary_foreground,
        );
        ctx.encoder.draw_text_box(
            "AI-native video editing workspace",
            typography.body.font_size,
            Point::new(brand_x, brand_y + 126.0),
            self.left_rect.width - 60.0,
            colors.primary_foreground.lerp(colors.background, 0.18),
        );

        let accent = Rect::new(
            self.left_rect.x + 30.0,
            self.left_rect.y + self.left_rect.height - 92.0,
            self.left_rect.width - 60.0,
            44.0,
        );
        ctx.encoder
            .draw_rect(accent, colors.background.lerp(colors.primary, 0.28), 10.0);
        ctx.encoder.draw_text(
            "自研 UI",
            typography.button.font_size,
            Point::new(accent.x + 14.0, accent.y + 15.0),
            colors.primary_foreground,
        );

        let content_x = self.right_rect.x + CONTENT_PAD_X;
        let content_y = self.right_rect.y + CONTENT_PAD_Y + 16.0;
        let content_width = self.right_rect.width - CONTENT_PAD_X * 2.0;
        ctx.encoder.draw_text(
            "开始工作",
            typography.heading_h2.font_size,
            Point::new(content_x, content_y),
            colors.popover_foreground,
        );
        ctx.encoder.draw_text_box(
            "创建剪辑项目，或打开现有 Mondrian 工程。",
            typography.body.font_size,
            Point::new(content_x, content_y + 30.0),
            content_width,
            colors.muted_foreground,
        );

        self.paint_button(
            ctx,
            self.new_project_rect,
            "新建项目",
            true,
            StartupHit::NewProject,
        );
        self.paint_button(
            ctx,
            self.open_project_rect,
            "打开项目",
            false,
            StartupHit::OpenProject,
        );

        let mut section_y = self.new_project_rect.y + ACTION_BUTTON_HEIGHT + 34.0;
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
            let recent_rect =
                Rect::new(content_x, recent_y + 24.0, content_width, RECENT_ROW_HEIGHT);
            ctx.encoder.draw_rect(recent_rect, colors.card, spacing.radius_md);
            ctx.encoder.draw_text(
                "暂无最近项目",
                typography.body.font_size,
                Point::new(recent_rect.x + 14.0, recent_rect.y + 17.0),
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
                    ctx.encoder.draw_text_box(
                        &project.title,
                        typography.body.font_size,
                        Point::new(rect.x + 14.0, rect.y + 15.0),
                        rect.width - 28.0,
                        colors.card_foreground,
                    );
                    ctx.encoder.draw_text_box(
                        &project.subtitle,
                        typography.small.font_size,
                        Point::new(rect.x + 14.0, rect.y + 32.0),
                        rect.width - 28.0,
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

impl SelfHostedStartupScreen {
    fn paint_button(
        &self,
        ctx: &mut PaintContext,
        rect: Rect,
        label: &str,
        primary: bool,
        hit: StartupHit,
    ) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let fill = self.button_color(hit, primary, ctx);
        let text = if primary {
            colors.primary_foreground
        } else {
            colors.card_foreground
        };
        ctx.encoder.draw_rect(rect, fill, spacing.radius_md);
        let text_width = label.chars().count() as f32 * 14.0;
        ctx.encoder.draw_text(
            label,
            ctx.theme.typography.button.font_size,
            Point::new(
                rect.x + (rect.width - text_width).max(0.0) * 0.5,
                rect.y + 21.0,
            ),
            text,
        );
    }
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
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_editor_state::Action;
    use mondrian_platform::{FileFilter, PlatformService};
    use mondrian_ui_core::widget::EventRequests;
    use std::cell::RefCell;
    use std::path::Path;

    struct SaveProjectPlatform {
        project_file: PathBuf,
    }

    impl PlatformService for SaveProjectPlatform {
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

    fn action_name(action: &Action) -> (&str, &str) {
        match action {
            Action::Custom { namespace, name, .. } => (namespace.as_str(), name.as_str()),
            other => panic!("expected custom action, got {other:?}"),
        }
    }

    fn dispatch_click(screen: &mut SelfHostedStartupScreen, point: Point) -> Vec<Action> {
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

    #[test]
    fn startup_screen_measures_to_product_startup_window_size() {
        let screen = SelfHostedStartupScreen::new();

        assert_eq!(
            screen.measure(LayoutConstraint::LOOSE),
            Size::new(STARTUP_WINDOW_WIDTH, STARTUP_WINDOW_HEIGHT)
        );
    }

    #[test]
    fn startup_screen_dispatches_project_actions() {
        let mut screen = SelfHostedStartupScreen::new();
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
        let mut screen = SelfHostedStartupScreen::new();
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
        let mut screen = SelfHostedStartupScreen::new();
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
    fn startup_screen_dispatches_recovery_project_action() {
        let mut screen = SelfHostedStartupScreen::new();
        let project_file = PathBuf::from("E:/projects/recover.mdp");
        let autosave_file = PathBuf::from("E:/runtime/autosave/project.autosave.mdp");
        screen.set_recovery_projects(vec![StartupRecoveryProject {
            project_file: project_file.clone(),
            autosave_file: autosave_file.clone(),
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
        assert_eq!(name, APP_SHELL_RECOVER_PROJECT);
        let payload: ProjectRecoverFromAutosavePayload =
            serde_json::from_value(payload.clone()).expect("recovery payload");
        assert_eq!(payload.project_file, project_file);
        assert_eq!(payload.autosave_file, autosave_file);
    }

    #[test]
    fn startup_screen_handles_new_project_modal_shell_actions() {
        let mut screen = SelfHostedStartupScreen::new();
        let platform = SaveProjectPlatform {
            project_file: PathBuf::from("E:/projects/modal-create.mdp"),
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
}
