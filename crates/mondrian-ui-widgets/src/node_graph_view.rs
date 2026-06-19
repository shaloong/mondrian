//! Domain-light node graph visualization widget.
//!
//! The widget owns screen-space layout, painting, and selection chrome for a
//! compact directed graph. Application layers map domain objects such as clips,
//! effects, masks, or render passes into [`NodeGraphNode`] and
//! [`NodeGraphEdge`] values without coupling this crate to editor state.

use mondrian_core::Color;
use mondrian_editor_state::Action;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, MouseButton, UiEvent, Widget};

use crate::paint::{color_with_alpha, mix_color, paint_focus_ring, soft_border};

const DEFAULT_WIDTH: f32 = 460.0;
const DEFAULT_HEIGHT: f32 = 260.0;
const HEADER_HEIGHT: f32 = 42.0;
const PADDING: f32 = 18.0;
const NODE_WIDTH: f32 = 142.0;
const NODE_HEIGHT: f32 = 72.0;
const NODE_GAP_X: f32 = 48.0;
const NODE_GAP_Y: f32 = 34.0;
const PORT_SIZE: f32 = 8.0;

/// A node shown by [`NodeGraphView`].
#[derive(Debug, Clone, PartialEq)]
pub struct NodeGraphNode {
    /// Stable application-facing node id.
    pub id: String,
    /// Primary text shown in the node.
    pub title: String,
    /// Secondary detail text shown below the title.
    pub subtitle: String,
    /// Optional semantic accent color for the node's leading strip.
    pub accent: Option<Color>,
    /// Whether the node is disabled in the backing graph.
    pub disabled: bool,
}

impl NodeGraphNode {
    /// Create a node with a stable id and title.
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            subtitle: String::new(),
            accent: None,
            disabled: false,
        }
    }

    /// Set secondary detail text.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set the semantic accent color.
    pub fn with_accent(mut self, accent: Color) -> Self {
        self.accent = Some(accent);
        self
    }

    /// Set whether the backing node is disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A directed connection between two node ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeGraphEdge {
    /// Source node id.
    pub from: String,
    /// Target node id.
    pub to: String,
}

impl NodeGraphEdge {
    /// Create a directed graph edge.
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self { from: from.into(), to: to.into() }
    }
}

/// Adapter that maps a selected node id to an editor [`Action`].
pub type NodeGraphSelectAction = dyn Fn(&str) -> Action;

/// Compact directed graph view for editor panels.
pub struct NodeGraphView {
    id: WidgetId,
    bounds: Rect,
    title: String,
    subtitle: String,
    nodes: Vec<NodeGraphNode>,
    edges: Vec<NodeGraphEdge>,
    selected_node_id: Option<String>,
    node_rects: Vec<(String, Rect)>,
    focused: bool,
    focus_visible: bool,
    enabled: bool,
    on_select: Option<Box<NodeGraphSelectAction>>,
}

impl NodeGraphView {
    /// Create a node graph view from explicit nodes and edges.
    pub fn new(nodes: Vec<NodeGraphNode>, edges: Vec<NodeGraphEdge>) -> Self {
        Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            title: "Node Graph".to_owned(),
            subtitle: String::new(),
            nodes,
            edges,
            selected_node_id: None,
            node_rects: Vec::new(),
            focused: false,
            focus_visible: false,
            enabled: true,
            on_select: None,
        }
    }

    /// Set whether the graph accepts pointer, keyboard, and focus input.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.set_enabled(enabled);
        self
    }

    /// Disable pointer, keyboard, and focus input.
    pub fn disabled(self) -> Self {
        self.enabled(false)
    }

    /// Whether the graph accepts user input.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set whether the graph accepts pointer, keyboard, and focus input.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.focused = false;
            self.focus_visible = false;
        }
    }

    /// Set the graph title.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the graph subtitle.
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = subtitle.into();
        self
    }

    /// Set the selected node by id.
    pub fn with_selected_node(mut self, node_id: impl Into<String>) -> Self {
        self.selected_node_id = Some(node_id.into());
        self
    }

    /// Dispatch an action when the user selects a node.
    pub fn on_select(mut self, action: impl Fn(&str) -> Action + 'static) -> Self {
        self.on_select = Some(Box::new(action));
        self
    }

    /// Number of nodes in the graph.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges in the graph.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Currently selected node id.
    pub fn selected_node_id(&self) -> Option<&str> {
        self.selected_node_id.as_deref()
    }

    fn graph_rect(&self) -> Rect {
        Rect::new(
            self.bounds.x,
            self.bounds.y + HEADER_HEIGHT,
            self.bounds.width,
            (self.bounds.height - HEADER_HEIGHT).max(0.0),
        )
    }

    fn node_rect_by_id(&self, id: &str) -> Option<Rect> {
        self.node_rects
            .iter()
            .find_map(|(node_id, rect)| (node_id == id).then_some(*rect))
    }

    fn node_id_at(&self, position: Point) -> Option<String> {
        self.node_rects
            .iter()
            .find_map(|(id, rect)| rect.contains(position).then(|| id.clone()))
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected_node_id
            .as_deref()
            .and_then(|selected| self.nodes.iter().position(|node| node.id == selected))
    }

    fn select_index_from_input(&mut self, index: usize, ctx: &mut EventContext) -> EventResult {
        let Some(node) = self.nodes.get(index) else {
            return EventResult::Ignored;
        };
        self.selected_node_id = Some(node.id.clone());
        if let Some(on_select) = &self.on_select {
            (ctx.dispatch)(on_select(&node.id));
        }
        ctx.request_repaint();
        EventResult::Handled
    }

    fn select_node_from_input(&mut self, node_id: String, ctx: &mut EventContext) -> EventResult {
        let Some(index) = self.nodes.iter().position(|node| node.id == node_id) else {
            return EventResult::Ignored;
        };
        self.select_index_from_input(index, ctx)
    }

    fn focus_from_pointer(&mut self, ctx: &mut EventContext) {
        self.focused = true;
        self.focus_visible = false;
        ctx.focus.request_focus(self.id);
    }

    fn step_selection(&mut self, direction: i32, ctx: &mut EventContext) -> EventResult {
        if self.nodes.is_empty() {
            return EventResult::Ignored;
        }
        let next = match self.selected_index() {
            Some(current) if direction < 0 => (current + self.nodes.len() - 1) % self.nodes.len(),
            Some(current) => (current + 1) % self.nodes.len(),
            None if direction < 0 => self.nodes.len() - 1,
            None => 0,
        };
        self.select_index_from_input(next, ctx)
    }

    fn relayout_nodes(&mut self) {
        let rects = layout_node_rects(self.graph_rect(), self.nodes.len());
        self.node_rects = self
            .nodes
            .iter()
            .zip(rects)
            .map(|(node, rect)| (node.id.clone(), rect))
            .collect();
    }
}

impl Widget for NodeGraphView {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
        self.relayout_nodes();
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        if !self.enabled {
            self.focused = false;
            self.focus_visible = false;
            return EventResult::Ignored;
        }

        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. } => {
                self.focus_visible = false;
                let Some(node_id) = self.node_id_at(*position) else {
                    return EventResult::Ignored;
                };
                self.focus_from_pointer(ctx);
                self.select_node_from_input(node_id, ctx)
            }
            UiEvent::KeyDown { key: KeyCode::Right | KeyCode::Down, .. } if self.focused => {
                self.step_selection(1, ctx)
            }
            UiEvent::KeyDown { key: KeyCode::Left | KeyCode::Up, .. } if self.focused => {
                self.step_selection(-1, ctx)
            }
            UiEvent::KeyDown { key: KeyCode::Enter | KeyCode::Space, .. } if self.focused => {
                let index = self.selected_index().unwrap_or(0);
                self.select_index_from_input(index, ctx)
            }
            UiEvent::FocusGained => {
                self.focused = true;
                self.focus_visible = true;
                EventResult::Handled
            }
            UiEvent::FocusLost => {
                self.focused = false;
                self.focus_visible = false;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let typography = &ctx.theme.typography;
        let alpha = if self.enabled { 1.0 } else { 0.55 };

        ctx.encoder
            .draw_rect(self.bounds, color_with_alpha(colors.background, alpha), 0.0);
        if self.focus_visible {
            paint_focus_ring(ctx, self.bounds, spacing.radius_md);
        }
        ctx.encoder.draw_text_box(
            &self.title,
            typography.body.font_size,
            Point::new(self.bounds.x + 14.0, self.bounds.y + 12.0),
            (self.bounds.width - 28.0).max(32.0),
            color_with_alpha(colors.foreground, alpha),
        );
        if !self.subtitle.is_empty() {
            ctx.encoder.draw_text_box(
                &self.subtitle,
                typography.small.font_size,
                Point::new(self.bounds.x + 126.0, self.bounds.y + 13.0),
                (self.bounds.width - 140.0).max(32.0),
                color_with_alpha(colors.muted_foreground, alpha),
            );
        }

        let graph_rect = self.graph_rect();
        ctx.encoder.draw_rect(
            graph_rect,
            color_with_alpha(mix_color(colors.card, colors.background, 0.35), alpha),
            0.0,
        );

        if self.nodes.is_empty() {
            ctx.encoder.draw_text_box(
                "No graph nodes",
                typography.body.font_size,
                Point::new(graph_rect.x + PADDING, graph_rect.y + PADDING),
                (graph_rect.width - PADDING * 2.0).max(32.0),
                color_with_alpha(colors.muted_foreground, alpha),
            );
            return;
        }

        for edge in &self.edges {
            let (Some(from), Some(to)) = (
                self.node_rect_by_id(&edge.from),
                self.node_rect_by_id(&edge.to),
            ) else {
                continue;
            };
            paint_edge(ctx, from, to);
        }

        for (node, (_, rect)) in self.nodes.iter().zip(self.node_rects.iter()) {
            let selected = self.selected_node_id.as_deref() == Some(node.id.as_str());
            let disabled_alpha = alpha * if node.disabled { 0.56 } else { 1.0 };
            let border = if selected {
                colors.ring
            } else {
                soft_border(colors.border)
            };
            let fill = if selected {
                mix_color(colors.popover, colors.primary, 0.12)
            } else {
                mix_color(colors.popover, colors.background, 0.08)
            };

            ctx.encoder.draw_rect(
                *rect,
                color_with_alpha(border, disabled_alpha),
                spacing.radius_md,
            );
            ctx.encoder.draw_rect(
                rect.inset(1.0, 1.0),
                color_with_alpha(fill, disabled_alpha),
                (spacing.radius_md - 1.0).max(0.0),
            );

            let accent = node.accent.unwrap_or(colors.primary);
            ctx.encoder.draw_rect(
                Rect::new(rect.x + 1.0, rect.y + 1.0, 4.0, rect.height - 2.0),
                color_with_alpha(accent, disabled_alpha),
                (spacing.radius_md - 1.0).max(0.0),
            );
            paint_ports(ctx, *rect, selected, disabled_alpha);

            let text_color = if node.disabled {
                colors.muted_foreground
            } else {
                colors.foreground
            };
            ctx.push_clip(rect.inset(12.0, 8.0));
            ctx.encoder.draw_text_box(
                &node.title,
                typography.body.font_size,
                Point::new(rect.x + 16.0, rect.y + 14.0),
                (rect.width - 32.0).max(24.0),
                color_with_alpha(text_color, disabled_alpha),
            );
            if !node.subtitle.is_empty() {
                ctx.encoder.draw_text_box(
                    &node.subtitle,
                    typography.small.font_size,
                    Point::new(rect.x + 16.0, rect.y + 42.0),
                    (rect.width - 32.0).max(24.0),
                    color_with_alpha(colors.muted_foreground, disabled_alpha),
                );
            }
            ctx.pop_clip();
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }

    fn can_focus(&self) -> bool {
        self.enabled && !self.nodes.is_empty()
    }
}

fn paint_edge(ctx: &mut PaintContext, from: Rect, to: Rect) {
    let colors = &ctx.theme.colors;
    let start = Point::new(from.x + from.width, from.y + from.height * 0.5);
    let end = Point::new(to.x, to.y + to.height * 0.5);
    let mid_x = start.x + (end.x - start.x) * 0.5;
    let color = color_with_alpha(colors.muted_foreground, 0.42);

    ctx.encoder.draw_line(start, Point::new(mid_x, start.y), 1.5, color);
    ctx.encoder.draw_line(
        Point::new(mid_x, start.y),
        Point::new(mid_x, end.y),
        1.5,
        color,
    );
    ctx.encoder.draw_line(Point::new(mid_x, end.y), end, 1.5, color);
}

fn paint_ports(ctx: &mut PaintContext, rect: Rect, selected: bool, alpha: f32) {
    let colors = &ctx.theme.colors;
    let fill = if selected {
        colors.primary
    } else {
        colors.muted_foreground
    };
    let y = rect.y + rect.height * 0.5 - PORT_SIZE * 0.5;
    ctx.encoder.draw_rect(
        Rect::new(rect.x - PORT_SIZE * 0.5, y, PORT_SIZE, PORT_SIZE),
        color_with_alpha(fill, alpha),
        PORT_SIZE * 0.5,
    );
    ctx.encoder.draw_rect(
        Rect::new(
            rect.x + rect.width - PORT_SIZE * 0.5,
            y,
            PORT_SIZE,
            PORT_SIZE,
        ),
        color_with_alpha(fill, alpha),
        PORT_SIZE * 0.5,
    );
}

fn layout_node_rects(bounds: Rect, count: usize) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }

    let inner = bounds.inset(PADDING, PADDING);
    let columns =
        (((inner.width + NODE_GAP_X) / (NODE_WIDTH + NODE_GAP_X)).floor() as usize).clamp(1, count);
    let rows = count.div_ceil(columns);
    let total_height = rows as f32 * NODE_HEIGHT + rows.saturating_sub(1) as f32 * NODE_GAP_Y;
    let start_y = inner.y + ((inner.height - total_height).max(0.0) * 0.5);
    let mut rects = Vec::with_capacity(count);

    for index in 0..count {
        let row = index / columns;
        let col = index % columns;
        let row_count = (count - row * columns).min(columns);
        let total_width =
            row_count as f32 * NODE_WIDTH + row_count.saturating_sub(1) as f32 * NODE_GAP_X;
        let start_x = inner.x + ((inner.width - total_width).max(0.0) * 0.5);
        rects.push(Rect::new(
            start_x + col as f32 * (NODE_WIDTH + NODE_GAP_X),
            start_y + row as f32 * (NODE_HEIGHT + NODE_GAP_Y),
            NODE_WIDTH.min(inner.width.max(1.0)),
            NODE_HEIGHT,
        ));
    }

    rects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn layout_node_rects_centers_single_row_nodes() {
        let rects = layout_node_rects(Rect::new(0.0, 0.0, 640.0, 220.0), 3);

        assert_eq!(rects.len(), 3);
        assert!(rects[0].x > 20.0);
        assert!(rects[1].x > rects[0].x);
        assert_eq!(rects[0].y, rects[1].y);
        assert_eq!(rects[1].y, rects[2].y);
    }

    #[test]
    fn layout_node_rects_wraps_when_width_is_constrained() {
        let rects = layout_node_rects(Rect::new(0.0, 0.0, 240.0, 260.0), 3);

        assert_eq!(rects.len(), 3);
        assert!(rects[1].y > rects[0].y);
        assert!(rects[2].y > rects[1].y);
    }

    #[test]
    fn node_graph_mouse_down_selects_node_and_dispatches_action() {
        let selected = Rc::new(RefCell::new(Vec::new()));
        let mut graph = NodeGraphView::new(
            vec![
                NodeGraphNode::new("source", "Source"),
                NodeGraphNode::new("output", "Output"),
            ],
            vec![NodeGraphEdge::new("source", "output")],
        )
        .on_select(|_id| Action::Play);
        graph.layout(Rect::new(0.0, 0.0, 420.0, 220.0));
        let target = graph.node_rect_by_id("output").expect("output rect").center();
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let dispatch = {
            let selected = Rc::clone(&selected);
            move |action| selected.borrow_mut().push(action)
        };
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        let result = graph.event(
            &UiEvent::MouseDown {
                position: target,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(result, EventResult::Handled);
        assert_eq!(graph.selected_node_id(), Some("output"));
        assert_eq!(selected.borrow().as_slice(), &[Action::Play]);
    }

    #[test]
    fn node_graph_pointer_selection_focuses_for_keyboard_navigation() {
        let selected = Rc::new(RefCell::new(Vec::new()));
        let mut graph = NodeGraphView::new(
            vec![
                NodeGraphNode::new("source", "Source"),
                NodeGraphNode::new("grade", "Grade"),
                NodeGraphNode::new("output", "Output"),
            ],
            vec![],
        )
        .on_select(|id| Action::OpenProject(std::path::PathBuf::from(id)));
        graph.layout(Rect::new(0.0, 0.0, 520.0, 240.0));
        let source = graph.node_rect_by_id("source").expect("source rect").center();
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let dispatch = {
            let selected = Rc::clone(&selected);
            move |action| selected.borrow_mut().push(action)
        };
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            graph.event(
                &UiEvent::MouseDown {
                    position: source,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            graph.event(
                &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );

        assert_eq!(graph.selected_node_id(), Some("grade"));
        let actions = selected.borrow();
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[0],
            Action::OpenProject(std::path::PathBuf::from("source"))
        );
        assert_eq!(
            actions[1],
            Action::OpenProject(std::path::PathBuf::from("grade"))
        );
    }

    #[test]
    fn node_graph_keyboard_navigation_selects_nodes_when_focused() {
        let selected = Rc::new(RefCell::new(Vec::new()));
        let mut graph = NodeGraphView::new(
            vec![
                NodeGraphNode::new("source", "Source"),
                NodeGraphNode::new("grade", "Grade"),
                NodeGraphNode::new("output", "Output"),
            ],
            vec![],
        )
        .on_select(|id| Action::OpenProject(std::path::PathBuf::from(id)));
        graph.layout(Rect::new(0.0, 0.0, 520.0, 240.0));
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let dispatch = {
            let selected = Rc::clone(&selected);
            move |action| selected.borrow_mut().push(action)
        };
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert_eq!(
            graph.event(&UiEvent::FocusGained, &mut ctx),
            EventResult::Handled
        );
        assert_eq!(
            graph.event(
                &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(graph.selected_node_id(), Some("source"));
        assert!(ctx.requests.repaint);

        assert_eq!(
            graph.event(
                &UiEvent::KeyDown { key: KeyCode::Right, modifiers: Modifiers::none() },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(graph.selected_node_id(), Some("grade"));

        let actions = selected.borrow();
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[0],
            Action::OpenProject(std::path::PathBuf::from("source"))
        );
        assert_eq!(
            actions[1],
            Action::OpenProject(std::path::PathBuf::from("grade"))
        );
    }

    #[test]
    fn disabled_node_graph_ignores_input_and_focus() {
        let selected = Rc::new(RefCell::new(Vec::new()));
        let mut graph = NodeGraphView::new(vec![NodeGraphNode::new("source", "Source")], vec![])
            .on_select(|_| Action::Play)
            .disabled();
        graph.layout(Rect::new(0.0, 0.0, 420.0, 220.0));
        let target = graph.node_rect_by_id("source").expect("source rect").center();
        let mut focus = DummyFocus;
        let mut shortcut = DummyShortcut;
        let mut tooltip = DummyTooltip;
        let dispatch = {
            let selected = Rc::clone(&selected);
            move |action| selected.borrow_mut().push(action)
        };
        let mut ctx = make_event_ctx(&mut focus, &mut shortcut, &mut tooltip, &dispatch);

        assert!(!graph.is_enabled());
        assert!(!graph.can_focus());
        assert_eq!(
            graph.event(
                &UiEvent::MouseDown {
                    position: target,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Ignored
        );
        assert_eq!(graph.selected_node_id(), None);
        assert!(selected.borrow().is_empty());
    }

    #[test]
    fn empty_node_graph_does_not_participate_in_focus_traversal() {
        let graph = NodeGraphView::new(Vec::new(), Vec::new());

        assert_eq!(graph.node_count(), 0);
        assert!(!graph.can_focus());
    }
}
