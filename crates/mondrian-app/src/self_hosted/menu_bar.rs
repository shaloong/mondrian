//! Self-hosted application menu bar.
//!
//! This module owns the product menu model, shortcut hints, state-aware row
//! availability, and the top-level menu bar widget used by the self-hosted
//! shell. Root layout, modal state, and native dialog resolution stay in
//! `self_hosted::shell`.

use mondrian_editor_state::state::{PanelKind, WorkspacePreset};
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};
use mondrian_ui_widgets::menu::{Dropdown, MenuItem};

use crate::app::ui_actions::{
    app_shell_about_action, app_shell_import_media_dialog_action,
    app_shell_new_project_dialog_action, app_shell_open_project_dialog_action,
    app_shell_preferences_action, app_shell_quit_action, app_shell_save_project_as_dialog_action,
    APP_SHELL_IMPORT_MEDIA_DIALOG, APP_SHELL_NAMESPACE, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
};
use crate::app::AppState;
use crate::self_hosted::icons::AppIcon;
use crate::self_hosted::shortcuts::shortcut_label_for_action;

/// Height reserved for the self-hosted top menu bar.
pub const MENU_BAR_HEIGHT: f32 = 28.0;

/// Default Mondrian menu structure for self-hosted shells.
pub fn default_menu_items() -> Vec<(&'static str, Vec<MenuItem>)> {
    vec![
        (
            "File",
            vec![
                menu_item_with_icon(
                    MenuItem::new("New Project...", app_shell_new_project_dialog_action()),
                    AppIcon::PlusFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Open Project...", app_shell_open_project_dialog_action()),
                    AppIcon::FolderOpenFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Import Media...", app_shell_import_media_dialog_action()),
                    AppIcon::Import,
                ),
                menu_item_with_icon(MenuItem::new("Save", Action::SaveProject), AppIcon::Save),
                menu_item_with_icon(
                    MenuItem::new("Save As...", app_shell_save_project_as_dialog_action()),
                    AppIcon::Save,
                ),
                MenuItem::separator(),
                menu_item_with_shortcut(MenuItem::new("Close Project", Action::CloseProject)),
                menu_item_with_shortcut(MenuItem::new("Quit", app_shell_quit_action())),
            ],
        ),
        (
            "Edit",
            vec![
                menu_item_with_icon(MenuItem::new("Undo", Action::Undo), AppIcon::Undo),
                menu_item_with_icon(MenuItem::new("Redo", Action::Redo), AppIcon::Redo),
                menu_item_with_icon(MenuItem::new("Cut", Action::Cut), AppIcon::Cut),
                menu_item_with_icon(MenuItem::new("Copy", Action::Copy), AppIcon::Copy),
                menu_item_with_icon(
                    MenuItem::new("Paste", Action::Paste),
                    AppIcon::ClipboardText,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Preferences...", app_shell_preferences_action()),
                    AppIcon::List,
                ),
            ],
        ),
        (
            "View",
            vec![
                menu_item_with_icon(
                    MenuItem::new("Viewer", Action::FocusPanel(PanelKind::Viewer)),
                    AppIcon::FullScreen,
                ),
                menu_item_with_icon(
                    MenuItem::new("Timeline", Action::FocusPanel(PanelKind::Timeline)),
                    AppIcon::Clock,
                ),
                menu_item_with_icon(
                    MenuItem::new("Inspector", Action::FocusPanel(PanelKind::Inspector)),
                    AppIcon::List,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Assets", Action::FocusPanel(PanelKind::Assets)),
                    AppIcon::Folder,
                ),
                menu_item_with_icon(
                    MenuItem::new("Effects", Action::FocusPanel(PanelKind::Effects)),
                    AppIcon::Effect,
                ),
                menu_item_with_icon(
                    MenuItem::new("Node Graph", Action::FocusPanel(PanelKind::NodeGraph)),
                    AppIcon::Grid,
                ),
                menu_item_with_icon(
                    MenuItem::new("Export", Action::FocusPanel(PanelKind::Export)),
                    AppIcon::Export,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Toggle Fullscreen", Action::ToggleFullscreen),
                    AppIcon::FullScreen,
                ),
            ],
        ),
        (
            "Workspace",
            vec![
                menu_item_with_icon(
                    MenuItem::new("Editing", Action::SwitchWorkspace(WorkspacePreset::Editing)),
                    AppIcon::Cursor,
                ),
                menu_item_with_icon(
                    MenuItem::new("Color", Action::SwitchWorkspace(WorkspacePreset::Color)),
                    AppIcon::Circle,
                ),
                menu_item_with_icon(
                    MenuItem::new("Audio", Action::SwitchWorkspace(WorkspacePreset::Audio)),
                    AppIcon::Music,
                ),
                menu_item_with_icon(
                    MenuItem::new(
                        "Compositing",
                        Action::SwitchWorkspace(WorkspacePreset::Compositing),
                    ),
                    AppIcon::Grid,
                ),
                menu_item_with_icon(
                    MenuItem::new("Export", Action::SwitchWorkspace(WorkspacePreset::Export)),
                    AppIcon::Export,
                ),
            ],
        ),
        (
            "Help",
            vec![menu_item_with_icon(
                MenuItem::new("About Mondrian", app_shell_about_action()),
                AppIcon::Info,
            )],
        ),
    ]
}

/// Default Mondrian menu structure with application-state availability applied.
///
/// The semantic menu table stays stable; this adapter only disables rows that
/// cannot produce a useful editor action for the supplied state snapshot.
pub fn default_menu_items_for_app_state(state: &AppState) -> Vec<(&'static str, Vec<MenuItem>)> {
    default_menu_items()
        .into_iter()
        .map(|(label, items)| {
            (
                label,
                items
                    .into_iter()
                    .map(|item| apply_app_state_menu_availability(item, state))
                    .collect(),
            )
        })
        .collect()
}

fn apply_app_state_menu_availability(item: MenuItem, state: &AppState) -> MenuItem {
    if item.is_separator() || app_state_action_enabled(&item.action, state) {
        item
    } else {
        item.disabled()
    }
}

/// Whether a shell-dispatched action can produce a useful editor operation for
/// the supplied application state snapshot.
pub fn app_state_action_enabled(action: &Action, state: &AppState) -> bool {
    match action {
        Action::SaveProject => state.has_open_project(),
        Action::SaveProjectAs(_) => state.sequence.is_some(),
        Action::CloseProject => state.sequence.is_some() || state.current_project_path.is_some(),
        Action::ImportMedia(_) => state.asset_library.is_some(),
        Action::Undo => state.can_undo_action(),
        Action::Redo => state.can_redo_action(),
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_IMPORT_MEDIA_DIALOG =>
        {
            state.asset_library.is_some()
        }
        Action::Custom { namespace, name, .. }
            if namespace == APP_SHELL_NAMESPACE && name == APP_SHELL_SAVE_PROJECT_AS_DIALOG =>
        {
            state.sequence.is_some()
        }
        _ => true,
    }
}

fn menu_item_with_icon(item: MenuItem, icon: AppIcon) -> MenuItem {
    menu_item_with_shortcut(item)
        .with_icon(icon.vector_icon().expect("bundled menu icon asset should parse"))
}

fn menu_item_with_shortcut(item: MenuItem) -> MenuItem {
    let Some(shortcut) = shortcut_label_for_action(&item.action) else {
        return item;
    };
    item.with_shortcut(shortcut)
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

    /// Build a menu bar whose rows reflect the current application state.
    pub fn for_app_state(state: &AppState) -> Self {
        Self::new(default_menu_items_for_app_state(state))
    }

    /// Current laid-out menu bar bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ui_actions::{APP_SHELL_ABOUT, APP_SHELL_QUIT};
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::EventRequests;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

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

    fn menu_item<'a>(
        menu_items: &'a [(&'static str, Vec<MenuItem>)],
        menu_label: &str,
        item_label: &str,
    ) -> &'a MenuItem {
        menu_items
            .iter()
            .find_map(|(label, items)| (*label == menu_label).then_some(items))
            .and_then(|items| items.iter().find(|item| item.label == item_label))
            .unwrap_or_else(|| panic!("missing {menu_label}/{item_label} menu item"))
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
    fn default_menu_items_use_semantic_vector_icons() {
        let menu_items = default_menu_items();

        for (menu_label, item_label) in [
            ("File", "New Project..."),
            ("File", "Save"),
            ("Edit", "Undo"),
            ("Edit", "Redo"),
            ("Edit", "Cut"),
            ("Edit", "Copy"),
            ("Edit", "Paste"),
            ("Edit", "Preferences..."),
            ("View", "Timeline"),
            ("View", "Effects"),
            ("Workspace", "Audio"),
            ("Help", "About Mondrian"),
        ] {
            assert!(
                menu_item(&menu_items, menu_label, item_label).icon.is_some(),
                "{menu_label}/{item_label} should carry a semantic icon"
            );
        }
    }

    #[test]
    fn default_menu_items_show_registered_shortcut_hints() {
        let menu_items = default_menu_items();

        for (menu_label, item_label, shortcut) in [
            ("File", "New Project...", "Ctrl+N"),
            ("File", "Open Project...", "Ctrl+O"),
            ("File", "Save", "Ctrl+S"),
            ("File", "Close Project", "Ctrl+W"),
            ("Edit", "Undo", "Ctrl+Z"),
            ("Edit", "Redo", "Ctrl+Shift+Z"),
            ("Edit", "Cut", "Ctrl+X"),
            ("Edit", "Copy", "Ctrl+C"),
            ("Edit", "Paste", "Ctrl+V"),
            ("View", "Timeline", "Ctrl+Alt+T"),
            ("View", "Toggle Fullscreen", "F11"),
            ("Workspace", "Editing", "Ctrl+Alt+1"),
        ] {
            assert_eq!(
                menu_item(&menu_items, menu_label, item_label).shortcut.as_deref(),
                Some(shortcut),
                "{menu_label}/{item_label} should show {shortcut}"
            );
        }
    }

    #[test]
    fn app_state_menu_items_disable_project_actions_without_project() {
        let state = AppState::new();
        let menu_items = default_menu_items_for_app_state(&state);

        assert!(menu_item(&menu_items, "File", "New Project...").enabled);
        assert!(menu_item(&menu_items, "File", "Open Project...").enabled);
        assert!(!menu_item(&menu_items, "File", "Import Media...").enabled);
        assert!(!menu_item(&menu_items, "File", "Save").enabled);
        assert!(!menu_item(&menu_items, "File", "Save As...").enabled);
        assert!(!menu_item(&menu_items, "File", "Close Project").enabled);
        assert!(menu_item(&menu_items, "File", "Quit").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Undo").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Redo").enabled);
    }

    #[test]
    fn app_state_menu_items_enable_draft_save_as_but_not_direct_save() {
        let mut state = AppState::new();
        state.sequence = Some(mondrian_timeline::sequence::Sequence::new("Draft"));

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(!menu_item(&menu_items, "File", "Import Media...").enabled);
        assert!(!menu_item(&menu_items, "File", "Save").enabled);
        assert!(menu_item(&menu_items, "File", "Save As...").enabled);
        assert!(menu_item(&menu_items, "File", "Close Project").enabled);
    }

    #[test]
    fn app_state_menu_items_enable_project_file_actions_for_open_project() {
        let mut state = AppState::new();
        state.sequence = Some(mondrian_timeline::sequence::Sequence::new("Edit"));
        state.current_project_path = Some(PathBuf::from("E:/projects/edit.mdp"));

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(menu_item(&menu_items, "File", "Save").enabled);
        assert!(menu_item(&menu_items, "File", "Save As...").enabled);
        assert!(menu_item(&menu_items, "File", "Close Project").enabled);
    }

    #[test]
    fn app_state_menu_items_track_undo_redo_history() {
        let mut state = AppState::new();
        let before = mondrian_timeline::sequence::Sequence::new("Edit");
        let mut after = before.clone();
        after.name = "Edit renamed".to_owned();
        state.sequence = Some(after.clone());
        state.cmd_history.record_executed(Box::new(
            mondrian_timeline::command::SequenceSnapshotCommand::new(
                "Rename sequence",
                before,
                after,
            ),
        ));

        let menu_items = default_menu_items_for_app_state(&state);
        assert!(menu_item(&menu_items, "Edit", "Undo").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Redo").enabled);

        state
            .cmd_history
            .undo(state.sequence.as_mut().expect("sequence"))
            .expect("undo should succeed");
        let menu_items = default_menu_items_for_app_state(&state);
        assert!(!menu_item(&menu_items, "Edit", "Undo").enabled);
        assert!(menu_item(&menu_items, "Edit", "Redo").enabled);
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
    fn default_menu_items_use_stable_app_shell_preferences_action() {
        let menu_items = default_menu_items();
        let preferences = menu_item(&menu_items, "Edit", "Preferences...");

        match &preferences.action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, APP_SHELL_NAMESPACE);
                assert_eq!(name, crate::app::ui_actions::APP_SHELL_PREFERENCES);
                assert!(payload.is_null());
            }
            other => panic!("expected app-shell preferences action, got {other:?}"),
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
                assert_eq!(name, APP_SHELL_QUIT);
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
}
