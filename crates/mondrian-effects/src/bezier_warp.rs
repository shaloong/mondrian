//! Bezier warp effect (DST-15): remaps the layer through a Coons patch.
//!
//! The twelve authored control points are a ring of four corners plus two edge
//! tangents between each pair of corners. They are authored in a 0..256
//! reference box, normalized by [`BEZIER_WARP_REFERENCE_EXTENT`] on the way in,
//! and the whole patch is evaluated in that normalized space; sampling
//! multiplies the solved `(u, v)` back by the frame size, so the effect is
//! independent of the frame size and of the project resolution.
//!
//! # The patch
//!
//! All four cubic edges run in the same direction — top and bottom left to
//! right, left and right top to bottom (which is why the dump's bottom row is
//! authored right to left and its left column bottom to top: the two author
//! orders are reversed here) — and they build the Coons patch
//!
//! ```text
//! P(u, v) = (1-v)·T(u) + v·B(u) + (1-u)·L(v) + u·R(v)
//!         - [(1-u)(1-v)·TL + u(1-v)·TR + (1-u)·v·BL + u·v·BR]
//! ```
//!
//! so the edges are interpolated exactly (`P(u, 0) = T(u)`, `P(u, 1) = B(u)`,
//! `P(0, v) = L(v)`, `P(1, v) = R(v)`) and the surface stays inside the convex
//! hull of the twelve points.
//!
//! # The inverse
//!
//! Rendering needs the inverse map: for every output pixel the kernel solves
//! `P(u, v) = target` with `steps = 3 + quality` Newton iterations. The
//! iteration count is fixed — an early exit on a converged pixel would let the
//! CPU and the GPU disagree on the last step, because they evaluate the same
//! arithmetic in different precisions. The only branch in the solver is the
//! failure guard (`det <= 0`, a non-finite value, or `|u| > 4` / `|v| > 4`),
//! which leaves that pixel as the untouched straight source, bit for bit, so a
//! folded patch can never tear, loop, or produce NaN. Every other pixel clamps
//! the solved `(u, v)` to `[0, 1]`, which maps out-of-domain output pixels onto
//! the nearest edge texel (edge stretching, the same convention as the other
//! remap effects in this crate).
//!
//! # Identity
//!
//! [`BezierWarpGrade::is_identity`] is structural rather than bitwise: the four
//! corners must be the reference corners exactly, and every edge cubic must
//! stay within [`BEZIER_WARP_IDENTITY_TOLERANCE_PIXELS`] of its chord. The
//! tolerance is what lets the dump's `85.3248046875` tangents count as the
//! identity — `256/3` is `85.3333…`, so the default patch is a 0.01 % shrink,
//! i.e. 0.0256 px of drift. A bitwise comparison would reject the defaults, and
//! the 0.0256 px step at the tolerance boundary is the documented price.
//!
//! # Alpha
//!
//! Straight in, straight out. The kernel snapshots the straight frame,
//! premultiplies a *copy*, samples the copy bilinearly, un-premultiplies each
//! sample, and writes the result back into the straight frame — so a
//! transparent neighbour never darkens a visible pixel and there is no
//! whole-frame premultiply/un-premultiply round trip.

use crate::coverage::straight_rgba_from_premultiplied;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::premultiply_rgba_controlled;

/// Width and height of the author-facing reference box.
pub const BEZIER_WARP_REFERENCE_EXTENT: f32 = 256.0;

/// Smallest accepted `quality` option.
pub const BEZIER_WARP_MIN_QUALITY: u32 = 1;

/// Largest accepted `quality` option.
pub const BEZIER_WARP_MAX_QUALITY: u32 = 10;

/// Default `quality` option.
pub const BEZIER_WARP_DEFAULT_QUALITY: u32 = 1;

/// How far an edge cubic may leave its chord and still count as straight, in
/// reference-space pixels.
pub const BEZIER_WARP_IDENTITY_TOLERANCE_PIXELS: f64 = 0.125;

/// Newton iterations added to the `quality` option to get the step count.
pub const BEZIER_WARP_STEPS_BASE: u32 = 3;

/// The authored default control points in the 0..256 reference box, in dump
/// order: top-left corner, top edge tangents left to right, top-right corner,
/// right edge tangents top to bottom, bottom-right corner, bottom edge tangents
/// right to left, bottom-left corner, left edge tangents bottom to top.
///
/// The tangents sit at `0.3333` of the box rather than at exactly `256/3`, so
/// the default patch is a 0.01 % pincushion instead of a perfect identity; see
/// [`BEZIER_WARP_IDENTITY_TOLERANCE_PIXELS`] for why that still counts as one.
pub const BEZIER_WARP_DEFAULT_REFERENCE_POINTS: [[f32; 2]; 12] = [
    [0.0, 0.0],
    [85.324_81, 0.0],
    [170.649_61, 0.0],
    [256.0, 0.0],
    [256.0, 85.324_81],
    [256.0, 170.649_61],
    [256.0, 256.0],
    [170.649_61, 256.0],
    [85.324_81, 256.0],
    [0.0, 256.0],
    [0.0, 170.649_61],
    [0.0, 85.324_81],
];

/// Number of segments the identity probe splits every edge chord into.
const BEZIER_WARP_IDENTITY_SEGMENTS: usize = 8;

/// Largest control coordinate the kernel accepts, in normalized units.
const BEZIER_WARP_MAX_NORMALIZED_COORDINATE: f32 = 4.0;

/// Where the divergence guard stops the Newton iteration, in normalized units.
const BEZIER_WARP_DIVERGENCE_LIMIT: f64 = 4.0;

/// A point in the normalized patch space.
type Point = (f64, f64);

/// The four floats that carry the iteration count to the GPU.
type ControlTail = [f32; 4];

/// Why a [`BezierWarpGrade`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BezierWarpError {
    /// A control point coordinate is outside the accepted range.
    ControlPointRange,
    /// The `quality` option is outside [`BEZIER_WARP_MIN_QUALITY`] ..
    /// [`BEZIER_WARP_MAX_QUALITY`].
    QualityOutOfRange,
    /// A control point coordinate is NaN or infinite.
    NonFinite,
}

impl std::fmt::Display for BezierWarpError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControlPointRange => formatter.write_str(
                "bezier warp control points must stay inside the reference box (0..256)",
            ),
            Self::QualityOutOfRange => {
                formatter.write_str("bezier warp quality must be between 1 and 10")
            }
            Self::NonFinite => formatter.write_str("bezier warp control points must be finite"),
        }
    }
}

impl std::error::Error for BezierWarpError {}

/// A validated bezier warp: twelve normalized control points and a quality.
///
/// The points are stored in the dump order, clockwise from the top-left
/// corner, and normalized by [`BEZIER_WARP_REFERENCE_EXTENT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BezierWarpGrade {
    points: [[f32; 2]; 12],
    quality: u32,
}

impl BezierWarpGrade {
    /// Builds a grade from points already normalized into the 0..1 box.
    ///
    /// # Errors
    ///
    /// Returns [`BezierWarpError::QualityOutOfRange`] for a quality outside
    /// `1..=10`, [`BezierWarpError::NonFinite`] for a NaN or infinite
    /// coordinate, and [`BezierWarpError::ControlPointRange`] for a coordinate
    /// outside `-4..=4` (a point that far out cannot describe a patch anyone
    /// authored on purpose, and it keeps the solver's arithmetic bounded).
    pub fn new(points: [[f32; 2]; 12], quality: u32) -> Result<Self, BezierWarpError> {
        if !(BEZIER_WARP_MIN_QUALITY..=BEZIER_WARP_MAX_QUALITY).contains(&quality) {
            return Err(BezierWarpError::QualityOutOfRange);
        }
        for point in points {
            for coordinate in point {
                if !coordinate.is_finite() {
                    return Err(BezierWarpError::NonFinite);
                }
                if coordinate.abs() > BEZIER_WARP_MAX_NORMALIZED_COORDINATE {
                    return Err(BezierWarpError::ControlPointRange);
                }
            }
        }
        Ok(Self { points, quality })
    }

    /// Builds a grade from the authored 0..256 reference box.
    ///
    /// # Errors
    ///
    /// The same failures as [`BezierWarpGrade::new`], measured on the
    /// normalized points.
    pub fn from_reference_points(
        points: [[f32; 2]; 12],
        quality: u32,
    ) -> Result<Self, BezierWarpError> {
        let mut normalized = [[0.0_f32; 2]; 12];
        for (target, point) in normalized.iter_mut().zip(points) {
            *target = [
                point[0] / BEZIER_WARP_REFERENCE_EXTENT,
                point[1] / BEZIER_WARP_REFERENCE_EXTENT,
            ];
        }
        Self::new(normalized, quality)
    }

    /// The twelve normalized control points, in dump order.
    #[must_use]
    pub fn points(&self) -> [[f32; 2]; 12] {
        self.points
    }

    /// The requested quality.
    #[must_use]
    pub fn quality(&self) -> u32 {
        self.quality
    }

    /// The number of Newton iterations the kernel runs per pixel.
    #[must_use]
    pub fn steps(&self) -> u32 {
        BEZIER_WARP_STEPS_BASE + self.quality
    }

    /// Whether the patch leaves every pixel where it was.
    ///
    /// The corners must be the reference corners exactly, and no edge may bow
    /// away from its chord by more than
    /// [`BEZIER_WARP_IDENTITY_TOLERANCE_PIXELS`]. A tangent that slides along
    /// its own straight edge therefore keeps the identity, which is what makes
    /// the authored defaults (whose tangents sit at `85.3248…`, not the exact
    /// `256/3`) count as untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        if self.points[0] != [0.0, 0.0]
            || self.points[3] != [1.0, 0.0]
            || self.points[6] != [1.0, 1.0]
            || self.points[9] != [0.0, 1.0]
        {
            return false;
        }
        let tolerance =
            BEZIER_WARP_IDENTITY_TOLERANCE_PIXELS / f64::from(BEZIER_WARP_REFERENCE_EXTENT);
        self.edge_curves().iter().all(|edge| {
            let chord = (edge.end.0 - edge.start.0, edge.end.1 - edge.start.1);
            let chord_length = (chord.0 * chord.0 + chord.1 * chord.1).sqrt();
            (0..=BEZIER_WARP_IDENTITY_SEGMENTS).all(|segment| {
                let t = segment as f64 / BEZIER_WARP_IDENTITY_SEGMENTS as f64;
                let curve = edge.eval(t);
                let offset = (curve.0 - edge.start.0, curve.1 - edge.start.1);
                // Perpendicular distance to the chord, so a tangent that slides
                // along its own straight edge keeps the identity.
                let bow = (chord.0 * offset.1 - chord.1 * offset.0).abs() / chord_length;
                bow <= tolerance
            })
        })
    }

    /// The uniform payload: six cells of two points, then the step count.
    #[must_use]
    pub fn gpu_controls(&self) -> ([[f32; 4]; 6], [f32; 4]) {
        let point = |index: usize| self.points[index];
        let cells = [
            [point(0)[0], point(0)[1], point(1)[0], point(1)[1]],
            [point(2)[0], point(2)[1], point(3)[0], point(3)[1]],
            [point(4)[0], point(4)[1], point(5)[0], point(5)[1]],
            [point(6)[0], point(6)[1], point(7)[0], point(7)[1]],
            [point(8)[0], point(8)[1], point(9)[0], point(9)[1]],
            [point(10)[0], point(10)[1], point(11)[0], point(11)[1]],
        ];
        (cells, [self.steps() as f32, 0.0, 0.0, 0.0])
    }

    /// The four edge cubics, all running in the same direction.
    ///
    /// Top runs left to right, bottom left to right, left side top to bottom,
    /// and right side top to bottom. The dump authors the bottom edge right to
    /// left and the left edge bottom to top, so those two are reversed here.
    fn edge_curves(&self) -> [Cubic; 4] {
        let point = |index: usize| {
            let stored = self.points[index];
            (f64::from(stored[0]), f64::from(stored[1]))
        };
        [
            Cubic {
                start: point(0),
                first: point(1),
                second: point(2),
                end: point(3),
            },
            Cubic {
                start: point(9),
                first: point(8),
                second: point(7),
                end: point(6),
            },
            Cubic {
                start: point(0),
                first: point(11),
                second: point(10),
                end: point(9),
            },
            Cubic {
                start: point(3),
                first: point(4),
                second: point(5),
                end: point(6),
            },
        ]
    }

    /// The patch with its derived curves, ready for evaluation.
    fn resolved(&self) -> ResolvedPatch {
        let edges = self.edge_curves();
        ResolvedPatch {
            corners: [edges[0].start, edges[0].end, edges[1].end, edges[1].start],
            edges,
            steps: self.steps(),
        }
    }

    /// Evaluates the patch forward: which normalized point does `(u, v)` land
    /// on.
    #[must_use]
    pub fn patch(&self, u: f64, v: f64) -> Point {
        self.resolved().patch(u, v)
    }

    /// The partial derivative of [`BezierWarpGrade::patch`] with respect to `u`.
    #[must_use]
    pub fn patch_du(&self, u: f64, v: f64) -> Point {
        self.resolved().patch_du(u, v)
    }

    /// The partial derivative of [`BezierWarpGrade::patch`] with respect to `v`.
    #[must_use]
    pub fn patch_dv(&self, u: f64, v: f64) -> Point {
        self.resolved().patch_dv(u, v)
    }

    /// Solves the inverse map for one normalized target point.
    ///
    /// Returns `None` when the patch folds over that target or the iteration
    /// runs away; the caller then keeps the source pixel. On success the result
    /// is clamped to `[0, 1]`.
    #[must_use]
    pub fn solve(&self, target: Point) -> Option<Point> {
        self.resolved().solve(target)
    }
}

/// A cubic bezier edge of the patch.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cubic {
    start: Point,
    first: Point,
    second: Point,
    end: Point,
}

impl Cubic {
    /// Evaluates the curve at `t`.
    fn eval(self, t: f64) -> Point {
        let inverse = 1.0 - t;
        let a = inverse * inverse * inverse;
        let b = 3.0 * inverse * inverse * t;
        let c = 3.0 * inverse * t * t;
        let d = t * t * t;
        (
            a * self.start.0 + b * self.first.0 + c * self.second.0 + d * self.end.0,
            a * self.start.1 + b * self.first.1 + c * self.second.1 + d * self.end.1,
        )
    }

    /// Evaluates the derivative at `t`: `3[(1-t)²(P1-P0) + 2(1-t)t(P2-P1) +
    /// t²(P3-P2)]`.
    fn derivative(self, t: f64) -> Point {
        let inverse = 1.0 - t;
        let a = 3.0 * inverse * inverse;
        let b = 6.0 * inverse * t;
        let c = 3.0 * t * t;
        (
            a * (self.first.0 - self.start.0)
                + b * (self.second.0 - self.first.0)
                + c * (self.end.0 - self.second.0),
            a * (self.first.1 - self.start.1)
                + b * (self.second.1 - self.first.1)
                + c * (self.end.1 - self.second.1),
        )
    }
}

/// The patch in the shape the kernel evaluates.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ResolvedPatch {
    /// Top-left, top-right, bottom-right, bottom-left.
    corners: [Point; 4],
    /// Top, bottom, left, right.
    edges: [Cubic; 4],
    steps: u32,
}

impl ResolvedPatch {
    /// The Coons patch itself.
    fn patch(self, u: f64, v: f64) -> Point {
        let top = self.edges[0].eval(u);
        let bottom = self.edges[1].eval(u);
        let left = self.edges[2].eval(v);
        let right = self.edges[3].eval(v);
        let [top_left, top_right, bottom_right, bottom_left] = self.corners;
        let across = 1.0 - u;
        let down = 1.0 - v;
        (
            down * top.0 + v * bottom.0 + across * left.0 + u * right.0
                - (across * down * top_left.0
                    + u * down * top_right.0
                    + across * v * bottom_left.0
                    + u * v * bottom_right.0),
            down * top.1 + v * bottom.1 + across * left.1 + u * right.1
                - (across * down * top_left.1
                    + u * down * top_right.1
                    + across * v * bottom_left.1
                    + u * v * bottom_right.1),
        )
    }

    /// `∂P/∂u`.
    fn patch_du(self, u: f64, v: f64) -> Point {
        let top = self.edges[0].derivative(u);
        let bottom = self.edges[1].derivative(u);
        let left = self.edges[2].eval(v);
        let right = self.edges[3].eval(v);
        let [top_left, top_right, bottom_right, bottom_left] = self.corners;
        let down = 1.0 - v;
        (
            down * top.0 + v * bottom.0 - left.0
                + right.0
                + (down * (top_left.0 - top_right.0) + v * (bottom_left.0 - bottom_right.0)),
            down * top.1 + v * bottom.1 - left.1
                + right.1
                + (down * (top_left.1 - top_right.1) + v * (bottom_left.1 - bottom_right.1)),
        )
    }

    /// `∂P/∂v`.
    fn patch_dv(self, u: f64, v: f64) -> Point {
        let top = self.edges[0].eval(u);
        let bottom = self.edges[1].eval(u);
        let left = self.edges[2].derivative(v);
        let right = self.edges[3].derivative(v);
        let [top_left, top_right, bottom_right, bottom_left] = self.corners;
        let across = 1.0 - u;
        (
            across * left.0 + u * right.0 - top.0
                + bottom.0
                + (across * (top_left.0 - bottom_left.0) + u * (top_right.0 - bottom_right.0)),
            across * left.1 + u * right.1 - top.1
                + bottom.1
                + (across * (top_left.1 - bottom_left.1) + u * (top_right.1 - bottom_right.1)),
        )
    }

    /// The fixed-count Newton solve described in the module docs.
    fn solve(self, target: Point) -> Option<Point> {
        self.solve_with(target, self.steps)
    }

    /// The same solve with an explicit step count, so tests can show the
    /// residual shrinking before it reaches the f64 floor.
    fn solve_with(self, target: Point, steps: u32) -> Option<Point> {
        let (mut u, mut v) = target;
        for _ in 0..steps {
            let patch = self.patch(u, v);
            let residual = (patch.0 - target.0, patch.1 - target.1);
            let du = self.patch_du(u, v);
            let dv = self.patch_dv(u, v);
            let determinant = du.0 * dv.1 - dv.0 * du.1;
            if determinant.is_nan() || determinant <= 0.0 {
                return None;
            }
            let step_u = (-residual.0 * dv.1 + residual.1 * dv.0) / determinant;
            let step_v = (-residual.1 * du.0 + residual.0 * du.1) / determinant;
            if !(step_u.is_finite() && step_v.is_finite()) {
                return None;
            }
            u += step_u;
            v += step_v;
            if !(u.is_finite() && v.is_finite()) {
                return None;
            }
            if u.abs() > BEZIER_WARP_DIVERGENCE_LIMIT || v.abs() > BEZIER_WARP_DIVERGENCE_LIMIT {
                return None;
            }
        }
        Some((u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)))
    }
}

/// Applies a bezier warp to a straight-alpha `f32` frame.
///
/// Returns `false` when the frame does not describe a `width × height` image;
/// the pixels are left untouched in every failure case.
#[must_use]
pub fn bezier_warp_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: BezierWarpGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_bezier_warp_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies a bezier warp with a cancellation checkpoint.
///
/// # Errors
///
/// Only the checkpoint's error: the warp itself cannot fail. A frame that does
/// not describe a `width × height` image returns `Ok(false)` untouched, and a
/// [`BezierWarpGrade::is_identity`] patch returns `Ok(true)` untouched.
pub fn apply_bezier_warp_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: BezierWarpGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    let patch = grade.resolved();
    let frame = &mut working[..pixel_count];
    let source = frame.to_vec();
    let mut staged = source.clone();
    premultiply_rgba_controlled(&mut staged, checkpoint)?;
    let scale_x = width as f64;
    let scale_y = height as f64;
    crate::parallel::for_each_row_band(frame, width, 0..height, |row_start, band| {
        for (offset, row_pixels) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in row_pixels.iter_mut().enumerate() {
                let index = row * width + column;
                let target = (column as f64 / scale_x, row as f64 / scale_y);
                let Some((u, v)) = patch.solve(target) else {
                    *pixel = source[index];
                    continue;
                };
                let sample =
                    sample_bilinear_clamped(&staged, width, height, u * scale_x, v * scale_y);
                *pixel = straight_rgba_from_premultiplied(sample);
            }
        }
    });
    checkpoint()?;
    Ok(true)
}

/// The uniform payload of a bezier warp: six two-point cells, then the step
/// count in the first lane of a seventh cell.
pub type BezierWarpControls = ([[f32; 4]; 6], ControlTail);

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_REFERENCE_POINTS: [[f32; 2]; 12] = [
        [0.0, 0.0],
        [85.324_81, 0.0],
        [170.649_61, 0.0],
        [256.0, 0.0],
        [256.0, 85.324_81],
        [256.0, 170.649_61],
        [256.0, 256.0],
        [170.649_61, 256.0],
        [85.324_81, 256.0],
        [0.0, 256.0],
        [0.0, 170.649_61],
        [0.0, 85.324_81],
    ];

    fn default_grade(quality: u32) -> BezierWarpGrade {
        BezierWarpGrade::from_reference_points(DEFAULT_REFERENCE_POINTS, quality)
            .expect("the authored defaults are inside the reference box")
    }

    /// A symmetric patch that shrinks to the middle half of the frame.
    fn shrunk_grade(quality: u32) -> BezierWarpGrade {
        let low = 0.25_f32;
        let high = 0.75_f32;
        let third = (high - low) / 3.0;
        BezierWarpGrade::new(
            [
                [low, low],
                [low + third, low],
                [high - third, low],
                [high, low],
                [high, low + third],
                [high, high - third],
                [high, high],
                [high - third, high],
                [low + third, high],
                [low, high],
                [low, high - third],
                [low, low + third],
            ],
            quality,
        )
        .expect("the shrunk patch is well formed")
    }

    fn frame(width: usize, height: usize, seed: u32) -> Vec<[f32; 4]> {
        let mut state = seed;
        (0..width * height)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let mut channel = || {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 8) as f32 / 16_777_216.0
                };
                [channel(), channel(), channel(), channel()]
            })
            .collect()
    }

    fn bend_the_top_edge(grade: BezierWarpGrade, offset: f32) -> BezierWarpGrade {
        let mut points = grade.points();
        points[1] = [
            DEFAULT_REFERENCE_POINTS[1][0] / BEZIER_WARP_REFERENCE_EXTENT,
            offset,
        ];
        BezierWarpGrade::new(points, grade.quality()).expect("the bent patch is valid")
    }

    fn stored_point(grade: &BezierWarpGrade, index: usize) -> Point {
        let point = grade.points()[index];
        (f64::from(point[0]), f64::from(point[1]))
    }

    #[test]
    fn rejects_author_state_outside_the_declared_ranges() {
        assert_eq!(
            BezierWarpGrade::from_reference_points(DEFAULT_REFERENCE_POINTS, 0),
            Err(BezierWarpError::QualityOutOfRange)
        );
        assert_eq!(
            BezierWarpGrade::from_reference_points(DEFAULT_REFERENCE_POINTS, 11),
            Err(BezierWarpError::QualityOutOfRange)
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut points = DEFAULT_REFERENCE_POINTS;
            points[5] = [bad, 0.0];
            assert_eq!(
                BezierWarpGrade::from_reference_points(points, 1),
                Err(BezierWarpError::NonFinite)
            );
        }
        let mut points = DEFAULT_REFERENCE_POINTS;
        points[5] = [f32::MAX, 0.0];
        assert_eq!(
            BezierWarpGrade::from_reference_points(points, 1),
            Err(BezierWarpError::ControlPointRange)
        );
        assert_eq!(default_grade(10).steps(), 13);
        assert_eq!(default_grade(1).steps(), 4);
    }

    #[test]
    fn the_default_points_are_the_identity() {
        assert!(default_grade(1).is_identity());
        assert!(default_grade(10).is_identity());
    }

    #[test]
    fn a_moved_tangent_along_the_edge_stays_the_identity() {
        // Sliding the top-left tangent towards the corner keeps the top edge on
        // its chord, so nothing moves.
        let mut points = DEFAULT_REFERENCE_POINTS;
        points[1] = [60.0, 0.0];
        let grade = BezierWarpGrade::from_reference_points(points, 1).expect("valid");
        assert!(grade.is_identity());
    }

    #[test]
    fn bending_an_edge_leaves_the_identity() {
        assert!(!bend_the_top_edge(default_grade(1), 0.07).is_identity());
        // A whole reference pixel of bow is far outside the tolerance.
        let mut points = DEFAULT_REFERENCE_POINTS;
        points[1] = [85.324_81, 1.0];
        let grade = BezierWarpGrade::from_reference_points(points, 1).expect("valid");
        assert!(!grade.is_identity());
    }

    #[test]
    fn the_default_patch_is_within_a_fifth_of_a_pixel_of_identity() {
        let grade = default_grade(1);
        for row in 0..9 {
            for column in 0..9 {
                let u = column as f64 / 8.0;
                let v = row as f64 / 8.0;
                let patch = grade.patch(u, v);
                let dx = patch.0 - u;
                let dy = patch.1 - v;
                let drift = (dx * dx + dy * dy).sqrt() * f64::from(BEZIER_WARP_REFERENCE_EXTENT);
                assert!(drift <= 0.03, "drift {drift} at ({u}, {v})");
            }
        }
    }

    #[test]
    fn the_corners_are_pinned() {
        let grade = bend_the_top_edge(shrunk_grade(1), 0.2);
        let [top, bottom, left, right] = grade.edge_curves();
        let tolerance = 1e-9;
        let close = |actual: Point, expected: Point| {
            (actual.0 - expected.0).abs() <= tolerance && (actual.1 - expected.1).abs() <= tolerance
        };
        assert!(close(grade.patch(0.0, 0.0), top.start), "top-left corner");
        assert!(close(grade.patch(1.0, 0.0), top.end), "top-right corner");
        assert!(
            close(grade.patch(1.0, 1.0), bottom.end),
            "bottom-right corner"
        );
        assert!(
            close(grade.patch(0.0, 1.0), bottom.start),
            "bottom-left corner"
        );
        assert!(close(grade.patch(0.0, 0.0), stored_point(&grade, 0)));
        assert!(close(grade.patch(1.0, 0.0), stored_point(&grade, 3)));
        assert!(close(grade.patch(1.0, 1.0), stored_point(&grade, 6)));
        assert!(close(grade.patch(0.0, 1.0), stored_point(&grade, 9)));
        assert!(
            close(left.start, top.start),
            "left edge starts at the top-left corner"
        );
        assert!(
            close(right.start, top.end),
            "right edge starts at the top-right corner"
        );
        assert!(
            close(left.end, bottom.start),
            "left edge ends at the bottom-left corner"
        );
        assert!(
            close(right.end, bottom.end),
            "right edge ends at the bottom-right corner"
        );
    }

    #[test]
    fn the_edges_are_interpolated() {
        let grade = bend_the_top_edge(shrunk_grade(3), 0.18);
        let [top, bottom, left, right] = grade.edge_curves();
        for sample in 0..=8 {
            let t = sample as f64 / 8.0;
            let tolerance = 1e-9;
            let close = |actual: Point, expected: Point| {
                (actual.0 - expected.0).abs() <= tolerance
                    && (actual.1 - expected.1).abs() <= tolerance
            };
            assert!(close(grade.patch(t, 0.0), top.eval(t)), "top edge at {t}");
            assert!(
                close(grade.patch(t, 1.0), bottom.eval(t)),
                "bottom edge at {t}"
            );
            assert!(close(grade.patch(0.0, t), left.eval(t)), "left edge at {t}");
            assert!(
                close(grade.patch(1.0, t), right.eval(t)),
                "right edge at {t}"
            );
        }
    }

    #[test]
    fn the_solver_inverts_the_patch() {
        let grade = bend_the_top_edge(shrunk_grade(5), 0.22);
        let mut state = 0x2f6e_2b1d_u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f64::from(state >> 8) / f64::from(1_u32 << 24)
        };
        let mut worst = 0.0_f64;
        for _ in 0..64 {
            let u = 0.05 + 0.9 * next();
            let v = 0.05 + 0.9 * next();
            let target = grade.patch(u, v);
            let solved = grade.solve(target).expect("the patch does not fold here");
            let landed = grade.patch(solved.0, solved.1);
            let dx = landed.0 - target.0;
            let dy = landed.1 - target.1;
            let residual = (dx * dx + dy * dy).sqrt() * f64::from(BEZIER_WARP_REFERENCE_EXTENT);
            worst = worst.max(residual);
        }
        assert!(worst <= 1e-6, "worst residual {worst} reference pixels");
    }

    #[test]
    fn a_higher_quality_shrinks_the_residual() {
        // A pincushion: every edge bows into the middle by 60 reference pixels,
        // which is strong, one-to-one curvature (the determinant stays above
        // 0.4). Newton is quadratic, so by four steps every non-singular patch
        // is already at the f64 floor; the strict shrink therefore shows up at
        // one, two and three steps, and the quality only has to keep the floor.
        let curved = |bow: f32, quality: u32| {
            let mut points = DEFAULT_REFERENCE_POINTS;
            let inset = 256.0 - bow;
            points[1] = [85.324_81, bow];
            points[2] = [170.649_61, bow];
            points[7] = [170.649_61, inset];
            points[8] = [85.324_81, inset];
            points[4] = [inset, 85.324_81];
            points[5] = [inset, 170.649_61];
            points[10] = [bow, 170.649_61];
            points[11] = [bow, 85.324_81];
            BezierWarpGrade::from_reference_points(points, quality).expect("valid")
        };
        let coarse = curved(60.0, 1);
        let fine = curved(60.0, 10);
        assert_eq!(coarse.steps(), 4);
        assert_eq!(fine.steps(), BEZIER_WARP_STEPS_BASE + 10);
        let patch = coarse.resolved();

        // Forward-map an interior grid: those targets have their preimage
        // inside the domain, so no solution gets clamped to the edge.
        let mut targets = Vec::new();
        for row in 1..8 {
            for column in 1..8 {
                let u = column as f64 / 8.0;
                let v = row as f64 / 8.0;
                targets.push(patch.patch(u, v));
            }
        }
        let residual = |steps: u32| {
            let mut worst = 0.0_f64;
            for target in &targets {
                let Some(solved) = patch.solve_with(*target, steps) else {
                    continue;
                };
                let landed = patch.patch(solved.0, solved.1);
                let dx = landed.0 - target.0;
                let dy = landed.1 - target.1;
                worst =
                    worst.max((dx * dx + dy * dy).sqrt() * f64::from(BEZIER_WARP_REFERENCE_EXTENT));
            }
            worst
        };
        let (one, two, three) = (residual(1), residual(2), residual(3));
        assert!(one > two, "one step leaves {one} px, two leave {two} px");
        assert!(
            two > three,
            "two steps leave {two} px, three leave {three} px"
        );
        let coarse = residual(coarse.steps());
        let fine = residual(fine.steps());
        assert!(fine <= 1e-5, "quality 10 leaves {fine} px");
        assert!(
            coarse >= fine,
            "quality 1 leaves {coarse} px, quality 10 leaves {fine} px"
        );
    }

    #[test]
    fn a_folded_patch_falls_back_to_the_identity() {
        // The mirrored patch maps (u, v) to (1 - u, v): its Jacobian
        // determinant is -1 everywhere, so every pixel must keep its source.
        let mirrored = BezierWarpGrade::new(
            [
                [1.0, 0.0],
                [2.0 / 3.0, 0.0],
                [1.0 / 3.0, 0.0],
                [0.0, 0.0],
                [0.0, 1.0 / 3.0],
                [0.0, 2.0 / 3.0],
                [0.0, 1.0],
                [1.0 / 3.0, 1.0],
                [2.0 / 3.0, 1.0],
                [1.0, 1.0],
                [1.0, 2.0 / 3.0],
                [1.0, 1.0 / 3.0],
            ],
            4,
        )
        .expect("valid");
        assert!(!mirrored.is_identity());
        assert_eq!(mirrored.solve((0.5, 0.5)), None);
        let mut pixels = frame(9, 7, 7);
        let untouched = pixels.clone();
        assert!(bezier_warp_frame_f32(&mut pixels, 9, 7, mirrored));
        assert_eq!(pixels, untouched);
    }

    #[test]
    fn out_of_domain_targets_clamp_to_the_edge() {
        let grade = shrunk_grade(4);
        let mut pixels = frame(8, 8, 11);
        let source = pixels.clone();
        assert!(bezier_warp_frame_f32(&mut pixels, 8, 8, grade));
        // The patch only covers the middle half, so the outer pixels solve
        // outside the domain, clamp to its boundary, and stretch the border
        // texel instead of reading outside the frame.
        assert_eq!(pixels[0], source[0]);
        assert_eq!(pixels[8 + 1], source[0]);
        assert_eq!(pixels[2 * 8 + 2], source[0]);
        assert_eq!(pixels[4 * 8 + 4], source[4 * 8 + 4]);
        assert_eq!(pixels[7 * 8 + 7], source[7 * 8 + 7]);
    }

    #[test]
    fn transparent_neighbours_do_not_stain_visible_pixels() {
        let grade = shrunk_grade(6);
        let mut pixels = vec![[0.0_f32; 4]; 16 * 16];
        // One opaque white texel, everything else fully transparent black.
        pixels[4 * 16 + 4] = [1.0, 1.0, 1.0, 1.0];
        // Output pixel (6, 6) lands exactly on texel (4, 4).
        assert!(bezier_warp_frame_f32(&mut pixels, 16, 16, grade));
        let sampled = pixels[6 * 16 + 6];
        assert!(
            (sampled[0] - 1.0).abs() <= 1e-5
                && (sampled[1] - 1.0).abs() <= 1e-5
                && (sampled[2] - 1.0).abs() <= 1e-5
                && (sampled[3] - 1.0).abs() <= 1e-5,
            "the transparent neighbours leaked into the sample: {sampled:?}"
        );
        // A pixel far from the white texel stays transparent, not grey.
        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let grade = shrunk_grade(4);
        let mut pixels = vec![[0.0_f32; 4]; 16 * 16];
        pixels[4 * 16 + 4] = [4.0, -1.0, 9.5, 1.0];
        assert!(bezier_warp_frame_f32(&mut pixels, 16, 16, grade));
        let sampled = pixels[6 * 16 + 6];
        assert!((sampled[0] - 4.0).abs() <= 1e-5, "{sampled:?}");
        assert!((sampled[1] + 1.0).abs() <= 1e-5, "{sampled:?}");
        assert!((sampled[2] - 9.5).abs() <= 1e-5, "{sampled:?}");
    }

    #[test]
    fn the_frame_entry_point_is_deterministic_and_guarded() {
        // frames_with_the_wrong_length_are_rejected
        let grade = shrunk_grade(2);
        let mut short = frame(4, 4, 3);
        short.pop();
        let untouched = short.clone();
        assert!(!bezier_warp_frame_f32(&mut short, 4, 4, grade));
        assert_eq!(short, untouched);
        let mut empty = Vec::new();
        assert!(!bezier_warp_frame_f32(&mut empty, 0, 0, grade));
        // the_frame_entry_point_is_deterministic
        let mut first = frame(11, 9, 5);
        let mut second = first.clone();
        assert!(bezier_warp_frame_f32(&mut first, 11, 9, grade));
        assert!(bezier_warp_frame_f32(&mut second, 11, 9, grade));
        assert_eq!(first, second);
        // gpu_controls_carry_the_authored_parameters
        let (cells, control) = default_grade(6).gpu_controls();
        assert_eq!(cells[0][0], 0.0);
        assert_eq!(cells[0][2], 85.324_81 / 256.0);
        assert_eq!(cells[1][0], 170.649_61 / 256.0);
        assert_eq!(cells[1][2], 1.0);
        assert_eq!(cells[5][0], 0.0);
        assert_eq!(cells[5][3], 85.324_81 / 256.0);
        assert_eq!(control, [9.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn chunked_rows_match_a_whole_frame_render() {
        let grade = bend_the_top_edge(shrunk_grade(4), 0.15);
        let width = 32;
        let height = 96;
        let source = frame(width, height, 13);
        let mut whole = source.clone();
        assert!(bezier_warp_frame_f32(&mut whole, width, height, grade));

        // The same arithmetic, but written out row band by row band, the way a
        // tiled renderer would evaluate it.
        let patch = grade.resolved();
        let mut staged = source.clone();
        premultiply_rgba_controlled(
            &mut staged,
            &mut (|| Ok::<(), std::convert::Infallible>(())),
        )
        .expect("no checkpoint failure");
        let rows_per_chunk = (4096 / width).max(1);
        let mut chunked = vec![[0.0_f32; 4]; width * height];
        for chunk_start in (0..height).step_by(rows_per_chunk) {
            let chunk_end = (chunk_start + rows_per_chunk).min(height);
            for row in chunk_start..chunk_end {
                for column in 0..width {
                    let index = row * width + column;
                    let target = (column as f64 / width as f64, row as f64 / height as f64);
                    chunked[index] = match patch.solve(target) {
                        None => source[index],
                        Some((u, v)) => straight_rgba_from_premultiplied(sample_bilinear_clamped(
                            &staged,
                            width,
                            height,
                            u * width as f64,
                            v * height as f64,
                        )),
                    };
                }
            }
        }
        assert_eq!(whole, chunked);
    }
}
