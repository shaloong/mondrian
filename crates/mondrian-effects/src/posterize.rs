//! Posterize (STY-09 色调分离): quantise each colour channel into a fixed
//! number of evenly spaced levels.
//!
//! The level count is authored as a whole number between 2 and 255. Values are
//! quantised in the working (scene-linear) domain; nothing is clamped, so
//! highlights and negative samples keep their sign and simply snap to the
//! nearest level on the same lattice. Alpha is never touched.

/// Fewest levels the list allows (a two-level split).
pub const POSTERIZE_MIN_LEVELS: u32 = 2;
/// Most levels the list allows (effectively per-8-bit steps).
pub const POSTERIZE_MAX_LEVELS: u32 = 255;

/// Rejected [`PosterizeGrade`] authoring values.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PosterizeError {
    /// The level count is outside `2..=255`.
    #[error("posterize levels must be within [2, 255]")]
    Levels,
}

/// Validated posterize parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PosterizeGrade {
    levels: u32,
}

impl PosterizeGrade {
    /// Validates the level count.
    pub fn new(levels: u32) -> Result<Self, PosterizeError> {
        if !(POSTERIZE_MIN_LEVELS..=POSTERIZE_MAX_LEVELS).contains(&levels) {
            return Err(PosterizeError::Levels);
        }
        Ok(Self { levels })
    }

    /// Number of quantisation levels per channel.
    pub const fn levels(self) -> u32 {
        self.levels
    }

    /// Posterize always quantises, so it is never the identity.
    pub const fn is_identity(self) -> bool {
        false
    }

    /// Quantised value of a single channel.
    pub fn quantise(self, value: f32) -> f32 {
        let steps = (self.levels - 1) as f32;
        (value * steps).round() / steps
    }

    /// Quantises the colour channels and keeps alpha.
    pub fn apply(self, pixel: [f32; 4]) -> [f32; 4] {
        [
            self.quantise(pixel[0]),
            self.quantise(pixel[1]),
            self.quantise(pixel[2]),
            pixel[3],
        ]
    }

    /// Quantises the colour channels for the point-grade helper.
    pub fn apply_rgb(self, rgb: [f32; 3]) -> [f32; 3] {
        [
            self.quantise(rgb[0]),
            self.quantise(rgb[1]),
            self.quantise(rgb[2]),
        ]
    }

    /// Packed GPU controls: `[levels, 0, 0, 0]`.
    pub fn gpu_controls(self) -> [f32; 4] {
        [self.levels as f32, 0.0, 0.0, 0.0]
    }
}

/// Quantises a whole frame.
pub fn posterize_frame_f32(
    pixels: &mut [[f32; 4]],
    _width: usize,
    _height: usize,
    grade: PosterizeGrade,
) {
    for pixel in pixels.iter_mut() {
        *pixel = grade.apply(*pixel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(levels: u32) -> PosterizeGrade {
        PosterizeGrade::new(levels).expect("valid posterize")
    }

    #[test]
    fn rejects_level_counts_outside_the_listed_window() {
        assert_eq!(PosterizeGrade::new(1), Err(PosterizeError::Levels));
        assert_eq!(PosterizeGrade::new(0), Err(PosterizeError::Levels));
        assert_eq!(PosterizeGrade::new(256), Err(PosterizeError::Levels));
        assert!(PosterizeGrade::new(2).is_ok());
        assert!(PosterizeGrade::new(255).is_ok());
    }

    #[test]
    fn two_levels_split_the_lattice_in_half() {
        let grade = grade(2);
        assert_eq!(grade.quantise(0.0), 0.0);
        assert_eq!(grade.quantise(0.49), 0.0);
        assert_eq!(grade.quantise(0.51), 1.0);
        assert_eq!(grade.quantise(1.0), 1.0);
    }

    #[test]
    fn seven_levels_snap_to_sixths() {
        let grade = grade(7);
        // 6 steps means every level is 1/6 apart.
        for (input, expected) in [
            (0.0_f32, 0.0_f32),
            (0.08, 0.0),
            (0.09, 1.0 / 6.0),
            (0.5, 0.5),
            (0.75, 1.0 / 6.0 * 5.0),
        ] {
            assert!(
                (grade.quantise(input) - expected).abs() < 1.0e-6,
                "{input} -> {}",
                grade.quantise(input)
            );
        }
    }

    #[test]
    fn extended_range_values_keep_their_sign() {
        let grade = grade(3);
        assert_eq!(grade.quantise(2.0), 2.0);
        assert_eq!(grade.quantise(-0.5), -0.5);
        assert_eq!(grade.quantise(1.75), 2.0);
    }

    #[test]
    fn alpha_is_preserved_and_channels_are_independent() {
        let grade = grade(4);
        let pixel = grade.apply([0.1, 0.4, 0.9, 0.25]);
        assert!((pixel[0] - 0.0).abs() < 1.0e-6, "{pixel:?}");
        assert!((pixel[1] - 1.0 / 3.0).abs() < 1.0e-6, "{pixel:?}");
        assert!((pixel[2] - 1.0).abs() < 1.0e-6, "{pixel:?}");
        assert_eq!(pixel[3], 0.25);
    }

    #[test]
    fn a_frame_quantises_in_place() {
        let mut pixels = vec![[0.1, 0.5, 0.9, 1.0], [0.0, 1.0, 0.5, 0.5]];
        posterize_frame_f32(&mut pixels, 2, 1, grade(2));
        assert_eq!(pixels[0], [0.0, 1.0, 1.0, 1.0]);
        assert_eq!(pixels[1], [0.0, 1.0, 1.0, 0.5]);
    }

    #[test]
    fn gpu_controls_carry_the_level_count() {
        let controls = grade(9).gpu_controls();
        assert_eq!(controls, [9.0, 0.0, 0.0, 0.0]);
    }
}
