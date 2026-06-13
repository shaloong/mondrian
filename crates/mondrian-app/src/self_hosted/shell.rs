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
    app_shell_import_media_dialog_action, app_shell_open_project_dialog_action,
    app_shell_save_project_as_dialog_action, APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE,
    APP_SHELL_OPEN_PROJECT_DIALOG, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
};
use crate::self_hosted::panels::{build_demo_dock_tree, build_dock_tree, SelfHostedPanelModels};

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

/// Default Mondrian menu structure for self-hosted shells.
pub fn default_menu_items() -> Vec<(&'static str, Vec<MenuItem>)> {
    vec![
        (
            "File",
            vec![
                MenuItem::new("New Project", Action::NewProject),
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

/// Root widget for the self-hosted editor window.
pub struct SelfHostedAppRoot {
    id: WidgetId,
    menu_bar: MenuBar,
    dock: DockSplitter,
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
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if self.menu_bar.event(event, ctx) == EventResult::Handled {
            return EventResult::Handled;
        }
        self.dock.event(event, ctx)
    }

    fn paint(&self, ctx: &mut PaintContext) {
        self.menu_bar.paint(ctx);
        self.dock.paint(ctx);
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn child_count(&self) -> usize {
        2
    }

    fn child(&self, index: usize) -> Option<&dyn Widget> {
        match index {
            0 => Some(&self.menu_bar),
            1 => Some(&self.dock),
            _ => None,
        }
    }

    fn child_mut(&mut self, index: usize) -> Option<&mut dyn Widget> {
        match index {
            0 => Some(&mut self.menu_bar),
            1 => Some(&mut self.dock),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{
        app_shell_import_media_dialog_action, app_shell_open_project_dialog_action,
        app_shell_save_project_as_dialog_action,
    };
    use mondrian_editor_state::state::PanelKind;
    use mondrian_platform::NoopPlatformService;
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
