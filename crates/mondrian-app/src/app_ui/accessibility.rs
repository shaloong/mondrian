//! macOS accessibility adapter for the existing Widget tree and event router.

use accesskit::{
    Action as AxAction, Affine, Node, NodeId, Rect as AxRect, Role, TreeId, TreeInfo, TreeUpdate,
};
use accesskit_winit::{Adapter, Event as AxEvent};
use mondrian_editor_state::Action;
use mondrian_ui_core::tree::WidgetTreeView;
use mondrian_ui_core::types::{
    EventResult, KeyCode, Modifiers, MouseButton, Rect, UiEvent, WidgetId,
};
use mondrian_ui_core::widget::{AccessibilityRole, AccessibilityValue};
use mondrian_ui_core::{Widget, WidgetTree};
use mondrian_ui_events::EventRouter;
use std::collections::{HashMap, HashSet};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use super::runtime::WinitUiRuntime;

const ROOT: NodeId = NodeId(0);

#[derive(Default)]
struct SemanticTree {
    ids: HashMap<WidgetId, NodeId>,
    widgets: HashMap<NodeId, WidgetId>,
    virtual_ids: HashMap<(WidgetId, u64), NodeId>,
    virtual_targets: HashMap<NodeId, (WidgetId, u64)>,
    next_id: u64,
}

impl SemanticTree {
    fn id_for(&mut self, widget: WidgetId) -> NodeId {
        *self.ids.entry(widget).or_insert_with(|| {
            self.next_id += 1;
            NodeId(self.next_id)
        })
    }

    fn id_for_virtual(&mut self, key: (WidgetId, u64)) -> NodeId {
        *self.virtual_ids.entry(key).or_insert_with(|| {
            self.next_id += 1;
            NodeId(self.next_id)
        })
    }

    fn snapshot(
        &mut self,
        root: &dyn Widget,
        focused: Option<WidgetId>,
        bounds: Rect,
        scale: f64,
    ) -> TreeUpdate {
        let mut nodes = Vec::new();
        let mut present = HashSet::new();
        let mut present_virtual = HashSet::new();
        let children = self.visit(root, bounds, &mut nodes, &mut present, &mut present_virtual);
        self.ids.retain(|widget, _| present.contains(widget));
        self.widgets.retain(|_, widget| present.contains(widget));
        self.virtual_ids.retain(|key, _| present_virtual.contains(key));
        self.virtual_targets.retain(|_, key| present_virtual.contains(key));

        let mut window = Node::new(Role::Window);
        window.set_label("Mondrian");
        window.set_bounds(ax_rect(bounds));
        window.set_transform(Affine::scale(scale));
        window.set_children(children);
        nodes.push((ROOT, window));

        TreeUpdate {
            focus: focused.and_then(|widget| self.ids.get(&widget).copied()).unwrap_or(ROOT),
            nodes,
            tree: Some(TreeInfo::new(ROOT)),
            tree_id: TreeId::ROOT,
        }
    }

    fn visit(
        &mut self,
        widget: &dyn Widget,
        clip: Rect,
        nodes: &mut Vec<(NodeId, Node)>,
        present: &mut HashSet<WidgetId>,
        present_virtual: &mut HashSet<(WidgetId, u64)>,
    ) -> Vec<NodeId> {
        let semantic = widget.accessibility();
        let visible_bounds = widget.accessibility_bounds().map(|bounds| bounds.intersection(&clip));
        if visible_bounds.is_some_and(|bounds| bounds.width <= 0.0 || bounds.height <= 0.0) {
            return Vec::new();
        }

        let child_clip = widget
            .child_hit_test_clip()
            .map(|child_clip| clip.intersection(&child_clip))
            .unwrap_or(clip);
        let mut children = Vec::new();
        for index in 0..widget.child_count() {
            if let Some(child) = widget.child(index) {
                children.extend(self.visit(child, child_clip, nodes, present, present_virtual));
            }
        }

        for region in widget.accessibility_virtual_children() {
            let bounds = region.bounds.intersection(&clip);
            if bounds.width <= 0.0 || bounds.height <= 0.0 {
                continue;
            }
            let key = (widget.id(), region.key);
            let id = self.id_for_virtual(key);
            present_virtual.insert(key);
            self.virtual_targets.insert(id, key);
            let mut node = Node::new(role(region.role));
            node.set_label(region.name);
            node.set_bounds(ax_rect(bounds));
            if region.enabled {
                node.add_action(AxAction::Click);
            } else {
                node.set_disabled();
            }
            nodes.push((id, node));
            children.push(id);
        }

        let Some(semantic) = semantic else {
            return children;
        };
        let id = self.id_for(semantic.widget_id);
        present.insert(semantic.widget_id);
        self.widgets.insert(id, semantic.widget_id);
        let mut node = Node::new(role(semantic.role));
        if let Some(name) = semantic.name {
            node.set_label(name);
        }
        if let Some(bounds) = visible_bounds {
            node.set_bounds(ax_rect(bounds));
        }
        if widget.child_hit_test_clip().is_some() {
            node.set_clips_children();
        }
        if semantic.state.disabled {
            node.set_disabled();
        }
        if semantic.state.read_only {
            node.set_read_only();
        }
        if semantic.role == AccessibilityRole::Checkbox
            && let Some(checked) = semantic.state.checked
        {
            node.set_toggled(checked.into());
        }
        if let Some(expanded) = semantic.state.expanded {
            node.set_expanded(expanded);
        }
        if let Some(selected) = semantic.state.selected {
            node.set_selected(selected);
        }
        if semantic.state.focusable && !semantic.state.disabled {
            node.add_action(AxAction::Focus);
            if matches!(
                semantic.role,
                AccessibilityRole::Button | AccessibilityRole::Checkbox | AccessibilityRole::Menu
            ) {
                node.add_action(AxAction::Click);
            }
            if matches!(
                semantic.role,
                AccessibilityRole::TextInput
                    | AccessibilityRole::MultilineTextInput
                    | AccessibilityRole::Slider
            ) && !semantic.state.read_only
            {
                node.add_action(AxAction::SetValue);
            }
        }
        if let Some(value) = semantic.value {
            match value {
                AccessibilityValue::Text(value) => node.set_value(value),
                AccessibilityValue::Number { value, min, max } => {
                    if semantic.role == AccessibilityRole::Slider {
                        node.set_numeric_value(f64::from(value));
                        node.set_min_numeric_value(f64::from(min));
                        node.set_max_numeric_value(f64::from(max));
                    } else {
                        node.set_description(format!("{value:.2}; range {min:.2} to {max:.2}"));
                    }
                }
                AccessibilityValue::Scroll { x, y, max_x, max_y } => {
                    node.set_scroll_x(f64::from(x));
                    node.set_scroll_y(f64::from(y));
                    node.set_scroll_x_max(f64::from(max_x));
                    node.set_scroll_y_max(f64::from(max_y));
                }
                AccessibilityValue::Color { r, g, b, a } => {
                    node.set_description(format!("RGBA {r:.3}, {g:.3}, {b:.3}, {a:.3}"));
                }
                AccessibilityValue::Timeline {
                    playhead_frame, track_count, clip_count, ..
                } => {
                    node.set_description(format!(
                        "Frame {playhead_frame}; {track_count} tracks; {clip_count} clips"
                    ));
                }
                AccessibilityValue::Collection { total_count, selected_count, .. } => {
                    node.set_description(format!("{total_count} items; {selected_count} selected"));
                }
                AccessibilityValue::Viewer { source_width, source_height, has_frame, .. } => {
                    node.set_description(format!(
                        "{source_width} by {source_height}; frame available: {has_frame}"
                    ));
                }
            }
        }
        node.set_children(children);
        nodes.push((id, node));
        vec![id]
    }
}

fn ax_rect(bounds: Rect) -> AxRect {
    AxRect::from_origin_size(
        (f64::from(bounds.x), f64::from(bounds.y)),
        (f64::from(bounds.width), f64::from(bounds.height)),
    )
}

fn role(role: AccessibilityRole) -> Role {
    match role {
        AccessibilityRole::Label => Role::Label,
        AccessibilityRole::Button => Role::Button,
        AccessibilityRole::Checkbox => Role::CheckBox,
        AccessibilityRole::Slider => Role::Slider,
        AccessibilityRole::TextInput => Role::TextInput,
        AccessibilityRole::MultilineTextInput => Role::MultilineTextInput,
        AccessibilityRole::Menu => Role::Menu,
        AccessibilityRole::MenuItem => Role::MenuItem,
        AccessibilityRole::ScrollView => Role::ScrollView,
        AccessibilityRole::Splitter => Role::Splitter,
        AccessibilityRole::Timeline => Role::Group,
        AccessibilityRole::Grid => Role::Grid,
        AccessibilityRole::Canvas => Role::Canvas,
        AccessibilityRole::ColorPicker => Role::ColorWell,
        AccessibilityRole::Group => Role::Group,
    }
}

/// One adapter and node identity map per native window generation.
pub struct MacAccessibility {
    adapter: Adapter,
    semantics: SemanticTree,
    last_nodes: HashMap<NodeId, Node>,
    initialized: bool,
}

impl MacAccessibility {
    pub fn new<E: From<AxEvent> + Send + 'static>(
        event_loop: &ActiveEventLoop,
        window: &Window,
        proxy: EventLoopProxy<E>,
    ) -> Self {
        let adapter = Adapter::with_event_loop_proxy(event_loop, window, proxy);
        Self {
            adapter,
            semantics: SemanticTree::default(),
            last_nodes: HashMap::new(),
            initialized: false,
        }
    }

    pub fn process_event(&mut self, window: &Window, event: &winit::event::WindowEvent) {
        self.adapter.process_event(window, event);
    }

    pub fn note_user_event(&mut self, event: &accesskit_winit::WindowEvent) {
        if matches!(
            event,
            accesskit_winit::WindowEvent::InitialTreeRequested
                | accesskit_winit::WindowEvent::AccessibilityDeactivated
        ) {
            self.initialized = false;
            self.last_nodes.clear();
        }
    }

    pub fn update(
        &mut self,
        window: &Window,
        root: &dyn Widget,
        focused: Option<WidgetId>,
        bounds: Rect,
    ) {
        self.adapter.update_if_active(|| {
            let full = self.semantics.snapshot(root, focused, bounds, window.scale_factor());
            let nodes = if self.initialized {
                full.nodes
                    .iter()
                    .filter(|(id, node)| self.last_nodes.get(id) != Some(node))
                    .cloned()
                    .collect()
            } else {
                full.nodes.clone()
            };
            self.last_nodes = full.nodes.into_iter().collect();
            let tree = (!self.initialized).then(|| TreeInfo::new(ROOT));
            self.initialized = true;
            TreeUpdate {
                nodes,
                tree,
                tree_id: TreeId::ROOT,
                focus: full.focus,
            }
        });
    }

    pub fn route_action(
        &self,
        request: &accesskit::ActionRequest,
        window: &Window,
        root: &mut dyn Widget,
        router: &mut EventRouter,
        runtime: &mut WinitUiRuntime,
        dispatch: &dyn Fn(Action),
    ) -> bool {
        if let Some(&(parent_id, key)) = self.semantics.virtual_targets.get(&request.target_node) {
            if request.action != AxAction::Click {
                return false;
            }
            let region = {
                let tree = WidgetTreeView::new(root);
                let Some(parent) = tree.get(parent_id) else {
                    return false;
                };
                let Some(region) = parent
                    .accessibility_virtual_children()
                    .into_iter()
                    .find(|region| region.key == key && region.enabled)
                else {
                    return false;
                };
                if !parent.hit_test(region.bounds.center())
                    && !parent.overlay_hit_test(region.bounds.center())
                {
                    return false;
                }
                region
            };
            let position = region.bounds.center();
            runtime.route_window_event(
                window,
                router,
                root,
                UiEvent::MouseMove { position, modifiers: Modifiers::none() },
                dispatch,
            );
            let down = runtime.route_window_event(
                window,
                router,
                root,
                UiEvent::MouseDown {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                dispatch,
            );
            let up = runtime.route_window_event(
                window,
                router,
                root,
                UiEvent::MouseUp {
                    position,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                dispatch,
            );
            return down == EventResult::Handled && up == EventResult::Handled;
        }
        let Some(&widget_id) = self.semantics.widgets.get(&request.target_node) else {
            return false;
        };
        let Some(semantic) =
            WidgetTreeView::new(root).get(widget_id).and_then(Widget::accessibility)
        else {
            return false;
        };
        if !semantic.state.focusable || semantic.state.disabled {
            return false;
        }
        if semantic.state.read_only && request.action == AxAction::SetValue {
            return false;
        }
        if !matches!(
            request.action,
            AxAction::Focus | AxAction::Click | AxAction::SetValue
        ) {
            return false;
        }
        if request.action == AxAction::Click
            && !matches!(
                semantic.role,
                AccessibilityRole::Button | AccessibilityRole::Checkbox | AccessibilityRole::Menu
            )
        {
            return false;
        }
        if request.action == AxAction::SetValue
            && !matches!(
                semantic.role,
                AccessibilityRole::TextInput
                    | AccessibilityRole::MultilineTextInput
                    | AccessibilityRole::Slider
            )
        {
            return false;
        }
        if request.action == AxAction::SetValue {
            match (&semantic.role, &semantic.value, &request.data) {
                (
                    AccessibilityRole::Slider,
                    Some(AccessibilityValue::Number { min, max, .. }),
                    Some(accesskit::ActionData::NumericValue(value)),
                ) if value.is_finite()
                    && *value >= f64::from(*min)
                    && *value <= f64::from(*max) => {}
                (
                    AccessibilityRole::TextInput | AccessibilityRole::MultilineTextInput,
                    _,
                    Some(accesskit::ActionData::Value(_)),
                ) => {}
                _ => return false,
            }
        }
        if !runtime.focus_accessibility_widget(window, router, root, widget_id, dispatch) {
            return false;
        }
        match request.action {
            AxAction::Focus => {}
            AxAction::Click => {
                for event in [
                    UiEvent::KeyDown { key: KeyCode::Enter, modifiers: Modifiers::none() },
                    UiEvent::KeyUp { key: KeyCode::Enter, modifiers: Modifiers::none() },
                ] {
                    runtime.route_window_event(window, router, root, event, dispatch);
                }
            }
            AxAction::SetValue => {
                if semantic.role == AccessibilityRole::Slider {
                    let Some(accesskit::ActionData::NumericValue(value)) = &request.data else {
                        return false;
                    };
                    if !value.is_finite() || *value < f32::MIN as f64 || *value > f32::MAX as f64 {
                        return false;
                    }
                    runtime.route_window_event(
                        window,
                        router,
                        root,
                        UiEvent::SetNumericValue(*value as f32),
                        dispatch,
                    );
                    return true;
                }
                let Some(accesskit::ActionData::Value(value)) = &request.data else {
                    return false;
                };
                runtime.route_window_event(
                    window,
                    router,
                    root,
                    UiEvent::KeyDown {
                        key: KeyCode::A,
                        modifiers: Modifiers { meta: true, ..Modifiers::none() },
                    },
                    dispatch,
                );
                let event = if value.is_empty() {
                    UiEvent::KeyDown {
                        key: KeyCode::Backspace,
                        modifiers: Modifiers::none(),
                    }
                } else {
                    UiEvent::TextInput(value.to_string())
                };
                runtime.route_window_event(window, router, root, event, dispatch);
            }
            _ => unreachable!(),
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_ui_widgets::button::Button;

    #[test]
    fn native_tree_keeps_logical_bounds_and_scales_once_at_window_root() {
        let mut button = Button::new("Play");
        let widget_id = button.id();
        button.layout(Rect::new(10.0, 20.0, 100.0, 30.0));
        let mut semantics = SemanticTree::default();
        let window_bounds = Rect::new(0.0, 0.0, 300.0, 200.0);

        let initial = semantics.snapshot(&button, None, window_bounds, 2.0);
        let native_id = semantics.ids[&widget_id];
        let window = initial.nodes.iter().find(|(id, _)| *id == ROOT).unwrap().1.clone();
        let control = initial.nodes.iter().find(|(id, _)| *id == native_id).unwrap().1.clone();
        assert_eq!(window.transform(), Some(&Affine::scale(2.0)));
        assert_eq!(window.children(), &[native_id]);
        assert_eq!(control.role(), Role::Button);
        assert_eq!(
            control.bounds(),
            Some(ax_rect(Rect::new(10.0, 20.0, 100.0, 30.0)))
        );

        let focused = semantics.snapshot(&button, Some(widget_id), window_bounds, 2.0);
        assert_eq!(focused.focus, native_id);
        assert_eq!(semantics.ids[&widget_id], native_id);
    }
}
