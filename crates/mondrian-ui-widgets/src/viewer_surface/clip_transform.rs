//! Viewer-only gesture preview for a selected Clip transform.

use mondrian_ui_core::types::{Point, Rect};
use mondrian_ui_core::widget::PaintContext;

const HIT_RADIUS: f32 = 10.0;
const HANDLE_RADIUS: f32 = 4.5;

/// Evaluated Clip transform in Sequence pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerClipTransform {
    /// Sequence pixel position of the Clip anchor.
    pub position: [f32; 2],
    /// Relative scale in each source axis.
    pub scale: [f32; 2],
    /// Clockwise visual rotation in degrees.
    pub rotation_degrees: f32,
    /// Source pixel anchor used by the Clip transform.
    pub anchor: [f32; 2],
    /// Whether the selection may be edited at this author time.
    pub editable: bool,
}

/// One completed transform gesture, committed by the App through a stable parameter address.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewerClipTransformEdit {
    /// New absolute Sequence pixel position.
    Position([f32; 2]),
    /// New absolute scale.
    Scale([f32; 2]),
    /// New absolute rotation in degrees.
    Rotation(f32),
    /// New source anchor and compensating Sequence position in one edit.
    Anchor {
        anchor: [f32; 2],
        position: [f32; 2],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Handle {
    Move,
    Scale(usize),
    Rotate,
    Anchor,
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    handle: Handle,
    origin_pointer: Point,
    origin: ViewerClipTransform,
}

pub(crate) struct ClipTransformEditor {
    authored: ViewerClipTransform,
    preview: ViewerClipTransform,
    hover: Option<Handle>,
    drag: Option<Drag>,
}

impl ClipTransformEditor {
    pub(crate) fn new(authored: ViewerClipTransform) -> Self {
        Self {
            authored,
            preview: authored,
            hover: None,
            drag: None,
        }
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(crate) fn pointer_down(&mut self, canvas: Rect, source: [f32; 2], point: Point) -> bool {
        if !self.authored.editable
            || !valid_geometry(self.preview, canvas, source)
            || !canvas.contains(point)
        {
            return false;
        }
        let Some(handle) = self.hit_test(canvas, source, point) else {
            return false;
        };
        self.drag = Some(Drag {
            handle,
            origin_pointer: point,
            origin: self.preview,
        });
        self.hover = Some(handle);
        true
    }

    pub(crate) fn pointer_move(&mut self, canvas: Rect, source: [f32; 2], point: Point) -> bool {
        if let Some(drag) = self.drag {
            self.preview = dragged_transform(drag, canvas, source, point);
            return true;
        }
        let hover = self.hit_test(canvas, source, point);
        let changed = hover != self.hover;
        self.hover = hover;
        changed || hover.is_some()
    }

    pub(crate) fn pointer_up(
        &mut self,
        canvas: Rect,
        source: [f32; 2],
        point: Point,
    ) -> Option<ViewerClipTransformEdit> {
        let drag = self.drag.take()?;
        self.preview = dragged_transform(drag, canvas, source, point);
        self.hover = self.hit_test(canvas, source, point);
        match drag.handle {
            Handle::Move if self.preview.position != self.authored.position => {
                Some(ViewerClipTransformEdit::Position(self.preview.position))
            }
            Handle::Scale(_) if self.preview.scale != self.authored.scale => {
                Some(ViewerClipTransformEdit::Scale(self.preview.scale))
            }
            Handle::Rotate if self.preview.rotation_degrees != self.authored.rotation_degrees => {
                Some(ViewerClipTransformEdit::Rotation(
                    self.preview.rotation_degrees,
                ))
            }
            Handle::Anchor if self.preview.anchor != self.authored.anchor => {
                Some(ViewerClipTransformEdit::Anchor {
                    anchor: self.preview.anchor,
                    position: self.preview.position,
                })
            }
            _ => {
                self.preview = self.authored;
                None
            }
        }
    }

    pub(crate) fn cancel(&mut self) -> bool {
        let changed = self.drag.take().is_some() || self.hover.take().is_some();
        self.preview = self.authored;
        changed
    }

    pub(crate) fn paint(&self, ctx: &mut PaintContext, canvas: Rect, source: [f32; 2]) {
        if !valid_geometry(self.preview, canvas, source) {
            return;
        }
        let corners = corners(self.preview, canvas, source);
        let accent = ctx.theme.colors.accent;
        for index in 0..4 {
            ctx.encoder.draw_line(corners[index], corners[(index + 1) % 4], 1.4, accent);
        }
        for (index, point) in corners.into_iter().enumerate() {
            let radius = if self.hover == Some(Handle::Scale(index)) {
                HANDLE_RADIUS + 2.0
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
                accent,
                2.0,
            );
        }
        let rotation = rotation_handle(self.preview, canvas, source);
        let radius = if self.hover == Some(Handle::Rotate) {
            HANDLE_RADIUS + 2.0
        } else {
            HANDLE_RADIUS
        };
        ctx.encoder.draw_rect(
            Rect::new(
                rotation.x - radius,
                rotation.y - radius,
                radius * 2.0,
                radius * 2.0,
            ),
            accent,
            radius,
        );
        let anchor = transformed_point(self.preview, canvas, source, self.preview.anchor);
        let arm = if self.hover == Some(Handle::Anchor) {
            9.0
        } else {
            7.0
        };
        ctx.encoder.draw_line(
            Point::new(anchor.x - arm, anchor.y),
            Point::new(anchor.x + arm, anchor.y),
            1.5,
            accent,
        );
        ctx.encoder.draw_line(
            Point::new(anchor.x, anchor.y - arm),
            Point::new(anchor.x, anchor.y + arm),
            1.5,
            accent,
        );
    }

    fn hit_test(&self, canvas: Rect, source: [f32; 2], point: Point) -> Option<Handle> {
        if !self.authored.editable
            || !valid_geometry(self.preview, canvas, source)
            || !canvas.contains(point)
        {
            return None;
        }
        let anchor = transformed_point(self.preview, canvas, source, self.preview.anchor);
        if near(point, anchor) && self.preview.scale.iter().all(|value| value.abs() >= 0.0001) {
            return Some(Handle::Anchor);
        }
        for (index, corner) in corners(self.preview, canvas, source).into_iter().enumerate() {
            if near(point, corner) {
                return Some(Handle::Scale(index));
            }
        }
        if near(point, rotation_handle(self.preview, canvas, source)) {
            return Some(Handle::Rotate);
        }
        contains_transformed_frame(self.preview, canvas, source, point).then_some(Handle::Move)
    }
}

fn valid_canvas(canvas: Rect, source: [f32; 2]) -> bool {
    canvas.width >= 40.0
        && canvas.height >= 40.0
        && source[0].is_finite()
        && source[1].is_finite()
        && source[0] > 0.0
        && source[1] > 0.0
}

fn valid_geometry(transform: ViewerClipTransform, canvas: Rect, source: [f32; 2]) -> bool {
    valid_canvas(canvas, source)
        && transform
            .position
            .into_iter()
            .chain(transform.scale)
            .chain(transform.anchor)
            .chain([transform.rotation_degrees])
            .all(f32::is_finite)
        && corners(transform, canvas, source)
            .into_iter()
            .all(|point| point.x.is_finite() && point.y.is_finite())
}

fn near(a: Point, b: Point) -> bool {
    (a.x - b.x).hypot(a.y - b.y) <= HIT_RADIUS
}

fn source_corner(index: usize, source: [f32; 2]) -> [f32; 2] {
    match index {
        0 => [0.0, 0.0],
        1 => [source[0], 0.0],
        2 => [source[0], source[1]],
        _ => [0.0, source[1]],
    }
}

fn transformed_point(
    transform: ViewerClipTransform,
    canvas: Rect,
    source: [f32; 2],
    local: [f32; 2],
) -> Point {
    let theta = transform.rotation_degrees.to_radians();
    let (sin, cos) = theta.sin_cos();
    let x = (local[0] - transform.anchor[0]) * transform.scale[0];
    let y = (local[1] - transform.anchor[1]) * transform.scale[1];
    let sequence_x = transform.position[0] + x * cos - y * sin;
    let sequence_y = transform.position[1] + x * sin + y * cos;
    Point::new(
        canvas.x + sequence_x * canvas.width / source[0],
        canvas.y + sequence_y * canvas.height / source[1],
    )
}

fn corners(transform: ViewerClipTransform, canvas: Rect, source: [f32; 2]) -> [Point; 4] {
    std::array::from_fn(|index| {
        transformed_point(transform, canvas, source, source_corner(index, source))
    })
}

fn rotation_handle(transform: ViewerClipTransform, canvas: Rect, source: [f32; 2]) -> Point {
    let top = transformed_point(transform, canvas, source, [source[0] * 0.5, 0.0]);
    let center = transformed_point(
        transform,
        canvas,
        source,
        [source[0] * 0.5, source[1] * 0.5],
    );
    let dx = center.x - top.x;
    let dy = center.y - top.y;
    let length = dx.hypot(dy).max(1.0);
    Point::new(top.x + dx / length * 24.0, top.y + dy / length * 24.0)
}

fn contains_transformed_frame(
    transform: ViewerClipTransform,
    canvas: Rect,
    source: [f32; 2],
    point: Point,
) -> bool {
    if transform.scale[0].abs() < 0.0001 || transform.scale[1].abs() < 0.0001 {
        return false;
    }
    let sequence_x = (point.x - canvas.x) * source[0] / canvas.width - transform.position[0];
    let sequence_y = (point.y - canvas.y) * source[1] / canvas.height - transform.position[1];
    let theta = transform.rotation_degrees.to_radians();
    let (sin, cos) = theta.sin_cos();
    let x = (sequence_x * cos + sequence_y * sin) / transform.scale[0] + transform.anchor[0];
    let y = (-sequence_x * sin + sequence_y * cos) / transform.scale[1] + transform.anchor[1];
    (0.0..=source[0]).contains(&x) && (0.0..=source[1]).contains(&y)
}

fn dragged_transform(
    drag: Drag,
    canvas: Rect,
    source: [f32; 2],
    point: Point,
) -> ViewerClipTransform {
    if !valid_canvas(canvas, source) {
        return drag.origin;
    }
    let mut next = drag.origin;
    let dx = (point.x - drag.origin_pointer.x) * source[0] / canvas.width;
    let dy = (point.y - drag.origin_pointer.y) * source[1] / canvas.height;
    if !dx.is_finite() || !dy.is_finite() {
        return drag.origin;
    }
    match drag.handle {
        Handle::Move => {
            next.position[0] += dx;
            next.position[1] += dy;
        }
        Handle::Scale(index) => {
            let theta = drag.origin.rotation_degrees.to_radians();
            let (sin, cos) = theta.sin_cos();
            let local_dx = dx * cos + dy * sin;
            let local_dy = -dx * sin + dy * cos;
            let corner = source_corner(index, source);
            let denominator_x = corner[0] - drag.origin.anchor[0];
            let denominator_y = corner[1] - drag.origin.anchor[1];
            if denominator_x.abs() >= 1.0 {
                next.scale[0] = constrained_scale(drag.origin.scale[0], local_dx / denominator_x);
            }
            if denominator_y.abs() >= 1.0 {
                next.scale[1] = constrained_scale(drag.origin.scale[1], local_dy / denominator_y);
            }
        }
        Handle::Rotate => {
            let pivot = transformed_point(drag.origin, canvas, source, drag.origin.anchor);
            let start = (drag.origin_pointer.y - pivot.y).atan2(drag.origin_pointer.x - pivot.x);
            let end = (point.y - pivot.y).atan2(point.x - pivot.x);
            let delta = (end - start).sin().atan2((end - start).cos()).to_degrees();
            next.rotation_degrees += delta;
        }
        Handle::Anchor => {
            if drag.origin.scale[0].abs() < 0.0001 || drag.origin.scale[1].abs() < 0.0001 {
                return drag.origin;
            }
            let theta = drag.origin.rotation_degrees.to_radians();
            let (sin, cos) = theta.sin_cos();
            next.anchor[0] += (dx * cos + dy * sin) / drag.origin.scale[0];
            next.anchor[1] += (-dx * sin + dy * cos) / drag.origin.scale[1];
            next.position[0] += dx;
            next.position[1] += dy;
        }
    }
    if valid_geometry(next, canvas, source) {
        next
    } else {
        drag.origin
    }
}

fn constrained_scale(original: f32, delta: f32) -> f32 {
    let candidate = original + delta;
    if !candidate.is_finite() {
        return original;
    }
    if original < 0.0 {
        candidate.clamp(-32.0, -0.01)
    } else {
        candidate.clamp(0.01, 32.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ViewerClipTransform {
        ViewerClipTransform {
            position: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotation_degrees: 0.0,
            anchor: [0.0, 0.0],
            editable: true,
        }
    }

    #[test]
    fn move_is_absolute_and_cancel_does_not_publish() {
        let canvas = Rect::new(10.0, 20.0, 200.0, 100.0);
        let source = [1000.0, 500.0];
        let mut editor = ClipTransformEditor::new(identity());
        assert!(editor.pointer_down(canvas, source, Point::new(80.0, 70.0)));
        assert!(editor.pointer_move(canvas, source, Point::new(100.0, 80.0)));
        assert!(editor.cancel());
        assert_eq!(editor.preview, identity());
        assert!(editor.pointer_down(canvas, source, Point::new(80.0, 70.0)));
        assert_eq!(
            editor.pointer_up(canvas, source, Point::new(100.0, 80.0)),
            Some(ViewerClipTransformEdit::Position([100.0, 50.0]))
        );
    }

    #[test]
    fn scale_handle_respects_rotation_and_never_crosses_singular_zero() {
        let canvas = Rect::new(0.0, 0.0, 200.0, 100.0);
        let source = [1000.0, 500.0];
        let mut editor = ClipTransformEditor::new(identity());
        assert!(editor.pointer_down(canvas, source, Point::new(200.0, 100.0)));
        assert_eq!(
            editor.pointer_up(canvas, source, Point::new(220.0, 110.0)),
            Some(ViewerClipTransformEdit::Scale([1.1, 1.1]))
        );
        let drag = Drag {
            handle: Handle::Scale(2),
            origin_pointer: Point::new(200.0, 100.0),
            origin: identity(),
        };
        let collapsed = dragged_transform(drag, canvas, source, Point::new(-1000.0, -1000.0));
        assert_eq!(collapsed.scale, [0.01, 0.01]);
    }

    #[test]
    fn rotation_handle_changes_degrees_and_read_only_rejects_drag() {
        let canvas = Rect::new(0.0, 0.0, 200.0, 100.0);
        let source = [1000.0, 500.0];
        let mut editor = ClipTransformEditor::new(identity());
        let rotation = rotation_handle(identity(), canvas, source);
        assert!(editor.pointer_down(canvas, source, rotation));
        let edit = editor.pointer_up(canvas, source, Point::new(80.0, 24.0));
        assert!(matches!(edit, Some(ViewerClipTransformEdit::Rotation(value)) if value > 0.0));
        let mut locked = identity();
        locked.editable = false;
        assert!(!ClipTransformEditor::new(locked).pointer_down(canvas, source, rotation));
    }

    #[test]
    fn anchor_drag_compensates_position_without_moving_picture() {
        let canvas = Rect::new(0.0, 0.0, 200.0, 100.0);
        let source = [1000.0, 500.0];
        let mut transform = identity();
        transform.position = [100.0, 50.0];
        transform.scale = [2.0, 2.0];
        transform.rotation_degrees = 90.0;
        let before = transformed_point(transform, canvas, source, [500.0, 250.0]);
        let pivot = transformed_point(transform, canvas, source, transform.anchor);
        let mut editor = ClipTransformEditor::new(transform);
        assert!(editor.pointer_down(canvas, source, pivot));
        let Some(ViewerClipTransformEdit::Anchor { anchor, position }) =
            editor.pointer_up(canvas, source, Point::new(pivot.x + 10.0, pivot.y))
        else {
            panic!("anchor edit");
        };
        transform.anchor = anchor;
        transform.position = position;
        let after = transformed_point(transform, canvas, source, [500.0, 250.0]);
        assert!((before.x - after.x).abs() < 0.001);
        assert!((before.y - after.y).abs() < 0.001);
    }

    #[test]
    fn extreme_author_values_do_not_create_infinite_overlay_geometry() {
        let canvas = Rect::new(0.0, 0.0, 200.0, 100.0);
        let source = [1000.0, 500.0];
        let mut extreme = identity();
        extreme.scale = [f32::MAX, f32::MAX];
        let mut editor = ClipTransformEditor::new(extreme);
        assert!(!editor.pointer_down(canvas, source, canvas.center()));
        assert!(!valid_geometry(extreme, canvas, source));
    }
}
