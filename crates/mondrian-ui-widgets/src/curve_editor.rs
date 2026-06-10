//! Normalized curve editor primitive.
//!
//! This widget owns screen-space interaction for editable 0..1 curve points.
//! Domain layers map keyframes, effect curves, or tone curves into this compact
//! representation and commit mutations outside the widget.

use mondrian_core::Color;
use mondrian_ui_core::types::*;
use mondrian_ui_core::widget::{EventContext, PaintContext};
use mondrian_ui_core::{EventResult, UiEvent, Widget};

const DEFAULT_WIDTH: f32 = 220.0;
const DEFAULT_HEIGHT: f32 = 104.0;
const PADDING: f32 = 10.0;
const HIT_RADIUS: f32 = 8.0;
const POINT_RADIUS: f32 = 4.0;
const SELECTED_POINT_RADIUS: f32 = 5.5;
const MIN_POINT_GAP: f32 = 0.001;

/// A normalized editable curve point.
///
/// `x` and `y` are clamped to the inclusive `0.0..=1.0` range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurvePoint {
    /// Normalized horizontal position.
    pub x: f32,
    /// Normalized vertical value.
    pub y: f32,
}

impl CurvePoint {
    /// Create a normalized point.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x: x.clamp(0.0, 1.0), y: y.clamp(0.0, 1.0) }
    }
}

/// Interactive normalized curve editor.
pub struct CurveEditor {
    id: WidgetId,
    bounds: Rect,
    points: Vec<CurvePoint>,
    selected: Option<usize>,
    dragging: Option<usize>,
    grid_columns: usize,
    grid_rows: usize,
}

impl CurveEditor {
    /// Create a curve editor with a simple ease-like default curve.
    pub fn new() -> Self {
        Self::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.35, 0.62),
            CurvePoint::new(0.7, 0.44),
            CurvePoint::new(1.0, 1.0),
        ])
    }

    /// Create a curve editor from normalized points.
    pub fn with_points(points: Vec<CurvePoint>) -> Self {
        let mut editor = Self {
            id: WidgetId::new(),
            bounds: Rect::ZERO,
            points: Vec::new(),
            selected: None,
            dragging: None,
            grid_columns: 4,
            grid_rows: 3,
        };
        editor.set_points(points);
        editor
    }

    /// Current curve points in monotonic-x order.
    pub fn points(&self) -> &[CurvePoint] {
        &self.points
    }

    /// Replace curve points. Points are clamped and sorted by x.
    pub fn set_points(&mut self, mut points: Vec<CurvePoint>) {
        if points.is_empty() {
            points.push(CurvePoint::new(0.0, 0.0));
            points.push(CurvePoint::new(1.0, 1.0));
        }
        points.iter_mut().for_each(|point| {
            *point = CurvePoint::new(point.x, point.y);
        });
        points.sort_by(|a, b| a.x.total_cmp(&b.x));
        if let Some(first) = points.first_mut() {
            first.x = 0.0;
        }
        if let Some(last) = points.last_mut() {
            last.x = 1.0;
        }
        self.points = points;
        self.selected = self.selected.filter(|index| *index < self.points.len());
        self.dragging = self.dragging.filter(|index| *index < self.points.len());
    }

    /// Selected point index, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }

    /// Select a point by index.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index.filter(|index| *index < self.points.len());
    }

    fn plot_rect(&self) -> Rect {
        self.bounds.inset(PADDING, PADDING)
    }

    fn to_screen(&self, point: CurvePoint) -> Point {
        let plot = self.plot_rect();
        Point::new(
            plot.x + point.x * plot.width,
            plot.y + (1.0 - point.y) * plot.height,
        )
    }

    fn screen_to_curve(&self, point: Point) -> CurvePoint {
        let plot = self.plot_rect();
        let x = if plot.width > 0.0 {
            (point.x - plot.x) / plot.width
        } else {
            0.0
        };
        let y = if plot.height > 0.0 {
            1.0 - (point.y - plot.y) / plot.height
        } else {
            0.0
        };
        CurvePoint::new(x, y)
    }

    fn hit_point(&self, position: Point) -> Option<usize> {
        let radius2 = HIT_RADIUS * HIT_RADIUS;
        self.points
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, point)| {
                let screen = self.to_screen(*point);
                let dx = position.x - screen.x;
                let dy = position.y - screen.y;
                let distance2 = dx * dx + dy * dy;
                (distance2 <= radius2).then_some((index, distance2))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    fn constrained_point(&self, index: usize, mut point: CurvePoint) -> CurvePoint {
        let last = self.points.len().saturating_sub(1);
        if index == 0 {
            point.x = 0.0;
        } else if let Some(prev) = self.points.get(index - 1) {
            point.x = point.x.max(prev.x + MIN_POINT_GAP);
        }
        if index == last {
            point.x = 1.0;
        } else if let Some(next) = self.points.get(index + 1) {
            point.x = point.x.min(next.x - MIN_POINT_GAP);
        }
        CurvePoint::new(point.x, point.y)
    }

    fn move_point(&mut self, index: usize, point: CurvePoint) -> bool {
        let Some(current) = self.points.get(index).copied() else {
            return false;
        };
        let next = self.constrained_point(index, point);
        if current == next {
            return false;
        }
        self.points[index] = next;
        true
    }

    fn nudge_selected(&mut self, key: KeyCode, modifiers: Modifiers) -> bool {
        let Some(index) = self.selected else {
            return false;
        };
        let Some(point) = self.points.get(index).copied() else {
            return false;
        };
        let step = if modifiers.shift { 0.05 } else { 0.01 };
        let mut next = point;
        match key {
            KeyCode::Left => next.x -= step,
            KeyCode::Right => next.x += step,
            KeyCode::Up => next.y += step,
            KeyCode::Down => next.y -= step,
            _ => return false,
        }
        self.move_point(index, next)
    }
}

impl Default for CurveEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for CurveEditor {
    fn id(&self) -> WidgetId {
        self.id
    }

    fn measure(&self, constraint: LayoutConstraint) -> Size {
        constraint.constrain(Size::new(DEFAULT_WIDTH, DEFAULT_HEIGHT))
    }

    fn layout(&mut self, bounds: Rect) {
        self.bounds = bounds;
    }

    fn event(&mut self, event: &UiEvent, ctx: &mut EventContext) -> EventResult {
        match event {
            UiEvent::MouseDown { position, button: MouseButton::Left, .. }
                if self.bounds.contains(*position) =>
            {
                if let Some(index) = self.hit_point(*position) {
                    self.selected = Some(index);
                    self.dragging = Some(index);
                    ctx.request_pointer_capture(self.id);
                    return EventResult::Handled;
                }
                self.selected = None;
                EventResult::Handled
            }
            UiEvent::MouseMove { position, .. } => {
                if let Some(index) = self.dragging {
                    self.move_point(index, self.screen_to_curve(*position));
                    return EventResult::Handled;
                }
                EventResult::Ignored
            }
            UiEvent::MouseUp { button: MouseButton::Left, .. } if self.dragging.is_some() => {
                self.dragging = None;
                ctx.release_pointer_capture(self.id);
                EventResult::Handled
            }
            UiEvent::KeyDown { key, modifiers } if self.nudge_selected(*key, *modifiers) => {
                EventResult::Handled
            }
            UiEvent::KeyDown { key: KeyCode::Escape, .. } if self.selected.is_some() => {
                self.selected = None;
                self.dragging = None;
                EventResult::Handled
            }
            _ => EventResult::Ignored,
        }
    }

    fn paint(&self, ctx: &mut PaintContext) {
        let colors = &ctx.theme.colors;
        let spacing = &ctx.theme.spacing;
        let plot = self.plot_rect();
        let grid = color_with_alpha(colors.border, 0.45);
        let curve = colors.primary;
        let point_fill = colors.popover;

        ctx.encoder.draw_rect(self.bounds, colors.card, spacing.radius_md);
        ctx.encoder.draw_rect(
            plot,
            color_with_alpha(colors.muted, 0.42),
            spacing.radius_sm,
        );

        for col in 0..=self.grid_columns {
            let x = plot.x + plot.width * col as f32 / self.grid_columns.max(1) as f32;
            ctx.encoder.draw_line(
                Point::new(x, plot.y),
                Point::new(x, plot.y + plot.height),
                1.0,
                grid,
            );
        }
        for row in 0..=self.grid_rows {
            let y = plot.y + plot.height * row as f32 / self.grid_rows.max(1) as f32;
            ctx.encoder.draw_line(
                Point::new(plot.x, y),
                Point::new(plot.x + plot.width, y),
                1.0,
                grid,
            );
        }

        for pair in self.points.windows(2) {
            let a = self.to_screen(pair[0]);
            let b = self.to_screen(pair[1]);
            ctx.encoder.draw_line(a, b, 2.0, curve);
        }

        for (index, point) in self.points.iter().enumerate() {
            let selected = self.selected == Some(index);
            let radius = if selected {
                SELECTED_POINT_RADIUS
            } else {
                POINT_RADIUS
            };
            let center = self.to_screen(*point);
            let rect = Rect::new(
                center.x - radius,
                center.y - radius,
                radius * 2.0,
                radius * 2.0,
            );
            ctx.encoder.draw_rect(
                rect.inset(-1.0, -1.0),
                color_with_alpha(colors.border, 0.78),
                radius + 1.0,
            );
            ctx.encoder.draw_rect(rect, point_fill, radius);
            if selected {
                ctx.encoder.draw_rect(
                    rect.inset(1.5, 1.5),
                    colors.primary,
                    (radius - 1.5).max(0.0),
                );
            }
        }
    }

    fn hit_test(&self, point: Point) -> bool {
        self.bounds.contains(point)
    }
}

fn color_with_alpha(mut color: Color, alpha: f32) -> Color {
    color.a = (color.a * alpha).clamp(0.0, 1.0);
    color
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{make_event_ctx, DummyFocus, DummyShortcut, DummyTooltip};
    use mondrian_ui_core::widget::DrawCommandEncoder;
    use mondrian_ui_theme::ThemePreset;

    #[derive(Default)]
    struct RecordingEncoder {
        rects: Vec<Rect>,
        lines: usize,
    }

    impl DrawCommandEncoder for RecordingEncoder {
        fn push_clip(&mut self, _bounds: Rect) {}

        fn pop_clip(&mut self) {}

        fn draw_rect(&mut self, bounds: Rect, _color: Color, _corner_radius: f32) {
            self.rects.push(bounds);
        }

        fn draw_line(&mut self, _start: Point, _end: Point, _width: f32, _color: Color) {
            self.lines += 1;
        }

        fn draw_text(&mut self, _text: &str, _font_size: f32, _position: Point, _color: Color) {}

        fn push_translate(&mut self, _offset: glam::Vec2) {}

        fn pop_transform(&mut self) {}
    }

    fn event_ctx() -> EventContext<'static> {
        let f: &'static mut DummyFocus = Box::leak(Box::new(DummyFocus));
        let s: &'static mut DummyShortcut = Box::leak(Box::new(DummyShortcut));
        let t: &'static mut DummyTooltip = Box::leak(Box::new(DummyTooltip));
        make_event_ctx(f, s, t, &|_| {})
    }

    #[test]
    fn set_points_clamps_sorts_and_anchors_endpoints() {
        let editor = CurveEditor::with_points(vec![
            CurvePoint { x: 1.4, y: -1.0 },
            CurvePoint { x: 0.5, y: 0.25 },
            CurvePoint { x: -0.2, y: 2.0 },
        ]);

        assert_eq!(editor.points()[0], CurvePoint::new(0.0, 1.0));
        assert_eq!(editor.points()[1], CurvePoint::new(0.5, 0.25));
        assert_eq!(editor.points()[2], CurvePoint::new(1.0, 0.0));
    }

    #[test]
    fn dragging_point_updates_value_and_releases_capture() {
        let mut editor =
            CurveEditor::with_points(vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]);
        editor.layout(Rect::new(0.0, 0.0, 200.0, 100.0));
        let start = editor.to_screen(editor.points()[1]);
        let target = Point::new(190.0, 90.0);
        let mut ctx = event_ctx();

        assert_eq!(
            editor.event(
                &UiEvent::MouseDown {
                    position: start,
                    button: MouseButton::Left,
                    modifiers: Modifiers::none(),
                },
                &mut ctx,
            ),
            EventResult::Handled
        );
        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Capture(
                editor.id()
            ))
        );
        editor.event(
            &UiEvent::MouseMove { position: target, modifiers: Modifiers::none() },
            &mut ctx,
        );
        editor.event(
            &UiEvent::MouseUp {
                position: target,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
            },
            &mut ctx,
        );

        assert_eq!(
            ctx.requests.pointer_capture,
            Some(mondrian_ui_core::widget::PointerCaptureRequest::Release(
                editor.id()
            ))
        );
        assert_eq!(editor.points()[1].x, 1.0);
        assert!(editor.points()[1].y < 0.2);
    }

    #[test]
    fn interior_point_cannot_cross_neighbors() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);

        assert!(editor.move_point(1, CurvePoint::new(1.2, 0.75)));
        assert!(editor.points()[1].x < editor.points()[2].x);
        assert!(editor.points()[1].x > editor.points()[0].x);
    }

    #[test]
    fn keyboard_nudges_selected_point() {
        let mut editor = CurveEditor::with_points(vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ]);
        editor.select(Some(1));
        let mut ctx = event_ctx();

        editor.event(
            &UiEvent::KeyDown { key: KeyCode::Up, modifiers: Modifiers::shift() },
            &mut ctx,
        );

        assert!(editor.points()[1].y > 0.54);
    }

    #[test]
    fn paint_draws_grid_curve_and_points() {
        let mut editor = CurveEditor::new();
        editor.layout(Rect::new(0.0, 0.0, 220.0, 104.0));
        let theme = ThemePreset::Dark.build();
        let mut encoder = RecordingEncoder::default();
        let mut ctx = PaintContext {
            encoder: &mut encoder,
            theme: &theme,
            clip_rect: Rect::new(0.0, 0.0, 220.0, 104.0),
        };

        editor.paint(&mut ctx);

        assert!(encoder.rects.len() >= 2 + editor.points().len() * 2);
        assert!(encoder.lines >= editor.grid_columns + editor.grid_rows + editor.points().len());
    }
}
