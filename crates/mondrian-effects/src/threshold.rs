//! Luminance threshold.
//!
//! The kernel compares the working-space luminance of every pixel with the
//! authored level and writes a black or white pixel. It is a pure point
//! operation: the result only depends on the pixel itself.
//!
//! Contract notes:
//! - the level is authored on the working-space luminance, so an HDR highlight
//!   above the level stays white and a negative level keeps the frame black;
//! - the level is never clamped, which lets the authored range cover the full
//!   [-30000, 30000] window from the effect list;
//! - pixels without coverage stay untouched, and alpha is preserved.

use mondrian_core::WorkingColorSpace;

/// Lowest authored level.
pub const THRESHOLD_MIN_LEVEL: f32 = -30_000.0;
/// Highest authored level.
pub const THRESHOLD_MAX_LEVEL: f32 = 30_000.0;

/// Validation failure for [`ThresholdGrade`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ThresholdError {
    /// The level is not a finite number inside the authored window.
    #[error("threshold level must be finite and within [-30000, 30000]")]
    Level,
    /// The working-space luminance coefficients are not usable.
    #[error("threshold luminance coefficients must be finite and non-zero")]
    LuminanceCoefficients,
}

/// Authorable parameters of the threshold effect.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThresholdGrade {
    level: f32,
    luminance_coefficients: [f32; 3],
}

impl ThresholdGrade {
    /// Builds a grade, rejecting values the authoring schema cannot express.
    pub fn new(level: f32, luminance_coefficients: [f32; 3]) -> Result<Self, ThresholdError> {
        if !level.is_finite() || !(THRESHOLD_MIN_LEVEL..=THRESHOLD_MAX_LEVEL).contains(&level) {
            return Err(ThresholdError::Level);
        }
        let total: f32 = luminance_coefficients.iter().sum();
        if luminance_coefficients.iter().any(|value| !value.is_finite()) || total <= 0.0 {
            return Err(ThresholdError::LuminanceCoefficients);
        }
        Ok(Self { level, luminance_coefficients })
    }

    /// Builds a grade for one working space.
    pub fn for_space(level: f32, space: WorkingColorSpace) -> Result<Self, ThresholdError> {
        Self::new(level, space.luminance_coefficients())
    }

    /// Authored threshold level.
    pub const fn level(self) -> f32 {
        self.level
    }

    /// Working-space luminance coefficients used by the kernel.
    pub const fn luminance_coefficients(self) -> [f32; 3] {
        self.luminance_coefficients
    }

    /// The threshold effect has no identity value: every level quantises.
    pub const fn is_identity(self) -> bool {
        false
    }

    /// Applies the threshold to one straight-alpha working-space colour.
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        let coefficients = self.luminance_coefficients;
        let luminance =
            pixel[0] * coefficients[0] + pixel[1] * coefficients[1] + pixel[2] * coefficients[2];
        let value = if luminance >= self.level { 1.0 } else { 0.0 };
        [value, value, value, pixel[3]]
    }

    /// Applies the threshold to one working-space colour, keeping alpha outside.
    pub fn apply_rgb(self, rgb: [f32; 3]) -> [f32; 3] {
        let coefficients = self.luminance_coefficients;
        let luminance =
            rgb[0] * coefficients[0] + rgb[1] * coefficients[1] + rgb[2] * coefficients[2];
        let value = if luminance >= self.level { 1.0 } else { 0.0 };
        [value, value, value]
    }

    /// Packs the values the GPU point pass needs: level and coefficients.
    pub fn gpu_controls(self) -> ([f32; 4], [f32; 4]) {
        (
            [self.level, 0.0, 0.0, 0.0],
            [
                self.luminance_coefficients[0],
                self.luminance_coefficients[1],
                self.luminance_coefficients[2],
                0.0,
            ],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACE: [f32; 3] = [0.2126, 0.7152, 0.0722];

    #[test]
    fn new_rejects_values_outside_the_authored_window() {
        assert_eq!(
            ThresholdGrade::new(f32::NAN, SPACE),
            Err(ThresholdError::Level)
        );
        assert_eq!(
            ThresholdGrade::new(30_001.0, SPACE),
            Err(ThresholdError::Level)
        );
        assert_eq!(
            ThresholdGrade::new(0.5, [0.0, 0.0, 0.0]),
            Err(ThresholdError::LuminanceCoefficients)
        );
        assert!(ThresholdGrade::new(0.5, SPACE).is_ok());
    }

    #[test]
    fn level_selects_black_or_white_per_pixel() {
        let grade = ThresholdGrade::new(0.5, SPACE).expect("grade");
        assert_eq!(grade.apply([0.2, 0.2, 0.2, 1.0]), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(grade.apply([0.8, 0.8, 0.8, 1.0]), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn luminance_decides_not_the_channel_average() {
        let grade = ThresholdGrade::new(0.5, SPACE).expect("grade");
        // Bright green passes the level although two channels are dark.
        assert_eq!(grade.apply([0.0, 0.8, 0.0, 1.0])[0], 1.0);
        // Bright blue does not: its luminance coefficient is small.
        assert_eq!(grade.apply([0.0, 0.0, 1.0, 1.0])[0], 0.0);
    }

    #[test]
    fn level_is_not_clamped_and_keeps_extended_range() {
        let grade = ThresholdGrade::new(-2.0, SPACE).expect("grade");
        assert_eq!(grade.apply([0.1, 0.1, 0.1, 1.0])[0], 1.0);
        let high = ThresholdGrade::new(3.0, SPACE).expect("grade");
        assert_eq!(high.apply([2.0, 2.0, 2.0, 1.0])[0], 0.0);
    }

    #[test]
    fn alpha_is_preserved() {
        let grade = ThresholdGrade::new(0.5, SPACE).expect("grade");
        assert_eq!(grade.apply([0.9, 0.9, 0.9, 0.375])[3], 0.375);
    }

    #[test]
    fn gpu_controls_round_trip() {
        let grade = ThresholdGrade::new(0.25, SPACE).expect("grade");
        let (params, color) = grade.gpu_controls();
        assert_eq!(params, [0.25, 0.0, 0.0, 0.0]);
        assert_eq!(&color[..3], &SPACE);
    }
}
