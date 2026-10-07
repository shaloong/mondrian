//! Shared popup menu behavior.
//!
//! Dropdowns and context menus have different anchors/triggers, but navigation
//! and explicit submenu entry share one interpretation.

use super::model::{menu_item_activation, MenuItem, MenuItemCommand, MenuItemKind};
use mondrian_ui_core::types::KeyCode;

pub(crate) fn root_hovered_index(hover_depth: Option<(usize, usize)>) -> Option<usize> {
    hover_depth.filter(|(depth, _)| *depth == 0).map(|(_, index)| index)
}

pub(crate) fn first_activatable_index(items: &[MenuItem]) -> Option<usize> {
    items.iter().position(MenuItem::is_activatable)
}

pub(crate) fn next_activatable_index(
    items: &[MenuItem],
    current_index: Option<usize>,
    direction: i32,
) -> Option<usize> {
    let activatable: Vec<usize> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| item.is_activatable().then_some(index))
        .collect();
    if activatable.is_empty() {
        return None;
    }
    let current = current_index
        .and_then(|index| activatable.iter().position(|candidate| *candidate == index));
    let next = match (current, direction) {
        (Some(index), d) if d < 0 => (index + activatable.len() - 1) % activatable.len(),
        (Some(index), _) => (index + 1) % activatable.len(),
        (None, d) if d < 0 => activatable.len() - 1,
        (None, _) => 0,
    };
    activatable.get(next).copied()
}

pub(crate) enum PopupKeyOutcome {
    Unhandled,
    Handled,
    Close,
    Activate(MenuItemCommand),
}

fn items_at_path<'a>(mut items: &'a [MenuItem], path: &[usize]) -> Option<&'a [MenuItem]> {
    for index in path {
        let MenuItemKind::Submenu { children } = &items.get(*index)?.kind else {
            return None;
        };
        items = children;
    }
    Some(items)
}

pub(crate) fn open_submenu_at(
    items: &[MenuItem],
    chain: &mut Vec<usize>,
    hovered: &mut Option<(usize, usize)>,
    depth: usize,
    index: usize,
) -> bool {
    let Some(path) = chain.get(..depth) else {
        return false;
    };
    let Some(item) = items_at_path(items, path).and_then(|rows| rows.get(index)) else {
        return false;
    };
    if !item.is_activatable() {
        return false;
    }
    let MenuItemKind::Submenu { children } = &item.kind else {
        return false;
    };
    let first = first_activatable_index(children);
    chain.truncate(depth);
    chain.push(index);
    *hovered = first.map(|index| (depth + 1, index));
    true
}

pub(crate) fn navigate_popup(
    items: &[MenuItem],
    chain: &mut Vec<usize>,
    hovered: &mut Option<(usize, usize)>,
    key: KeyCode,
) -> PopupKeyOutcome {
    if matches!(key, KeyCode::Left | KeyCode::Escape) {
        if let Some(index) = chain.pop() {
            *hovered = Some((chain.len(), index));
            return PopupKeyOutcome::Handled;
        }
        return if key == KeyCode::Escape {
            PopupKeyOutcome::Close
        } else {
            PopupKeyOutcome::Unhandled
        };
    }
    if !matches!(
        key,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::Right
            | KeyCode::Enter
            | KeyCode::Space
    ) {
        return PopupKeyOutcome::Unhandled;
    }
    let depth = hovered.map_or(chain.len(), |(depth, _)| depth);
    let Some(rows) = chain.get(..depth).and_then(|path| items_at_path(items, path)) else {
        return PopupKeyOutcome::Handled;
    };
    let current = hovered.filter(|(level, _)| *level == depth).map(|(_, index)| index);
    match key {
        KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
            let next = match key {
                KeyCode::Home => first_activatable_index(rows),
                KeyCode::End => rows.iter().rposition(MenuItem::is_activatable),
                _ => next_activatable_index(rows, current, if key == KeyCode::Up { -1 } else { 1 }),
            };
            chain.truncate(depth);
            *hovered = next.map(|index| (depth, index));
            PopupKeyOutcome::Handled
        }
        KeyCode::Right | KeyCode::Enter | KeyCode::Space => {
            let Some(index) = current else {
                return if key == KeyCode::Right && depth == 0 {
                    PopupKeyOutcome::Unhandled
                } else {
                    PopupKeyOutcome::Handled
                };
            };
            let Some(item) = rows.get(index).filter(|item| item.is_activatable()) else {
                return if key == KeyCode::Right && depth == 0 {
                    PopupKeyOutcome::Unhandled
                } else {
                    PopupKeyOutcome::Handled
                };
            };
            if item.is_submenu() {
                open_submenu_at(items, chain, hovered, depth, index);
                return PopupKeyOutcome::Handled;
            }
            if key == KeyCode::Right {
                return if depth == 0 {
                    PopupKeyOutcome::Unhandled
                } else {
                    PopupKeyOutcome::Handled
                };
            }
            menu_item_activation(item).map_or(PopupKeyOutcome::Handled, PopupKeyOutcome::Activate)
        }
        _ => PopupKeyOutcome::Unhandled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_editor_state::Action;

    #[test]
    fn nested_keyboard_navigation_selects_exact_leaf_and_returns_one_level() {
        let items = vec![MenuItem::submenu(
            "More",
            vec![
                MenuItem::new("Disabled", Action::SaveProject).disabled(),
                MenuItem::separator(),
                MenuItem::submenu(
                    "Deep",
                    vec![
                        MenuItem::new("Cut", Action::Cut),
                        MenuItem::new("Paste", Action::Paste),
                    ],
                ),
            ],
        )];
        let mut chain = Vec::new();
        let mut hovered = Some((0, 0));
        for key in [
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::Down,
            KeyCode::Right,
            KeyCode::Left,
            KeyCode::Escape,
            KeyCode::Right,
            KeyCode::Right,
            KeyCode::End,
        ] {
            assert!(matches!(
                navigate_popup(&items, &mut chain, &mut hovered, key),
                PopupKeyOutcome::Handled
            ));
        }
        assert!(matches!(
            navigate_popup(&items, &mut chain, &mut hovered, KeyCode::Enter),
            PopupKeyOutcome::Activate(MenuItemCommand::Action(Action::Paste))
        ));
    }

    #[test]
    fn empty_disabled_and_invalid_submenus_cannot_activate_another_command() {
        let mut items = vec![
            MenuItem::new("Cut", Action::Cut),
            MenuItem::new("Copy", Action::Copy),
        ];
        let mut chain = Vec::new();
        let mut hovered = Some((0, 0));
        items[0].enabled = false;
        for key in [KeyCode::Enter, KeyCode::Space] {
            assert!(matches!(
                navigate_popup(&items, &mut chain, &mut hovered, key),
                PopupKeyOutcome::Handled
            ));
        }
        assert!(matches!(
            navigate_popup(&items, &mut chain, &mut hovered, KeyCode::Up),
            PopupKeyOutcome::Handled
        ));
        assert!(matches!(
            navigate_popup(&items, &mut chain, &mut hovered, KeyCode::Enter),
            PopupKeyOutcome::Activate(MenuItemCommand::Action(Action::Copy))
        ));
        let empty = vec![MenuItem::submenu("Empty", Vec::new())];
        chain.clear();
        hovered = Some((0, 0));
        assert!(matches!(
            navigate_popup(&empty, &mut chain, &mut hovered, KeyCode::Right),
            PopupKeyOutcome::Handled
        ));
        for key in [
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Enter,
            KeyCode::Space,
            KeyCode::Right,
        ] {
            assert!(matches!(
                navigate_popup(&empty, &mut chain, &mut hovered, key),
                PopupKeyOutcome::Handled
            ));
        }
        assert!(matches!(
            navigate_popup(&empty, &mut chain, &mut hovered, KeyCode::Left),
            PopupKeyOutcome::Handled
        ));
        assert!(matches!(
            navigate_popup(&empty, &mut chain, &mut hovered, KeyCode::Escape),
            PopupKeyOutcome::Close
        ));
        hovered = None;
        assert!(matches!(
            navigate_popup(&items, &mut chain, &mut hovered, KeyCode::Enter),
            PopupKeyOutcome::Handled
        ));
        assert!(matches!(
            navigate_popup(&items, &mut chain, &mut hovered, KeyCode::Space),
            PopupKeyOutcome::Handled
        ));
        chain.push(usize::MAX);
        hovered = Some((1, usize::MAX));
        assert!(matches!(
            navigate_popup(&items, &mut chain, &mut hovered, KeyCode::Enter),
            PopupKeyOutcome::Handled
        ));
        assert!(matches!(
            navigate_popup(&[], &mut Vec::new(), &mut None, KeyCode::Right),
            PopupKeyOutcome::Unhandled
        ));
    }

    #[test]
    fn popup_navigation_skips_disabled_and_separator_rows() {
        let items = vec![
            MenuItem::new("Copy", Action::Copy),
            MenuItem::new("Disabled", Action::Paste).disabled(),
            MenuItem::separator(),
            MenuItem::new("Cut", Action::Cut),
        ];

        assert_eq!(first_activatable_index(&items), Some(0));
        assert_eq!(next_activatable_index(&items, Some(0), 1), Some(3));
        assert_eq!(next_activatable_index(&items, Some(0), -1), Some(3));
        assert_eq!(next_activatable_index(&items, None, 1), Some(0));
    }
}
