//! Screen-space marquee gestures for selecting mask anchors. No authored state.

use mondrian_ui_core::types::Point;

const SAMPLE_SPACING: f64 = 2.0;
const MIN_RECT_EXTENT: f64 = 3.0;
const LASSO_BOUNDARY: f64 = 0.25;
const MAX_LASSO_POINTS: usize = 256;

/// How a drag selects anchors in the Viewer mask editor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewerMaskSelectionMode {
    /// Select anchors inside the drag's axis-aligned rectangle.
    #[default]
    Rectangle,
    /// Select anchors inside a freehand closed outline using the even-odd rule.
    Lasso,
}

pub(super) enum AnchorSelectionDrag {
    Rectangle { origin: Point, end: Point },
    Lasso { samples: Vec<Point>, end: Point },
}

impl AnchorSelectionDrag {
    pub(super) fn start(mode: ViewerMaskSelectionMode, origin: Point) -> Option<Self> {
        if !finite(origin) {
            return None;
        }
        Some(match mode {
            ViewerMaskSelectionMode::Rectangle => Self::Rectangle { origin, end: origin },
            ViewerMaskSelectionMode::Lasso => Self::Lasso { samples: vec![origin], end: origin },
        })
    }

    pub(super) fn track(&mut self, point: Point) {
        if !finite(point) {
            return;
        }
        match self {
            Self::Rectangle { end, .. } => *end = point,
            Self::Lasso { samples, end } => {
                *end = point;
                if squared_len(delta(point, samples[samples.len() - 1])) < SAMPLE_SPACING.powi(2) {
                    return;
                }
                if samples.len() == MAX_LASSO_POINTS {
                    // Coarsen the whole path while retaining its origin. Do not
                    // freeze the outline or discard the rest of a long gesture.
                    let mut i = 0;
                    samples.retain(|_| {
                        let keep = i % 2 == 0;
                        i += 1;
                        keep
                    });
                }
                samples.push(point);
            }
        }
    }

    pub(super) fn for_each_edge(&self, mut edge: impl FnMut(Point, Point)) {
        match self {
            Self::Rectangle { origin, end } => {
                let b = Point::new(end.x, origin.y);
                let d = Point::new(origin.x, end.y);
                edge(*origin, b);
                edge(b, *end);
                edge(*end, d);
                edge(d, *origin);
            }
            Self::Lasso { samples, end } => {
                for (a, b) in lasso_edges(samples, *end) {
                    edge(a, b);
                }
            }
        }
    }

    /// `None` is a cancelled/degenerate gesture, distinct from a valid gesture
    /// that selects no anchors (and should clear the current selection).
    pub(super) fn finish(
        mut self,
        release: Point,
        anchors: impl Iterator<Item = Point>,
    ) -> Option<Vec<usize>> {
        if !finite(release) {
            return None;
        }
        self.track(release);
        match self {
            Self::Rectangle { origin, end } => {
                let [dx, dy] = delta(end, origin);
                if dx.abs() < MIN_RECT_EXTENT || dy.abs() < MIN_RECT_EXTENT {
                    return None;
                }
                let x = origin.x.min(end.x)..=origin.x.max(end.x);
                let y = origin.y.min(end.y)..=origin.y.max(end.y);
                Some(
                    anchors
                        .enumerate()
                        .filter_map(|(i, p)| (x.contains(&p.x) && y.contains(&p.y)).then_some(i))
                        .collect(),
                )
            }
            Self::Lasso { samples, end } => {
                let origin = samples[0];
                let path = || samples.iter().copied().chain([end]);
                let axis = path()
                    .map(|p| delta(p, origin))
                    .find(|&v| squared_len(v) >= MIN_RECT_EXTENT.powi(2))?;
                // Signed area is not a degeneracy test for even-odd paths: a
                // figure eight can have zero signed area and two valid lobes.
                if !path().any(|p| {
                    let [x, y] = delta(p, origin);
                    (axis[0] * y - axis[1] * x).abs() >= squared_len(axis).sqrt()
                }) {
                    return None;
                }
                let bounds = path().fold([origin.x, origin.y, origin.x, origin.y], |b, p| {
                    [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]
                });
                Some(
                    anchors
                        .enumerate()
                        .filter_map(|(i, p)| {
                            let [x, y] = [f64::from(p.x), f64::from(p.y)];
                            ((f64::from(bounds[0]) - LASSO_BOUNDARY
                                ..=f64::from(bounds[2]) + LASSO_BOUNDARY)
                                .contains(&x)
                                && (f64::from(bounds[1]) - LASSO_BOUNDARY
                                    ..=f64::from(bounds[3]) + LASSO_BOUNDARY)
                                    .contains(&y)
                                && inside_lasso(&samples, end, p))
                            .then_some(i)
                        })
                        .collect(),
                )
            }
        }
    }
}

fn finite(p: Point) -> bool {
    p.x.is_finite() && p.y.is_finite()
}

fn delta(p: Point, origin: Point) -> [f64; 2] {
    [
        f64::from(p.x) - f64::from(origin.x),
        f64::from(p.y) - f64::from(origin.y),
    ]
}

fn squared_len([x, y]: [f64; 2]) -> f64 {
    x * x + y * y
}

fn lasso_edges(samples: &[Point], end: Point) -> impl Iterator<Item = (Point, Point)> + '_ {
    samples
        .iter()
        .copied()
        .zip(samples.iter().skip(1).copied().chain([end]))
        .chain([(end, samples[0])])
}

fn inside_lasso(samples: &[Point], end: Point, p: Point) -> bool {
    let mut inside = false;
    for (a, b) in lasso_edges(samples, end) {
        let [dx, dy] = delta(b, a);
        let [x, y] = delta(p, a);
        let len = squared_len([dx, dy]);
        if len > 0.0 {
            let t = ((x * dx + y * dy) / len).clamp(0.0, 1.0);
            if squared_len([x - t * dx, y - t * dy]) <= LASSO_BOUNDARY.powi(2) {
                return true;
            }
        }
        // Half-open range on Y counts each vertex once. Horizontal edges
        // cannot straddle Y, but are still covered by the boundary test.
        if (a.y > p.y) != (b.y > p.y) && x < dx * y / dy {
            inside = !inside;
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point([x, y]: [f32; 2]) -> Point {
        Point::new(x, y)
    }

    fn select(
        mode: ViewerMaskSelectionMode,
        trace: &[[f32; 2]],
        anchors: &[[f32; 2]],
    ) -> Option<Vec<usize>> {
        let (release, path) = trace.split_last().unwrap();
        let mut drag = AnchorSelectionDrag::start(mode, point(path[0])).unwrap();
        for &p in &path[1..] {
            drag.track(point(p));
        }
        drag.finish(point(*release), anchors.iter().copied().map(point))
    }

    #[test]
    fn rectangle_is_inclusive_and_independent_of_drag_direction() {
        assert_eq!(
            select(
                ViewerMaskSelectionMode::Rectangle,
                &[[20.0, 20.0], [-3.0, 2.0]],
                &[[-3.0, 2.0], [10.0, 10.0], [20.0, 20.0], [21.0, 8.0]],
            ),
            Some(vec![0, 1, 2]),
        );
    }

    #[test]
    fn rectangle_distinguishes_a_click_from_an_empty_selection() {
        for (release, expected) in [([0.0, 0.0], None), ([10.0, 10.0], Some(vec![]))] {
            assert_eq!(
                select(
                    ViewerMaskSelectionMode::Rectangle,
                    &[[0.0, 0.0], release],
                    &[[-1.0, 0.0]]
                ),
                expected,
            );
        }
    }

    #[test]
    fn lasso_respects_concavity_and_closes_to_the_first_sample() {
        assert_eq!(
            select(
                ViewerMaskSelectionMode::Lasso,
                &[
                    [0.0, 0.0],
                    [12.0, 0.0],
                    [12.0, 12.0],
                    [6.0, 5.0],
                    [0.0, 12.0]
                ],
                &[[6.0, 9.0], [0.0, 5.0], [6.0, 4.0], [12.0, 6.0], [6.0, -1.0]],
            ),
            Some(vec![1, 2, 3]),
        );
    }

    #[test]
    fn lasso_accepts_two_lobes_of_a_self_crossing_path() {
        assert_eq!(
            select(
                ViewerMaskSelectionMode::Lasso,
                &[[0.0, 0.0], [10.0, 10.0], [0.0, 10.0], [10.0, 0.0]],
                &[[5.0, 2.0], [5.0, 8.0], [1.0, 5.0], [5.0, 5.0]],
            ),
            Some(vec![0, 1, 3]),
        );
    }

    #[test]
    fn degenerate_or_non_finite_gestures_do_not_replace_selection() {
        assert!(AnchorSelectionDrag::start(
            ViewerMaskSelectionMode::Lasso,
            Point::new(f32::INFINITY, 0.0)
        )
        .is_none());
        for path in [
            [[0.0, 0.0], [3.0, 3.0], [10.0, 10.0]],
            [[0.0, 0.0], [5.0, 5.0], [f32::NAN, 10.0]],
        ] {
            assert_eq!(
                select(ViewerMaskSelectionMode::Lasso, &path, &[[3.0, 3.0]]),
                None
            );
        }
    }

    #[test]
    fn long_lasso_bounds_storage_and_keeps_tracking() {
        let mut drag =
            AnchorSelectionDrag::start(ViewerMaskSelectionMode::Lasso, Point::ZERO).unwrap();
        for x in 1..1000 {
            drag.track(Point::new((x * 4) as f32, 30.0));
        }
        let mut first = None;
        let mut last = None;
        let mut count = 0;
        drag.for_each_edge(|a, b| {
            first.get_or_insert(a);
            last = Some((a, b));
            count += 1;
        });
        assert!(count <= MAX_LASSO_POINTS + 1);
        assert_eq!(first, Some(Point::ZERO));
        assert_eq!(last, Some((Point::new(3996.0, 30.0), Point::ZERO)));
    }
}
