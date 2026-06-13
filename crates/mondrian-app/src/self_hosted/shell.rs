//! Reusable widget shell for self-hosted Mondrian windows.
//!
//! Developer binaries own native window setup and event-loop plumbing. This
//! module owns the reusable root widget composition above the dock/panel layer.

use mondrian_editor_state::state::PanelKind;
use mondrian_editor_state::Action;
use mondrian_platform::{FileFilter, PlatformService};
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, Widget};
use mondrian_ui_widgets::dock_splitter::DockSplitter;
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};
use std::path::Path;

use crate::app::ui_actions::{
    app_shell_cancel_new_project_dialog_action, app_shell_confirm_new_project_dialog_action,
    app_shell_import_media_dialog_action, app_shell_new_project_dialog_action,
    app_shell_new_project_name_changed_action, app_shell_open_project_dialog_action,
    app_shell_save_project_as_dialog_action, project_create_with_settings_action,
    ProjectCreateWithSettingsPayload, APP_SHELL_CANCEL_NEW_PROJECT_DIALOG,
    APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG, APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_NEW_PROJECT_DIALOG, APP_SHELL_NEW_PROJECT_NAME_CHANGED,
    APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
};
use crate::self_hosted::panels::{build_demo_dock_tree, build_dock_tree, SelfHostedPanelModels};
use mondrian_core::ProjectSettings;
use mondrian_timeline::SequenceSettings;
use mondrian_ui_widgets::{Button, TextInput};

/// Height reserved for the self-hosted top menu bar.
pub const MENU_BAR_HEIGHT: f32 = 28.0;

/// Default file extension for Mondrian project containers.
pub const PROJECT_FILE_EXTENSION: &str = "mdp";

/// Self-hosted new-project form state.
///
/// The draft deliberately carries the same settings structs consumed by
/// `AppState` so future form widgets edit the eventual creation payload instead
/// of a parallel DTO that can drift from project lifecycle semantics.
#[derive(Debug, Clone)]
pub struct SelfHostedNewProjectDraft {
    pub name: String,
    pub sequence_settings: SequenceSettings,
    pub project_settings: ProjectSettings,
}

impl Default for SelfHostedNewProjectDraft {
    fn default() -> Self {
        Self {
            name: "Untitled".into(),
            sequence_settings: SequenceSettings::default(),
            project_settings: ProjectSettings::default(),
        }
    }
}

impl SelfHostedNewProjectDraft {
    /// Build a draft using the file stem as the initial project name.
    pub fn from_project_path(path: &Path) -> Self {
        Self {
            name: project_name_from_path(path),
            ..Self::default()
        }
    }

    /// Validate the draft before turning it into a creation payload.
    pub fn validate(&self) -> mondrian_core::Result<()> {
        self.sequence_settings.validate()
    }

    /// Convert the current form state into the action payload consumed by
    /// `AppState`.
    pub fn into_payload(
        self,
        project_file: impl Into<std::path::PathBuf>,
    ) -> ProjectCreateWithSettingsPayload {
        ProjectCreateWithSettingsPayload {
            project_file: project_file.into(),
            name: self.name,
            sequence_settings: self.sequence_settings,
            project_settings: self.project_settings,
        }
    }
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
    match action {
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_NEW_PROJECT_DIALOG =>
        {
            let path = platform.save_file_dialog(
                "Create Mondrian Project",
                &format!("Untitled.{PROJECT_FILE_EXTENSION}"),
                &project_file_filters(),
            )?;
            let draft = SelfHostedNewProjectDraft::from_project_path(&path);
            Some(project_create_with_settings_action(
                draft.into_payload(path),
            ))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_OPEN_PROJECT_DIALOG =>
        {
            let paths =
                platform.open_file_dialog("Open Mondrian Project", &project_file_filters())?;
            paths.into_iter().next().map(Action::OpenProject)
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
        {
            let paths = platform.open_file_dialog("Import Media", &media_import_filters())?;
            (!paths.is_empty()).then_some(Action::ImportMedia(paths))
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            let default_name = current_project_path
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("untitled.{PROJECT_FILE_EXTENSION}"));
            platform
                .save_file_dialog(
                    "Save Mondrian Project As",
                    &default_name,
                    &project_file_filters(),
                )
                .map(Action::SaveProjectAs)
        }
        action => Some(action),
    }
}

fn project_name_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.trim().is_empty())
        .map(|stem| stem.trim().to_string())
        .unwrap_or_else(|| "Untitled".to_string())
}

fn default_project_file_name(name: &str) -> String {
    let stem: String = name
        .trim()
        .chars()
        .map(|ch| match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let stem = stem.trim_matches(['.', ' ']).trim();
    if stem.is_empty() {
        format!("Untitled.{PROJECT_FILE_EXTENSION}")
    } else {
        format!("{stem}.{PROJECT_FILE_EXTENSION}")
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
                MenuItem::new("Quit", Action::CloseProject),
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
                MenuItem::new("Toggle Console", Action::TogglePanel(PanelKind::Console)),
                MenuItem::new("Toggle Timeline", Action::TogglePanel(PanelKind::Timeline)),
                MenuItem::new(
                    "Toggle Inspector",
                    Action::TogglePanel(PanelKind::Inspector),
                ),
            ],
        ),
        (
            "Help",
            vec![MenuItem::new(
                "About Mondrian",
                Action::Custom {
                    namespace: "app".into(),
                    name: "about".into(),
                    payload: serde_json::Value::Null,
                },
            )],
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

struct NewProjectDialog {
    id: WidgetId,
    draft: SelfHostedNewProjectDraft,
    bounds: Rect,
    card: Rect,
    name_input: TextInput,
    cancel_button: Button,
    create_button: Button,
}

impl NewProjectDialog {
    fn new(draft: SelfHostedNewProjectDraft) -> Self {
        let name_input = TextInput::new("Project name")
            .with_text(&draft.name)
            .on_change(app_shell_new_project_name_changed_action);
        Self {
            id: WidgetId::new(),
            draft,
            bounds: Rect::ZERO,
            card: Rect::ZERO,
            name_input,
            cancel_button: Button::new("Cancel")
                .on_click(app_shell_cancel_new_project_dialog_action()),
            create_button: Button::new("Create...")
                .on_click(app_shell_confirm_new_project_dialog_action()),
        }
    }

    fn set_name(&mut self, name: String) {
        self.draft.name = name;
    }

    fn draft(&self) -> &SelfHostedNewProjectDraft {
        &self.draft
    }
}

impl Widget for NewProjectDialog {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, _constraint: LayoutConstraint) -> Size {
        Size::new(420.0, 220.0)
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        let card_width = bounds.width.clamp(280.0, 420.0);
        let card_height = 220.0;
        self.card = Rect::new(
            bounds.x + (bounds.width - card_width) * 0.5,
            bounds.y + (bounds.height - card_height) * 0.5,
            card_width,
            card_height,
        );
        let content = self.card.inset(20.0, 20.0);
        self.name_input
            .layout(Rect::new(content.x, content.y + 82.0, content.width, 34.0));

        let button_y = self.card.y + self.card.height - 52.0;
        self.cancel_button.layout(Rect::new(
            self.card.x + self.card.width - 204.0,
            button_y,
            88.0,
            32.0,
        ));
        self.create_button.layout(Rect::new(
            self.card.x + self.card.width - 108.0,
            button_y,
            88.0,
            32.0,
        ));
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::KeyDown { key: KeyCode::Escape, .. } => {
                (ctx.dispatch)(app_shell_cancel_new_project_dialog_action());
                return EventResult::Handled;
            }
            UiEvent::KeyDown { key: KeyCode::Enter, .. } => {
                (ctx.dispatch)(app_shell_confirm_new_project_dialog_action());
                return EventResult::Handled;
            }
            UiEvent::MouseDown { position, .. } if !self.card.contains(*position) => {
                return EventResult::Handled;
            }
            _ => {}
        }

        if self.create_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.cancel_button.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        if self.name_input.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        EventResult::Ignored
    }

    fn paint(&self, ctx: &mut PaintContext) {
        ctx.encoder.draw_rect(
            self.bounds,
            mondrian_core::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.38 },
            0.0,
        );
        ctx.encoder.draw_rect(self.card, ctx.theme.colors.card, 8.0);
        let top_left = Point::new(self.card.x, self.card.y);
        let top_right = Point::new(self.card.x + self.card.width, self.card.y);
        let bottom_left = Point::new(self.card.x, self.card.y + self.card.height);
        let bottom_right = Point::new(
            self.card.x + self.card.width,
            self.card.y + self.card.height,
        );
        ctx.encoder.draw_line(top_left, top_right, 1.0, ctx.theme.colors.border);
        ctx.encoder.draw_line(bottom_left, bottom_right, 1.0, ctx.theme.colors.border);
        ctx.encoder.draw_line(top_left, bottom_left, 1.0, ctx.theme.colors.border);
        ctx.encoder.draw_line(top_right, bottom_right, 1.0, ctx.theme.colors.border);

        let content = self.card.inset(20.0, 20.0);
        ctx.encoder.draw_text(
            "New Project",
            18.0,
            Point::new(content.x, content.y + 22.0),
            ctx.theme.colors.foreground,
        );
        ctx.encoder.draw_text_box(
            "Create a project file and initialize the timeline with default production settings.",
            12.0,
            Point::new(content.x, content.y + 46.0),
            content.width,
            ctx.theme.colors.muted_foreground,
        );
        ctx.encoder.draw_text(
            "Name",
            12.0,
            Point::new(content.x, content.y + 76.0),
            ctx.theme.colors.muted_foreground,
        );
        self.name_input.paint(ctx);
        self.cancel_button.paint(ctx);
        self.create_button.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        3
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.name_input),
            1 => Some(&self.cancel_button),
            2 => Some(&self.create_button),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.name_input),
            1 => Some(&mut self.cancel_button),
            2 => Some(&mut self.create_button),
            _ => None,
        }
    }
}

/// Root widget for the self-hosted editor window.
pub struct SelfHostedAppRoot {
    id: WidgetId,
    menu_bar: MenuBar,
    dock: DockSplitter,
    new_project_dialog: Option<NewProjectDialog>,
    bounds: Rect,
}

impl SelfHostedAppRoot {
    /// Build a root widget from app-facing panel models.
    pub fn from_models(models: SelfHostedPanelModels) -> Self {
        Self::new(MenuBar::default(), build_dock_tree(models))
    }

    /// Build a root widget using developer demo fixtures.
    pub fn demo() -> Self {
        Self::new(MenuBar::default(), build_demo_dock_tree())
    }

    /// Build a root widget from explicit shell parts.
    pub fn new(menu_bar: MenuBar, dock: DockSplitter) -> Self {
        Self {
            id: WidgetId::new(),
            menu_bar,
            dock,
            new_project_dialog: None,
            bounds: Rect::ZERO,
        }
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
        self.dock = build_dock_tree(models);
        self.dock.restore_layout(&layout);
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
        match action {
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_NEW_PROJECT_DIALOG =>
            {
                self.new_project_dialog =
                    Some(NewProjectDialog::new(SelfHostedNewProjectDraft::default()));
                if self.bounds.width > 0.0 && self.bounds.height > 0.0 {
                    self.layout(self.bounds);
                }
                None
            }
            Action::Custom { namespace, name, payload }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_NEW_PROJECT_NAME_CHANGED =>
            {
                if let Some(dialog) = &mut self.new_project_dialog {
                    if let Ok(name) = serde_json::from_value::<String>(payload) {
                        dialog.set_name(name);
                    }
                }
                None
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CANCEL_NEW_PROJECT_DIALOG =>
            {
                self.new_project_dialog = None;
                None
            }
            Action::Custom { namespace, name, .. }
                if namespace == APP_SHELL_NAMESPACE
                    && name == APP_SHELL_CONFIRM_NEW_PROJECT_DIALOG =>
            {
                let draft = self
                    .new_project_dialog
                    .as_ref()
                    .map(|dialog| dialog.draft().clone())
                    .unwrap_or_default();
                if draft.validate().is_err() {
                    return None;
                }
                let path = platform.save_file_dialog(
                    "Create Mondrian Project",
                    &default_project_file_name(&draft.name),
                    &project_file_filters(),
                )?;
                self.new_project_dialog = None;
                Some(project_create_with_settings_action(
                    draft.into_payload(path),
                ))
            }
            action => resolve_app_shell_action(action, platform, current_project_path),
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
        self.menu_bar
            .layout(Rect::new(bounds.x, bounds.y, bounds.width, MENU_BAR_HEIGHT));
        self.dock.layout(Rect::new(
            bounds.x,
            bounds.y + MENU_BAR_HEIGHT,
            bounds.width,
            (bounds.height - MENU_BAR_HEIGHT).max(0.0),
        ));
        if let Some(dialog) = &mut self.new_project_dialog {
            dialog.layout(bounds);
        }
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if let Some(dialog) = &mut self.new_project_dialog {
            if dialog.event(event, ctx) == EventResult::Handled {
                return EventResult::Handled;
            }
            if matches!(
                event,
                UiEvent::MouseDown { .. }
                    | UiEvent::MouseUp { .. }
                    | UiEvent::MouseMove { .. }
                    | UiEvent::MouseWheel { .. }
            ) {
                return EventResult::Handled;
            }
        }
        if self.menu_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.dock.event(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.menu_bar.paint(ctx);
        self.dock.paint(ctx);
        if let Some(dialog) = &self.new_project_dialog {
            dialog.paint(ctx);
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        2 + usize::from(self.new_project_dialog.is_some())
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.menu_bar),
            1 => Some(&self.dock),
            2 => self.new_project_dialog.as_ref().map(|dialog| dialog as &dyn Widget),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.menu_bar),
            1 => Some(&mut self.dock),
            2 => self.new_project_dialog.as_mut().map(|dialog| dialog as &mut dyn Widget),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        app_shell_cancel_new_project_dialog_action, app_shell_confirm_new_project_dialog_action,
        app_shell_import_media_dialog_action, app_shell_new_project_dialog_action,
        app_shell_new_project_name_changed_action, app_shell_open_project_dialog_action,
        app_shell_save_project_as_dialog_action, ProjectCreateWithSettingsPayload,
        PROJECT_CREATE_WITH_SETTINGS, PROJECT_NAMESPACE,
    };
    use mondrian_core::{Rational, Resolution};
    use mondrian_editor_state::state::PanelKind;
    use mondrian_platform::NoopPlatformService;
    use mondrian_timeline::sequence::PreviewRenderFormat;
    use mondrian_ui_core::Widget;
    use mondrian_ui_core::{
        EventContext, EventRequests, FocusManager, ShortcutBinding, ShortcutManager, ShortcutScope,
        TooltipManager, TooltipState,
    };
    use std::path::{Path, PathBuf};

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

    struct DummyFocus;

    impl FocusManager for DummyFocus {
        fn focused_widget(&self) -> Option<WidgetId> {
            None
        }

        fn focused_panel(&self) -> Option<PanelKind> {
            None
        }

        fn request_focus(&mut self, _widget: WidgetId, _panel: PanelKind) {}

        fn release_focus(&mut self, _widget: WidgetId) {}

        fn focus_next(&mut self) {}

        fn focus_prev(&mut self) {}

        fn clear_focus(&mut self) {}
    }

    struct DummyShortcut;

    impl ShortcutManager for DummyShortcut {
        fn register(&mut self, _scope: ShortcutScope, _binding: ShortcutBinding, _action: Action) {}

        fn unregister(&mut self, _scope: ShortcutScope, _binding: &ShortcutBinding) {}

        fn resolve(&self, _key: KeyCode, _modifiers: Modifiers) -> Option<Action> {
            None
        }

        fn clear_scope(&mut self, _scope: ShortcutScope) {}

        fn clear_all(&mut self) {}
    }

    struct DummyTooltip;

    impl TooltipManager for DummyTooltip {
        fn show(&mut self, _text: String, _position: Point) {}

        fn hide(&mut self) {}

        fn current(&self) -> Option<&TooltipState> {
            None
        }

        fn update(&mut self, _delta_ms: u64) {}
    }

    fn event_ctx<'a>(
        focus: &'a mut DummyFocus,
        shortcut: &'a mut DummyShortcut,
        tooltip: &'a mut DummyTooltip,
        requests: &'a mut EventRequests,
    ) -> EventContext<'a> {
        EventContext {
            focus,
            shortcut,
            tooltip,
            dispatch: &|_| {},
            platform: &NoopPlatformService,
            requests,
        }
    }

    #[test]
    fn default_menu_bar_exposes_primary_menu_groups() {
        let menu = MenuBar::default();

        assert_eq!(menu.child_count(), 4);
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
        assert!(root.new_project_dialog.is_some());
        assert_eq!(root.child_count(), 3);

        assert_eq!(
            root.handle_shell_action(
                app_shell_new_project_name_changed_action("Rough Cut"),
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

        assert!(root.new_project_dialog.is_none());
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
        assert!(root.new_project_dialog.is_none());
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
    fn set_models_preserves_user_splitter_ratio() {
        let mut root = SelfHostedAppRoot::demo();
        root.layout(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let grab = root.dock().collect_grab_zones()[0].0.center();
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let mut requests = EventRequests::default();

        {
            let mut ctx = event_ctx(&mut focus, &mut shortcut, &mut tooltip, &mut requests);
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
                        position: Point::new(620.0, grab.y),
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
            assert_eq!(
                root.dock_mut().event(
                    &UiEvent::MouseUp {
                        position: Point::new(620.0, grab.y),
                        button: MouseButton::Left,
                        modifiers: Modifiers::none(),
                    },
                    &mut ctx,
                ),
                EventResult::Handled
            );
        }

        let dragged_ratio = root.dock().ratio();
        assert!(dragged_ratio > 0.4);

        root.set_models(SelfHostedPanelModels::demo());

        assert!((root.dock().ratio() - dragged_ratio).abs() < f32::EPSILON);
    }
}
