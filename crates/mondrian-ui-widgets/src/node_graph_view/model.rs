use mondrian_ui_core::types::{Point, Rect};

use super::{
    NodeGraphNode, HEADER_HEIGHT, NODE_GAP_X, NODE_GAP_Y, NODE_HEIGHT, NODE_WIDTH, PADDING,
    PORT_SIZE,
};

pub(super) fn graph_rect(bounds: Rect) -> Rect {
    Rect::new(
        bounds.x,
        bounds.y + HEADER_HEIGHT,
        bounds.width,
        (bounds.height - HEADER_HEIGHT).max(0.0),
    )
}

pub(super) fn layout_node_rects(bounds: Rect, count: usize) -> Vec<Rect> {
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

pub(super) fn node_rect_by_id(node_rects: &[(String, Rect)], id: &str) -> Option<Rect> {
    node_rects.iter().find_map(|(node_id, rect)| (node_id == id).then_some(*rect))
}

pub(super) fn node_id_at(node_rects: &[(String, Rect)], position: Point) -> Option<String> {
    node_rects
        .iter()
        .find_map(|(id, rect)| rect.contains(position).then(|| id.clone()))
}

pub(super) fn selected_index(
    nodes: &[NodeGraphNode],
    selected_node_id: Option<&str>,
) -> Option<usize> {
    selected_node_id.and_then(|selected| nodes.iter().position(|node| node.id == selected))
}

pub(super) fn step_selection(
    node_count: usize,
    selected_index: Option<usize>,
    direction: i32,
) -> Option<usize> {
    if node_count == 0 {
        return None;
    }
    Some(match selected_index {
        Some(current) if direction < 0 => (current + node_count - 1) % node_count,
        Some(current) => (current + 1) % node_count,
        None if direction < 0 => node_count - 1,
        None => 0,
    })
}

pub(super) fn edge_node_index(node_count: usize, last: bool) -> Option<usize> {
    if last {
        node_count.checked_sub(1)
    } else {
        (node_count > 0).then_some(0)
    }
}

pub(super) fn edge_segments(from: Rect, to: Rect) -> [(Point, Point); 3] {
    let start = Point::new(from.x + from.width, from.y + from.height * 0.5);
    let end = Point::new(to.x, to.y + to.height * 0.5);
    let mid_x = start.x + (end.x - start.x) * 0.5;
    [
        (start, Point::new(mid_x, start.y)),
        (Point::new(mid_x, start.y), Point::new(mid_x, end.y)),
        (Point::new(mid_x, end.y), end),
    ]
}

pub(super) fn port_rects(rect: Rect) -> [Rect; 2] {
    let y = rect.y + rect.height * 0.5 - PORT_SIZE * 0.5;
    [
        Rect::new(rect.x - PORT_SIZE * 0.5, y, PORT_SIZE, PORT_SIZE),
        Rect::new(
            rect.x + rect.width - PORT_SIZE * 0.5,
            y,
            PORT_SIZE,
            PORT_SIZE,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> NodeGraphNode {
        NodeGraphNode::new(id, id)
    }

    #[test]
    fn graph_rect_reserves_fixed_header_and_clamps_body_height() {
        assert_eq!(
            graph_rect(Rect::new(10.0, 20.0, 300.0, 120.0)),
            Rect::new(10.0, 62.0, 300.0, 78.0)
        );
        assert_eq!(
            graph_rect(Rect::new(0.0, 0.0, 100.0, 10.0)),
            Rect::new(0.0, 42.0, 100.0, 0.0)
        );
    }

    #[test]
    fn layout_node_rects_centers_single_row_and_wraps_constrained_widths() {
        let wide = layout_node_rects(Rect::new(0.0, 0.0, 520.0, 220.0), 2);

        assert_eq!(wide.len(), 2);
        assert_eq!(wide[0].y, wide[1].y);
        assert!(wide[1].x > wide[0].x);

        let narrow = layout_node_rects(Rect::new(0.0, 0.0, 180.0, 260.0), 3);
        assert_eq!(narrow.len(), 3);
        assert_eq!(narrow[0].x, narrow[1].x);
        assert!(narrow[1].y > narrow[0].y);
        assert_eq!(narrow[0].width, 142.0);
    }

    #[test]
    fn node_lookup_uses_stable_ids_and_screen_rects() {
        let rects = vec![
            ("source".to_owned(), Rect::new(10.0, 20.0, 80.0, 40.0)),
            ("output".to_owned(), Rect::new(120.0, 20.0, 80.0, 40.0)),
        ];

        assert_eq!(node_rect_by_id(&rects, "output"), Some(rects[1].1));
        assert_eq!(
            node_id_at(&rects, Point::new(30.0, 30.0)),
            Some("source".to_owned())
        );
        assert_eq!(node_id_at(&rects, Point::new(100.0, 30.0)), None);
    }

    #[test]
    fn selection_steps_wrap_and_edge_selection_handles_empty_graphs() {
        assert_eq!(step_selection(0, None, 1), None);
        assert_eq!(step_selection(3, None, 1), Some(0));
        assert_eq!(step_selection(3, None, -1), Some(2));
        assert_eq!(step_selection(3, Some(2), 1), Some(0));
        assert_eq!(step_selection(3, Some(0), -1), Some(2));
        assert_eq!(edge_node_index(0, false), None);
        assert_eq!(edge_node_index(3, false), Some(0));
        assert_eq!(edge_node_index(3, true), Some(2));
    }

    #[test]
    fn selected_index_resolves_current_node_id() {
        let nodes = vec![node("source"), node("effect"), node("output")];

        assert_eq!(selected_index(&nodes, Some("effect")), Some(1));
        assert_eq!(selected_index(&nodes, Some("missing")), None);
        assert_eq!(selected_index(&nodes, None), None);
    }

    #[test]
    fn edge_segments_and_ports_are_derived_from_node_rects() {
        let from = Rect::new(10.0, 20.0, 100.0, 40.0);
        let to = Rect::new(210.0, 80.0, 100.0, 40.0);
        let segments = edge_segments(from, to);
        let ports = port_rects(from);

        assert_eq!(segments[0].0, Point::new(110.0, 40.0));
        assert_eq!(segments[2].1, Point::new(210.0, 100.0));
        assert_eq!(ports[0], Rect::new(6.0, 36.0, 8.0, 8.0));
        assert_eq!(ports[1], Rect::new(106.0, 36.0, 8.0, 8.0));
    }
}
