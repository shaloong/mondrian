//! Tritone: maps luminance onto a three-colour ramp.
//!
//! The kernel reads each pixel's working-space luminance and interpolates a
//! ramp from the shadow colour through the midtone colour to the highlight
//! colour, then mixes that ramp back against the source by the author's
//! "blend with original" amount. It is a pure point operation: the result only
//! depends on the pixel itself.
//!
//! Contract notes:
//! - the ramp is defined on the display range `[0, 1]`, so an extended-range or
//!   negative luminance selects the nearest endpoint colour instead of
//!   extrapolating the ramp into unbounded colours. Only the ramp parameter is
//!   clamped; the colours mixed back in are not;
//! - the colours themselves are never clamped, so HDR authoring survives into
//!   the ramp endpoints;
//! - alpha is preserved.

use mondrian_core::WorkingColorSpace;

/// Validation failure for [`TritoneGrade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TritoneError {
    /// The blend amount is not a finite number inside [0, 1].
    #[error("tritone blend with original must be finite and within [0, 1]")]
    Blend,
    /// One of the three ramp colours is not usable.
    #[error("tritone ramp colours must be finite")]
    Colour,
    /// The working-space luminance coefficients are not usable.
    #[error("tritone luminance coefficients must be finite and non-zero")]
    LuminanceCoefficients,
}

/// Authorable parameters of the tritone effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TritoneGrade {
    shadows: [f32; 3],
    midtones: [f32; 3],
    highlights: [f32; 3],
    blend_with_original: f32,
    luminance_coefficients: [f32; 3],
}

impl TritoneGrade {
    /// Builds a grade, rejecting values the authoring schema cannot express.
    pub fn new(
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        blend_with_original: f32,
        luminance_coefficients: [f32; 3],
    ) -> Result<Self, TritoneError> {
        if !blend_with_original.is_finite() || !(0.0..=1.0).contains(&blend_with_original) {
            return Err(TritoneError::Blend);
        }
        for colour in [shadows, midtones, highlights] {
            if colour.iter().any(|value| !value.is_finite()) {
                return Err(TritoneError::Colour);
            }
        }
        let total: f32 = luminance_coefficients.iter().sum();
        if luminance_coefficients.iter().any(|value| !value.is_finite()) || total <= 0.0 {
            return Err(TritoneError::LuminanceCoefficients);
        }
        Ok(Self {
            shadows,
            midtones,
            highlights,
            blend_with_original,
            luminance_coefficients,
        })
    }

    /// Builds a grade for one working space.
    pub fn for_space(
        shadows: [f32; 3],
        midtones: [f32; 3],
        highlights: [f32; 3],
        blend_with_original: f32,
        space: WorkingColorSpace,
    ) -> Result<Self, TritoneError> {
        Self::new(
            shadows,
            midtones,
            highlights,
            blend_with_original,
            space.luminance_coefficients(),
        )
    }

    /// Colour the darkest end of the ramp receives.
    pub const fn shadows(self) -> [f32; 3] {
        self.shadows
    }

    /// Colour the middle of the ramp receives.
    pub const fn midtones(self) -> [f32; 3] {
        self.midtones
    }

    /// Colour the brightest end of the ramp receives.
    pub const fn highlights(self) -> [f32; 3] {
        self.highlights
    }

    /// Authored amount of the source mixed back in.
    pub const fn blend_with_original(self) -> f32 {
        self.blend_with_original
    }

    /// Working-space luminance coefficients used by the kernel.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// A full blend with the original leaves every pixel untouched.
    pub fn is_identity(self) -> bool {
        self.blend_with_original >= 1.0 - 1e-6
    }

    /// Luminance the ramp is sampled at, clamped into the display range.
    pub fn ramp_position(self, rgb: [f32; 3]) -> f32 {
        let coefficients = self.luminance_coefficients;
        let luminance =
            rgb[0] * coefficients[0] + rgb[1] * coefficients[1] + rgb[2] * coefficients[2];
        luminance.clamp(0.0, 1.0)
    }

    /// Colour the ramp itself produces for a luminance position.
    pub fn ramp_colour(self, position: f32) -> [f32; 3] {
        let position = position.clamp(0.0, 1.0);
        let (from, to, weight) = if position <= 0.5 {
            (self.shadows, self.midtones, position * 2.0)
        } else {
            (self.midtones, self.highlights, position * 2.0 - 1.0)
        };
        let mut result = [0.0; 3];
        for channel in 0..3 {
            result[channel] = from[channel] + (to[channel] - from[channel]) * weight;
        }
        result
    }

    /// Applies the grade to one straight-alpha working-space colour.
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        let rgb = self.apply_rgb([pixel[0], pixel[1], pixel[2]]);
        [rgb[0], rgb[1], rgb[2], pixel[3]]
    }

    /// Applies the grade to one working-space colour, keeping alpha outside.
    pub fn apply_rgb(self, rgb: [f32; 3]) -> [f32; 3] {
        let target = self.ramp_colour(self.ramp_position(rgb));
        let mut result = [0.0; 3];
        for channel in 0..3 {
            result[channel] =
                target[channel] + (rgb[channel] - target[channel]) * self.blend_with_original;
        }
        result
    }

    /// Packs the values the GPU point pass needs.
    ///
    /// The first vector carries the blend amount followed by the working-space
    /// luminance coefficients, then the three ramp colours run across the
    /// remaining three vectors in order.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.blend_with_original,
                self.luminance_coefficients[0],
                self.luminance_coefficients[1],
                self.luminance_coefficients[2],
            ],
            [
                self.shadows[0],
                self.shadows[1],
                self.shadows[2],
                self.midtones[0],
            ],
            [
                self.midtones[1],
                self.midtones[2],
                self.highlights[0],
                self.highlights[1],
            ],
            [self.highlights[2], 0.0, 0.0, 0.0],
        )
    }
}

/// Applies the grade to every pixel of one frame, in place.
pub fn tritone_frame_f32(pixels: &mut [[f32; 4]], grade: &TritoneGrade) -> bool {
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
    const SHADOWS: [f32; 3] = [0.0, 0.0, 0.0];
    const MIDTONES: [f32; 3] = [0.5, 0.39, 0.27];
    const HIGHLIGHTS: [f32; 3] = [1.0, 1.0, 1.0];

    fn grade(blend_with_original: f32) -> TritoneGrade {
        TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, blend_with_original, SPACE).expect("grade")
    }

    #[test]
    fn new_rejects_values_outside_the_authored_window() {
        assert_eq!(
            TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, 1.5, SPACE),
            Err(TritoneError::Blend)
        );
        assert_eq!(
            TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, -0.01, SPACE),
            Err(TritoneError::Blend)
        );
        assert_eq!(
            TritoneGrade::new(SHADOWS, [f32::NAN, 0.0, 0.0], HIGHLIGHTS, 0.0, SPACE),
            Err(TritoneError::Colour)
        );
        assert_eq!(
            TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, 0.0, [0.0; 3]),
            Err(TritoneError::LuminanceCoefficients)
        );
        assert!(TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, 1.0, SPACE).is_ok());
    }

    #[test]
    fn a_full_blend_with_the_original_is_the_identity() {
        let grade = grade(1.0);
        assert!(grade.is_identity());
        assert_eq!(grade.apply([0.4, 0.5, 2.5, 0.25]), [0.4, 0.5, 2.5, 0.25]);
        let mut frame = vec![[0.1, 0.2, 0.3, 1.0]];
        assert!(!tritone_frame_f32(&mut frame, &grade));
        assert_eq!(frame[0], [0.1, 0.2, 0.3, 1.0]);
    }

    #[test]
    fn the_ramp_endpoints_and_middle_land_on_the_authored_colours() {
        let grade = grade(0.0);
        assert_eq!(grade.apply_rgb([0.0, 0.0, 0.0]), SHADOWS);
        assert_eq!(grade.ramp_colour(0.5), MIDTONES);
        assert_eq!(grade.apply_rgb([1.0, 1.0, 1.0]), HIGHLIGHTS);
        // A neutral pixel at 50% luminance is neutral in the working space, not
        // in sRGB, so it lands on the midtone colour exactly.
        assert_eq!(grade.apply_rgb([0.5, 0.5, 0.5]), MIDTONES);
    }

    #[test]
    fn the_ramp_interpolates_between_the_neighbouring_colours() {
        let grade = grade(0.0);
        let quarter = grade.ramp_colour(0.25);
        assert!((quarter[0] - 0.25).abs() < 1e-6);
        assert!((quarter[1] - 0.195).abs() < 1e-6);
        let three_quarters = grade.ramp_colour(0.75);
        assert!((three_quarters[0] - 0.75).abs() < 1e-6);
        assert!((three_quarters[1] - 0.695).abs() < 1e-6);
        // The unclamped ramp accessor still clamps its position argument.
        assert_eq!(grade.ramp_colour(-3.0), SHADOWS);
        assert_eq!(grade.ramp_colour(9.0), HIGHLIGHTS);
    }

    #[test]
    fn extended_range_luminance_selects_the_endpoint_colour() {
        let grade = grade(0.0);
        assert_eq!(grade.apply_rgb([2.0, 2.0, 2.0]), HIGHLIGHTS);
        assert_eq!(grade.apply_rgb([-0.5, -0.5, -0.5]), SHADOWS);
        // The author's colours themselves are never clamped: a hot highlight
        // survives into the output untouched.
        let hot =
            TritoneGrade::new(SHADOWS, MIDTONES, [4.0, 0.5, 0.25], 0.0, SPACE).expect("grade");
        assert_eq!(hot.apply_rgb([2.0, 2.0, 2.0]), [4.0, 0.5, 0.25]);
    }

    #[test]
    fn the_blend_amount_mixes_the_ramp_against_the_source() {
        let half = grade(0.5);
        let source = [1.0, 0.0, 0.25];
        let ramp = half.ramp_colour(half.ramp_position(source));
        let result = half.apply_rgb(source);
        for channel in 0..3 {
            let expected = ramp[channel] + (source[channel] - ramp[channel]) * 0.5;
            assert!((result[channel] - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn alpha_is_preserved() {
        let grade = grade(0.0);
        assert_eq!(grade.apply([0.2, 0.4, 0.6, 0.0])[3], 0.0);
        assert_eq!(grade.apply([0.2, 0.4, 0.6, 0.375])[3], 0.375);
    }

    #[test]
    fn luminance_coefficients_come_from_the_working_space() {
        let rec709 =
            TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, 0.0, [0.2126, 0.7152, 0.0722])
                .expect("grade");
        let rec2020 =
            TritoneGrade::new(SHADOWS, MIDTONES, HIGHLIGHTS, 0.0, [0.2627, 0.678, 0.0593])
                .expect("grade");
        let green = [0.0, 0.6, 0.0];
        assert!(
            (rec709.ramp_position(green) - rec2020.ramp_position(green)).abs() > 1e-4,
            "the two spaces weight green differently"
        );
    }

    #[test]
    fn a_frame_is_graded_in_place() {
        let grade = grade(0.0);
        let mut frame = vec![[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 0.5]];
        assert!(tritone_frame_f32(&mut frame, &grade));
        assert_eq!(frame[0], [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(frame[1], [1.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn gpu_controls_carry_the_ramp_and_the_blend() {
        let grade = TritoneGrade::new(
            [0.1, 0.2, 0.3],
            [0.4, 0.5, 0.6],
            [0.7, 0.8, 0.9],
            0.25,
            SPACE,
        )
        .expect("grade");
        let (params, color, extra0, extra1) = grade.gpu_controls();
        assert_eq!(params, [0.25, 0.2126, 0.7152, 0.0722]);
        assert_eq!(color, [0.1, 0.2, 0.3, 0.4]);
        assert_eq!(extra0, [0.5, 0.6, 0.7, 0.8]);
        assert_eq!(extra1[0], 0.9);
    }
}
