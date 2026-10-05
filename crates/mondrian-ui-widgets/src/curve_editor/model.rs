use mondrian_ui_core::types::{Point, Rect};

use super::{CurvePoint, HIT_RADIUS, MIN_POINT_GAP, PADDING};

pub(super) fn normalize_points(
    mut points: Vec<CurvePoint>,
    author_values: bool,
) -> Vec<CurvePoint> {
    points.retain(|point| point.x.is_finite() && point.y.is_finite());
    if points.is_empty() {
        points.push(CurvePoint::new(0.0, 0.0));
        points.push(CurvePoint::new(1.0, 1.0));
    } else if points.len() == 1 {
        let y = points[0].y;
        points.clear();
        points.push(CurvePoint { x: 0.0, y });
        points.push(CurvePoint { x: 1.0, y });
    }
    points.iter_mut().for_each(|point| {
        point.x = point.x.clamp(0.0, 1.0);
        if !author_values {
            point.y = f64::from(point.y.clamp(0.0, 1.0) as f32);
        }
    });
    points.sort_by(|a, b| a.x.total_cmp(&b.x));
    if let Some(first) = points.first_mut() {
        first.x = 0.0;
    }
    if let Some(last) = points.last_mut() {
        last.x = 1.0;
    }
    points
}

#[derive(Clone, Copy)]
pub(super) enum CurvePointSpacing {
    Transfer,
    Temporal,
}

impl CurvePointSpacing {
    fn neighbor_bounds(self, previous: Option<f32>, next: Option<f32>) -> (f32, f32) {
        let lower = previous.map_or(0.0, |value| match self {
            Self::Transfer => value + MIN_POINT_GAP,
            Self::Temporal => value.next_up(),
        });
        let upper = next.map_or(1.0, |value| match self {
            Self::Transfer => value - MIN_POINT_GAP,
            Self::Temporal => value.next_down(),
        });
        (lower, upper)
    }
}

pub(super) fn constrained_point(
    points: &[CurvePoint],
    index: usize,
    mut point: CurvePoint,
    lock_x: bool,
    spacing: CurvePointSpacing,
) -> CurvePoint {
    let Some(current) = points.get(index) else {
        return point;
    };
    if lock_x {
        point.x = current.x;
    } else if matches!(spacing, CurvePointSpacing::Temporal) {
        point.x = point.x.clamp(0.0, 1.0);
    } else {
        let (min_x, max_x) = spacing.neighbor_bounds(
            points.get(index.wrapping_sub(1)).map(|previous| previous.x),
            points.get(index + 1).map(|next| next.x),
        );
        point.x = if min_x <= max_x {
            point.x.clamp(min_x, max_x)
        } else {
            current.x
        };
    }
    point
}

const MIN_VIEW_SPAN: f64 = 1.0 / 65536.0;

fn minimum_value_radius(value: f64) -> f64 {
    (value.abs() * f64::EPSILON * 16.0).max(f64::from_bits(1))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CurveViewport {
    pub(super) time_start: f64,
    pub(super) time_span: f64,
    pub(super) value_center: f64,
    pub(super) value_radius: f64,
}

impl Default for CurveViewport {
    fn default() -> Self {
        Self {
            time_start: 0.0,
            time_span: 1.0,
            value_center: 0.5,
            value_radius: 0.5,
        }
    }
}

impl CurveViewport {
    pub(super) fn with_value_range(
        range: mondrian_core::automation::ParameterNumericRange,
    ) -> Self {
        let center = range.min * 0.5 + range.max * 0.5;
        let radius = (range.max * 0.5 - range.min * 0.5).max(minimum_value_radius(center));
        let mut view = Self {
            value_center: center,
            value_radius: radius,
            ..Self::default()
        };
        view.bound_values();
        view
    }

    fn bound_values(&mut self) {
        self.value_center = self.value_center.clamp(-f64::MAX, f64::MAX);
        self.value_radius =
            self.value_radius.clamp(minimum_value_radius(self.value_center), f64::MAX);
        self.value_center = self
            .value_center
            .clamp(-f64::MAX + self.value_radius, f64::MAX - self.value_radius);
    }

    pub(super) fn value_at(&self, fraction: f64) -> f64 {
        ((fraction * 2.0 - 1.0) * self.value_radius + self.value_center).clamp(-f64::MAX, f64::MAX)
    }

    pub(super) fn value_position(&self, value: f64) -> f64 {
        let difference = value - self.value_center;
        if difference.is_finite() {
            let ratio = difference / self.value_radius;
            if ratio.is_finite() {
                ratio * 0.5 + 0.5
            } else {
                (difference * 0.5) / self.value_radius + 0.5
            }
        } else {
            (value * 0.5 - self.value_center * 0.5) / self.value_radius + 0.5
        }
    }

    pub(super) fn zoom(&mut self, fraction: [f64; 2], factor: f64, value_axis: bool) {
        if !factor.is_finite() || factor <= 0.0 || fraction.iter().any(|v| !v.is_finite()) {
            return;
        }
        if value_axis {
            let anchor = self.value_at(fraction[1]);
            self.value_radius =
                (self.value_radius * factor).clamp(minimum_value_radius(anchor), f64::MAX);
            self.value_center = anchor - (fraction[1] * 2.0 - 1.0) * self.value_radius;
            self.bound_values();
        } else {
            let anchor = self.time_start + fraction[0] * self.time_span;
            self.time_span = (self.time_span * factor).clamp(MIN_VIEW_SPAN, 1.0);
            self.time_start =
                (anchor - fraction[0] * self.time_span).clamp(0.0, 1.0 - self.time_span);
        }
    }

    pub(super) fn pan(&mut self, delta: [f64; 2]) {
        if delta.iter().any(|v| !v.is_finite()) {
            return;
        }
        self.time_start =
            (self.time_start + delta[0] * self.time_span).clamp(0.0, 1.0 - self.time_span);
        self.value_center += (delta[1] * 2.0) * self.value_radius;
        self.bound_values();
    }

    pub(super) fn reveal(&mut self, point: CurvePoint) {
        let x = f64::from(point.x);
        let y = point.y;
        let margin = self.time_span * 0.1;
        if x < self.time_start {
            self.time_start = (x - margin).max(0.0);
        } else if x > self.time_start + self.time_span {
            self.time_start = (x + margin - self.time_span).min(1.0 - self.time_span);
        }
        self.value_radius = self.value_radius.max(minimum_value_radius(y));
        let position = self.value_position(y);
        if !(0.0..=1.0).contains(&position) {
            self.value_center = y + if position < 0.0 { 0.8 } else { -0.8 } * self.value_radius;
        }
        self.bound_values();
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CurveGeometry {
    bounds: Rect,
    viewport: CurveViewport,
}

impl CurveGeometry {
    pub(super) fn new(bounds: Rect, viewport: CurveViewport) -> Self {
        Self { bounds, viewport }
    }

    pub(super) fn plot_rect(&self) -> Rect {
        self.bounds.inset(PADDING, PADDING)
    }

    pub(super) fn curve_to_screen(&self, point: CurvePoint) -> Point {
        let plot = self.plot_rect();
        Point::new(
            (f64::from(plot.x)
                + (f64::from(point.x) - self.viewport.time_start) / self.viewport.time_span
                    * f64::from(plot.width)) as f32,
            (f64::from(plot.y)
                + (1.0 - self.viewport.value_position(point.y)) * f64::from(plot.height))
                as f32,
        )
    }

    pub(super) fn screen_fraction(&self, point: Point) -> [f64; 2] {
        let plot = self.plot_rect();
        [
            if plot.width > 0.0 {
                f64::from(point.x - plot.x) / f64::from(plot.width)
            } else {
                0.0
            },
            if plot.height > 0.0 {
                1.0 - f64::from(point.y - plot.y) / f64::from(plot.height)
            } else {
                0.0
            },
        ]
    }

    pub(super) fn screen_to_curve(&self, point: Point) -> CurvePoint {
        let fraction = self.screen_fraction(point);
        CurvePoint {
            x: (self.viewport.time_start + fraction[0] * self.viewport.time_span).clamp(0.0, 1.0)
                as f32,
            y: self.viewport.value_at(fraction[1]),
        }
    }

    // Clip in f64 author-projection coordinates before lowering to screen f32.
    // Finite offscreen controls may otherwise overflow the GPU's line geometry.
    pub(super) fn visible_segment(&self, a: CurvePoint, b: CurvePoint) -> Option<(Point, Point)> {
        let a = [f64::from(a.x), a.y];
        let b = [f64::from(b.x), b.y];
        if a.iter().chain(b.iter()).any(|v| !v.is_finite()) {
            return None;
        }
        let low = self.viewport.value_at(0.0);
        let high = self.viewport.value_at(1.0);
        let scale = if !(b[1] - a[1]).is_finite()
            || !(a[1] - low).is_finite()
            || !(high - a[1]).is_finite()
        {
            0.5
        } else {
            1.0
        };
        let a = [a[0], a[1] * scale];
        let b = [b[0], b[1] * scale];
        let delta = [b[0] - a[0], b[1] - a[1]];
        let mut start: f64 = 0.0;
        let mut end: f64 = 1.0;
        let mut start_edges = [None; 2];
        let mut end_edges = [None; 2];
        for (p, q, axis, edge) in [
            (-delta[0], a[0] - self.viewport.time_start, 0, 0.0),
            (
                delta[0],
                self.viewport.time_start + self.viewport.time_span - a[0],
                0,
                1.0,
            ),
            (-delta[1], a[1] - low * scale, 1, 0.0),
            (delta[1], high * scale - a[1], 1, 1.0),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
            } else {
                let t = q / p;
                if p < 0.0 && t >= start {
                    if t > start {
                        start_edges = [None; 2];
                    }
                    start = t;
                    start_edges[axis] = Some(edge);
                } else if p > 0.0 && t <= end {
                    if t < end {
                        end_edges = [None; 2];
                    }
                    end = t;
                    end_edges[axis] = Some(edge);
                }
            }
            if start > end {
                return None;
            }
        }
        let project = |t: f64, edges: [Option<f64>; 2]| {
            let plot = self.plot_rect();
            let x = edges[0].unwrap_or_else(|| {
                (a[0] + delta[0] * t - self.viewport.time_start) / self.viewport.time_span
            });
            let y = edges[1]
                .unwrap_or_else(|| self.viewport.value_position((a[1] + delta[1] * t) / scale));
            Point::new(
                (f64::from(plot.x) + x * f64::from(plot.width)) as f32,
                (f64::from(plot.y) + (1.0 - y) * f64::from(plot.height)) as f32,
            )
        };
        Some((project(start, start_edges), project(end, end_edges)))
    }

    pub(super) fn hit_point(&self, points: &[CurvePoint], position: Point) -> Option<usize> {
        if !self.plot_rect().contains(position) {
            return None;
        }
        let radius2 = HIT_RADIUS * HIT_RADIUS;
        points
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, point)| {
                let screen = self.curve_to_screen(*point);
                if !self.plot_rect().contains(screen) {
                    return None;
                }
                let dx = position.x - screen.x;
                let dy = position.y - screen.y;
                let distance2 = dx * dx + dy * dy;
                (distance2 <= radius2).then_some((index, distance2))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    pub(super) fn insertion_at(
        &self,
        points: &[CurvePoint],
        position: Point,
        spacing: CurvePointSpacing,
    ) -> Option<(usize, CurvePoint)> {
        let plot = self.plot_rect();
        if !plot.contains(position) || points.len() < 2 {
            return None;
        }

        let point = self.screen_to_curve(position);
        let index = points.partition_point(|candidate| candidate.x < point.x);
        if index == 0 || index >= points.len() {
            return None;
        }

        let (min_x, max_x) =
            spacing.neighbor_bounds(Some(points[index - 1].x), Some(points[index].x));
        if min_x > max_x {
            return None;
        }

        Some((
            index,
            CurvePoint { x: point.x.clamp(min_x, max_x), y: point.y },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: impl Into<f64>, expected: impl Into<f64>) {
        let actual = actual.into();
        let expected = expected.into();
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn normalize_points_clamps_sorts_and_anchors_endpoints() {
        let points = normalize_points(
            vec![
                CurvePoint { x: 0.8, y: 2.0 },
                CurvePoint { x: -1.0, y: 0.25 },
                CurvePoint { x: 0.4, y: -1.0 },
            ],
            false,
        );

        assert_eq!(
            points,
            vec![
                CurvePoint::new(0.0, 0.25),
                CurvePoint::new(0.4, 0.0),
                CurvePoint::new(1.0, 1.0),
            ]
        );
    }

    #[test]
    fn normalize_points_expands_single_point_to_flat_curve() {
        let points = normalize_points(vec![CurvePoint::new(0.4, 0.65)], false);

        assert_eq!(
            points,
            vec![CurvePoint::new(0.0, 0.65), CurvePoint::new(1.0, 0.65)]
        );
    }

    #[test]
    fn constrained_point_keeps_endpoints_anchored_and_interior_ordered() {
        let points = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.4, 0.5),
            CurvePoint::new(0.6, 0.5),
            CurvePoint::new(1.0, 1.0),
        ];

        assert_eq!(
            constrained_point(
                &points,
                0,
                CurvePoint::new(0.5, 0.2),
                true,
                CurvePointSpacing::Transfer
            ),
            CurvePoint::new(0.0, 0.2)
        );
        assert_close(
            constrained_point(
                &points,
                1,
                CurvePoint::new(0.9, 0.7),
                false,
                CurvePointSpacing::Transfer,
            )
            .x,
            0.599,
        );
        assert_close(
            constrained_point(
                &points,
                2,
                CurvePoint::new(0.1, 0.7),
                false,
                CurvePointSpacing::Transfer,
            )
            .x,
            0.401,
        );
    }

    #[test]
    fn editable_endpoint_moves_inside_domain_without_crossing_neighbor() {
        let points = vec![CurvePoint::new(0.0, 0.25), CurvePoint::new(1.0, 0.75)];

        assert_eq!(
            constrained_point(
                &points,
                0,
                CurvePoint::new(0.4, 0.5),
                false,
                CurvePointSpacing::Transfer
            ),
            CurvePoint::new(0.4, 0.5)
        );
        assert_close(
            constrained_point(
                &points,
                0,
                CurvePoint::new(1.0, 0.5),
                false,
                CurvePointSpacing::Transfer,
            )
            .x,
            0.999,
        );
    }

    #[test]
    fn viewport_zoom_pan_limits_and_transformed_geometry_are_finite() {
        let mut viewport = CurveViewport::default();
        viewport.zoom([0.5, 0.75], 0.25, false);
        viewport.zoom([0.5, 0.75], 0.5, true);
        viewport.pan([0.1, -0.2]);
        let geometry = CurveGeometry::new(Rect::new(10.0, 20.0, 220.0, 104.0), viewport);
        let point = CurvePoint::new(0.5, 0.75);
        let curve = geometry.screen_to_curve(geometry.curve_to_screen(point));
        assert_close(curve.x, point.x);
        assert_close(curve.y, point.y);
        for _ in 0..200 {
            viewport.zoom([0.5, 0.5], 0.01, false);
            viewport.zoom([0.5, 0.5], 0.01, true);
        }
        assert_eq!(viewport.time_span, MIN_VIEW_SPAN);
        assert_eq!(
            viewport.value_radius,
            minimum_value_radius(viewport.value_center)
        );
        viewport.pan([f64::MAX, f64::MAX]);
        assert!(viewport.time_start >= 0.0 && viewport.time_start + viewport.time_span <= 1.0);
        assert!(viewport.value_at(0.0).is_finite());
        let before = viewport;
        viewport.zoom([0.5, 0.5], f64::NAN, false);
        viewport.pan([f64::NAN, 0.0]);
        assert_eq!(viewport, before);
        for _ in 0..200 {
            viewport.zoom([0.5, 0.5], 100.0, false);
            viewport.zoom([0.5, 0.5], 100.0, true);
        }
        assert_eq!(viewport.time_span, 1.0);
        assert!(viewport.value_radius.is_finite() && viewport.value_at(0.0).is_finite());
    }

    #[test]
    fn extreme_value_reveal_remains_resolvable_and_default_endpoints_do_not_pan() {
        let mut viewport = CurveViewport::default();
        viewport.reveal(CurvePoint::new(0.0, 0.0));
        viewport.reveal(CurvePoint::new(1.0, 1.0));
        assert_eq!(viewport, CurveViewport::default());
        let point = CurvePoint { x: 0.5, y: f64::from(f32::MAX) };
        viewport.reveal(point);
        assert!(viewport.value_at(1.0) > viewport.value_at(0.0));
        let geometry = CurveGeometry::new(Rect::new(0.0, 0.0, 220.0, 104.0), viewport);
        assert!(geometry.plot_rect().contains(geometry.curve_to_screen(point)));
        viewport.zoom([0.5, 0.5], 0.00001, true);
        assert!(viewport.value_at(1.0) > viewport.value_at(0.0));
    }

    #[test]
    fn viewport_clips_segments_and_excludes_offscreen_keys_from_hits() {
        let viewport = CurveViewport {
            time_start: 0.25,
            time_span: 0.5,
            value_center: 0.5,
            value_radius: 0.25,
        };
        let geometry = CurveGeometry::new(Rect::new(0.0, 0.0, 220.0, 104.0), viewport);
        let (start, end) = geometry
            .visible_segment(CurvePoint::new(0.0, 0.5), CurvePoint::new(1.0, 0.5))
            .expect("crossing line");
        assert_eq!(start, Point::new(10.0, 52.0));
        assert_eq!(end, Point::new(210.0, 52.0));
        assert!(geometry
            .visible_segment(CurvePoint::new(0.0, 0.0), CurvePoint::new(0.1, 0.1))
            .is_none());
        let offscreen = CurvePoint::new(0.24, 0.5);
        assert!(
            geometry.hit_point(&[offscreen], Point::new(10.0, 52.0)).is_none(),
            "invisible key cannot capture a visible-edge click"
        );
        let (_, extreme) = geometry
            .visible_segment(
                CurvePoint::new(0.5, 0.5),
                CurvePoint { x: 0.5, y: f64::from(f32::MAX) },
            )
            .expect("extreme handle");
        assert!(extreme.x.is_finite() && extreme.y.is_finite());
        assert!((extreme.y - 10.0).abs() < 0.001);
    }

    #[test]
    fn author_value_geometry_covers_full_finite_span_and_subnormal_views() {
        use mondrian_core::automation::ParameterNumericRange;
        let bounds = Rect::new(0.0, 0.0, 220.0, 104.0);
        for (min, max) in [
            (-f64::MAX, f64::MAX),
            (-2.0, 2.0),
            (0.0, f64::from_bits(2)),
            (f64::MAX * 0.9, f64::MAX),
        ] {
            let mut viewport = CurveViewport::with_value_range(
                ParameterNumericRange::new(min, max).expect("range"),
            );
            let geometry = CurveGeometry::new(bounds, viewport);
            for y in [min, max] {
                let point = geometry.curve_to_screen(CurvePoint { x: 0.5, y });
                assert!(point.x.is_finite() && point.y.is_finite());
                assert!(
                    geometry.plot_rect().contains(point),
                    "endpoint {y} in {min}..{max}"
                );
            }
            viewport.pan([0.0, f64::MAX]);
            viewport.zoom([0.5, 0.5], f64::MAX, true);
            assert!(viewport.value_center.is_finite() && viewport.value_radius.is_finite());
        }
        let geometry = CurveGeometry::new(bounds, CurveViewport::default());
        let (a, b) = geometry
            .visible_segment(
                CurvePoint { x: 0.5, y: -f64::MAX },
                CurvePoint { x: 0.5, y: f64::MAX },
            )
            .expect("full finite crossing");
        assert_eq!(a, Point::new(110.0, 94.0));
        assert_eq!(b, Point::new(110.0, 10.0));
    }

    #[test]
    fn geometry_round_trips_curve_and_screen_points() {
        let geometry = CurveGeometry::new(
            Rect::new(10.0, 20.0, 220.0, 104.0),
            CurveViewport::default(),
        );
        let point = CurvePoint::new(0.25, 0.75);
        let screen = geometry.curve_to_screen(point);
        let curve = geometry.screen_to_curve(screen);

        assert_close(curve.x, point.x);
        assert_close(curve.y, point.y);
    }

    #[test]
    fn hit_point_picks_nearest_topmost_point_inside_radius() {
        let geometry =
            CurveGeometry::new(Rect::new(0.0, 0.0, 120.0, 120.0), CurveViewport::default());
        let points = vec![CurvePoint::new(0.5, 0.5), CurvePoint::new(0.5, 0.5)];

        assert_eq!(
            geometry.hit_point(&points, geometry.curve_to_screen(points[0])),
            Some(1)
        );
    }

    #[test]
    fn insertion_at_rejects_edges_and_clamps_between_neighbors() {
        let geometry =
            CurveGeometry::new(Rect::new(0.0, 0.0, 120.0, 120.0), CurveViewport::default());
        let points = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ];

        assert_eq!(
            geometry.insertion_at(&points, Point::new(10.0, 60.0), CurvePointSpacing::Transfer),
            None
        );
        let (index, point) = geometry
            .insertion_at(&points, Point::new(70.0, 40.0), CurvePointSpacing::Transfer)
            .expect("interior insert");
        assert_eq!(index, 2);
        assert!(point.x > 0.5 && point.x < 1.0);
    }
}
