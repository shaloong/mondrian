//! The colour offset effect from the built-in effect list.
//!
//! The list authors a phase for the red, green and blue channel plus an overflow
//! mode. The phase is expressed in pixels; the meaning of a positive phase and
//! the exact overflow rules are not specified by the list, so this module fixes
//! them the same way the offset effect does:
//!
//! * a positive phase moves the channel content to the right, which means the
//!   sample is taken at `column - phase`;
//! * a fractional phase is resolved with bilinear interpolation across the two
//!   neighbouring texels of the same row;
//! * the overflow mode decides what happens outside the frame: `wrap` repeats
//!   the row, `clamp` repeats the edge texel and `mirror` reflects it;
//! * alpha is left alone and no value is clamped, so extended range and negative
//!   scene linear samples keep their sign.

/// Largest authored phase magnitude in pixels.
pub const COLOR_OFFSET_MAX_PHASE_PIXELS: f32 = 1_000_000.0;

/// Number of declared overflow options.
pub const COLOR_OFFSET_OVERFLOW_COUNT: usize = 3;

const PHASE_EPSILON: f32 = 1.0e-6;

/// How a channel sample outside the frame is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorOffsetOverflow {
    /// Repeat the row, wrapping the sample around the frame.
    Wrap,
    /// Repeat the edge texel.
    Clamp,
    /// Reflect the row about its edges.
    Mirror,
}

impl ColorOffsetOverflow {
    /// Every mode in menu order.
    pub const ALL: [Self; COLOR_OFFSET_OVERFLOW_COUNT] = [Self::Wrap, Self::Clamp, Self::Mirror];

    /// The stable authoring key.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Wrap => "wrap",
            Self::Clamp => "clamp",
            Self::Mirror => "mirror",
        }
    }

    /// Resolves an authoring key.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }

    /// The list menu numbering, starting at one.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Wrap => 1,
            Self::Clamp => 2,
            Self::Mirror => 3,
        }
    }

    /// Resolves the list menu numbering.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Wrap),
            2 => Some(Self::Clamp),
            3 => Some(Self::Mirror),
            _ => None,
        }
    }

    /// The value carried into the shader.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Wrap => 1,
            Self::Clamp => 2,
            Self::Mirror => 3,
        }
    }
}

/// Why a grade could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorOffsetError {
    /// One of the phases was not a finite number inside the authored range.
    Phase,
}

impl std::fmt::Display for ColorOffsetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("colour offset phases must be finite and within a million pixels")
    }
}

impl std::error::Error for ColorOffsetError {}

/// The resolved state of the colour offset effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorOffsetGrade {
    phases: [f32; 3],
    overflow: ColorOffsetOverflow,
}

impl ColorOffsetGrade {
    /// Builds a grade.
    ///
    /// # Errors
    ///
    /// Returns [`ColorOffsetError::Phase`] when a phase is not finite or exceeds
    /// the authored range.
    pub fn new(phases: [f32; 3], overflow: ColorOffsetOverflow) -> Result<Self, ColorOffsetError> {
        for phase in phases {
            if !phase.is_finite() || phase.abs() > COLOR_OFFSET_MAX_PHASE_PIXELS {
                return Err(ColorOffsetError::Phase);
            }
        }
        Ok(Self { phases, overflow })
    }

    /// The authored phases.
    #[must_use]
    pub const fn phases(&self) -> [f32; 3] {
        self.phases
    }

    /// The overflow mode.
    #[must_use]
    pub const fn overflow(&self) -> ColorOffsetOverflow {
        self.overflow
    }

    /// Whether the effect leaves the frame untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.phases.iter().all(|phase| phase.abs() <= PHASE_EPSILON)
    }

    /// The largest authored displacement, rounded up, used as a halo.
    #[must_use]
    pub fn halo_pixels(&self) -> u32 {
        self.phases.iter().fold(0.0_f32, |widest, phase| widest.max(phase.abs())).ceil() as u32
    }

    /// Applies the effect to a whole frame.
    pub fn apply(&self, pixels: &mut [[f32; 4]], width: usize, height: usize) -> bool {
        let Some(expected) = width.checked_mul(height) else {
            return false;
        };
        if width == 0 || height == 0 || pixels.len() != expected {
            return false;
        }
        let source = pixels.to_vec();
        let phases = self.phases.map(f64::from);
        crate::parallel::for_each_row_band(pixels, width, 0..height, |first_row, band| {
            for (offset, row) in band.chunks_mut(width).enumerate() {
                let row_index = first_row + offset;
                for (column, pixel) in row.iter_mut().enumerate() {
                    let original = source[row_index * width + column];
                    // Fully transparent samples stay untouched, matching the compositor
                    // short circuit and the renderer pass.
                    if original[3] <= 0.0 {
                        *pixel = original;
                        continue;
                    }
                    let mut result = [0.0_f32; 4];
                    for (channel, phase) in phases.iter().enumerate() {
                        result[channel] = sample_channel(
                            &source,
                            width,
                            height,
                            column as f64 - phase,
                            row_index,
                            channel,
                            self.overflow,
                        );
                    }
                    result[3] = source[row_index * width + column][3];
                    *pixel = result;
                }
            }
        });
        true
    }

    /// The uniform payload for the spatial shader.
    ///
    /// `params = [red_phase, green_phase, blue_phase, overflow]`, `extra` unused.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.phases[0],
                self.phases[1],
                self.phases[2],
                self.overflow.as_u32() as f32,
            ],
            [0.0; 4],
        )
    }
}

fn wrap_index(value: i64, width: usize) -> usize {
    let extent = width as i64;
    value.rem_euclid(extent).min(extent - 1) as usize
}

fn mirror_index(value: i64, width: usize) -> usize {
    let extent = width as i64;
    if extent <= 1 {
        return 0;
    }
    let period = 2 * (extent - 1);
    let folded = value.rem_euclid(period);
    if folded < extent {
        folded as usize
    } else {
        (period - folded) as usize
    }
}

fn clamped_index(value: f64, width: usize) -> usize {
    value.clamp(0.0, (width - 1) as f64).round() as usize
}

fn overflow_index(value: f64, width: usize, overflow: ColorOffsetOverflow) -> usize {
    match overflow {
        ColorOffsetOverflow::Wrap => wrap_index(value.floor() as i64, width),
        ColorOffsetOverflow::Clamp => clamped_index(value, width),
        ColorOffsetOverflow::Mirror => mirror_index(value.floor() as i64, width),
    }
}

fn sample_channel(
    source: &[[f32; 4]],
    width: usize,
    _height: usize,
    column: f64,
    row: usize,
    channel: usize,
    overflow: ColorOffsetOverflow,
) -> f32 {
    let left = column.floor();
    let fraction = column - left;
    let first = overflow_index(left, width, overflow);
    let second = overflow_index(left + 1.0, width, overflow);
    let base = row * width;
    let low = source[base + first][channel];
    let high = source[base + second][channel];
    (low as f64 + (high as f64 - low as f64) * fraction) as f32
}

/// Applies the effect to a whole frame, honouring a cancellation checkpoint.
pub(crate) fn apply_color_offset_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ColorOffsetGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    checkpoint()?;
    let applied = grade.apply(pixels, width, height);
    checkpoint()?;
    Ok(applied)
}

/// Applies the effect to a whole frame.
pub fn color_offset_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: ColorOffsetGrade,
) -> bool {
    grade.apply(pixels, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(width: usize, height: usize) -> Vec<[f32; 4]> {
        let mut pixels = vec![[0.0_f32; 4]; width * height];
        for row in 0..height {
            for column in 0..width {
                let value = column as f32 / 4.0;
                pixels[row * width + column] = [value, value, value, 1.0];
            }
        }
        pixels
    }

    fn grade(phases: [f32; 3], overflow: ColorOffsetOverflow) -> ColorOffsetGrade {
        ColorOffsetGrade::new(phases, overflow).expect("grade")
    }

    #[test]
    fn phases_outside_the_authored_range_are_rejected() {
        assert_eq!(
            ColorOffsetGrade::new([f32::INFINITY, 0.0, 0.0], ColorOffsetOverflow::Clamp),
            Err(ColorOffsetError::Phase)
        );
        assert_eq!(
            ColorOffsetGrade::new(
                [COLOR_OFFSET_MAX_PHASE_PIXELS + 1.0, 0.0, 0.0],
                ColorOffsetOverflow::Clamp
            ),
            Err(ColorOffsetError::Phase)
        );
    }

    #[test]
    fn the_overflow_menu_matches_the_list() {
        for mode in ColorOffsetOverflow::ALL {
            assert_eq!(ColorOffsetOverflow::from_menu(mode.as_menu()), Some(mode));
            assert_eq!(ColorOffsetOverflow::from_key(mode.as_key()), Some(mode));
        }
        assert_eq!(ColorOffsetOverflow::from_menu(0), None);
        assert_eq!(ColorOffsetOverflow::from_menu(4), None);
    }

    #[test]
    fn zero_phases_leave_the_frame_untouched() {
        let mut pixels = ramp(8, 2);
        let original = pixels.clone();
        let grade = grade([0.0, 0.0, 0.0], ColorOffsetOverflow::Clamp);
        assert!(grade.is_identity());
        assert!(grade.apply(&mut pixels, 8, 2));
        assert_eq!(pixels, original);
    }

    #[test]
    fn a_positive_integer_phase_shifts_the_content_to_the_right() {
        let mut pixels = ramp(6, 1);
        let original = pixels.clone();
        let grade = grade([1.0, 0.0, 0.0], ColorOffsetOverflow::Clamp);
        assert!(grade.apply(&mut pixels, 6, 1));
        // Red reads the texel to its left, the other channels stay put.
        assert_eq!(pixels[2][0], original[1][0]);
        assert_eq!(pixels[0][0], original[0][0]);
        assert_eq!(pixels[2][1], original[2][1]);
        assert_eq!(pixels[2][2], original[2][2]);
    }

    #[test]
    fn the_channels_move_independently() {
        let mut pixels = ramp(6, 1);
        let original = pixels.clone();
        let grade = grade([1.0, 0.0, -1.0], ColorOffsetOverflow::Clamp);
        assert!(grade.apply(&mut pixels, 6, 1));
        assert_eq!(pixels[3][0], original[2][0]);
        assert_eq!(pixels[3][1], original[3][1]);
        assert_eq!(pixels[3][2], original[4][2]);
    }

    #[test]
    fn a_fractional_phase_interpolates_between_neighbours() {
        let mut pixels = ramp(4, 1);
        let original = pixels.clone();
        let grade = grade([0.5, 0.0, 0.0], ColorOffsetOverflow::Clamp);
        assert!(grade.apply(&mut pixels, 4, 1));
        let expected = (original[1][0] as f64 + original[2][0] as f64) * 0.5;
        assert!((pixels[2][0] as f64 - expected).abs() <= 1.0e-6);
    }

    #[test]
    fn the_wrap_mode_repeats_the_row() {
        let mut pixels = ramp(4, 1);
        let original = pixels.clone();
        let grade = grade([-1.0, 0.0, 0.0], ColorOffsetOverflow::Wrap);
        assert!(grade.apply(&mut pixels, 4, 1));
        assert_eq!(pixels[0][0], original[1][0]);
        assert_eq!(pixels[3][0], original[0][0]);
    }

    #[test]
    fn the_clamp_mode_repeats_the_edge_texel() {
        let mut pixels = ramp(4, 1);
        let original = pixels.clone();
        let grade = grade([-1.0, 0.0, 0.0], ColorOffsetOverflow::Clamp);
        assert!(grade.apply(&mut pixels, 4, 1));
        assert_eq!(pixels[3][0], original[3][0]);
    }

    #[test]
    fn the_mirror_mode_reflects_the_row() {
        let mut pixels = ramp(4, 1);
        let original = pixels.clone();
        let grade = grade([-1.0, 0.0, 0.0], ColorOffsetOverflow::Mirror);
        assert!(grade.apply(&mut pixels, 4, 1));
        assert_eq!(pixels[3][0], original[2][0]);
    }

    #[test]
    fn alpha_is_never_moved_or_scaled() {
        let mut pixels = vec![[0.2_f32, 0.4, 0.6, 0.25], [0.5, 0.5, 0.5, 0.75]];
        let grade = grade([3.0, -2.0, 0.5], ColorOffsetOverflow::Wrap);
        assert!(grade.apply(&mut pixels, 2, 1));
        assert_eq!(pixels[0][3], 0.25);
        assert_eq!(pixels[1][3], 0.75);
    }

    #[test]
    fn extended_range_and_negative_samples_are_not_clamped() {
        let mut pixels = vec![[2.5_f32, -0.75, 1.25, 0.5], [0.0, 0.0, 0.0, 1.0]];
        let grade = grade([0.0, 0.0, 0.0], ColorOffsetOverflow::Clamp);
        assert!(grade.apply(&mut pixels, 2, 1));
        assert_eq!(pixels[0], [2.5, -0.75, 1.25, 0.5]);
    }

    #[test]
    fn a_geometry_mismatch_is_reported_as_not_applied() {
        let mut pixels = vec![[0.5_f32; 4]; 4];
        let grade = grade([1.0, 0.0, 0.0], ColorOffsetOverflow::Clamp);
        assert!(!grade.apply(&mut pixels, 3, 2));
        assert!(!color_offset_frame_f32(&mut pixels, 0, 2, grade));
    }

    #[test]
    fn rows_are_independent_and_repeatable() {
        let grade = grade([1.5, -0.5, 2.0], ColorOffsetOverflow::Mirror);
        let mut first = ramp(7, 3);
        let mut second = first.clone();
        assert!(grade.apply(&mut first, 7, 3));
        assert!(grade.apply(&mut second, 7, 3));
        assert_eq!(first, second);
        // The same displacement applies on every row.
        assert_eq!(first[0][0], first[7][0]);
        assert_eq!(first[0][0], first[14][0]);
    }

    #[test]
    fn the_gpu_controls_carry_the_phases_and_the_overflow() {
        let grade = grade([1.5, -2.0, 0.25], ColorOffsetOverflow::Mirror);
        assert_eq!(grade.gpu_controls(), ([1.5, -2.0, 0.25, 3.0], [0.0; 4]));
        assert_eq!(grade.halo_pixels(), 2);
    }
}
