//! Domain-light Power Window projection and Viewer gesture controller.
//!
//! Geometry is normalized to the complete Viewer canvas. The controller owns
//! only an in-gesture preview; a completed drag emits one complete shape value
//! and the App authoring transaction remains the sole authority.

use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

use crate::paint::color_with_alpha;

const HANDLE_RADIUS: f32 = 4.5;
const HIT_RADIUS: f32 = 9.0;
const CURVE_STEPS: usize = 12;

/// One Bezier anchor and its relative incoming/outgoing handles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerPowerWindowBezierPoint {
    /// Normalized anchor position.
    pub position: [f32; 2],
    /// Relative incoming cubic handle.
    pub control_in: [f32; 2],
    /// Relative outgoing cubic handle.
    pub control_out: [f32; 2],
}

/// Power Window geometry presented and edited by the Viewer.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewerPowerWindowShape {
    /// Axis-aligned normalized rectangle.
    Rectangle {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        corner_radius: f32,
    },
    /// Axis-aligned normalized ellipse.
    Ellipse { center: [f32; 2], radii: [f32; 2] },
    /// Cubic Bezier path.
    Bezier {
        points: Vec<ViewerPowerWindowBezierPoint>,
        closed: bool,
    },
}

/// Immutable App projection for one selected Power Window.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerPowerWindow {
    /// Evaluated geometry at the current Clip-local author time.
    pub shape: ViewerPowerWindowShape,
    /// Whether Viewer gestures may author a replacement shape.
    pub editable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    Body,
    RectangleCorner(usize),
    EllipseCenter,
    EllipseRadiusX,
    EllipseRadiusY,
    BezierAnchor(usize),
    BezierControlIn(usize),
    BezierControlOut(usize),
}

#[derive(Debug, Clone)]
struct DragState {
    handle: Handle,
    origin_pointer: [f32; 2],
    origin_shape: ViewerPowerWindowShape,
}

/// Stateful gesture preview for one immutable author projection.
pub(crate) struct PowerWindowEditor {
    authored: ViewerPowerWindow,
    preview: ViewerPowerWindowShape,
    hovered: Option<Handle>,
    drag: Option<DragState>,
}

impl PowerWindowEditor {
    pub(crate) fn new(authored: ViewerPowerWindow) -> Self {
        Self {
            preview: authored.shape.clone(),
            authored,
            hovered: None,
            drag: None,
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(crate) fn pointer_down(&mut self, canvas: Rect, point: Point) -> bool {
        if !self.authored.editable || !canvas.contains(point) {
            return false;
        }
        let Some(handle) = self.hit_test(canvas, point) else {
            return false;
        };
        let origin_pointer = normalized_point(canvas, point);
        self.drag = Some(DragState {
            handle,
            origin_pointer,
            origin_shape: self.preview.clone(),
        });
        self.hovered = Some(handle);
        true
    }

    pub(crate) fn pointer_move(&mut self, canvas: Rect, point: Point) -> bool {
        if let Some(drag) = self.drag.clone() {
            let pointer = normalized_point(canvas, point);
            self.preview = dragged_shape(&drag, pointer);
            return true;
        }
        let hovered = self.hit_test(canvas, point);
        if hovered == self.hovered {
            return hovered.is_some();
        }
        self.hovered = hovered;
        true
    }

    pub(crate) fn pointer_up(
        &mut self,
        canvas: Rect,
        point: Point,
    ) -> Option<ViewerPowerWindowShape> {
        let drag = self.drag.take()?;
        self.preview = dragged_shape(&drag, normalized_point(canvas, point));
        self.hovered = self.hit_test(canvas, point);
        (self.preview != self.authored.shape).then(|| self.preview.clone())
    }

    pub(crate) fn cancel(&mut self) -> bool {
        let changed = self.drag.take().is_some() || self.hovered.take().is_some();
        self.preview = self.authored.shape.clone();
        changed
    }

    pub(crate) fn paint(&self, ctx: &mut PaintContext, canvas: Rect) {
        let accent = ctx.theme.colors.accent;
        let muted = color_with_alpha(ctx.theme.colors.foreground, 0.72);
        match &self.preview {
            ViewerPowerWindowShape::Rectangle { x, y, width, height, .. } => {
                let corners = rectangle_corners(*x, *y, *width, *height);
                paint_polyline(ctx, canvas, &corners, true, accent);
                for (index, point) in corners.into_iter().enumerate() {
                    paint_handle(
                        ctx,
                        canvas_point(canvas, point),
                        self.hovered == Some(Handle::RectangleCorner(index)),
                        accent,
                    );
                }
            }
            ViewerPowerWindowShape::Ellipse { center, radii } => {
                let points = ellipse_polyline(*center, *radii);
                paint_polyline(ctx, canvas, &points, true, accent);
                for (handle, point) in [
                    (Handle::EllipseCenter, *center),
                    (Handle::EllipseRadiusX, [center[0] + radii[0], center[1]]),
                    (Handle::EllipseRadiusY, [center[0], center[1] + radii[1]]),
                ] {
                    if handle != Handle::EllipseCenter {
                        ctx.encoder.draw_line(
                            canvas_point(canvas, *center),
                            canvas_point(canvas, point),
                            1.0,
                            muted,
                        );
                    }
                    paint_handle(
                        ctx,
                        canvas_point(canvas, point),
                        self.hovered == Some(handle),
                        accent,
                    );
                }
            }
            ViewerPowerWindowShape::Bezier { points, closed } => {
                let curve = bezier_polyline(points, *closed);
                paint_polyline(ctx, canvas, &curve, false, accent);
                for (index, point) in points.iter().enumerate() {
                    let anchor = point.position;
                    let control_in = add(anchor, point.control_in);
                    let control_out = add(anchor, point.control_out);
                    for (handle, control) in [
                        (Handle::BezierControlIn(index), control_in),
                        (Handle::BezierControlOut(index), control_out),
                    ] {
                        ctx.encoder.draw_line(
                            canvas_point(canvas, anchor),
                            canvas_point(canvas, control),
                            1.0,
                            muted,
                        );
                        paint_handle(
                            ctx,
                            canvas_point(canvas, control),
                            self.hovered == Some(handle),
                            muted,
                        );
                    }
                    paint_handle(
                        ctx,
                        canvas_point(canvas, anchor),
                        self.hovered == Some(Handle::BezierAnchor(index)),
                        accent,
                    );
                }
            }
        }
    }

    fn hit_test(&self, canvas: Rect, point: Point) -> Option<Handle> {
        let handles = shape_handles(&self.preview);
        handles
            .into_iter()
            .filter_map(|(handle, normalized)| {
                let distance = distance_squared(canvas_point(canvas, normalized), point);
                (distance <= HIT_RADIUS * HIT_RADIUS).then_some((handle, distance))
            })
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(handle, _)| handle)
            .or_else(|| {
                shape_contains_or_near(&self.preview, normalized_point(canvas, point))
                    .then_some(Handle::Body)
            })
    }
}

fn dragged_shape(drag: &DragState, pointer: [f32; 2]) -> ViewerPowerWindowShape {
    let delta = sub(pointer, drag.origin_pointer);
    let mut shape = drag.origin_shape.clone();
    match (&mut shape, drag.handle) {
        (ViewerPowerWindowShape::Rectangle { x, y, .. }, Handle::Body) => {
            *x += delta[0];
            *y += delta[1];
        }
        (
            ViewerPowerWindowShape::Rectangle { x, y, width, height, .. },
            Handle::RectangleCorner(index),
        ) => {
            let corners = rectangle_corners(*x, *y, *width, *height);
            let opposite = corners[(index + 2) % 4];
            *x = pointer[0].min(opposite[0]);
            *y = pointer[1].min(opposite[1]);
            *width = (pointer[0] - opposite[0]).abs();
            *height = (pointer[1] - opposite[1]).abs();
        }
        (ViewerPowerWindowShape::Ellipse { center, .. }, Handle::Body | Handle::EllipseCenter) => {
            center[0] += delta[0];
            center[1] += delta[1];
        }
        (ViewerPowerWindowShape::Ellipse { center, radii }, Handle::EllipseRadiusX) => {
            radii[0] = (pointer[0] - center[0]).abs();
        }
        (ViewerPowerWindowShape::Ellipse { center, radii }, Handle::EllipseRadiusY) => {
            radii[1] = (pointer[1] - center[1]).abs();
        }
        (ViewerPowerWindowShape::Bezier { points, .. }, Handle::Body) => {
            for point in points {
                point.position = add(point.position, delta);
            }
        }
        (ViewerPowerWindowShape::Bezier { points, .. }, Handle::BezierAnchor(index)) => {
            if let Some(point) = points.get_mut(index) {
                point.position = add(point.position, delta);
            }
        }
        (ViewerPowerWindowShape::Bezier { points, .. }, Handle::BezierControlIn(index)) => {
            if let Some(point) = points.get_mut(index) {
                point.control_in = sub(pointer, point.position);
            }
        }
        (ViewerPowerWindowShape::Bezier { points, .. }, Handle::BezierControlOut(index)) => {
            if let Some(point) = points.get_mut(index) {
                point.control_out = sub(pointer, point.position);
            }
        }
        _ => {}
    }
    shape
}

fn shape_handles(shape: &ViewerPowerWindowShape) -> Vec<(Handle, [f32; 2])> {
    match shape {
        ViewerPowerWindowShape::Rectangle { x, y, width, height, .. } => {
            rectangle_corners(*x, *y, *width, *height)
                .into_iter()
                .enumerate()
                .map(|(index, point)| (Handle::RectangleCorner(index), point))
                .collect()
        }
        ViewerPowerWindowShape::Ellipse { center, radii } => vec![
            (Handle::EllipseCenter, *center),
            (Handle::EllipseRadiusX, [center[0] + radii[0], center[1]]),
            (Handle::EllipseRadiusY, [center[0], center[1] + radii[1]]),
        ],
        ViewerPowerWindowShape::Bezier { points, .. } => points
            .iter()
            .enumerate()
            .flat_map(|(index, point)| {
                [
                    (Handle::BezierAnchor(index), point.position),
                    (
                        Handle::BezierControlIn(index),
                        add(point.position, point.control_in),
                    ),
                    (
                        Handle::BezierControlOut(index),
                        add(point.position, point.control_out),
                    ),
                ]
            })
            .collect(),
    }
}

fn shape_contains_or_near(shape: &ViewerPowerWindowShape, point: [f32; 2]) -> bool {
    match shape {
        ViewerPowerWindowShape::Rectangle { x, y, width, height, .. } => {
            point[0] >= *x && point[0] <= *x + *width && point[1] >= *y && point[1] <= *y + *height
        }
        ViewerPowerWindowShape::Ellipse { center, radii } => {
            if radii[0] <= f32::EPSILON || radii[1] <= f32::EPSILON {
                return false;
            }
            let dx = (point[0] - center[0]) / radii[0];
            let dy = (point[1] - center[1]) / radii[1];
            dx * dx + dy * dy <= 1.0
        }
        ViewerPowerWindowShape::Bezier { points, closed } => {
            let polygon = bezier_polyline(points, *closed);
            (*closed && point_in_polygon(point, &polygon))
                || polyline_distance_squared(point, &polygon) <= 0.0004
        }
    }
}

fn rectangle_corners(x: f32, y: f32, width: f32, height: f32) -> [[f32; 2]; 4] {
    [
        [x, y],
        [x + width, y],
        [x + width, y + height],
        [x, y + height],
    ]
}

fn ellipse_polyline(center: [f32; 2], radii: [f32; 2]) -> Vec<[f32; 2]> {
    (0..48)
        .map(|index| {
            let angle = index as f32 / 48.0 * std::f32::consts::TAU;
            [
                center[0] + radii[0] * angle.cos(),
                center[1] + radii[1] * angle.sin(),
            ]
        })
        .collect()
}

fn bezier_polyline(points: &[ViewerPowerWindowBezierPoint], closed: bool) -> Vec<[f32; 2]> {
    if points.len() < 2 {
        return points.iter().map(|point| point.position).collect();
    }
    let pair_count = if closed {
        points.len()
    } else {
        points.len() - 1
    };
    let mut output = Vec::with_capacity(pair_count * CURVE_STEPS + 1);
    output.push(points[0].position);
    for index in 0..pair_count {
        let next = (index + 1) % points.len();
        for step in 1..=CURVE_STEPS {
            output.push(cubic(
                points[index],
                points[next],
                step as f32 / CURVE_STEPS as f32,
            ));
        }
    }
    output
}

fn cubic(a: ViewerPowerWindowBezierPoint, b: ViewerPowerWindowBezierPoint, t: f32) -> [f32; 2] {
    let inverse = 1.0 - t;
    let p0 = a.position;
    let p1 = add(a.position, a.control_out);
    let p2 = add(b.position, b.control_in);
    let p3 = b.position;
    [
        p0[0] * inverse.powi(3)
            + 3.0 * p1[0] * inverse.powi(2) * t
            + 3.0 * p2[0] * inverse * t * t
            + p3[0] * t.powi(3),
        p0[1] * inverse.powi(3)
            + 3.0 * p1[1] * inverse.powi(2) * t
            + 3.0 * p2[1] * inverse * t * t
            + p3[1] * t.powi(3),
    ]
}

fn paint_polyline(
    ctx: &mut PaintContext,
    canvas: Rect,
    points: &[[f32; 2]],
    closed: bool,
    color: mondrian_core::Color,
) {
    for pair in points.windows(2) {
        ctx.encoder.draw_line(
            canvas_point(canvas, pair[0]),
            canvas_point(canvas, pair[1]),
            1.5,
            color,
        );
    }
    if closed && points.len() > 2 {
        ctx.encoder.draw_line(
            canvas_point(canvas, points[points.len() - 1]),
            canvas_point(canvas, points[0]),
            1.5,
            color,
        );
    }
}

fn paint_handle(ctx: &mut PaintContext, point: Point, hovered: bool, color: mondrian_core::Color) {
    let radius = if hovered {
        HANDLE_RADIUS + 1.5
    } else {
        HANDLE_RADIUS
    };
    ctx.encoder.draw_rect(
        Rect::new(
            point.x - radius,
            point.y - radius,
            radius * 2.0,
            radius * 2.0,
        ),
        if hovered {
            ctx.theme.colors.foreground
        } else {
            color
        },
        radius,
    );
}

fn canvas_point(canvas: Rect, point: [f32; 2]) -> Point {
    Point::new(
        canvas.x + point[0] * canvas.width,
        canvas.y + point[1] * canvas.height,
    )
}

fn normalized_point(canvas: Rect, point: Point) -> [f32; 2] {
    [
        (point.x - canvas.x) / canvas.width.max(f32::EPSILON),
        (point.y - canvas.y) / canvas.height.max(f32::EPSILON),
    ]
}

fn add(left: [f32; 2], right: [f32; 2]) -> [f32; 2] {
    [left[0] + right[0], left[1] + right[1]]
}

fn sub(left: [f32; 2], right: [f32; 2]) -> [f32; 2] {
    [left[0] - right[0], left[1] - right[1]]
}

fn distance_squared(left: Point, right: Point) -> f32 {
    (left.x - right.x).powi(2) + (left.y - right.y).powi(2)
}

fn point_in_polygon(point: [f32; 2], polygon: &[[f32; 2]]) -> bool {
    let mut inside = false;
    let mut previous = polygon.last().copied().unwrap_or(point);
    for current in polygon.iter().copied() {
        if ((current[1] > point[1]) != (previous[1] > point[1]))
            && point[0]
                < (previous[0] - current[0]) * (point[1] - current[1]) / (previous[1] - current[1])
                    + current[0]
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn polyline_distance_squared(point: [f32; 2], polyline: &[[f32; 2]]) -> f32 {
    polyline
        .windows(2)
        .map(|pair| point_segment_distance_squared(point, pair[0], pair[1]))
        .fold(f32::INFINITY, f32::min)
}

fn point_segment_distance_squared(point: [f32; 2], start: [f32; 2], end: [f32; 2]) -> f32 {
    let segment = sub(end, start);
    let length = segment[0] * segment[0] + segment[1] * segment[1];
    if length <= f32::EPSILON {
        let delta = sub(point, start);
        return delta[0] * delta[0] + delta[1] * delta[1];
    }
    let from_start = sub(point, start);
    let t = ((from_start[0] * segment[0] + from_start[1] * segment[1]) / length).clamp(0.0, 1.0);
    let nearest = [start[0] + segment[0] * t, start[1] + segment[1] * t];
    let delta = sub(point, nearest);
    delta[0] * delta[0] + delta[1] * delta[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_point_close(actual: [f32; 2], expected: [f32; 2]) {
        assert!(
            (actual[0] - expected[0]).abs() < 1.0e-6,
            "x: {actual:?} != {expected:?}"
        );
        assert!(
            (actual[1] - expected[1]).abs() < 1.0e-6,
            "y: {actual:?} != {expected:?}"
        );
    }

    #[test]
    fn rectangle_corner_drag_keeps_non_negative_extent() {
        let drag = DragState {
            handle: Handle::RectangleCorner(0),
            origin_pointer: [0.2, 0.2],
            origin_shape: ViewerPowerWindowShape::Rectangle {
                x: 0.2,
                y: 0.2,
                width: 0.4,
                height: 0.4,
                corner_radius: 0.0,
            },
        };
        let ViewerPowerWindowShape::Rectangle { x, y, width, height, .. } =
            dragged_shape(&drag, [0.8, 0.9])
        else {
            panic!("rectangle expected");
        };
        assert_point_close([x, y], [0.6, 0.6]);
        assert_point_close([width, height], [0.2, 0.3]);
    }

    #[test]
    fn bezier_anchor_drag_preserves_relative_handles() {
        let point = ViewerPowerWindowBezierPoint {
            position: [0.2, 0.3],
            control_in: [-0.1, 0.0],
            control_out: [0.1, 0.0],
        };
        let drag = DragState {
            handle: Handle::BezierAnchor(0),
            origin_pointer: point.position,
            origin_shape: ViewerPowerWindowShape::Bezier { points: vec![point], closed: true },
        };
        let ViewerPowerWindowShape::Bezier { points, .. } = dragged_shape(&drag, [0.4, 0.5]) else {
            panic!("Bezier expected");
        };
        assert_eq!(points[0].position, [0.4, 0.5]);
        assert_eq!(points[0].control_in, point.control_in);
        assert_eq!(points[0].control_out, point.control_out);
    }

    #[test]
    fn ellipse_radius_handles_edit_one_axis_without_moving_center() {
        let origin_shape =
            ViewerPowerWindowShape::Ellipse { center: [0.5, 0.5], radii: [0.2, 0.3] };
        for (handle, pointer, expected_radii) in [
            (Handle::EllipseRadiusX, [0.85, 0.1], [0.35, 0.3]),
            (Handle::EllipseRadiusY, [0.1, 0.9], [0.2, 0.4]),
        ] {
            let drag = DragState {
                handle,
                origin_pointer: [0.0, 0.0],
                origin_shape: origin_shape.clone(),
            };
            let ViewerPowerWindowShape::Ellipse { center, radii } = dragged_shape(&drag, pointer)
            else {
                panic!("ellipse expected");
            };
            assert_point_close(center, [0.5, 0.5]);
            assert_point_close(radii, expected_radii);
        }
    }

    #[test]
    fn bezier_control_drag_writes_relative_handle_only() {
        let point = ViewerPowerWindowBezierPoint {
            position: [0.4, 0.5],
            control_in: [-0.1, 0.0],
            control_out: [0.1, 0.0],
        };
        let drag = DragState {
            handle: Handle::BezierControlOut(0),
            origin_pointer: [0.5, 0.5],
            origin_shape: ViewerPowerWindowShape::Bezier { points: vec![point], closed: false },
        };
        let ViewerPowerWindowShape::Bezier { points, .. } = dragged_shape(&drag, [0.65, 0.7])
        else {
            panic!("Bezier expected");
        };
        assert_eq!(points[0].position, point.position);
        assert_eq!(points[0].control_in, point.control_in);
        assert_point_close(points[0].control_out, [0.25, 0.2]);
    }

    #[test]
    fn open_bezier_does_not_treat_implicit_polygon_interior_as_body() {
        let shape = ViewerPowerWindowShape::Bezier {
            points: vec![
                ViewerPowerWindowBezierPoint {
                    position: [0.2, 0.2],
                    control_in: [0.0, 0.0],
                    control_out: [0.0, 0.0],
                },
                ViewerPowerWindowBezierPoint {
                    position: [0.8, 0.2],
                    control_in: [0.0, 0.0],
                    control_out: [0.0, 0.0],
                },
                ViewerPowerWindowBezierPoint {
                    position: [0.5, 0.8],
                    control_in: [0.0, 0.0],
                    control_out: [0.0, 0.0],
                },
            ],
            closed: false,
        };

        assert!(!shape_contains_or_near(&shape, [0.5, 0.4]));
        assert!(shape_contains_or_near(&shape, [0.5, 0.2]));
    }

    #[test]
    fn polygon_hit_test_handles_edges_with_negative_vertical_direction() {
        let polygon = [[0.2, 0.2], [0.8, 0.2], [0.8, 0.8], [0.2, 0.8]];

        assert!(point_in_polygon([0.5, 0.5], &polygon));
        assert!(!point_in_polygon([0.1, 0.5], &polygon));
        assert!(!point_in_polygon([0.9, 0.5], &polygon));
    }

    #[test]
    fn readonly_editor_refuses_gesture_and_emits_no_shape() {
        let canvas = Rect::new(0.0, 0.0, 100.0, 100.0);
        let mut editor = PowerWindowEditor::new(ViewerPowerWindow {
            shape: ViewerPowerWindowShape::Rectangle {
                x: 0.2,
                y: 0.2,
                width: 0.4,
                height: 0.4,
                corner_radius: 0.0,
            },
            editable: false,
        });

        assert!(!editor.pointer_down(canvas, Point::new(20.0, 20.0)));
        assert!(!editor.is_dragging());
        assert_eq!(editor.pointer_up(canvas, Point::new(40.0, 40.0)), None);
    }
}
