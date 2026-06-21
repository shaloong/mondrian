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
use mondrian_ui_widgets::menu::{Dropdown, DropdownTriggerStyle, MenuItem};

use crate::app::ui_actions::{
    app_shell_about_action, app_shell_import_media_dialog_action,
    app_shell_new_project_dialog_action, app_shell_open_project_dialog_action,
    app_shell_preferences_action, app_shell_quit_action, app_shell_save_project_as_dialog_action,
    app_shell_sequence_settings_action, sequence_delete_action, sequence_duplicate_action,
    sequence_new_action, sequence_return_to_parent_action, sequence_set_active_default_action,
    sequence_switch_active_action, timeline_clear_in_out_points_action, SequenceTargetPayload,
};
use crate::app::AppState;
use crate::self_hosted::action_availability::app_state_action_enabled;
use crate::self_hosted::icons::AppIcon;
use crate::self_hosted::shortcuts::{
    shortcut_label_for_action, shortcut_label_for_action_with_overrides, SelfHostedShortcutOverride,
};
use crate::self_hosted::workspace_layout::SelfHostedWorkspaceLayout;

/// Height reserved for the self-hosted top menu bar.
pub const MENU_BAR_HEIGHT: f32 = 24.0;

const MENU_BAR_TRIGGER_GAP: f32 = 2.0;

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
                menu_item_with_icon(MenuItem::new("Duplicate", Action::Duplicate), AppIcon::Copy),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Select All", Action::SelectAll),
                    AppIcon::Cursor,
                ),
                menu_item_with_icon(
                    MenuItem::new("Deselect All", Action::DeselectAll),
                    AppIcon::CursorFilled,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Delete Selection", Action::DeleteSelection),
                    AppIcon::Trash,
                ),
                menu_item_with_icon(
                    MenuItem::new("Ripple Delete", Action::RippleDeleteSelection),
                    AppIcon::Trash,
                ),
                menu_item_with_icon(
                    MenuItem::new("Split Clip at Playhead", Action::SplitClipAtPlayhead),
                    AppIcon::Cut,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Mark In", Action::MarkInAtPlayhead),
                    AppIcon::BracketsLeft,
                ),
                menu_item_with_icon(
                    MenuItem::new("Mark Out", Action::MarkOutAtPlayhead),
                    AppIcon::BracketsRight,
                ),
                menu_item_with_icon(
                    MenuItem::new("Clear In/Out", timeline_clear_in_out_points_action()),
                    AppIcon::Stopwatch,
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
                    MenuItem::new("Viewer", Action::TogglePanel(PanelKind::Viewer)),
                    AppIcon::FullScreen,
                ),
                menu_item_with_icon(
                    MenuItem::new("Timeline", Action::TogglePanel(PanelKind::Timeline)),
                    AppIcon::Clock,
                ),
                menu_item_with_icon(
                    MenuItem::new("Inspector", Action::TogglePanel(PanelKind::Inspector)),
                    AppIcon::List,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new("Assets", Action::TogglePanel(PanelKind::Assets)),
                    AppIcon::Folder,
                ),
                menu_item_with_icon(
                    MenuItem::new("Effects", Action::TogglePanel(PanelKind::Effects)),
                    AppIcon::Effect,
                ),
                menu_item_with_icon(
                    MenuItem::new("Node Graph", Action::TogglePanel(PanelKind::NodeGraph)),
                    AppIcon::Grid,
                ),
                menu_item_with_icon(
                    MenuItem::new("Export", Action::TogglePanel(PanelKind::Export)),
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
            "Playback",
            vec![
                menu_item_with_icon(
                    MenuItem::new("Go to Start", Action::GoToStart),
                    AppIcon::HomeFrameFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Step Back", Action::StepBack),
                    AppIcon::LeftFrameFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Play/Pause", Action::TogglePlay),
                    AppIcon::PlayFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Step Forward", Action::StepForward),
                    AppIcon::RightFrameFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Go to End", Action::GoToEnd),
                    AppIcon::EndFrameFilled,
                ),
            ],
        ),
        (
            "Sequence",
            vec![
                menu_item_with_icon(
                    MenuItem::new("New Sequence", sequence_new_action()),
                    AppIcon::PlusFilled,
                ),
                menu_item_with_icon(
                    MenuItem::new("Sequence Settings...", app_shell_sequence_settings_action()),
                    AppIcon::Save,
                ),
                MenuItem::separator(),
                menu_item_with_icon(
                    MenuItem::new(
                        "Return to Parent Sequence",
                        sequence_return_to_parent_action(),
                    ),
                    AppIcon::CaretLeft,
                ),
                menu_item_with_icon(
                    MenuItem::new(
                        "Set Active as Default",
                        sequence_set_active_default_action(),
                    ),
                    AppIcon::HomeFrameFilled,
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

/// Default Mondrian menu structure with user shortcut overrides applied.
pub fn default_menu_items_with_shortcut_overrides(
    overrides: &[SelfHostedShortcutOverride],
) -> Vec<(&'static str, Vec<MenuItem>)> {
    apply_shortcut_overrides_to_menu_items(default_menu_items(), overrides)
}

/// Default Mondrian menu structure with application-state availability applied.
///
/// The semantic menu table stays stable; this adapter only disables rows that
/// cannot produce a useful editor action for the supplied state snapshot.
pub fn default_menu_items_for_app_state(state: &AppState) -> Vec<(&'static str, Vec<MenuItem>)> {
    default_menu_items_for_app_state_with_shortcut_overrides(state, &[])
}

/// Default menu structure with state availability and shortcut overrides.
pub fn default_menu_items_for_app_state_with_shortcut_overrides(
    state: &AppState,
    overrides: &[SelfHostedShortcutOverride],
) -> Vec<(&'static str, Vec<MenuItem>)> {
    let items = default_menu_items()
        .into_iter()
        .map(|(label, items)| {
            if label == "Sequence" {
                return (label, sequence_menu_items_for_app_state(state));
            }
            (
                label,
                items
                    .into_iter()
                    .map(|item| apply_app_state_menu_availability(item, state))
                    .collect(),
            )
        })
        .collect::<Vec<_>>();
    apply_shortcut_overrides_to_menu_items(items, overrides)
}

/// Apply shell-local checked state for panel visibility and workspace preset.
pub fn apply_shell_menu_checked_state(
    items: Vec<(&'static str, Vec<MenuItem>)>,
    workspace_preset: WorkspacePreset,
    workspace_layout: Option<&SelfHostedWorkspaceLayout>,
) -> Vec<(&'static str, Vec<MenuItem>)> {
    items
        .into_iter()
        .map(|(label, rows)| {
            let rows = rows
                .into_iter()
                .map(|item| {
                    apply_shell_menu_item_checked_state(item, workspace_preset, workspace_layout)
                })
                .collect();
            (label, rows)
        })
        .collect()
}

fn apply_shell_menu_item_checked_state(
    item: MenuItem,
    workspace_preset: WorkspacePreset,
    workspace_layout: Option<&SelfHostedWorkspaceLayout>,
) -> MenuItem {
    let checked = match &item.action {
        Action::TogglePanel(panel) => {
            workspace_layout.is_some_and(|layout| layout.contains_panel(*panel))
        }
        Action::SwitchWorkspace(preset) => workspace_preset == *preset,
        _ => return item,
    };
    item.checked(checked)
}

fn sequence_menu_items_for_app_state(state: &AppState) -> Vec<MenuItem> {
    let active_sequence_id = state.active_sequence_id;
    let mut items = vec![
        menu_item_with_icon(
            MenuItem::new("New Sequence", sequence_new_action()),
            AppIcon::PlusFilled,
        ),
        menu_item_with_icon(
            MenuItem::new("Sequence Settings...", app_shell_sequence_settings_action()),
            AppIcon::Save,
        ),
        MenuItem::separator(),
        menu_item_with_icon(
            MenuItem::new(
                "Return to Parent Sequence",
                sequence_return_to_parent_action(),
            ),
            AppIcon::CaretLeft,
        ),
        menu_item_with_icon(
            MenuItem::new(
                "Set Active as Default",
                sequence_set_active_default_action(),
            ),
            AppIcon::HomeFrameFilled,
        ),
    ];

    if let Some(sequence_id) = active_sequence_id {
        items.push(menu_item_with_icon(
            MenuItem::new(
                "Duplicate Active Sequence",
                sequence_duplicate_action(SequenceTargetPayload { sequence_id }),
            ),
            AppIcon::Copy,
        ));
        items.push(menu_item_with_icon(
            MenuItem::new(
                "Delete Active Sequence",
                sequence_delete_action(SequenceTargetPayload { sequence_id }),
            ),
            AppIcon::Trash,
        ));
    } else {
        items.push(menu_item_with_icon(
            MenuItem::new("Duplicate Active Sequence", Action::NoOp).disabled(),
            AppIcon::Copy,
        ));
        items.push(menu_item_with_icon(
            MenuItem::new("Delete Active Sequence", Action::NoOp).disabled(),
            AppIcon::Trash,
        ));
    }

    let sequences = state.export_sequences_snapshot();
    if !sequences.is_empty() {
        items.push(MenuItem::separator());
        for sequence in sequences {
            let is_active = Some(sequence.id) == active_sequence_id;
            let is_default = Some(sequence.id) == state.default_sequence_id;
            let mut label = sequence.name.clone();
            if is_default {
                label.push_str(" (Default)");
            }
            let icon = if is_active {
                AppIcon::CaretRight
            } else {
                AppIcon::Clock
            };
            items.push(menu_item_with_icon(
                MenuItem::new(
                    label,
                    sequence_switch_active_action(SequenceTargetPayload {
                        sequence_id: sequence.id,
                    }),
                ),
                icon,
            ));
        }
    }

    items
        .into_iter()
        .map(|item| apply_app_state_menu_availability(item, state))
        .collect()
}

fn apply_app_state_menu_availability(item: MenuItem, state: &AppState) -> MenuItem {
    if item.is_separator() || app_state_action_enabled(&item.action, state) {
        item
    } else {
        item.disabled()
    }
}

fn menu_item_with_icon(item: MenuItem, icon: AppIcon) -> MenuItem {
    let item = menu_item_with_shortcut(item);
    match icon.vector_icon() {
        Ok(icon) => item.with_icon(icon),
        Err(_) => item,
    }
}

fn menu_item_with_shortcut(item: MenuItem) -> MenuItem {
    let Some(shortcut) = shortcut_label_for_action(&item.action) else {
        return item;
    };
    item.with_shortcut(shortcut)
}

fn apply_shortcut_overrides_to_menu_items(
    items: Vec<(&'static str, Vec<MenuItem>)>,
    overrides: &[SelfHostedShortcutOverride],
) -> Vec<(&'static str, Vec<MenuItem>)> {
    items
        .into_iter()
        .map(|(label, rows)| {
            (
                label,
                rows.into_iter()
                    .map(|mut item| {
                        item.shortcut =
                            shortcut_label_for_action_with_overrides(&item.action, overrides);
                        item
                    })
                    .collect(),
            )
        })
        .collect()
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
        let menus = items
            .into_iter()
            .map(|(label, items)| {
                Dropdown::new(label, items).with_trigger_style(DropdownTriggerStyle::MenuBar)
            })
            .collect();
        Self { id: WidgetId::new(), menus, bounds: Rect::ZERO }
    }

    /// Build a menu bar whose rows reflect the current application state.
    pub fn for_app_state(state: &AppState) -> Self {
        Self::new(default_menu_items_for_app_state(state))
    }

    /// Build a menu bar from app state and user shortcut overrides.
    pub fn for_app_state_with_shortcut_overrides(
        state: &AppState,
        overrides: &[SelfHostedShortcutOverride],
    ) -> Self {
        Self::new(default_menu_items_for_app_state_with_shortcut_overrides(
            state, overrides,
        ))
    }

    /// Refresh shell-local checked state without rebuilding menu availability
    /// or shortcut labels.
    pub fn refresh_shell_checked_state(
        &mut self,
        workspace_preset: WorkspacePreset,
        workspace_layout: Option<&SelfHostedWorkspaceLayout>,
    ) {
        for panel in PanelKind::ALL {
            self.set_checked_for_action(
                &Action::TogglePanel(panel),
                workspace_layout.is_some_and(|layout| layout.contains_panel(panel)),
            );
        }
        for preset in [
            WorkspacePreset::Editing,
            WorkspacePreset::Color,
            WorkspacePreset::Audio,
            WorkspacePreset::Compositing,
            WorkspacePreset::Export,
        ] {
            self.set_checked_for_action(
                &Action::SwitchWorkspace(preset),
                workspace_preset == preset,
            );
        }
    }

    /// Current laid-out menu bar bounds.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Return checked state for the first row matching an action.
    #[cfg(test)]
    pub(crate) fn checked_for_action(&self, action: &Action) -> Option<bool> {
        self.menus.iter().find_map(|menu| menu.checked_for_action(action))
    }

    fn open_menu_index(&self) -> Option<usize> {
        self.menus.iter().position(Dropdown::is_open)
    }

    fn set_checked_for_action(&mut self, action: &Action, checked: bool) {
        for menu in &mut self.menus {
            menu.set_checked_for_action(action, checked);
        }
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
        if self.menus.is_empty() {
            return;
        }
        let mut x = bounds.x;
        for menu in &mut self.menus {
            if x >= bounds.x + bounds.width {
                menu.layout(Rect::new(x, bounds.y, 0.0, MENU_BAR_HEIGHT));
                continue;
            }
            let preferred = menu.measure(LayoutConstraint::LOOSE).width;
            let remaining = (bounds.x + bounds.width - x).max(0.0);
            let width = preferred.min(remaining);
            menu.layout(Rect::new(x, bounds.y, width, MENU_BAR_HEIGHT));
            x += width + MENU_BAR_TRIGGER_GAP;
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
        ctx.encoder.draw_rect(
            bar_bg,
            ctx.theme.colors.background.lerp(ctx.theme.colors.card, 0.42),
            0.0,
        );
        for menu in &self.menus {
            menu.paint(ctx);
        }
    }

    fn paint_overlay(&self, ctx: &mut PaintContext) {
        for menu in &self.menus {
            menu.paint_overlay(ctx);
        }
    }

    fn overlay_hit_test(&self, point: Point) -> bool {
        self.menus.iter().any(|menu| menu.overlay_hit_test(point))
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
    use crate::app::ui_actions::{
        APP_SHELL_ABOUT, APP_SHELL_NAMESPACE, APP_SHELL_QUIT, SEQUENCE_DELETE, SEQUENCE_DUPLICATE,
        SEQUENCE_NAMESPACE, SEQUENCE_SWITCH_ACTIVE, TIMELINE_CLEAR_IN_OUT_POINTS,
        TIMELINE_NAMESPACE,
    };
    use crate::app::SelectedClipRef;
    use crate::self_hosted::test_utils::{event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_core::automation::{timecode_to_ticks, Keyframe, PropertyMutation, PropertyValue};
    use mondrian_core::types::{AssetId, TimeCode, TrackId};
    use mondrian_timeline::clip::{Clip, Transform2D};
    use mondrian_timeline::sequence::Sequence;
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

    fn press_enter(menu: &mut MenuBar, ctx: &mut EventContext<'_>) -> EventResult {
        menu.event(
            &UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
            ctx,
        )
    }

    fn trigger_point(menu: &MenuBar, index: usize) -> Point {
        let right = (menu.bounds.x + menu.bounds.width).ceil() as i32;
        for x in menu.bounds.x.floor() as i32..=right {
            let point = Point::new(x as f32 + 0.5, menu.bounds.y + MENU_BAR_HEIGHT * 0.5);
            if menu.trigger_index_at(point) == Some(index) {
                return point;
            }
        }
        panic!("missing trigger point for menu index {index}");
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

    fn state_with_selected_clip() -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Edit");
        let tb = sequence.time_base();
        let track_id = sequence.video_tracks[0].id;
        let clip = Clip::new(AssetId::new(), TimeCode::new(10, tb), TimeCode::new(20, tb));
        let clip_id = clip.id;
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.sequence = Some(sequence);
        state.selection.selected_clips =
            vec![SelectedClipRef { track_id, is_video_track: true, clip_id }];
        state
    }

    #[test]
    fn default_menu_bar_exposes_primary_menu_groups() {
        let menu = MenuBar::default();

        assert_eq!(menu.child_count(), 7);
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
                view_items.iter().any(|item| item.action == Action::TogglePanel(panel)),
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
    fn shell_menu_checked_state_marks_visible_panels_and_current_workspace() {
        let layout = SelfHostedWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 0,
                hidden_tabs: Vec::new(),
            }),
            second: Box::new(SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Viewer,
                active_index: 0,
                hidden_tabs: Vec::new(),
            }),
        };

        let menu_items = apply_shell_menu_checked_state(
            default_menu_items(),
            WorkspacePreset::Editing,
            Some(&layout),
        );

        assert!(menu_item(&menu_items, "View", "Assets").checked);
        assert!(menu_item(&menu_items, "View", "Effects").checked);
        assert!(menu_item(&menu_items, "View", "Viewer").checked);
        assert!(!menu_item(&menu_items, "View", "Timeline").checked);
        assert!(menu_item(&menu_items, "Workspace", "Editing").checked);
        assert!(!menu_item(&menu_items, "Workspace", "Color").checked);
    }

    #[test]
    fn shell_menu_checked_state_reflects_hidden_grouped_tabs_in_custom_layout() {
        let layout = SelfHostedWorkspaceLayout::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Assets,
                active_index: 0,
                hidden_tabs: vec![PanelKind::Effects],
            }),
            second: Box::new(SelfHostedWorkspaceLayout::Panel {
                kind: PanelKind::Viewer,
                active_index: 0,
                hidden_tabs: Vec::new(),
            }),
        };

        let menu_items = apply_shell_menu_checked_state(
            default_menu_items(),
            WorkspacePreset::Custom,
            Some(&layout),
        );

        assert!(menu_item(&menu_items, "View", "Assets").checked);
        assert!(!menu_item(&menu_items, "View", "Effects").checked);
        assert!(menu_item(&menu_items, "View", "Viewer").checked);
        assert!(!menu_item(&menu_items, "Workspace", "Editing").checked);
        assert!(!menu_item(&menu_items, "Workspace", "Export").checked);
    }

    #[test]
    fn self_hosted_view_menu_excludes_project_browser_and_console_panels() {
        let menu_items = default_menu_items();
        let view_items = menu_items
            .iter()
            .find_map(|(label, items)| (*label == "View").then_some(items))
            .expect("view menu");

        let labels = view_items
            .iter()
            .filter(|item| !item.is_separator())
            .map(|item| item.label.as_str())
            .collect::<Vec<_>>();

        assert!(
            !labels.iter().any(|label| *label == "Project" || *label == "Console"),
            "self-hosted product panels should not reintroduce project-browser or console entries"
        );
        assert!(
            labels.contains(&"Assets"),
            "project media belongs in the Assets panel"
        );
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
            ("Edit", "Duplicate"),
            ("Edit", "Select All"),
            ("Edit", "Deselect All"),
            ("Edit", "Delete Selection"),
            ("Edit", "Ripple Delete"),
            ("Edit", "Split Clip at Playhead"),
            ("Edit", "Mark In"),
            ("Edit", "Mark Out"),
            ("Edit", "Clear In/Out"),
            ("Edit", "Preferences..."),
            ("View", "Timeline"),
            ("View", "Effects"),
            ("Playback", "Go to Start"),
            ("Playback", "Step Back"),
            ("Playback", "Play/Pause"),
            ("Playback", "Step Forward"),
            ("Playback", "Go to End"),
            ("Sequence", "New Sequence"),
            ("Sequence", "Return to Parent Sequence"),
            ("Sequence", "Set Active as Default"),
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
            ("Edit", "Duplicate", "Ctrl+D"),
            ("Edit", "Select All", "Ctrl+A"),
            ("Edit", "Deselect All", "Esc"),
            ("Edit", "Delete Selection", "Delete"),
            ("Edit", "Ripple Delete", "Shift+Delete"),
            ("Edit", "Split Clip at Playhead", "Ctrl+K"),
            ("Edit", "Mark In", "I"),
            ("Edit", "Mark Out", "O"),
            ("Playback", "Go to Start", "Home"),
            ("Playback", "Step Back", "Left"),
            ("Playback", "Step Forward", "Right"),
            ("Playback", "Go to End", "End"),
            ("View", "Toggle Fullscreen", "F11"),
        ] {
            assert_eq!(
                menu_item(&menu_items, menu_label, item_label).shortcut.as_deref(),
                Some(shortcut),
                "{menu_label}/{item_label} should show {shortcut}"
            );
        }

        for (panel_label, shortcut) in [
            ("Viewer", "Ctrl+Alt+V"),
            ("Timeline", "Ctrl+Alt+T"),
            ("Inspector", "Ctrl+Alt+I"),
            ("Assets", "Ctrl+Alt+A"),
            ("Effects", "Ctrl+Alt+E"),
            ("Node Graph", "Ctrl+Alt+G"),
            ("Export", "Ctrl+Alt+X"),
        ] {
            assert_eq!(
                menu_item(&menu_items, "View", panel_label).shortcut.as_deref(),
                Some(shortcut),
                "View/{panel_label} should show {shortcut}"
            );
        }

        for (workspace_label, shortcut) in [
            ("Editing", "Ctrl+Alt+1"),
            ("Color", "Ctrl+Alt+2"),
            ("Audio", "Ctrl+Alt+3"),
            ("Compositing", "Ctrl+Alt+4"),
            ("Export", "Ctrl+Alt+5"),
        ] {
            assert_eq!(
                menu_item(&menu_items, "Workspace", workspace_label).shortcut.as_deref(),
                Some(shortcut),
                "Workspace/{workspace_label} should show {shortcut}"
            );
        }
    }

    #[test]
    fn menu_shortcut_hints_follow_user_overrides() {
        let overrides = vec![
            SelfHostedShortcutOverride {
                id: "file.save_project".to_owned(),
                binding: Some(crate::self_hosted::shortcuts::SelfHostedShortcutBinding {
                    key: crate::self_hosted::shortcuts::SelfHostedShortcutKey::S,
                    ctrl: true,
                    alt: true,
                    shift: false,
                    meta: false,
                }),
            },
            SelfHostedShortcutOverride { id: "panel.inspector".to_owned(), binding: None },
        ];
        let menu_items = default_menu_items_with_shortcut_overrides(&overrides);

        assert_eq!(
            menu_item(&menu_items, "File", "Save").shortcut.as_deref(),
            Some("Ctrl+Alt+S")
        );
        assert_eq!(
            menu_item(&menu_items, "View", "Inspector").shortcut.as_deref(),
            None
        );
    }

    #[test]
    fn menu_bar_triggers_stay_inside_layout_bounds() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(100.0, 0.0, 500.0, MENU_BAR_HEIGHT));

        assert!(menu.trigger_index_at(Point::new(100.5, 12.0)).is_some());
        assert_eq!(menu.trigger_index_at(Point::new(600.5, 14.0)), None);
    }

    #[test]
    fn menu_bar_triggers_use_compact_content_widths() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(0.0, 0.0, 720.0, MENU_BAR_HEIGHT));

        let help_trigger = trigger_point(&menu, menu.child_count() - 1);

        assert!(
            help_trigger.x < 360.0,
            "menu triggers should not be evenly stretched across the title bar"
        );
        assert_eq!(menu.trigger_index_at(Point::new(560.0, 12.0)), None);
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
        assert!(!menu_item(&menu_items, "Edit", "Cut").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Copy").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Paste").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Duplicate").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Select All").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Deselect All").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Delete Selection").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Ripple Delete").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Split Clip at Playhead").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Mark In").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Mark Out").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Clear In/Out").enabled);
        assert!(!menu_item(&menu_items, "Playback", "Go to Start").enabled);
        assert!(!menu_item(&menu_items, "Playback", "Step Back").enabled);
        assert!(!menu_item(&menu_items, "Playback", "Play/Pause").enabled);
        assert!(!menu_item(&menu_items, "Playback", "Step Forward").enabled);
        assert!(!menu_item(&menu_items, "Playback", "Go to End").enabled);
        assert!(menu_item(&menu_items, "Sequence", "New Sequence").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Return to Parent Sequence").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Set Active as Default").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Duplicate Active Sequence").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Delete Active Sequence").enabled);
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
        assert!(!menu_item(&menu_items, "Sequence", "Return to Parent Sequence").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Set Active as Default").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Duplicate Active Sequence").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Delete Active Sequence").enabled);
    }

    #[test]
    fn app_state_menu_items_reflect_sequence_navigation_state() {
        let mut state = AppState::new();
        let parent = Sequence::new("Parent");
        let parent_id = parent.id;
        let child = Sequence::new("Child");
        let child_id = child.id;
        state.sequence = Some(child);
        state.active_sequence_id = Some(child_id);
        state.default_sequence_id = Some(parent_id);
        state.sequences.push(parent);
        state.sequence_navigation_stack.push(parent_id);

        let menu_items = default_menu_items_for_app_state(&state);
        assert!(menu_item(&menu_items, "Sequence", "Return to Parent Sequence").enabled);
        assert!(menu_item(&menu_items, "Sequence", "Set Active as Default").enabled);
        assert!(menu_item(&menu_items, "Sequence", "Duplicate Active Sequence").enabled);
        assert!(menu_item(&menu_items, "Sequence", "Delete Active Sequence").enabled);
        assert!(menu_item(&menu_items, "Sequence", "Child").enabled);
        assert!(menu_item(&menu_items, "Sequence", "Parent (Default)").enabled);

        state.default_sequence_id = Some(child_id);
        let menu_items = default_menu_items_for_app_state(&state);
        assert!(menu_item(&menu_items, "Sequence", "Return to Parent Sequence").enabled);
        assert!(!menu_item(&menu_items, "Sequence", "Set Active as Default").enabled);
    }

    #[test]
    fn app_state_menu_sequence_rows_emit_typed_sequence_actions() {
        let mut state = AppState::new();
        let first = Sequence::new("First");
        let first_id = first.id;
        let second = Sequence::new("Second");
        let second_id = second.id;
        state.sequence = Some(first.clone());
        state.active_sequence_id = Some(first_id);
        state.default_sequence_id = Some(first_id);
        state.sequences.push(first);
        state.sequences.push(second);

        let menu_items = default_menu_items_for_app_state(&state);
        let duplicate = menu_item(&menu_items, "Sequence", "Duplicate Active Sequence");
        let delete = menu_item(&menu_items, "Sequence", "Delete Active Sequence");
        let switch = menu_item(&menu_items, "Sequence", "Second");

        match &duplicate.action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, SEQUENCE_NAMESPACE);
                assert_eq!(name, SEQUENCE_DUPLICATE);
                let payload: SequenceTargetPayload =
                    serde_json::from_value(payload.clone()).expect("duplicate payload");
                assert_eq!(payload.sequence_id, first_id);
            }
            other => panic!("expected sequence duplicate action, got {other:?}"),
        }
        match &delete.action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, SEQUENCE_NAMESPACE);
                assert_eq!(name, SEQUENCE_DELETE);
                let payload: SequenceTargetPayload =
                    serde_json::from_value(payload.clone()).expect("delete payload");
                assert_eq!(payload.sequence_id, first_id);
            }
            other => panic!("expected sequence delete action, got {other:?}"),
        }
        match &switch.action {
            Action::Custom { namespace, name, payload } => {
                assert_eq!(namespace, SEQUENCE_NAMESPACE);
                assert_eq!(name, SEQUENCE_SWITCH_ACTIVE);
                let payload: SequenceTargetPayload =
                    serde_json::from_value(payload.clone()).expect("switch payload");
                assert_eq!(payload.sequence_id, second_id);
            }
            other => panic!("expected sequence switch action, got {other:?}"),
        }
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
    fn app_state_menu_items_enable_cut_copy_for_selected_clip() {
        let mut state = state_with_selected_clip();
        state.seek(15);

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(menu_item(&menu_items, "Edit", "Cut").enabled);
        assert!(menu_item(&menu_items, "Edit", "Copy").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Paste").enabled);
        assert!(menu_item(&menu_items, "Edit", "Duplicate").enabled);
        assert!(menu_item(&menu_items, "Edit", "Select All").enabled);
        assert!(menu_item(&menu_items, "Edit", "Deselect All").enabled);
        assert!(menu_item(&menu_items, "Edit", "Delete Selection").enabled);
        assert!(menu_item(&menu_items, "Edit", "Ripple Delete").enabled);
        assert!(menu_item(&menu_items, "Edit", "Split Clip at Playhead").enabled);
        assert!(menu_item(&menu_items, "Edit", "Mark In").enabled);
        assert!(menu_item(&menu_items, "Edit", "Mark Out").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Clear In/Out").enabled);
        assert!(menu_item(&menu_items, "Playback", "Go to Start").enabled);
        assert!(menu_item(&menu_items, "Playback", "Step Back").enabled);
        assert!(menu_item(&menu_items, "Playback", "Play/Pause").enabled);
        assert!(menu_item(&menu_items, "Playback", "Step Forward").enabled);
        assert!(menu_item(&menu_items, "Playback", "Go to End").enabled);
    }

    #[test]
    fn app_state_menu_items_enable_clear_in_out_only_for_active_range() {
        let mut state = state_with_selected_clip();

        let menu_items = default_menu_items_for_app_state(&state);
        assert!(!menu_item(&menu_items, "Edit", "Clear In/Out").enabled);

        state.sequence.as_mut().expect("sequence").mark_in(12);
        let menu_items = default_menu_items_for_app_state(&state);
        let clear = menu_item(&menu_items, "Edit", "Clear In/Out");
        assert!(clear.enabled);
        match &clear.action {
            Action::Custom { namespace, name, .. } => {
                assert_eq!(namespace, TIMELINE_NAMESPACE);
                assert_eq!(name, TIMELINE_CLEAR_IN_OUT_POINTS);
            }
            other => panic!("expected timeline clear in/out action, got {other:?}"),
        }

        state.sequence.as_mut().expect("sequence").clear_in_out();
        state.sequence.as_mut().expect("sequence").mark_out(18);
        let menu_items = default_menu_items_for_app_state(&state);
        assert!(menu_item(&menu_items, "Edit", "Clear In/Out").enabled);
    }

    #[test]
    fn app_state_menu_items_disable_cut_for_locked_selected_track() {
        let mut state = state_with_selected_clip();
        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(!menu_item(&menu_items, "Edit", "Cut").enabled);
        assert!(menu_item(&menu_items, "Edit", "Copy").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Duplicate").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Delete Selection").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Ripple Delete").enabled);
    }

    #[test]
    fn app_state_menu_items_disable_delete_for_stale_selected_track() {
        let mut state = state_with_selected_clip();
        state.selection.selected_clips.clear();
        state.selection.selected_track_ids = vec![TrackId::new()];

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(!app_state_action_enabled(&Action::DeleteSelection, &state));
        assert!(!app_state_action_enabled(
            &Action::RippleDeleteSelection,
            &state
        ));
        assert!(!menu_item(&menu_items, "Edit", "Delete Selection").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Ripple Delete").enabled);
    }

    #[test]
    fn app_state_menu_items_disable_delete_for_last_track_targets() {
        let mut state = AppState::new();
        let sequence = Sequence::new("single-track");
        let video_track_ids =
            sequence.video_tracks.iter().map(|track| track.id).collect::<Vec<_>>();
        state.sequence = Some(sequence);
        state.selection.selected_track_ids = video_track_ids;

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(!app_state_action_enabled(&Action::DeleteSelection, &state));
        assert!(!app_state_action_enabled(
            &Action::RippleDeleteSelection,
            &state
        ));
        assert!(!menu_item(&menu_items, "Edit", "Delete Selection").enabled);
        assert!(!menu_item(&menu_items, "Edit", "Ripple Delete").enabled);
    }

    #[test]
    fn app_state_menu_items_enable_delete_for_removable_selected_track() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("multi-track");
        let removable_track_id = sequence.add_video_track();
        state.sequence = Some(sequence);
        state.selection.selected_track_ids = vec![removable_track_id];

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(app_state_action_enabled(&Action::DeleteSelection, &state));
        assert!(app_state_action_enabled(
            &Action::RippleDeleteSelection,
            &state
        ));
        assert!(menu_item(&menu_items, "Edit", "Delete Selection").enabled);
        assert!(menu_item(&menu_items, "Edit", "Ripple Delete").enabled);
    }

    #[test]
    fn app_state_menu_items_enable_paste_after_copying_clip() {
        let mut state = state_with_selected_clip();
        state.copy_selected_clips_to_clipboard().expect("copy clip");

        let menu_items = default_menu_items_for_app_state(&state);

        assert!(state.can_paste_from_app_clipboard());
        assert!(menu_item(&menu_items, "Edit", "Paste").enabled);

        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;
        let menu_items = default_menu_items_for_app_state(&state);

        assert!(!state.can_paste_from_app_clipboard());
        assert!(!menu_item(&menu_items, "Edit", "Paste").enabled);
    }

    #[test]
    fn app_state_menu_items_disable_animation_paste_for_locked_target_track() {
        let mut state = state_with_selected_clip();
        let selection = state.primary_selected_clip().expect("selected clip");
        let tb = state.sequence.as_ref().expect("sequence").time_base();
        let key_time = timecode_to_ticks(TimeCode::new(12, tb));
        state
            .mutate_clip_property(
                selection,
                PropertyMutation::SetKeyframe {
                    path: Transform2D::OPACITY_PATH.to_owned(),
                    keyframe: Keyframe::linear(key_time, PropertyValue::Float(0.5)),
                },
                "seed opacity keyframe",
            )
            .expect("seed keyframe");
        state.set_animation_keyframe_selection(vec![crate::app::AnimationKeyframeSelection {
            clip_id: selection.clip_id,
            path: Transform2D::OPACITY_PATH.to_owned(),
            time: key_time,
        }]);
        assert!(state.copy_selected_animation_keyframes(selection).expect("copy keyframe"));

        let menu_items = default_menu_items_for_app_state(&state);
        assert!(state.can_paste_from_app_clipboard());
        assert!(menu_item(&menu_items, "Edit", "Paste").enabled);

        state.sequence.as_mut().expect("sequence").video_tracks[0].is_locked = true;
        let menu_items = default_menu_items_for_app_state(&state);

        assert!(!state.can_paste_from_app_clipboard());
        assert!(!menu_item(&menu_items, "Edit", "Paste").enabled);
    }

    #[test]
    fn menu_bar_switches_open_menu_on_trigger_click() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(0.0, 0.0, 720.0, MENU_BAR_HEIGHT));
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

        let file_trigger = trigger_point(&menu, 0);
        let help_trigger = trigger_point(&menu, menu.child_count() - 1);

        click_menu(&mut menu, &mut ctx, file_trigger);
        click_menu(&mut menu, &mut ctx, help_trigger);
        assert_eq!(press_enter(&mut menu, &mut ctx), EventResult::Handled);

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
    fn menu_bar_reports_open_dropdown_overlay() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(0.0, 0.0, 720.0, MENU_BAR_HEIGHT));
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

        let file_trigger = trigger_point(&menu, 0);
        click_menu(&mut menu, &mut ctx, file_trigger);

        assert!(menu.overlay_hit_test(Point::new(8.0, MENU_BAR_HEIGHT + 8.0)));
    }

    #[test]
    fn menu_bar_switches_open_menu_on_trigger_hover() {
        let mut menu = MenuBar::default();
        menu.layout(Rect::new(0.0, 0.0, 720.0, MENU_BAR_HEIGHT));
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

        let file_trigger = trigger_point(&menu, 0);
        let help_trigger = trigger_point(&menu, menu.child_count() - 1);

        click_menu(&mut menu, &mut ctx, file_trigger);

        assert_eq!(
            menu.event(
                &UiEvent::MouseMove {
                    position: help_trigger,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(press_enter(&mut menu, &mut ctx), EventResult::Handled);

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
