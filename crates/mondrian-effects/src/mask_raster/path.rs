//! Immutable flattened Path geometry and exact nearest-segment acceleration.

use super::{controlled_checkpoint, ControlledMaskRasterError, MaskRasterError};
use crate::mask::BezierPoint;
use glam::Vec2;

const BEZIER_SUBDIVISIONS: usize = 8;
const BVH_LEAF_SEGMENTS: usize = 8;

#[derive(Debug)]
pub(super) struct PreparedPath {
    point: Option<Vec2>,
    segments: Vec<PathSegment>,
    nodes: Vec<BvhNode>,
    root: Option<usize>,
    closed: bool,
}

impl PreparedPath {
    pub(super) fn required_max_row_scratch_bytes(
        point_count: usize,
        closed: bool,
    ) -> Result<usize, MaskRasterError> {
        let pair_count = match point_count {
            0 | 1 => 0,
            count if closed => count,
            count => count - 1,
        };
        pair_count
            .checked_mul(BEZIER_SUBDIVISIONS)
            .and_then(|segments| segments.checked_mul(std::mem::size_of::<f32>()))
            .ok_or(MaskRasterError::GeometrySizeOverflow {
                reason: "Path row scratch byte count overflowed",
            })
    }

    pub(super) fn required_retained_bytes(
        point_count: usize,
        closed: bool,
    ) -> Result<usize, MaskRasterError> {
        let pair_count = match point_count {
            0 | 1 => 0,
            count if closed => count,
            count => count - 1,
        };
        let segment_count = pair_count.checked_mul(BEZIER_SUBDIVISIONS).ok_or(
            MaskRasterError::GeometrySizeOverflow { reason: "Bezier subdivision count overflowed" },
        )?;
        let node_capacity =
            segment_count.checked_mul(2).ok_or(MaskRasterError::GeometrySizeOverflow {
                reason: "Path spatial-index node count overflowed",
            })?;
        std::mem::size_of::<Self>()
            .checked_add(
                segment_count.checked_mul(std::mem::size_of::<PathSegment>()).ok_or(
                    MaskRasterError::GeometrySizeOverflow {
                        reason: "Path segment byte count overflowed",
                    },
                )?,
            )
            .and_then(|bytes| {
                node_capacity
                    .checked_mul(std::mem::size_of::<BvhNode>())
                    .and_then(|node_bytes| bytes.checked_add(node_bytes))
            })
            .ok_or(MaskRasterError::GeometrySizeOverflow {
                reason: "Path retained byte count overflowed",
            })
    }

    pub(super) fn prepare_controlled<E>(
        points: &[BezierPoint],
        closed: bool,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, ControlledMaskRasterError<E>> {
        controlled_checkpoint(checkpoint)?;
        if points.is_empty() {
            return Ok(Self {
                point: None,
                segments: Vec::new(),
                nodes: Vec::new(),
                root: None,
                closed,
            });
        }
        if points.len() == 1 {
            return Ok(Self {
                point: Some(points[0].position),
                segments: Vec::new(),
                nodes: Vec::new(),
                root: None,
                closed,
            });
        }

        let pair_count = if closed {
            points.len()
        } else {
            points.len() - 1
        };
        let segment_count = pair_count.checked_mul(BEZIER_SUBDIVISIONS).ok_or(
            MaskRasterError::GeometrySizeOverflow { reason: "Bezier subdivision count overflowed" },
        )?;
        let mut segments = Vec::with_capacity(segment_count);
        for index in 0..pair_count {
            controlled_checkpoint(checkpoint)?;
            let next = if index + 1 < points.len() {
                index + 1
            } else {
                0
            };
            subdivide_bezier(points[index], points[next], &mut segments);
        }
        let node_capacity =
            segments.len().checked_mul(2).ok_or(MaskRasterError::GeometrySizeOverflow {
                reason: "Path spatial-index node count overflowed",
            })?;
        let mut nodes = Vec::with_capacity(node_capacity);
        let segment_len = segments.len();
        let root = build_bvh_controlled(&mut segments, &mut nodes, 0, segment_len, checkpoint)?;
        Ok(Self {
            point: None,
            segments,
            nodes,
            root: Some(root),
            closed,
        })
    }

    pub(super) fn signed_distance(&self, point: Vec2, row_crossings: &[f32]) -> f32 {
        if let Some(single) = self.point {
            return point.distance(single);
        }
        let Some(root) = self.root else {
            return 1.0;
        };
        let mut best_squared = f32::MAX;
        self.query_distance(root, point, &mut best_squared);
        let distance = best_squared.sqrt();
        if self.closed {
            let first_right = row_crossings.partition_point(|crossing| *crossing <= point.x);
            if !(row_crossings.len() - first_right).is_multiple_of(2) {
                -distance
            } else {
                distance
            }
        } else {
            distance
        }
    }

    pub(super) fn row_crossings(&self, y: f32, output: &mut Vec<f32>) {
        output.clear();
        if !self.closed {
            return;
        }
        for segment in &self.segments {
            let crosses =
                (segment.a.y <= y && segment.b.y > y) || (segment.b.y <= y && segment.a.y > y);
            if crosses {
                let t = (y - segment.a.y) / (segment.b.y - segment.a.y);
                output.push(segment.a.x + t * (segment.b.x - segment.a.x));
            }
        }
        output.sort_unstable_by(|left, right| left.total_cmp(right));
    }

    pub(super) fn max_row_scratch_bytes(&self) -> usize {
        self.segments.len().saturating_mul(std::mem::size_of::<f32>())
    }

    pub(super) fn retained_bytes(&self) -> usize {
        Self::required_retained_bytes(
            if self.segments.is_empty() {
                usize::from(self.point.is_some())
            } else if self.closed {
                self.segments.len() / BEZIER_SUBDIVISIONS
            } else {
                self.segments.len() / BEZIER_SUBDIVISIONS + 1
            },
            self.closed,
        )
        .unwrap_or(usize::MAX)
    }

    #[cfg(test)]
    pub(super) fn segment_count(&self) -> usize {
        self.segments.len()
    }

    fn query_distance(&self, node_id: usize, point: Vec2, best_squared: &mut f32) {
        let node = &self.nodes[node_id];
        if node.bounds.distance_squared(point) >= *best_squared {
            return;
        }
        match node.kind {
            BvhNodeKind::Leaf { start, len } => {
                for segment in &self.segments[start..start + len] {
                    *best_squared =
                        (*best_squared).min(point_to_segment_distance_squared(point, *segment));
                }
            }
            BvhNodeKind::Branch { left, right } => {
                let left_distance = self.nodes[left].bounds.distance_squared(point);
                let right_distance = self.nodes[right].bounds.distance_squared(point);
                if left_distance <= right_distance {
                    self.query_distance(left, point, best_squared);
                    self.query_distance(right, point, best_squared);
                } else {
                    self.query_distance(right, point, best_squared);
                    self.query_distance(left, point, best_squared);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PathSegment {
    a: Vec2,
    b: Vec2,
    bounds: Bounds,
    centroid: Vec2,
}

impl PathSegment {
    fn new(a: Vec2, b: Vec2) -> Self {
        Self {
            a,
            b,
            bounds: Bounds::from_points(a, b),
            centroid: (a + b) * 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BvhNode {
    bounds: Bounds,
    kind: BvhNodeKind,
}

#[derive(Debug, Clone, Copy)]
enum BvhNodeKind {
    Leaf { start: usize, len: usize },
    Branch { left: usize, right: usize },
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    min: Vec2,
    max: Vec2,
}

impl Bounds {
    fn from_points(a: Vec2, b: Vec2) -> Self {
        Self { min: a.min(b), max: a.max(b) }
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }

    fn distance_squared(self, point: Vec2) -> f32 {
        let dx = if point.x < self.min.x {
            self.min.x - point.x
        } else if point.x > self.max.x {
            point.x - self.max.x
        } else {
            0.0
        };
        let dy = if point.y < self.min.y {
            self.min.y - point.y
        } else if point.y > self.max.y {
            point.y - self.max.y
        } else {
            0.0
        };
        dx * dx + dy * dy
    }
}

fn build_bvh_controlled<E>(
    segments: &mut [PathSegment],
    nodes: &mut Vec<BvhNode>,
    start: usize,
    end: usize,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<usize, ControlledMaskRasterError<E>> {
    controlled_checkpoint(checkpoint)?;
    let bounds = segments[start..end]
        .iter()
        .map(|segment| segment.bounds)
        .reduce(Bounds::union)
        .ok_or(MaskRasterError::InvalidGeometry {
            reason: "Path spatial index received no segments",
        })?;
    let node_id = nodes.len();
    nodes.push(BvhNode {
        bounds,
        kind: BvhNodeKind::Leaf { start, len: end - start },
    });
    if end - start <= BVH_LEAF_SEGMENTS {
        return Ok(node_id);
    }

    let extent = bounds.max - bounds.min;
    if extent.x >= extent.y {
        segments[start..end]
            .sort_unstable_by(|left, right| left.centroid.x.total_cmp(&right.centroid.x));
    } else {
        segments[start..end]
            .sort_unstable_by(|left, right| left.centroid.y.total_cmp(&right.centroid.y));
    }
    let middle = start + (end - start) / 2;
    let left = build_bvh_controlled(segments, nodes, start, middle, checkpoint)?;
    let right = build_bvh_controlled(segments, nodes, middle, end, checkpoint)?;
    nodes[node_id].kind = BvhNodeKind::Branch { left, right };
    Ok(node_id)
}

fn subdivide_bezier(a: BezierPoint, b: BezierPoint, output: &mut Vec<PathSegment>) {
    let inverse_steps = 1.0 / BEZIER_SUBDIVISIONS as f32;
    let mut previous = a.position;
    for step in 1..=BEZIER_SUBDIVISIONS {
        let point = cubic_bezier(a, b, step as f32 * inverse_steps);
        output.push(PathSegment::new(previous, point));
        previous = point;
    }
}

fn cubic_bezier(a: BezierPoint, b: BezierPoint, t: f32) -> Vec2 {
    let t2 = t * t;
    let t3 = t2 * t;
    let inverse = 1.0 - t;
    let inverse2 = inverse * inverse;
    let inverse3 = inverse2 * inverse;
    a.position * inverse3
        + (a.position + a.control_out) * (3.0 * inverse2 * t)
        + (b.position + b.control_in) * (3.0 * inverse * t2)
        + b.position * t3
}

fn point_to_segment_distance_squared(point: Vec2, segment: PathSegment) -> f32 {
    let direction = segment.b - segment.a;
    let from_start = point - segment.a;
    let length_squared = direction.length_squared();
    if length_squared < 1.0e-10 {
        return from_start.length_squared();
    }
    let t = (from_start.dot(direction) / length_squared).clamp(0.0, 1.0);
    point.distance_squared(segment.a + direction * t)
}
