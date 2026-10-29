//! Offset (translate with wrap-around) effect kernel.
//!
//! The effect translates the frame by an authored pixel shift and wraps the
//! pixels that fall off one edge back onto the opposite edge. That makes the
//! result seamless for tiling workflows and keeps the operation exact: an
//! integer shift only moves existing samples, it never invents colour.
//!
//! The kernel runs in the working scene-linear space on premultiplied colour,
//! so bilinear samples across the seam mix coverage correctly. Outputs keep the
//! straight-alpha contract of every other effect: coverage is capped at one by
//! the shared unpremultiply helper and colour is never clamped.

use crate::resample::sample_bilinear_wrapped;
use crate::straight_alpha::{premultiply_rgba_controlled, unpremultiply_rgba_controlled};

/// Author ceiling for the blend back to the original image, in percent.
pub const OFFSET_MAX_BLEND_PERCENT: f32 = 100.0;

/// Largest magnitude the schema can author for a shift, in pixels.
///
/// The wrap makes larger values equivalent, so the bound only keeps the
/// arithmetic inside a range where the wrap stays exact in `f32`.
pub const OFFSET_MAX_SHIFT_PIXELS: f32 = 1_000_000.0;

const BLEND_EPSILON: f32 = 1.0e-6;

/// Errors raised when author values cannot describe an executable offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetError {
    /// The shift is not finite or is beyond the declared author range.
    Shift,
    /// The blend is not finite or is outside `[0, 1]`.
    Blend,
}

impl std::fmt::Display for OffsetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shift => write!(
                formatter,
                "offset shift must be finite and within +/-{OFFSET_MAX_SHIFT_PIXELS} pixels"
            ),
            Self::Blend => write!(formatter, "offset blend must be within [0, 1]"),
        }
    }
}

impl std::error::Error for OffsetError {}

/// Immutable offset description shared by the CPU kernel and the GPU pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OffsetGrade {
    shift: [f32; 2],
    blend_with_original: f32,
}

impl OffsetGrade {
    /// Builds a grade, rejecting values the effect contract cannot execute.
    ///
    /// # Errors
    ///
    /// Returns [`OffsetError::Shift`] when either shift component is not finite
    /// or is beyond [`OFFSET_MAX_SHIFT_PIXELS`], and [`OffsetError::Blend`] when
    /// the blend is not finite or is outside `[0, 1]`.
    pub fn new(shift: [f32; 2], blend_with_original: f32) -> Result<Self, OffsetError> {
        for component in shift {
            if !component.is_finite() || component.abs() > OFFSET_MAX_SHIFT_PIXELS {
                return Err(OffsetError::Shift);
            }
        }
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(OffsetError::Blend);
        }
        Ok(Self { shift, blend_with_original })
    }

    /// The authored shift, in pixels.
    #[must_use]
    pub fn shift(self) -> [f32; 2] {
        self.shift
    }

    /// The blend back to the original image, in `[0, 1]`.
    #[must_use]
    pub fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// `true` when the effect cannot change any pixel.
    ///
    /// A full blend keeps the original image, and a zero shift moves every
    /// sample onto itself.
    #[must_use]
    pub fn is_identity(self) -> bool {
        if self.blend_with_original >= 1.0 - BLEND_EPSILON {
            return true;
        }
        self.shift[0].abs() <= BLEND_EPSILON && self.shift[1].abs() <= BLEND_EPSILON
    }

    /// The wrapped shift in pixels for a frame of the given size.
    ///
    /// Wrapping here means the GPU and CPU paths share one normalisation, and
    /// the sampler below only has to wrap fractional coordinates.
    #[must_use]
    pub fn wrapped_shift(self, width: usize, height: usize) -> (f64, f64) {
        let wrap_x = width.max(1) as f64;
        let wrap_y = height.max(1) as f64;
        (
            f64::from(self.shift[0]).rem_euclid(wrap_x),
            f64::from(self.shift[1]).rem_euclid(wrap_y),
        )
    }

    /// Packs the uniform payload consumed by the GPU pass.
    ///
    /// `params = [shift_x, shift_y, blend, 0]`; the shader derives the wrapped
    /// sample position from the frame size it already receives.
    #[must_use]
    pub fn gpu_controls(self) -> [f32; 4] {
        [self.shift[0], self.shift[1], self.blend_with_original, 0.0]
    }
}

/// Applies the offset to one frame in place.
///
/// Returns `false` when the frame cannot be processed, which the caller reports
/// as an unsupported operation.
#[must_use]
pub fn offset_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: OffsetGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_offset_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the offset in place with an explicit cancellation checkpoint.
pub(crate) fn apply_offset_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: OffsetGrade,
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

    let frame = &mut working[..pixel_count];
    premultiply_rgba_controlled(frame, checkpoint)?;

    let source = frame.to_vec();
    let (shift_x, shift_y) = grade.wrapped_shift(width, height);
    let blend = f64::from(grade.blend_with_original);

    checkpoint()?;
    let rows = frame.len() / width;
    crate::parallel::for_each_row_band(frame, width, 0..rows, |row_start, band| {
        for (offset, chunk) in band.chunks_mut(width).enumerate() {
            let row = row_start + offset;
            for (column, pixel) in chunk.iter_mut().enumerate() {
                let sample_x = column as f64 - shift_x;
                let sample_y = row as f64 - shift_y;
                let sample = sample_bilinear_wrapped(&source, width, height, sample_x, sample_y);
                let original = *pixel;
                let mut mixed = [0.0_f32; 4];
                for (channel, value) in mixed.iter_mut().enumerate() {
                    let shifted = f64::from(sample[channel]);
                    let kept = f64::from(original[channel]);
                    *value = (shifted + (kept - shifted) * blend) as f32;
                }
                *pixel = mixed;
            }
        }
    });

    unpremultiply_rgba_controlled(frame, checkpoint)?;
    checkpoint()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize, counter: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| {
                let value = (index + counter) as f32 / 32.0;
                [value, value * 0.5, value * 0.25, 1.0]
            })
            .collect()
    }

    #[test]
    fn new_rejects_values_the_schema_cannot_author() {
        assert_eq!(
            OffsetGrade::new([f32::NAN, 0.0], 0.0),
            Err(OffsetError::Shift)
        );
        assert_eq!(
            OffsetGrade::new([0.0, OFFSET_MAX_SHIFT_PIXELS + 1.0], 0.0),
            Err(OffsetError::Shift)
        );
        assert_eq!(OffsetGrade::new([0.0, 0.0], 1.5), Err(OffsetError::Blend));
        assert_eq!(
            OffsetGrade::new([0.0, 0.0], f32::INFINITY),
            Err(OffsetError::Blend)
        );
    }

    #[test]
    fn a_zero_shift_is_the_identity() {
        let grade = OffsetGrade::new([0.0, 0.0], 0.0).expect("grade");
        assert!(grade.is_identity());
        let mut pixels = frame(4, 3, 0);
        let original = pixels.clone();
        assert!(offset_frame_f32(&mut pixels, 4, 3, grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_full_blend_is_the_identity() {
        let grade = OffsetGrade::new([3.0, 2.0], 1.0).expect("grade");
        assert!(grade.is_identity());
        let mut pixels = frame(4, 3, 0);
        let original = pixels.clone();
        assert!(offset_frame_f32(&mut pixels, 4, 3, grade));
        assert_eq!(pixels, original);
    }

    #[test]
    fn an_integer_shift_moves_samples_and_wraps_the_seam() {
        // One distinct pixel per position: the wrapped shift must permutation-map
        // those exact samples, never interpolate.
        let width = 4;
        let height = 3;
        let mut pixels: Vec<[f32; 4]> =
            (0..width * height).map(|index| [index as f32, 0.0, 0.0, 1.0]).collect();
        let original = pixels.clone();
        let grade = OffsetGrade::new([1.0, 0.0], 0.0).expect("grade");
        assert!(offset_frame_f32(&mut pixels, width, height, grade));
        for row in 0..height {
            for column in 0..width {
                let expected = original[row * width + (column + width - 1) % width];
                assert_eq!(pixels[row * width + column], expected, "at {column},{row}");
            }
        }
    }

    #[test]
    fn a_vertical_shift_wraps_onto_the_opposite_edge() {
        let width = 3;
        let height = 4;
        let mut pixels: Vec<[f32; 4]> =
            (0..width * height).map(|index| [0.0, index as f32, 0.0, 1.0]).collect();
        let original = pixels.clone();
        let grade = OffsetGrade::new([0.0, 2.0], 0.0).expect("grade");
        assert!(offset_frame_f32(&mut pixels, width, height, grade));
        for row in 0..height {
            for column in 0..width {
                let expected = original[((row + height - 2) % height) * width + column];
                assert_eq!(pixels[row * width + column], expected, "at {column},{row}");
            }
        }
    }

    #[test]
    fn a_fractional_shift_interpolates_between_neighbours() {
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]];
        let grade = OffsetGrade::new([0.5, 0.0], 0.0).expect("grade");
        assert!(offset_frame_f32(&mut pixels, 2, 1, grade));
        // Column 0 samples the wrapped neighbour at 1.5 (half of both pixels).
        assert!((pixels[0][0] - 0.5).abs() < 1.0e-6, "{:?}", pixels[0]);
        assert!((pixels[1][0] - 0.5).abs() < 1.0e-6, "{:?}", pixels[1]);
    }

    #[test]
    fn the_blend_keeps_the_requested_share_of_the_original() {
        let width = 2;
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]];
        let original = pixels.clone();
        let grade = OffsetGrade::new([1.0, 0.0], 0.25).expect("grade");
        assert!(offset_frame_f32(&mut pixels, width, 1, grade));
        for (index, pixel) in pixels.iter().enumerate() {
            let kept = original[index];
            let shifted = original[(index + 1) % width];
            for channel in 0..3 {
                let expected = kept[channel] * 0.25 + shifted[channel] * 0.75;
                assert!(
                    (pixel[channel] - expected).abs() < 1.0e-6,
                    "{pixel:?} vs {expected}"
                );
            }
        }
    }

    #[test]
    fn transparent_neighbours_do_not_pull_the_visible_colour_down() {
        // Premultiplied sampling keeps the covered pixel's colour: the empty
        // neighbour contributes zero coverage and zero colour.
        let mut pixels = vec![[0.4, 0.2, 0.1, 0.5], [0.0, 0.0, 0.0, 0.0]];
        let grade = OffsetGrade::new([1.0, 0.0], 0.0).expect("grade");
        assert!(offset_frame_f32(&mut pixels, 2, 1, grade));
        assert!((pixels[1][0] - 0.4).abs() < 1.0e-5, "{:?}", pixels[1]);
        assert!((pixels[1][3] - 0.5).abs() < 1.0e-5, "{:?}", pixels[1]);
        assert_eq!(pixels[0], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn extended_range_values_are_not_clamped() {
        let mut pixels = vec![[1.75, -0.5, 0.25, 1.0], [0.0, 0.0, 0.0, 1.0]];
        let grade = OffsetGrade::new([1.0, 0.0], 0.0).expect("grade");
        assert!(offset_frame_f32(&mut pixels, 2, 1, grade));
        assert!((pixels[1][0] - 1.75).abs() < 1.0e-6, "{:?}", pixels[1]);
        assert!((pixels[1][1] + 0.5).abs() < 1.0e-6, "{:?}", pixels[1]);
    }

    #[test]
    fn the_shift_wraps_before_it_is_sampled() {
        let grade = OffsetGrade::new([-1.0, 5.0], 0.0).expect("grade");
        let (x, y) = grade.wrapped_shift(4, 3);
        assert!((x - 3.0).abs() < 1.0e-9);
        assert!((y - 2.0).abs() < 1.0e-9);
    }

    #[test]
    fn the_gpu_controls_carry_the_shift_and_the_blend() {
        let grade = OffsetGrade::new([12.0, -4.0], 0.4).expect("grade");
        assert_eq!(grade.gpu_controls(), [12.0, -4.0, 0.4, 0.0]);
    }

    #[test]
    fn a_frame_that_is_too_small_is_reported_as_unsupported() {
        let grade = OffsetGrade::new([1.0, 0.0], 0.0).expect("grade");
        let mut pixels = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        assert!(!offset_frame_f32(&mut pixels, 2, 2, grade));
    }
}
