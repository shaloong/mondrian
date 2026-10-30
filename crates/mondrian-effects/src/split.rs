//! Split the frame along a line and slide the two halves apart.
//!
//! The effect list names this entry `分离` (split) and gives it three parameters: a
//! line through two points and a pixel amount. The list does not say how the line
//! divides the frame, so this module fixes that geometry explicitly:
//!
//! * The directed line runs from `point_a` to `point_b`; its normal points to the
//!   left of that direction.
//! * Pixels on the positive side of the line are sampled half the amount *against*
//!   the normal, so their content slides towards the normal; pixels on the negative
//!   side slide the other way. With a positive amount the two halves therefore move
//!   apart by `split_pixels` in total, and the seam between them is deliberately
//!   hard: a pixel exactly on the line belongs to the positive half.
//!
//! The hard seam means a pixel whose signed distance rounds to a different sign on
//! two backends may follow the other half. That is inherent to a discontinuous split
//! (like a mirror axis), so parity tests must avoid sampling the seam itself.
//! * Sampling clamps to the frame edge, so a half never wraps around the far border
//!   and never invents coverage outside the source.
//!
//! The amount is zero by default, which leaves the frame untouched, and a degenerate
//! line (both points equal) has no direction to split along, so it is identity too.

use crate::parallel::for_each_row_band;
use crate::resample::sample_bilinear_clamped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest split the author surface can express, in pixels.
pub const SPLIT_MAX_PIXELS: f32 = 250.0;
/// Smallest split, in pixels.
pub const SPLIT_MIN_PIXELS: f32 = 0.0;
/// Largest absolute coordinate the author surface accepts for either point.
pub const SPLIT_MAX_COORDINATE: f32 = 1_000_000.0;

const AMOUNT_EPSILON: f32 = 1.0e-6;

/// Failures raised when validating a split grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitError {
    /// The split amount is not finite or lies outside the declared range.
    Amount,
    /// One of the two line points is not finite or lies outside the declared range.
    Point,
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amount => formatter.write_str("split amount is outside the declared range"),
            Self::Point => formatter.write_str("split line point is outside the declared range"),
        }
    }
}

impl std::error::Error for SplitError {}

/// A validated split grade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitGrade {
    point_a: [f32; 2],
    point_b: [f32; 2],
    split_pixels: f32,
}

impl SplitGrade {
    /// Validates the authored points and amount.
    pub fn new(
        point_a: [f32; 2],
        point_b: [f32; 2],
        split_pixels: f32,
    ) -> Result<Self, SplitError> {
        for point in [point_a, point_b] {
            for coordinate in point {
                if !coordinate.is_finite() || coordinate.abs() > SPLIT_MAX_COORDINATE {
                    return Err(SplitError::Point);
                }
            }
        }
        if !split_pixels.is_finite()
            || !(SPLIT_MIN_PIXELS..=SPLIT_MAX_PIXELS).contains(&split_pixels)
        {
            return Err(SplitError::Amount);
        }
        Ok(Self { point_a, point_b, split_pixels })
    }

    /// First point of the split line.
    #[must_use]
    pub fn point_a(&self) -> [f32; 2] {
        self.point_a
    }

    /// Second point of the split line.
    #[must_use]
    pub fn point_b(&self) -> [f32; 2] {
        self.point_b
    }

    /// Split amount in pixels.
    #[must_use]
    pub fn split_pixels(&self) -> f32 {
        self.split_pixels
    }

    /// Whether the grade leaves the frame untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.split_pixels <= AMOUNT_EPSILON || self.unit_normal().is_none()
    }

    /// Unit normal of the split line, or `None` for a degenerate line.
    #[must_use]
    fn unit_normal(&self) -> Option<[f64; 2]> {
        let delta_x = f64::from(self.point_b[0]) - f64::from(self.point_a[0]);
        let delta_y = f64::from(self.point_b[1]) - f64::from(self.point_a[1]);
        let length = delta_x.hypot(delta_y);
        if !length.is_finite() || length <= f64::from(AMOUNT_EPSILON) {
            return None;
        }
        Some([-delta_y / length, delta_x / length])
    }

    /// Signed distance from the split line, positive on the normal side.
    #[must_use]
    fn signed_distance(&self, column: usize, row: usize, normal: [f64; 2]) -> f64 {
        let offset_x = f64::from(column as f32) - f64::from(self.point_a[0]);
        let offset_y = f64::from(row as f32) - f64::from(self.point_a[1]);
        offset_x * normal[0] + offset_y * normal[1]
    }

    /// Pre-multiplied sample position for one pixel.
    fn sample_position(&self, column: usize, row: usize, normal: [f64; 2]) -> (f64, f64) {
        let distance = self.signed_distance(column, row, normal);
        let half = f64::from(self.split_pixels) * 0.5;
        let shift = if distance >= 0.0 { half } else { -half };
        (
            f64::from(column as f32) - normal[0] * shift,
            f64::from(row as f32) - normal[1] * shift,
        )
    }

    /// Applies the split to every pixel of a pre-multiplied frame region.
    ///
    /// Rows are independent because the kernel reads a snapshot of the source, so
    /// the band helper is free to spread them over worker threads.
    pub(crate) fn apply(&self, frame: &mut [[f32; 4]], width: usize, height: usize) {
        let Some(normal) = self.unit_normal() else {
            return;
        };
        let source = frame.to_vec();
        let source_view: &[[f32; 4]] = &source;
        for_each_row_band(frame, width, 0..height, |first_row, band| {
            for (offset, row_pixels) in band.chunks_mut(width).enumerate() {
                let row = first_row + offset;
                for (column, destination) in row_pixels.iter_mut().enumerate() {
                    let (sample_x, sample_y) = self.sample_position(column, row, normal);
                    *destination =
                        sample_bilinear_clamped(source_view, width, height, sample_x, sample_y);
                }
            }
        });
    }

    /// Normal and amount packed for the GPU pass.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.point_a[0],
                self.point_a[1],
                self.point_b[0],
                self.point_b[1],
            ],
            [self.split_pixels, 0.0, 0.0, 0.0],
        )
    }
}

/// Applies a split grade to a whole straight-alpha frame.
pub fn split_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: SplitGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_split_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies a split grade to a frame slice with checkpointing.
pub(crate) fn apply_split_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: SplitGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || expected != pixels.len() || grade.is_identity() {
        return Ok(false);
    }
    premultiply_rgba_controlled(pixels, checkpoint)?;
    grade.apply(pixels, width, height);
    unpremultiply_rgba_controlled(pixels, checkpoint)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = 0.1 * (column + row) as f32;
                pixels[row * width + column] = [value, 1.0 - value * 0.5, value * 0.25, 1.0];
            }
        }
        pixels
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4], tolerance: f32) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= tolerance,
                "channel {channel}: {actual:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn rejects_author_values_outside_the_declared_ranges() {
        assert_eq!(
            SplitGrade::new([0.0, 0.0], [10.0, 0.0], 251.0),
            Err(SplitError::Amount)
        );
        assert_eq!(
            SplitGrade::new([0.0, 0.0], [10.0, 0.0], -1.0),
            Err(SplitError::Amount)
        );
        assert_eq!(
            SplitGrade::new([0.0, 0.0], [f32::INFINITY, 0.0], 1.0),
            Err(SplitError::Point)
        );
        assert_eq!(
            SplitGrade::new([0.0, 0.0], [SPLIT_MAX_COORDINATE * 2.0, 0.0], 1.0),
            Err(SplitError::Point)
        );
        assert!(SplitGrade::new([0.0, 0.0], [10.0, 0.0], 250.0).is_ok());
    }

    #[test]
    fn zero_amount_and_degenerate_lines_are_identity() {
        assert!(SplitGrade::new([0.0, 0.0], [10.0, 10.0], 0.0).expect("grade").is_identity());
        assert!(SplitGrade::new([4.0, 4.0], [4.0, 4.0], 20.0).expect("grade").is_identity());
        let mut pixels = frame(6, 5);
        let original = pixels.clone();
        assert!(!split_frame_f32(
            &mut pixels,
            6,
            5,
            SplitGrade::new([0.0, 0.0], [10.0, 10.0], 0.0).expect("grade")
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn the_normal_is_perpendicular_to_the_line() {
        let normal = SplitGrade::new([0.0, 0.0], [10.0, 0.0], 4.0)
            .expect("grade")
            .unit_normal()
            .expect("normal");
        assert!(normal[0].abs() <= 1.0e-12);
        assert!((normal[1] - 1.0).abs() <= 1.0e-12);
        let diagonal = SplitGrade::new([0.0, 0.0], [3.0, 4.0], 4.0)
            .expect("grade")
            .unit_normal()
            .expect("normal");
        assert!((diagonal[0] - -0.8).abs() <= 1.0e-12);
        assert!((diagonal[1] - 0.6).abs() <= 1.0e-12);
    }

    #[test]
    fn the_axis_belongs_to_the_positive_half() {
        let grade = SplitGrade::new([0.0, 4.0], [9.0, 4.0], 2.0).expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(split_frame_f32(&mut pixels, 9, 9, grade));
        // The line runs to the right, so its normal points down; the axis pixel is
        // on the positive side and slides with that half, one pixel up.
        for column in 0..9 {
            assert_pixel_close(pixels[4 * 9 + column], original[3 * 9 + column], 1.0e-6);
        }
    }

    #[test]
    fn a_vertical_split_moves_the_halves_apart() {
        let grade = SplitGrade::new([0.0, 4.0], [8.0, 4.0], 2.0).expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(split_frame_f32(&mut pixels, 9, 9, grade));
        // Above the line the content slides down by one pixel, below it slides up.
        assert_pixel_close(pixels[2 * 9 + 4], original[3 * 9 + 4], 1.0e-6);
        assert_pixel_close(pixels[6 * 9 + 4], original[5 * 9 + 4], 1.0e-6);
    }

    #[test]
    fn a_horizontal_split_moves_the_halves_apart() {
        let grade = SplitGrade::new([4.0, 0.0], [4.0, 8.0], 2.0).expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(split_frame_f32(&mut pixels, 9, 9, grade));
        // The line runs downwards, so its normal points to the left.
        assert_pixel_close(pixels[4 * 9 + 2], original[4 * 9 + 3], 1.0e-6);
        assert_pixel_close(pixels[4 * 9 + 6], original[4 * 9 + 5], 1.0e-6);
    }

    #[test]
    fn a_fractional_amount_interpolates_between_neighbours() {
        let grade = SplitGrade::new([0.0, 4.0], [8.0, 4.0], 1.0).expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(split_frame_f32(&mut pixels, 9, 9, grade));
        let halfway = [
            (original[3 * 9 + 4][0] + original[4 * 9 + 4][0]) * 0.5,
            (original[3 * 9 + 4][1] + original[4 * 9 + 4][1]) * 0.5,
            (original[3 * 9 + 4][2] + original[4 * 9 + 4][2]) * 0.5,
            1.0,
        ];
        assert_pixel_close(pixels[3 * 9 + 4], halfway, 1.0e-5);
    }

    #[test]
    fn the_split_never_reads_a_neighbour_through_a_transparent_pixel() {
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 0.0]; 7 * 7];
        for row in 0..7 {
            for column in 0..7 {
                if row != 3 {
                    pixels[row * 7 + column] = [0.9, 0.2, 0.1, 1.0];
                }
            }
        }
        let grade = SplitGrade::new([0.0, 5.0], [6.0, 5.0], 2.0).expect("grade");
        assert!(split_frame_f32(&mut pixels, 7, 7, grade));
        for (index, pixel) in pixels.iter().enumerate() {
            if pixel[3] > 0.0 {
                assert!(
                    pixel[0] <= 0.9 + 1.0e-6,
                    "pixel {index} brightened through a transparent neighbour: {pixel:?}"
                );
            }
        }
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let mut pixels = vec![[4.0_f32, -2.0, 0.5, 1.0]; 8 * 8];
        let grade = SplitGrade::new([0.0, 0.0], [7.0, 0.0], 3.0).expect("grade");
        assert!(split_frame_f32(&mut pixels, 8, 8, grade));
        for pixel in &pixels {
            assert!((pixel[0] - 4.0).abs() <= 1.0e-5);
            assert!((pixel[1] + 2.0).abs() <= 1.0e-5);
        }
    }

    #[test]
    fn the_controlled_entry_point_matches_the_whole_frame_entry_point() {
        let grade = SplitGrade::new([1.0, 2.0], [7.0, 6.0], 3.0).expect("grade");
        let mut direct = frame(9, 8);
        assert!(split_frame_f32(&mut direct, 9, 8, grade));
        let mut controlled = frame(9, 8);
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(
            apply_split_f32_controlled(&mut controlled, 9, 8, grade, &mut checkpoint)
                .expect("controlled split")
        );
        assert_eq!(direct, controlled);
    }

    #[test]
    fn mismatched_geometry_is_ignored() {
        let grade = SplitGrade::new([0.0, 0.0], [7.0, 0.0], 2.0).expect("grade");
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 20];
        let original = pixels.clone();
        assert!(!split_frame_f32(&mut pixels, 3, 3, grade));
        assert_eq!(pixels, original);
        assert!(!split_frame_f32(&mut pixels, 0, 0, grade));
    }

    #[test]
    fn the_mapping_is_deterministic() {
        let grade = SplitGrade::new([2.0, 1.0], [5.0, 8.0], 4.0).expect("grade");
        let mut first = frame(9, 9);
        let mut second = frame(9, 9);
        assert!(split_frame_f32(&mut first, 9, 9, grade));
        assert!(split_frame_f32(&mut second, 9, 9, grade));
        assert_eq!(first, second);
    }

    #[test]
    fn gpu_controls_carry_the_line_and_the_amount() {
        let grade = SplitGrade::new([1.5, 2.25], [7.0, 6.5], 12.0).expect("grade");
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [1.5, 2.25, 7.0, 6.5]);
        assert_eq!(extra, [12.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_diagonal_line_splits_both_axes() {
        let grade = SplitGrade::new([0.0, 0.0], [8.0, 8.0], 2.0).expect("grade");
        let mut pixels = frame(9, 9);
        let original = pixels.clone();
        assert!(split_frame_f32(&mut pixels, 9, 9, grade));
        // The normal of a 45 degree line is (-1/sqrt2, 1/sqrt2); a pixel one step
        // below the diagonal therefore samples about 0.71 px up and left.
        let expected = sample_bilinear_clamped(
            &original,
            9,
            9,
            4.0 + std::f64::consts::FRAC_1_SQRT_2,
            4.0 - std::f64::consts::FRAC_1_SQRT_2,
        );
        assert_pixel_close(pixels[4 * 9 + 4], expected, 1.0e-6);
    }
}
