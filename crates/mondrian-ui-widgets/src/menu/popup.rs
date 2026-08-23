//! Shared popup menu behavior.
//!
//! Dropdowns and context menus have different anchors/triggers, but root-level
//! keyboard navigation should stay identical.

use super::model::MenuItem;

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

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_editor_state::Action;

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
