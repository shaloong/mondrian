//! Duotone tint.
//!
//! The kernel maps the working-space luminance of every pixel onto the line
//! between the authored black and white targets and blends that map back over
//! the source by the authored amount. It is a pure point operation: the result
//! only depends on the pixel itself.
//!
//! Contract notes:
//! - the map is evaluated on the working-space luminance, so the endpoints land
//!   on the authored targets instead of on a channel average;
//! - the mapped colour is not clamped, so an HDR highlight above diffuse white
//!   extrapolates past the white target instead of flattening to it;
//! - the blend is a straight linear mix, so an amount of zero is the exact
//!   identity and the source keeps its extended-range values;
//! - alpha is preserved.

use mondrian_core::WorkingColorSpace;

/// Validation failure for [`TintGrade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TintError {
    /// One of the two targets is not a finite colour.
    #[error("tint targets must be finite colours")]
    Target,
    /// The amount is not a finite number inside [0, 1].
    #[error("tint amount must be finite and within [0, 1]")]
    Amount,
    /// The working-space luminance coefficients are not usable.
    #[error("tint luminance coefficients must be finite and non-zero")]
    LuminanceCoefficients,
}

/// Authorable parameters of the tint effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TintGrade {
    black: [f32; 3],
    white: [f32; 3],
    amount: f32,
    luminance_coefficients: [f32; 3],
}

impl TintGrade {
    /// Builds a grade, rejecting values the authoring schema cannot express.
    pub fn new(
        black: [f32; 3],
        white: [f32; 3],
        amount: f32,
        luminance_coefficients: [f32; 3],
    ) -> Result<Self, TintError> {
        if black.iter().chain(white.iter()).any(|value| !value.is_finite()) {
            return Err(TintError::Target);
        }
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(TintError::Amount);
        }
        let total: f32 = luminance_coefficients.iter().sum();
        if luminance_coefficients.iter().any(|value| !value.is_finite()) || total <= 0.0 {
            return Err(TintError::LuminanceCoefficients);
        }
        Ok(Self { black, white, amount, luminance_coefficients })
    }

    /// Builds a grade for one working space.
    pub fn for_space(
        black: [f32; 3],
        white: [f32; 3],
        amount: f32,
        space: WorkingColorSpace,
    ) -> Result<Self, TintError> {
        Self::new(black, white, amount, space.luminance_coefficients())
    }

    /// Authored colour the darkest pixels map to.
    pub const fn black(self) -> [f32; 3] {
        self.black
    }

    /// Authored colour the brightest pixels map to.
    pub const fn white(self) -> [f32; 3] {
        self.white
    }

    /// Authored blend amount inside [0, 1].
    pub const fn amount(self) -> f32 {
        self.amount
    }

    /// Working-space luminance coefficients used by the kernel.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// A zero amount leaves every pixel untouched.
    pub fn is_identity(self) -> bool {
        self.amount <= 1e-6
    }

    /// Applies the tint to one straight-alpha working-space colour.
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        let rgb = self.apply_rgb([pixel[0], pixel[1], pixel[2]]);
        [rgb[0], rgb[1], rgb[2], pixel[3]]
    }

    /// Applies the tint to one working-space colour, keeping alpha outside.
    pub fn apply_rgb(self, rgb: [f32; 3]) -> [f32; 3] {
        let coefficients = self.luminance_coefficients;
        let luminance =
            rgb[0] * coefficients[0] + rgb[1] * coefficients[1] + rgb[2] * coefficients[2];
        let mut tinted = [0.0; 3];
        let mut result = [0.0; 3];
        for channel in 0..3 {
            tinted[channel] =
                self.black[channel] + (self.white[channel] - self.black[channel]) * luminance;
            result[channel] = rgb[channel] + (tinted[channel] - rgb[channel]) * self.amount;
        }
        result
    }

    /// Packs the values the GPU point pass needs.
    ///
    /// The first vector carries the amount and the working-space luminance
    /// coefficients, the second and third the two targets.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.amount,
                self.luminance_coefficients[0],
                self.luminance_coefficients[1],
                self.luminance_coefficients[2],
            ],
            [self.black[0], self.black[1], self.black[2], self.white[0]],
            [self.white[1], self.white[2], 0.0, 0.0],
        )
    }
}

/// Applies the tint to every pixel of one frame, in place.
pub fn tint_frame_f32(pixels: &mut [[f32; 4]], grade: &TintGrade) -> bool {
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
            TintGrade::new([f32::NAN, 0.0, 0.0], [1.0; 3], 1.0, SPACE),
            Err(TintError::Target)
        );
        assert_eq!(
            TintGrade::new([0.0; 3], [1.0; 3], 1.5, SPACE),
            Err(TintError::Amount)
        );
        assert_eq!(
            TintGrade::new([0.0; 3], [1.0; 3], 1.0, [0.0; 3]),
            Err(TintError::LuminanceCoefficients)
        );
        assert!(TintGrade::new([0.0; 3], [1.0; 3], 1.0, SPACE).is_ok());
    }

    #[test]
    fn zero_amount_is_the_identity() {
        let grade = TintGrade::new([0.1, 0.2, 0.3], [0.9, 0.8, 0.7], 0.0, SPACE).expect("grade");
        assert!(grade.is_identity());
        assert_eq!(grade.apply([0.4, 0.5, 2.5, 0.25]), [0.4, 0.5, 2.5, 0.25]);
    }

    #[test]
    fn the_endpoints_land_on_the_authored_targets() {
        let grade = TintGrade::new([0.1, 0.2, 0.3], [0.9, 0.8, 0.7], 1.0, SPACE).expect("grade");
        assert_eq!(grade.apply_rgb([0.0, 0.0, 0.0]), [0.1, 0.2, 0.3]);
        let white = grade.apply_rgb([1.0, 1.0, 1.0]);
        for channel in 0..3 {
            assert!((white[channel] - [0.9, 0.8, 0.7][channel]).abs() < 1e-6);
        }
    }

    #[test]
    fn the_map_follows_luminance_not_the_channel_average() {
        let grade = TintGrade::new([0.0; 3], [1.0; 3], 1.0, SPACE).expect("grade");
        // Bright green is far brighter than bright blue under the working-space
        // coefficients, so it maps much closer to the white target.
        let green = grade.apply_rgb([0.0, 0.8, 0.0])[0];
        let blue = grade.apply_rgb([0.0, 0.0, 0.8])[0];
        assert!(green > blue);
        assert!((green - 0.8 * 0.7152).abs() < 1e-6);
    }

    #[test]
    fn amount_blends_the_map_with_the_source() {
        let grade = TintGrade::new([0.0; 3], [0.0, 0.0, 0.0], 1.0, SPACE).expect("grade");
        assert_eq!(grade.apply_rgb([0.5, 0.5, 0.5]), [0.0, 0.0, 0.0]);
        let half = TintGrade::new([0.0; 3], [0.0; 3], 0.5, SPACE).expect("grade");
        assert_eq!(half.apply_rgb([0.5, 0.5, 0.5]), [0.25, 0.25, 0.25]);
    }

    #[test]
    fn the_map_is_not_clamped_above_diffuse_white() {
        let grade = TintGrade::new([0.0; 3], [1.0; 3], 1.0, SPACE).expect("grade");
        // Luminance 2.0 extrapolates to twice the white target.
        assert_eq!(grade.apply_rgb([2.0, 2.0, 2.0]), [2.0, 2.0, 2.0]);
        // The source keeps its own extended range at a partial amount.
        let partial = TintGrade::new([0.0; 3], [1.0; 3], 0.5, SPACE).expect("grade");
        assert!(partial.apply_rgb([4.0, 4.0, 4.0])[0] > 2.0);
    }

    #[test]
    fn alpha_is_preserved() {
        let grade = TintGrade::new([0.2; 3], [0.8; 3], 1.0, SPACE).expect("grade");
        assert_eq!(grade.apply([0.5, 0.5, 0.5, 0.375])[3], 0.375);
    }

    #[test]
    fn gpu_controls_carry_the_targets_and_the_amount() {
        let grade = TintGrade::new([0.1, 0.2, 0.3], [0.4, 0.5, 0.6], 0.25, SPACE).expect("grade");
        let (params, color, extra) = grade.gpu_controls();
        assert_eq!(params, [0.25, 0.2126, 0.7152, 0.0722]);
        assert_eq!(color, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(&extra[..2], &[0.5, 0.6]);
    }

    #[test]
    fn a_frame_is_tinted_in_place() {
        let grade = TintGrade::new([0.0; 3], [1.0; 3], 1.0, SPACE).expect("grade");
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]];
        assert!(tint_frame_f32(&mut frame, &grade));
        assert_eq!(frame[0][0], 0.0);
        assert!((frame[1][0] - 1.0).abs() < 1e-6);
    }
}
