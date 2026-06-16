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
    app_shell_quit_action, app_shell_save_project_as_dialog_action, APP_SHELL_IMPORT_MEDIA_DIALOG,
    APP_SHELL_NAMESPACE, APP_SHELL_SAVE_PROJECT_AS_DIALOG,
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
                    MenuItem::new("Project", Action::FocusPanel(PanelKind::Project)),
                    AppIcon::FolderOpenFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Console", Action::FocusPanel(PanelKind::Console)),
                    AppIcon::Info,
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
