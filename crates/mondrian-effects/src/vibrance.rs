//! Vibrance and overall saturation.
//!
//! The kernel scales every pixel's deviation from its working-space luminance.
//! Vibrance uses a weight that falls off with the pixel's own HSV saturation, so
//! muted colours move a lot while already saturated colours barely move; the
//! saturation control applies the same scale to every pixel. It is a pure point
//! operation: the result only depends on the pixel itself.
//!
//! Contract notes:
//! - the scale is applied in the scene-linear working space around the
//!   luminance, so luminance is preserved exactly and the operation commutes
//!   with exposure;
//! - the combined scale is `(1 + vibrance * weight) * (1 + saturation)` with
//!   `vibrance, saturation in [-1, 1]` and `weight in [0, 1]`, so it is bounded
//!   below by zero. The kernel therefore needs no clamp and cannot invert hues;
//! - because the scale is non-negative, extended-range and negative scene-linear
//!   values keep their sign and magnitude instead of being folded to a gamut;
//! - alpha is preserved.

use mondrian_core::WorkingColorSpace;

/// Validation failure for [`VibranceGrade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum VibranceError {
    /// One of the two controls is not a finite number inside [-1, 1].
    #[error("vibrance and saturation must be finite and within [-1, 1]")]
    Amount,
    /// The working-space luminance coefficients are not usable.
    #[error("vibrance luminance coefficients must be finite and non-zero")]
    LuminanceCoefficients,
}

/// Authorable parameters of the vibrance effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VibranceGrade {
    vibrance: f32,
    saturation: f32,
    luminance_coefficients: [f32; 3],
}

impl VibranceGrade {
    /// Builds a grade, rejecting values the authoring schema cannot express.
    pub fn new(
        vibrance: f32,
        saturation: f32,
        luminance_coefficients: [f32; 3],
    ) -> Result<Self, VibranceError> {
        if !vibrance.is_finite()
            || !saturation.is_finite()
            || !(-1.0..=1.0).contains(&vibrance)
            || !(-1.0..=1.0).contains(&saturation)
        {
            return Err(VibranceError::Amount);
        }
        let total: f32 = luminance_coefficients.iter().sum();
        if luminance_coefficients.iter().any(|value| !value.is_finite()) || total <= 0.0 {
            return Err(VibranceError::LuminanceCoefficients);
        }
        Ok(Self { vibrance, saturation, luminance_coefficients })
    }

    /// Builds a grade for one working space.
    pub fn for_space(
        vibrance: f32,
        saturation: f32,
        space: WorkingColorSpace,
    ) -> Result<Self, VibranceError> {
        Self::new(vibrance, saturation, space.luminance_coefficients())
    }

    /// Authored vibrance inside [-1, 1].
    pub const fn vibrance(self) -> f32 {
        self.vibrance
    }

    /// Authored overall saturation inside [-1, 1].
    pub const fn saturation(self) -> f32 {
        self.saturation
    }

    /// Working-space luminance coefficients used by the kernel.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// Both controls at zero leave every pixel untouched.
    pub fn is_identity(self) -> bool {
        self.vibrance.abs() <= 1e-6 && self.saturation.abs() <= 1e-6
    }

    /// Applies the grade to one straight-alpha working-space colour.
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        let rgb = self.apply_rgb([pixel[0], pixel[1], pixel[2]]);
        [rgb[0], rgb[1], rgb[2], pixel[3]]
    }

    /// Applies the grade to one working-space colour, keeping alpha outside.
    pub fn apply_rgb(self, rgb: [f32; 3]) -> [f32; 3] {
        let factor = self.scale(rgb);
        let coefficients = self.luminance_coefficients;
        let luminance =
            rgb[0] * coefficients[0] + rgb[1] * coefficients[1] + rgb[2] * coefficients[2];
        let mut result = [0.0; 3];
        for channel in 0..3 {
            result[channel] = luminance + (rgb[channel] - luminance) * factor;
        }
        result
    }

    /// Saturation scale the kernel applies to a colour's deviation from luma.
    ///
    /// The vibrance weight is `(1 - saturation)^2` of the colour's own HSV
    /// saturation, clamped into [0, 1] so that fully saturated and out-of-range
    /// colours cannot push the weight outside its window.
    pub fn scale(self, rgb: [f32; 3]) -> f32 {
        let weight = self.vibrance_weight(rgb);
        (1.0 + self.vibrance * weight) * (1.0 + self.saturation)
    }

    /// Vibrancy weight for one colour, one for neutral and zero at full saturation.
    ///
    /// The saturation reading is guarded by `max(maximum, 1e-6)` rather than by a
    /// branch, so an all-negative extended-range colour reads as fully saturated
    /// instead of producing a negative ratio. The GPU kernel uses the same
    /// expression, which keeps both paths bit-comparable.
    pub fn vibrance_weight(self, rgb: [f32; 3]) -> f32 {
        let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
        let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
        let saturation = ((maximum - minimum) / maximum.max(1.0e-6)).clamp(0.0, 1.0);
        let muted = 1.0 - saturation;
        muted * muted
    }

    /// Packs the values the GPU point pass needs.
    ///
    /// The first vector carries the two controls followed by the first two
    /// working-space luminance coefficients; the second starts with the third.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.vibrance,
                self.saturation,
                self.luminance_coefficients[0],
                self.luminance_coefficients[1],
            ],
            [self.luminance_coefficients[2], 0.0, 0.0, 0.0],
        )
    }
}

/// Applies the grade to every pixel of one frame, in place.
pub fn vibrance_frame_f32(pixels: &mut [[f32; 4]], grade: &VibranceGrade) -> bool {
    if grade.is_identity() {
        return false;
    }
    for pixel in pixels.iter_mut() {
        *pixel = grade.apply(*pixel);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: [f32; 3] = [0.2126, 0.7152, 0.0722];

    #[test]
    fn new_rejects_values_outside_the_authored_window() {
        assert_eq!(
            VibranceGrade::new(1.5, 0.0, SPACE),
            Err(VibranceError::Amount)
        );
        assert_eq!(
            VibranceGrade::new(0.0, -1.01, SPACE),
            Err(VibranceError::Amount)
        );
        assert_eq!(
            VibranceGrade::new(f32::NAN, 0.0, SPACE),
            Err(VibranceError::Amount)
        );
        assert_eq!(
            VibranceGrade::new(0.5, 0.5, [0.0; 3]),
            Err(VibranceError::LuminanceCoefficients)
        );
        assert!(VibranceGrade::new(-1.0, 1.0, SPACE).is_ok());
    }

    #[test]
    fn zero_controls_are_the_identity() {
        let grade = VibranceGrade::new(0.0, 0.0, SPACE).expect("grade");
        assert!(grade.is_identity());
        assert_eq!(grade.apply([0.4, 0.5, 2.5, 0.25]), [0.4, 0.5, 2.5, 0.25]);
        assert_eq!(grade.scale([1.0, 0.0, 0.0]), 1.0);
    }

    #[test]
    fn luminance_is_preserved() {
        let grade = VibranceGrade::new(1.0, 1.0, SPACE).expect("grade");
        let source = [0.2, 0.4, 0.6];
        let result = grade.apply_rgb(source);
        let luma = |rgb: [f32; 3]| rgb[0] * SPACE[0] + rgb[1] * SPACE[1] + rgb[2] * SPACE[2];
        assert!((luma(result) - luma(source)).abs() < 1e-6);
        // A neutral pixel has no deviation to scale and stays put.
        assert_eq!(grade.apply_rgb([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5]);
    }

    #[test]
    fn vibrance_moves_muted_colours_more_than_saturated_ones() {
        let grade = VibranceGrade::new(1.0, 0.0, SPACE).expect("grade");
        let muted = [0.5, 0.45, 0.42];
        let vivid = [0.5, 0.05, 0.05];
        assert!(grade.vibrance_weight(muted) > grade.vibrance_weight(vivid));
        let muted_gain = grade.apply_rgb(muted)[0] - muted[0];
        let vivid_gain = grade.apply_rgb(vivid)[0] - vivid[0];
        assert!(muted_gain > vivid_gain);
        // A fully saturated primary keeps its weight at zero.
        assert_eq!(grade.vibrance_weight([1.0, 0.0, 0.0]), 0.0);
        assert_eq!(grade.apply_rgb([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]);
    }

    #[test]
    fn saturation_scales_every_colour_equally() {
        let grade = VibranceGrade::new(0.0, 1.0, SPACE).expect("grade");
        assert!((grade.scale([0.5, 0.45, 0.42]) - 2.0).abs() < 1e-6);
        assert!((grade.scale([1.0, 0.0, 0.0]) - 2.0).abs() < 1e-6);
        let muted = grade.apply_rgb([0.5, 0.45, 0.42]);
        assert!(muted[0] > 0.5 && muted[2] < 0.42);
    }

    #[test]
    fn full_negative_saturation_collapses_to_the_luminance() {
        let grade = VibranceGrade::new(0.0, -1.0, SPACE).expect("grade");
        assert_eq!(grade.scale([0.3, 0.6, 0.9]), 0.0);
        let grey = grade.apply_rgb([0.3, 0.6, 0.9]);
        let expected = 0.3 * SPACE[0] + 0.6 * SPACE[1] + 0.9 * SPACE[2];
        for value in grey {
            assert!((value - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn the_combined_scale_cannot_go_negative() {
        for vibrance in [-1.0_f32, -0.5, 0.0, 0.5, 1.0] {
            for saturation in [-1.0_f32, -0.5, 0.0, 0.5, 1.0] {
                let grade = VibranceGrade::new(vibrance, saturation, SPACE).expect("grade");
                for colour in [[0.0, 0.0, 0.0], [0.5, 0.5, 0.5], [0.2, 0.4, 0.9]] {
                    assert!(grade.scale(colour) >= 0.0);
                }
            }
        }
    }

    #[test]
    fn extended_range_values_keep_their_sign_and_magnitude() {
        let grade = VibranceGrade::new(0.0, 1.0, SPACE).expect("grade");
        let hdr = grade.apply_rgb([4.0, 2.0, 1.0]);
        assert!(hdr[0] > 4.0);
        let negative = grade.apply_rgb([-0.4, -0.2, -0.1]);
        assert!(negative[0] < -0.4);
        assert!(negative[2] > -0.1);
    }

    #[test]
    fn alpha_is_preserved() {
        let grade = VibranceGrade::new(0.5, 0.5, SPACE).expect("grade");
        assert_eq!(grade.apply([0.5, 0.2, 0.2, 0.375])[3], 0.375);
    }

    #[test]
    fn gpu_controls_carry_the_controls_and_the_coefficients() {
        let grade = VibranceGrade::new(-0.25, 0.5, SPACE).expect("grade");
        let (params, extra) = grade.gpu_controls();
        assert_eq!(params, [-0.25, 0.5, 0.2126, 0.7152]);
        assert_eq!(extra[0], 0.0722);
    }

    #[test]
    fn a_frame_is_graded_in_place() {
        let grade = VibranceGrade::new(0.0, 1.0, SPACE).expect("grade");
        let mut frame = vec![[0.5, 0.45, 0.42, 1.0]];
        assert!(vibrance_frame_f32(&mut frame, &grade));
        assert!(frame[0][0] > 0.5);
        let identity = VibranceGrade::new(0.0, 0.0, SPACE).expect("grade");
        assert!(!vibrance_frame_f32(&mut frame, &identity));
    }
}
