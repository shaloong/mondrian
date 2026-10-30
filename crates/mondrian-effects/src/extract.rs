//! The extract matte effect from the built-in effect list.
//!
//! The list authors six controls: a source channel, a black point, a white
//! point, a softness for each of those points, and an invert toggle. All of
//! them are point operations, so the effect runs on the fused GPU point path
//! as well as the CPU kernel.
//!
//! # Domain
//!
//! The list serves every value as an eight bit display encoded number
//! (`0` to `255`). The working frame is scene linear, so the authored values are
//! read as normalised levels in that same domain: `255` becomes `1.0`. That is a
//! documented authoring convention rather than a silent eight bit round trip -
//! the effect never quantises anything, and values above `1.0` simply fall
//! outside the authored range.
//!
//! The result replaces the colour with the matte and leaves alpha untouched, so
//! the effect composes like a colour operation instead of a key.

/// Highest authored value of a level control (`0` to `255`).
pub const EXTRACT_AUTHOR_MAX_LEVEL: f32 = 255.0;

/// Number of declared channel options.
pub const EXTRACT_CHANNEL_COUNT: usize = 5;

const CHANNEL_EPSILON: f32 = 1.0e-6;

/// Source channel the matte is read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractChannel {
    /// The luminance of the working colour space.
    Luma,
    /// The red channel.
    Red,
    /// The green channel.
    Green,
    /// The blue channel.
    Blue,
    /// The coverage channel.
    Alpha,
}

impl ExtractChannel {
    /// Every channel in menu order.
    pub const ALL: [Self; EXTRACT_CHANNEL_COUNT] =
        [Self::Luma, Self::Red, Self::Green, Self::Blue, Self::Alpha];

    /// The stable authoring key of the channel.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Luma => "luma",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
        }
    }

    /// Resolves an authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|channel| channel.as_key() == key)
    }

    /// The list menu numbering, starting at one.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Luma => 1,
            Self::Red => 2,
            Self::Green => 3,
            Self::Blue => 4,
            Self::Alpha => 5,
        }
    }

    /// Resolves the list menu numbering.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Luma),
            2 => Some(Self::Red),
            3 => Some(Self::Green),
            4 => Some(Self::Blue),
            5 => Some(Self::Alpha),
            _ => None,
        }
    }

    /// The value carried into the shader.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Luma => 0,
            Self::Red => 1,
            Self::Green => 2,
            Self::Blue => 3,
            Self::Alpha => 4,
        }
    }
}

/// Why a grade could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractError {
    /// One of the levels was not a finite number inside the declared range.
    Level,
    /// The black point does not sit below the white point.
    Range,
    /// The luminance coefficients were not usable.
    Luminance,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Level => formatter
                .write_str("extract levels must be finite and inside the authored 0 to 255 range"),
            Self::Range => {
                formatter.write_str("extract expects the black point below the white point")
            }
            Self::Luminance => {
                formatter.write_str("extract luminance coefficients must be finite and positive")
            }
        }
    }
}

impl std::error::Error for ExtractError {}

/// The resolved state of the extract effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExtractGrade {
    channel: ExtractChannel,
    black_point: f32,
    white_point: f32,
    black_softness: f32,
    white_softness: f32,
    invert: bool,
    luminance: [f32; 3],
}

impl ExtractGrade {
    /// Builds a grade from normalised levels.
    ///
    /// # Errors
    ///
    /// Returns [`ExtractError::Level`] when a level is not finite or outside
    /// `[0, 1]`, and [`ExtractError::Range`] when the black point does not sit
    /// strictly below the white point.
    pub fn new(
        channel: ExtractChannel,
        black_point: f32,
        white_point: f32,
        black_softness: f32,
        white_softness: f32,
        invert: bool,
        luminance: [f32; 3],
    ) -> Result<Self, ExtractError> {
        for level in [black_point, white_point, black_softness, white_softness] {
            if !level.is_finite() || !(0.0..=1.0).contains(&level) {
                return Err(ExtractError::Level);
            }
        }
        if black_point + CHANNEL_EPSILON >= white_point {
            return Err(ExtractError::Range);
        }
        if luminance.iter().any(|coefficient| !coefficient.is_finite()) {
            return Err(ExtractError::Luminance);
        }
        Ok(Self {
            channel,
            black_point,
            white_point,
            black_softness,
            white_softness,
            invert,
            luminance,
        })
    }

    /// Builds a grade whose luminance comes from the working colour space.
    ///
    /// # Errors
    ///
    /// Propagates the level and range checks of [`Self::new`].
    pub fn for_space(
        channel: ExtractChannel,
        black_point: f32,
        white_point: f32,
        black_softness: f32,
        white_softness: f32,
        invert: bool,
        space: mondrian_core::WorkingColorSpace,
    ) -> Result<Self, ExtractError> {
        Self::new(
            channel,
            black_point,
            white_point,
            black_softness,
            white_softness,
            invert,
            space.luminance_coefficients(),
        )
    }

    /// The source channel.
    #[must_use]
    pub const fn channel(&self) -> ExtractChannel {
        self.channel
    }

    /// The normalised black point.
    #[must_use]
    pub const fn black_point(&self) -> f32 {
        self.black_point
    }

    /// The normalised white point.
    #[must_use]
    pub const fn white_point(&self) -> f32 {
        self.white_point
    }

    /// The normalised softness around the black point.
    #[must_use]
    pub const fn black_softness(&self) -> f32 {
        self.black_softness
    }

    /// The normalised softness around the white point.
    #[must_use]
    pub const fn white_softness(&self) -> f32 {
        self.white_softness
    }

    /// Whether the matte is inverted.
    #[must_use]
    pub const fn inverted(&self) -> bool {
        self.invert
    }

    /// The luminance coefficients used by the luma channel.
    #[must_use]
    pub const fn luminance_coefficients(&self) -> [f32; 3] {
        self.luminance
    }

    /// Reads the authored channel out of a frame pixel.
    #[must_use]
    pub fn source_value(&self, pixel: [f32; 4]) -> f32 {
        match self.channel {
            ExtractChannel::Luma => {
                pixel[0] * self.luminance[0]
                    + pixel[1] * self.luminance[1]
                    + pixel[2] * self.luminance[2]
            }
            ExtractChannel::Red => pixel[0],
            ExtractChannel::Green => pixel[1],
            ExtractChannel::Blue => pixel[2],
            ExtractChannel::Alpha => pixel[3],
        }
    }

    /// Maps a source value into the matte.
    #[must_use]
    pub fn matte(&self, value: f32) -> f32 {
        let lower = if self.black_softness > CHANNEL_EPSILON {
            smoothstep(
                (value - (self.black_point - self.black_softness)) / (2.0 * self.black_softness),
            )
        } else if value >= self.black_point {
            1.0
        } else {
            0.0
        };
        let upper = if self.white_softness > CHANNEL_EPSILON {
            1.0 - smoothstep(
                (value - (self.white_point - self.white_softness)) / (2.0 * self.white_softness),
            )
        } else if value <= self.white_point {
            1.0
        } else {
            0.0
        };
        let matte = (lower * upper).clamp(0.0, 1.0);
        if self.invert {
            1.0 - matte
        } else {
            matte
        }
    }

    /// Applies the effect to one pixel.
    ///
    /// A pixel without coverage is left untouched: the compositor short circuits
    /// zero coverage, so writing a matte into it would make the CPU and the GPU
    /// disagree about a pixel that is never visible.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let matte = self.matte(self.source_value(pixel));
        [matte, matte, matte, pixel[3]]
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [channel, black, white, invert]`,
    /// `color = [luminance_rgb, 0]`,
    /// `extra0 = [black_softness, white_softness, 0, 0]`.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.channel.as_u32() as f32,
                self.black_point,
                self.white_point,
                if self.invert { 1.0 } else { 0.0 },
            ],
            [self.luminance[0], self.luminance[1], self.luminance[2], 0.0],
            [self.black_softness, self.white_softness, 0.0, 0.0],
        )
    }
}

fn smoothstep(ratio: f32) -> f32 {
    let clamped = ratio.clamp(0.0, 1.0);
    clamped * clamped * (3.0 - 2.0 * clamped)
}

/// Applies the effect to a whole frame.
pub fn extract_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ExtractGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_extract_f32_controlled(pixels, width, height, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_extract_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ExtractGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() != expected {
        return Ok(false);
    }
    if grade.channel == ExtractChannel::Luma
        && grade.luminance.iter().all(|coefficient| *coefficient == 0.0)
    {
        return Ok(false);
    }
    for (index, pixel) in pixels.iter_mut().enumerate() {
        if index % CHECKPOINT_STRIDE == 0 {
            checkpoint()?;
        }
        *pixel = grade.apply(*pixel);
    }
    Ok(true)
}

const CHECKPOINT_STRIDE: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    const WORKING: [f32; 3] = [0.2126, 0.7152, 0.0722];

    fn grade(channel: ExtractChannel) -> ExtractGrade {
        ExtractGrade::new(channel, 0.0, 1.0, 0.0, 0.0, false, WORKING).expect("grade")
    }

    #[test]
    fn levels_outside_the_declared_range_are_rejected() {
        assert_eq!(
            ExtractGrade::new(ExtractChannel::Red, -0.1, 1.0, 0.0, 0.0, false, WORKING),
            Err(ExtractError::Level)
        );
        assert_eq!(
            ExtractGrade::new(ExtractChannel::Red, 0.0, 1.5, 0.0, 0.0, false, WORKING),
            Err(ExtractError::Level)
        );
        assert_eq!(
            ExtractGrade::new(ExtractChannel::Red, 0.0, 1.0, 0.0, f32::NAN, false, WORKING),
            Err(ExtractError::Level)
        );
    }

    #[test]
    fn a_black_point_at_or_above_the_white_point_is_rejected() {
        assert_eq!(
            ExtractGrade::new(ExtractChannel::Red, 0.6, 0.6, 0.0, 0.0, false, WORKING),
            Err(ExtractError::Range)
        );
        assert_eq!(
            ExtractGrade::new(ExtractChannel::Red, 0.8, 0.2, 0.0, 0.0, false, WORKING),
            Err(ExtractError::Range)
        );
    }

    #[test]
    fn non_finite_luminance_is_rejected() {
        assert_eq!(
            ExtractGrade::new(
                ExtractChannel::Luma,
                0.0,
                1.0,
                0.0,
                0.0,
                false,
                [f32::INFINITY, 0.0, 0.0]
            ),
            Err(ExtractError::Luminance)
        );
    }

    #[test]
    fn channel_menu_numbering_matches_the_list() {
        for channel in ExtractChannel::ALL {
            assert_eq!(ExtractChannel::from_menu(channel.as_menu()), Some(channel));
            assert_eq!(ExtractChannel::from_key(channel.as_key()), Some(channel));
        }
        assert_eq!(ExtractChannel::from_menu(0), None);
        assert_eq!(ExtractChannel::from_menu(6), None);
    }

    #[test]
    fn the_red_channel_reads_the_red_component() {
        let pixel = [0.25, 0.5, 0.75, 0.4];
        assert_eq!(grade(ExtractChannel::Red).source_value(pixel), 0.25);
        assert_eq!(grade(ExtractChannel::Green).source_value(pixel), 0.5);
        assert_eq!(grade(ExtractChannel::Blue).source_value(pixel), 0.75);
        assert_eq!(grade(ExtractChannel::Alpha).source_value(pixel), 0.4);
    }

    #[test]
    fn the_luma_channel_uses_the_working_coefficients() {
        let pixel = [0.6, 0.3, 0.1, 1.0];
        let expected = 0.6 * 0.2126 + 0.3 * 0.7152 + 0.1 * 0.0722;
        let value = grade(ExtractChannel::Luma).source_value(pixel);
        assert!((value - expected).abs() <= 1.0e-6, "{value} != {expected}");
    }

    #[test]
    fn a_hard_range_passes_the_inside_and_clears_the_outside() {
        let grade = ExtractGrade::new(ExtractChannel::Red, 0.2, 0.8, 0.0, 0.0, false, WORKING)
            .expect("grade");
        assert_eq!(grade.matte(0.2), 1.0);
        assert_eq!(grade.matte(0.5), 1.0);
        assert_eq!(grade.matte(0.8), 1.0);
        assert_eq!(grade.matte(0.19), 0.0);
        assert_eq!(grade.matte(0.81), 0.0);
    }

    #[test]
    fn the_reported_black_and_white_points_survive_the_range_check() {
        let grade = ExtractGrade::new(ExtractChannel::Blue, 0.1, 0.9, 0.05, 0.15, true, WORKING)
            .expect("grade");
        assert_eq!(grade.black_point(), 0.1);
        assert_eq!(grade.white_point(), 0.9);
        assert_eq!(grade.black_softness(), 0.05);
        assert_eq!(grade.white_softness(), 0.15);
        assert!(grade.inverted());
    }

    #[test]
    fn softness_ramps_the_matte_across_the_points() {
        let grade = ExtractGrade::new(ExtractChannel::Red, 0.4, 0.7, 0.1, 0.1, false, WORKING)
            .expect("grade");
        assert!(
            (grade.matte(0.4) - 0.5).abs() <= 1.0e-6,
            "{}",
            grade.matte(0.4)
        );
        assert!(grade.matte(0.3) < 0.1);
        assert!(grade.matte(0.5) > 0.9);
        assert!(
            (grade.matte(0.7) - 0.5).abs() <= 1.0e-6,
            "{}",
            grade.matte(0.7)
        );
        assert!(grade.matte(0.8) < 0.1);
    }

    #[test]
    fn the_invert_toggle_flips_the_matte() {
        let forward = ExtractGrade::new(ExtractChannel::Red, 0.2, 0.8, 0.0, 0.0, false, WORKING)
            .expect("grade");
        let inverted = ExtractGrade::new(ExtractChannel::Red, 0.2, 0.8, 0.0, 0.0, true, WORKING)
            .expect("grade");
        for value in [0.0_f32, 0.1, 0.3, 0.5, 0.9, 1.4] {
            assert!((forward.matte(value) + inverted.matte(value) - 1.0).abs() <= 1.0e-6);
        }
    }

    #[test]
    fn the_result_replaces_the_colour_and_keeps_alpha() {
        let grade = ExtractGrade::new(ExtractChannel::Red, 0.0, 0.5, 0.0, 0.0, false, WORKING)
            .expect("grade");
        let pixel = grade.apply([0.25, 0.9, 0.1, 0.75]);
        assert_eq!(pixel, [1.0, 1.0, 1.0, 0.75]);
        let outside = grade.apply([0.9, 0.0, 0.0, 0.25]);
        assert_eq!(outside, [0.0, 0.0, 0.0, 0.25]);
    }

    #[test]
    fn a_pixel_without_coverage_is_left_untouched() {
        let grade = ExtractGrade::new(ExtractChannel::Red, 0.5, 0.9, 0.0, 0.0, false, WORKING)
            .expect("grade");
        assert_eq!(grade.apply([0.9, 2.0, -1.0, 0.0]), [0.9, 2.0, -1.0, 0.0]);
        assert_eq!(grade.apply([0.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn extended_range_values_are_not_clamped_into_the_range() {
        let grade = ExtractGrade::new(ExtractChannel::Red, 0.4, 0.6, 0.0, 0.0, false, WORKING)
            .expect("grade");
        // A highlight above one still falls outside the white point.
        assert_eq!(grade.matte(2.5), 0.0);
        // A negative sample falls outside the black point.
        assert_eq!(grade.matte(-0.5), 0.0);
    }

    #[test]
    fn the_gpu_controls_carry_the_levels_the_channel_and_the_coefficients() {
        let grade = ExtractGrade::new(ExtractChannel::Blue, 0.2, 0.7, 0.1, 0.2, true, WORKING)
            .expect("grade");
        let (params, color, extra) = grade.gpu_controls();
        assert_eq!(params, [3.0, 0.2, 0.7, 1.0]);
        assert_eq!(color, [0.2126, 0.7152, 0.0722, 0.0]);
        assert_eq!(extra, [0.1, 0.2, 0.0, 0.0]);
    }

    #[test]
    fn a_geometry_mismatch_is_reported_as_not_applied() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 4];
        assert!(!extract_frame_f32(
            &mut pixels,
            3,
            2,
            grade(ExtractChannel::Red)
        ));
        assert!(!extract_frame_f32(
            &mut pixels,
            0,
            2,
            grade(ExtractChannel::Red)
        ));
    }

    #[test]
    fn the_whole_frame_entry_matches_the_point_kernel() {
        let grade = ExtractGrade::new(ExtractChannel::Luma, 0.1, 0.8, 0.05, 0.1, false, WORKING)
            .expect("grade");
        let mut pixels = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.5, 0.25, 0.75, 0.5],
            [1.5, -0.25, 0.5, 0.0],
            [0.9, 0.9, 0.9, 1.0],
        ];
        let expected: Vec<[f32; 4]> = pixels.iter().map(|pixel| grade.apply(*pixel)).collect();
        assert!(extract_frame_f32(&mut pixels, 2, 2, grade));
        assert_eq!(pixels, expected);
    }

    #[test]
    fn the_geometry_checkpoints_the_execution() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 4];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(apply_extract_f32_controlled(
            &mut pixels,
            2,
            2,
            grade(ExtractChannel::Red),
            &mut checkpoint
        )
        .expect("applied"));
        assert_eq!(calls, 1);
    }
}
