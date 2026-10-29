//! Read-only semantic projection of the shared menu model.

use super::{MenuItem, MenuItemKind};
use mondrian_ui_core::types::WidgetId;
use mondrian_ui_core::widget::{AccessibilityNode, AccessibilityRole, AccessibilityState};

pub(crate) struct MenuAccessibilityIds {
    id: WidgetId,
    children: Vec<Self>,
}

impl MenuAccessibilityIds {
    pub(crate) fn for_items(items: &[MenuItem]) -> Vec<Self> {
        items
            .iter()
            .map(|item| Self {
                id: WidgetId::new(),
                children: match &item.kind {
                    MenuItemKind::Submenu { children } => Self::for_items(children),
                    _ => Vec::new(),
                },
            })
            .collect()
    }
}

pub(crate) fn visit_menu_items(
    items: &[MenuItem],
    ids: &[MenuAccessibilityIds],
    parent: WidgetId,
    open_path: &[usize],
    hover: Option<(usize, usize)>,
    depth: usize,
    visitor: &mut dyn FnMut(AccessibilityNode),
) {
    for (index, (item, identity)) in items.iter().zip(ids).enumerate() {
        if item.is_separator() {
            continue;
        }
        let expanded = item.is_submenu().then_some(open_path.first() == Some(&index));
        let mut node = AccessibilityNode::new(identity.id, AccessibilityRole::MenuItem)
            .with_name(item.label.clone())
            .with_state(AccessibilityState {
                disabled: !item.is_enabled(),
                checked: item.checked,
                expanded,
                selected: Some(hover == Some((depth, index))),
                ..AccessibilityState::default()
            });
        node.parent_id = Some(parent);
        visitor(node);
        if expanded == Some(true)
            && let MenuItemKind::Submenu { children } = &item.kind
        {
            visit_menu_items(
                children,
                &identity.children,
                identity.id,
                &open_path[1..],
                hover,
                depth + 1,
                visitor,
            );
        }
    }
}

pub(crate) fn localize_menu_items(
    items: &mut [MenuItem],
    translate: &mut dyn FnMut(&str) -> String,
) {
    for item in items {
        if let Some(key) = &item.message_id {
            item.label = translate(key);
        }
        if let MenuItemKind::Submenu { children } = &mut item.kind {
            localize_menu_items(children, translate);
        }
    }
}
