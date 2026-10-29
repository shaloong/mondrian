//! Mirror reflection across an authored axis (TRF-05 in the effect list).
//!
//! The effect reflects the frame across a line that passes through the authored
//! centre at the authored angle: everything on the positive side of the axis is
//! kept as it is, and the opposite half is filled with the reflected picture.
//! The result is perfectly symmetric about the axis, which is what makes it
//! useful for symmetrical compositions.
//!
//! The mapping is a pure inverse resampling — destination pixel in, source
//! pixel out — so the kernel carries no state and does not depend on the
//! processing order. Reflection is its own inverse, so applying the same mirror
//! twice restores the kept half exactly and leaves the other half defined by the
//! first pass; the kernel therefore reads from a snapshot.
//!
//! The kernel works on premultiplied colour in the working scene-linear space
//! and never clamps colour; coverage is capped at one by the shared
//! unpremultiply helper.

use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Largest magnitude accepted for the authored axis angle, in degrees.
///
/// The list leaves the angle unbounded. The cap keeps the trigonometric
/// reduction exact enough to round-trip between paths in `f32`.
pub const MIRROR_MAX_ANGLE_DEGREES: f32 = 360_000.0;

/// Largest magnitude accepted for an authored centre coordinate, in pixels.
///
/// A centre far outside the frame puts every pixel on one side of the axis,
/// which is a well defined identity; the cap only rejects absurd values.
pub const MIRROR_MAX_CENTRE_PIXELS: f32 = 1_000_000.0;

/// Errors raised when author values cannot describe an executable mirror.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorError {
    /// The angle is not finite or exceeds the accepted magnitude.
    Angle,
    /// The centre is not a finite point within the accepted magnitude.
    Centre,
}

impl std::fmt::Display for MirrorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Angle => write!(
                formatter,
                "mirror angle must be finite and within +/-{MIRROR_MAX_ANGLE_DEGREES} degrees"
            ),
            Self::Centre => write!(
                formatter,
                "mirror centre must be a finite point within +/-{MIRROR_MAX_CENTRE_PIXELS} pixels"
            ),
        }
    }
}

impl std::error::Error for MirrorError {}

/// A resolved mirror: the axis centre and its direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MirrorGrade {
    angle_degrees: f32,
    centre: [f32; 2],
}

impl MirrorGrade {
    /// Builds a grade, rejecting state the declared ranges cannot express.
    pub fn new(angle_degrees: f32, centre: [f32; 2]) -> Result<Self, MirrorError> {
        if !angle_degrees.is_finite() || angle_degrees.abs() > MIRROR_MAX_ANGLE_DEGREES {
            return Err(MirrorError::Angle);
        }
        if !centre[0].is_finite()
            || !centre[1].is_finite()
            || centre[0].abs() > MIRROR_MAX_CENTRE_PIXELS
            || centre[1].abs() > MIRROR_MAX_CENTRE_PIXELS
        {
            return Err(MirrorError::Centre);
        }
        Ok(Self { angle_degrees, centre })
    }

    /// Direction of the mirror axis, in degrees.
    pub fn angle_degrees(&self) -> f32 {
        self.angle_degrees
    }

    /// Point the mirror axis passes through, in frame pixels.
    pub fn centre(&self) -> [f32; 2] {
        self.centre
    }

    /// Unit normal of the mirror axis: pixels with a non-negative signed
    /// distance to the axis keep their own colour.
    pub fn axis_normal(&self) -> [f64; 2] {
        let radians = f64::from(self.angle_degrees).to_radians();
        let (sin, cos) = radians.sin_cos();
        [-sin, cos]
    }

    /// Uniform packing for the GPU pass: `[centre_x, centre_y, angle, 0]`.
    pub fn gpu_controls(&self) -> [f32; 4] {
        [self.centre[0], self.centre[1], self.angle_degrees, 0.0]
    }

    /// Signed distance of a pixel position from the axis.
    pub fn signed_distance(&self, column: f64, row: f64, normal: [f64; 2]) -> f64 {
        (column - f64::from(self.centre[0])) * normal[0]
            + (row - f64::from(self.centre[1])) * normal[1]
    }
}

/// Applies the mirror to a whole frame, returning `false` when the frame cannot
/// be processed.
pub fn mirror_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: MirrorGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_mirror_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the mirror in place with an explicit cancellation checkpoint.
pub(crate) fn apply_mirror_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: MirrorGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }

    let normal = grade.axis_normal();
    // The signed distance is linear in the pixel position, so the smallest value
    // over the frame is attained at a corner. When even that corner sits on the
    // kept side nothing is reflected and the frame stays untouched.
    let max_column = (width - 1) as f64;
    let max_row = (height - 1) as f64;
    let corners = [
        grade.signed_distance(0.0, 0.0, normal),
        grade.signed_distance(max_column, 0.0, normal),
        grade.signed_distance(0.0, max_row, normal),
        grade.signed_distance(max_column, max_row, normal),
    ];
    let smallest = corners.iter().copied().fold(f64::INFINITY, f64::min);
    if smallest >= 0.0 {
        return Ok(true);
    }

    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let signed = grade.signed_distance(column as f64, row as f64, normal);
                if signed >= 0.0 {
                    continue;
                }
                let sample_x = column as f64 - 2.0 * signed * normal[0];
                let sample_y = row as f64 - 2.0 * signed * normal[1];
                *pixel =
                    sample_bilinear_clamped(source.as_slice(), width, height, sample_x, sample_y);
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

/// Bilinear sample whose integer coordinates clamp to the frame edges.
fn sample_bilinear_clamped(
    source: &[[f32; 4]],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
) -> [f32; 4] {
    let limit_x = (width - 1) as f64;
    let limit_y = (height - 1) as f64;
    let clamped_x = x.clamp(0.0, limit_x);
    let clamped_y = y.clamp(0.0, limit_y);
    let base_x = clamped_x.floor();
    let base_y = clamped_y.floor();
    let frac_x = (clamped_x - base_x) as f32;
    let frac_y = (clamped_y - base_y) as f32;
    let x0 = base_x as usize;
    let y0 = base_y as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);

    let top_left = source[y0 * width + x0];
    let top_right = source[y0 * width + x1];
    let bottom_left = source[y1 * width + x0];
    let bottom_right = source[y1 * width + x1];

    let mut sample = [0.0_f32; 4];
    for (channel, value) in sample.iter_mut().enumerate() {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * frac_x;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * frac_x;
        *value = top + (bottom - top) * frac_y;
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, counter: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let phase = (index * counter % 17) as f32 * 0.041;
            *pixel = [phase, 0.3 + phase * 0.5, 0.85 - phase, 1.0];
        }
        pixels
    }

    fn grade(angle: f32, centre: [f32; 2]) -> MirrorGrade {
        MirrorGrade::new(angle, centre).expect("valid mirror grade")
    }

    fn assert_pixel_close(actual: [f32; 4], expected: [f32; 4]) {
        for channel in 0..4 {
            assert!(
                (actual[channel] - expected[channel]).abs() <= 1.0e-6,
                "channel {channel}: {actual:?} vs {expected:?}"
            );
        }
    }

    #[test]
    fn rejects_author_state_outside_the_declared_ranges() {
        assert_eq!(
            MirrorGrade::new(f32::NAN, [1.0, 1.0]),
            Err(MirrorError::Angle)
        );
        assert_eq!(
            MirrorGrade::new(360_001.0, [1.0, 1.0]),
            Err(MirrorError::Angle)
        );
        assert_eq!(
            MirrorGrade::new(0.0, [f32::INFINITY, 1.0]),
            Err(MirrorError::Centre)
        );
        assert_eq!(
            MirrorGrade::new(0.0, [1.0, -1_000_001.0]),
            Err(MirrorError::Centre)
        );
        assert!(MirrorGrade::new(-12.5, [128.0, 64.0]).is_ok());
    }

    #[test]
    fn a_zero_angle_axis_mirrors_rows_above_the_centre() {
        let mut pixels = frame(3, 4, 3);
        let original = pixels.clone();
        assert!(mirror_frame_f32(&mut pixels, 3, 4, grade(0.0, [1.0, 1.5])));

        // Rows two and three sit on the kept side and keep their own colour; rows
        // one and zero are filled from the reflection of rows two and three.
        // The frame is three pixels wide, so row `n` starts at index `n * 3`.
        for column in 0..3 {
            assert_eq!(pixels[6 + column], original[6 + column]);
            assert_eq!(pixels[9 + column], original[9 + column]);
            assert_eq!(pixels[3 + column], original[6 + column]);
            assert_eq!(pixels[column], original[9 + column]);
        }
    }

    #[test]
    fn a_ninety_degree_axis_mirrors_columns() {
        let mut pixels = frame(4, 3, 5);
        let original = pixels.clone();
        assert!(mirror_frame_f32(&mut pixels, 4, 3, grade(90.0, [1.5, 1.0])));

        for row in 0..3 {
            // The normal points along -x, so columns past the centre are the
            // ones that get reflected. The trigonometric reduction leaves a
            // tiny cosine residue, hence the tolerance.
            assert_pixel_close(pixels[row * 4 + 3], original[row * 4]);
            assert_pixel_close(pixels[row * 4 + 2], original[row * 4 + 1]);
            assert_pixel_close(pixels[row * 4], original[row * 4]);
        }
    }

    #[test]
    fn the_axis_row_is_unchanged() {
        let mut pixels = frame(5, 5, 7);
        let original = pixels.clone();
        assert!(mirror_frame_f32(&mut pixels, 5, 5, grade(0.0, [2.0, 2.0])));
        assert_eq!(
            &pixels[2 * 5..3 * 5],
            &original[2 * 5..3 * 5],
            "the row the axis passes through must keep its own colour"
        );
    }

    #[test]
    fn a_centre_beyond_the_frame_keeps_every_pixel() {
        let mut pixels = frame(4, 3, 2);
        let original = pixels.clone();
        // With a normal along +y and the centre above the frame, every pixel has
        // a positive signed distance.
        assert!(mirror_frame_f32(
            &mut pixels,
            4,
            3,
            grade(0.0, [0.0, -10.0])
        ));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_diagonal_axis_reflects_across_the_line() {
        // A 45 degree axis through the pixel (1, 1) swaps x and y: the picture
        // above the diagonal is replaced by the reflection below it.
        let mut pixels = vec![[0.0_f32, 0.0, 0.0, 1.0]; 4 * 4];
        for (index, pixel) in pixels.iter_mut().enumerate() {
            let column = index % 4;
            let row = index / 4;
            pixel[0] = column as f32;
            pixel[1] = row as f32;
        }
        assert!(mirror_frame_f32(&mut pixels, 4, 4, grade(45.0, [1.0, 1.0])));
        // Reflection across the diagonal is the transpose, so pixels below the
        // diagonal (row < column) read the transposed position while the kept
        // half keeps its own colour.
        for column in 0..4 {
            for row in 0..4 {
                let pixel = pixels[row * 4 + column];
                let expected = if row < column {
                    [row as f32, column as f32]
                } else {
                    [column as f32, row as f32]
                };
                assert!(
                    (pixel[0] - expected[0]).abs() <= 1.0e-5
                        && (pixel[1] - expected[1]).abs() <= 1.0e-5,
                    "pixel ({column}, {row}) should read the mirrored sample"
                );
            }
        }
    }

    #[test]
    fn applying_the_same_mirror_twice_keeps_the_kept_half() {
        let mut once = frame(5, 4, 11);
        let original = once.clone();
        assert!(mirror_frame_f32(&mut once, 5, 4, grade(0.0, [2.0, 1.5])));
        let mut twice = once.clone();
        assert!(mirror_frame_f32(&mut twice, 5, 4, grade(0.0, [2.0, 1.5])));
        assert_eq!(twice, once);
        // The kept rows are still the rows of the source picture.
        for row in 2..4 {
            assert_eq!(
                &twice[row * 5..(row + 1) * 5],
                &original[row * 5..(row + 1) * 5]
            );
        }
    }

    #[test]
    fn transparent_neighbours_do_not_darken_the_colour() {
        let mut pixels = vec![[0.0_f32; 4]; 2];
        pixels[0] = [0.4, 0.2, 0.1, 0.5];
        assert!(mirror_frame_f32(&mut pixels, 2, 1, grade(90.0, [0.5, 0.0])));
        let reflected = pixels[1];
        assert!((reflected[0] - 0.4).abs() <= 1.0e-6);
        assert!((reflected[1] - 0.2).abs() <= 1.0e-6);
        assert!((reflected[2] - 0.1).abs() <= 1.0e-6);
        assert!((reflected[3] - 0.5).abs() <= 1.0e-6);
    }

    #[test]
    fn extended_range_colour_is_not_clamped() {
        let mut pixels = vec![[0.0_f32; 4]; 2];
        pixels[0] = [2.5, -0.5, 1.75, 0.5];
        assert!(mirror_frame_f32(&mut pixels, 2, 1, grade(90.0, [0.5, 0.0])));
        let reflected = pixels[1];
        assert!((reflected[0] - 2.5).abs() <= 1.0e-6);
        assert!((reflected[1] + 0.5).abs() <= 1.0e-6);
        assert!((reflected[2] - 1.75).abs() <= 1.0e-6);
    }

    #[test]
    fn coverage_is_capped_at_one() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 2];
        assert!(mirror_frame_f32(&mut pixels, 2, 1, grade(90.0, [0.5, 0.0])));
        for pixel in &pixels {
            assert!((0.0..=1.0).contains(&pixel[3]));
        }
    }

    #[test]
    fn the_controlled_entry_matches_the_frame_entry() {
        let mut direct = frame(5, 6, 13);
        let mut controlled = direct.clone();
        let mirror = grade(23.0, [2.5, 3.0]);
        assert!(mirror_frame_f32(&mut direct, 5, 6, mirror));
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(
            apply_mirror_f32_controlled(&mut controlled, 5, 6, mirror, &mut checkpoint)
                .expect("checkpoint never fails")
        );
        assert_eq!(direct, controlled);
    }

    #[test]
    fn mismatched_geometry_is_rejected() {
        let mut pixels = frame(4, 4, 3);
        assert!(!mirror_frame_f32(&mut pixels, 5, 4, grade(0.0, [0.0, 0.0])));
        assert!(!mirror_frame_f32(&mut pixels, 0, 0, grade(0.0, [0.0, 0.0])));
    }

    #[test]
    fn processing_is_deterministic() {
        let mirror = grade(31.0, [3.0, 2.0]);
        let mut first = frame(6, 5, 17);
        let mut second = first.clone();
        assert!(mirror_frame_f32(&mut first, 6, 5, mirror));
        assert!(mirror_frame_f32(&mut second, 6, 5, mirror));
        assert_eq!(first, second);
    }

    #[test]
    fn gpu_controls_carry_the_centre_and_the_axis() {
        let controls = grade(37.5, [12.0, 34.0]).gpu_controls();
        assert_eq!(controls, [12.0, 34.0, 37.5, 0.0]);
    }
}
