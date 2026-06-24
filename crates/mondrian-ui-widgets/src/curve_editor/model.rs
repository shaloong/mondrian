use mondrian_ui_core::types::{Point, Rect};

use super::{CurvePoint, HIT_RADIUS, MIN_POINT_GAP, PADDING};

pub(super) fn normalize_points(mut points: Vec<CurvePoint>) -> Vec<CurvePoint> {
    if points.is_empty() {
        points.push(CurvePoint::new(0.0, 0.0));
        points.push(CurvePoint::new(1.0, 1.0));
    } else if points.len() == 1 {
        let y = points[0].y;
        points.clear();
        points.push(CurvePoint::new(0.0, y));
        points.push(CurvePoint::new(1.0, y));
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
    points
}

pub(super) fn constrained_point(
    points: &[CurvePoint],
    index: usize,
    mut point: CurvePoint,
) -> CurvePoint {
    let last = points.len().saturating_sub(1);
    if index == 0 {
        point.x = 0.0;
    } else if let Some(prev) = points.get(index - 1) {
        point.x = point.x.max(prev.x + MIN_POINT_GAP);
    }
    if index == last {
        point.x = 1.0;
    } else if let Some(next) = points.get(index + 1) {
        point.x = point.x.min(next.x - MIN_POINT_GAP);
    }
    CurvePoint::new(point.x, point.y)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CurveGeometry {
    bounds: Rect,
}

impl CurveGeometry {
    pub(super) fn new(bounds: Rect) -> Self {
        Self { bounds }
    }

    pub(super) fn plot_rect(&self) -> Rect {
        self.bounds.inset(PADDING, PADDING)
    }

    pub(super) fn curve_to_screen(&self, point: CurvePoint) -> Point {
        let plot = self.plot_rect();
        Point::new(
            plot.x + point.x * plot.width,
            plot.y + (1.0 - point.y) * plot.height,
        )
    }

    pub(super) fn screen_to_curve(&self, point: Point) -> CurvePoint {
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

    pub(super) fn hit_point(&self, points: &[CurvePoint], position: Point) -> Option<usize> {
        let radius2 = HIT_RADIUS * HIT_RADIUS;
        points
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(index, point)| {
                let screen = self.curve_to_screen(*point);
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

        let min_x = points[index - 1].x + MIN_POINT_GAP;
        let max_x = points[index].x - MIN_POINT_GAP;
        if min_x > max_x {
            return None;
        }

        Some((index, CurvePoint::new(point.x.clamp(min_x, max_x), point.y)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 0.0001,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn normalize_points_clamps_sorts_and_anchors_endpoints() {
        let points = normalize_points(vec![
            CurvePoint { x: 0.8, y: 2.0 },
            CurvePoint { x: -1.0, y: 0.25 },
            CurvePoint { x: 0.4, y: -1.0 },
        ]);

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
        let points = normalize_points(vec![CurvePoint::new(0.4, 0.65)]);

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
            constrained_point(&points, 0, CurvePoint::new(0.5, 0.2)),
            CurvePoint::new(0.0, 0.2)
        );
        assert_close(
            constrained_point(&points, 1, CurvePoint::new(0.9, 0.7)).x,
            0.599,
        );
        assert_close(
            constrained_point(&points, 2, CurvePoint::new(0.1, 0.7)).x,
            0.401,
        );
    }

    #[test]
    fn geometry_round_trips_curve_and_screen_points() {
        let geometry = CurveGeometry::new(Rect::new(10.0, 20.0, 220.0, 104.0));
        let point = CurvePoint::new(0.25, 0.75);
        let screen = geometry.curve_to_screen(point);
        let curve = geometry.screen_to_curve(screen);

        assert_close(curve.x, point.x);
        assert_close(curve.y, point.y);
    }

    #[test]
    fn hit_point_picks_nearest_topmost_point_inside_radius() {
        let geometry = CurveGeometry::new(Rect::new(0.0, 0.0, 120.0, 120.0));
        let points = vec![CurvePoint::new(0.5, 0.5), CurvePoint::new(0.5, 0.5)];

        assert_eq!(
            geometry.hit_point(&points, geometry.curve_to_screen(points[0])),
            Some(1)
        );
    }

    #[test]
    fn insertion_at_rejects_edges_and_clamps_between_neighbors() {
        let geometry = CurveGeometry::new(Rect::new(0.0, 0.0, 120.0, 120.0));
        let points = vec![
            CurvePoint::new(0.0, 0.0),
            CurvePoint::new(0.5, 0.5),
            CurvePoint::new(1.0, 1.0),
        ];

        assert_eq!(geometry.insertion_at(&points, Point::new(10.0, 60.0)), None);
        let (index, point) =
            geometry.insertion_at(&points, Point::new(70.0, 40.0)).expect("interior insert");
        assert_eq!(index, 2);
        assert!(point.x > 0.5 && point.x < 1.0);
    }
}
