//! The levels effect from the built-in effect list.
//!
//! The list authors eight controls: a channel menu, an input black and an input
//! white point, a gamma, an output black and an output white point and one
//! clipping menu for each end of the output range. Every control is a point
//! operation, so the effect runs on the fused GPU point path as well as the CPU
//! kernel.
//!
//! # Documented reading
//!
//! The list names the controls but does not define the arithmetic, so this
//! module fixes one reading and documents it here.
//!
//! - The channel menu selects what is remapped. `rgb` remaps the red, green and
//!   blue channels independently with the same numbers, `red`, `green` and
//!   `blue` remap only that channel, and `alpha` remaps only the alpha channel.
//!   Every other channel is returned unchanged.
//! - The input points normalise the sample:
//!   `t = (value - input_black) / (input_white - input_black)`. When the two
//!   points are equal the mapping degenerates into the hard step
//!   `t = value <= input_black ? 0 : 1`, so no sample is ever divided by zero
//!   and no NaN is produced.
//! - Gamma shapes the non-negative part: `t = max(t, 0) ^ (1 / gamma)`. A gamma
//!   above one therefore brightens, which is the direction the display encoded
//!   effect this list mirrors uses. Gamma must be above zero and at most five.
//!   Negative normalised samples are held at zero instead of being raised to a
//!   fractional power.
//! - The output points map the shaped sample back:
//!   `out = output_black + t * (output_white - output_black)`.
//!
//! The two clipping menus are the list's unlabelled `1..3` menus, read here as:
//!
//! - `off`: no clipping. Extended-range samples keep their sign and magnitude.
//! - `clamp_output`: clamp into the output range, i.e. into
//!   `min(output_black, output_white)..max(output_black, output_white)`.
//! - `clamp_display`: clamp into the display range `[0, 1]` after the output
//!   mapping.
//!
//! `clip_output_black` clips the low end and `clip_output_white` clips the high
//! end, so two `clamp_display` menus leave the result inside `[0, 1]`.
//!
//! # Domain
//!
//! The working frame is scene linear and the points are working-domain numbers:
//! no sample is re-encoded. The gamma stage holds sub-zero normalised samples at
//! zero, so the neutral points declared the identity map a negative
//! extended-range sample to zero rather than leaving it alone. In every colour
//! mode a pixel without coverage is left completely untouched because the
//! compositor short circuits zero coverage, so writing into one would make the
//! CPU and the GPU disagree about a pixel that is never visible. The alpha mode
//! is the exception: it exists to author coverage, so it processes every pixel,
//! including one without coverage, and the GPU compositor lets an alpha mode
//! grade past its zero-coverage short circuit for the same reason.

/// Highest authored input or output point (`-10000` to `10000`).
pub const LEVELS_MAX_LEVEL: f32 = 10_000.0;

/// Lowest authored input or output point.
pub const LEVELS_MIN_LEVEL: f32 = -10_000.0;

/// Highest authored gamma (`0` to `5`; zero itself is rejected).
pub const LEVELS_MAX_GAMMA: f32 = 5.0;

/// Lowest authored gamma, exclusive.
pub const LEVELS_MIN_GAMMA: f32 = 0.0;

/// Number of channel menu options the list declares.
pub const LEVELS_CHANNEL_COUNT: usize = 5;

/// Number of clipping menu options the list declares.
pub const LEVELS_CLIP_COUNT: usize = 3;

/// Channel menu of the levels effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelsChannel {
    /// Remap red, green and blue independently with the same numbers.
    Rgb,
    /// Remap only the red channel.
    Red,
    /// Remap only the green channel.
    Green,
    /// Remap only the blue channel.
    Blue,
    /// Remap only the alpha channel.
    Alpha,
}

impl LevelsChannel {
    /// Every channel in menu order.
    pub const ALL: [Self; LEVELS_CHANNEL_COUNT] =
        [Self::Rgb, Self::Red, Self::Green, Self::Blue, Self::Alpha];

    /// The key the effect list stores for this channel.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Alpha => "alpha",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|channel| channel.as_key() == key)
    }

    /// The one-based menu number the list declares for this channel.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Rgb => 1,
            Self::Red => 2,
            Self::Green => 3,
            Self::Blue => 4,
            Self::Alpha => 5,
        }
    }

    /// Resolves a one-based menu number, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Rgb),
            2 => Some(Self::Red),
            3 => Some(Self::Green),
            4 => Some(Self::Blue),
            5 => Some(Self::Alpha),
            _ => None,
        }
    }

    /// Which of red, green and blue this channel remaps.
    const fn colour_mask(self) -> [bool; 3] {
        match self {
            Self::Rgb => [true, true, true],
            Self::Red => [true, false, false],
            Self::Green => [false, true, false],
            Self::Blue => [false, false, true],
            Self::Alpha => [false, false, false],
        }
    }
}

/// Clipping menu of the levels effect, applied to one end of the output range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelsClip {
    /// No clipping: extended-range samples are left alone.
    Off,
    /// Clamp into the output range.
    ClampOutput,
    /// Clamp into the display range `[0, 1]`.
    ClampDisplay,
}

impl LevelsClip {
    /// Every clipping mode in menu order.
    pub const ALL: [Self; LEVELS_CLIP_COUNT] = [Self::Off, Self::ClampOutput, Self::ClampDisplay];

    /// The key the effect list stores for this clipping mode.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ClampOutput => "clamp_output",
            Self::ClampDisplay => "clamp_display",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|clip| clip.as_key() == key)
    }

    /// The one-based menu number the list declares for this clipping mode.
    #[must_use]
    pub const fn as_menu(self) -> i64 {
        match self {
            Self::Off => 1,
            Self::ClampOutput => 2,
            Self::ClampDisplay => 3,
        }
    }

    /// Resolves a one-based menu number, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: i64) -> Option<Self> {
        match menu {
            1 => Some(Self::Off),
            2 => Some(Self::ClampOutput),
            3 => Some(Self::ClampDisplay),
            _ => None,
        }
    }

    /// The lower bound this mode imposes, or `None` without clipping.
    fn low_bound(self, output_black: f32, output_white: f32) -> Option<f32> {
        match self {
            Self::Off => None,
            Self::ClampOutput => Some(output_black.min(output_white)),
            Self::ClampDisplay => Some(0.0),
        }
    }

    /// The upper bound this mode imposes, or `None` without clipping.
    fn high_bound(self, output_black: f32, output_white: f32) -> Option<f32> {
        match self {
            Self::Off => None,
            Self::ClampOutput => Some(output_black.max(output_white)),
            Self::ClampDisplay => Some(1.0),
        }
    }
}

/// Authoring value rejected by [`LevelsGrade::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LevelsError {
    /// The input black point was not finite or fell outside `-10000..=10000`.
    InputBlack,
    /// The input white point was not finite or fell outside `-10000..=10000`.
    InputWhite,
    /// Gamma was not finite, was not above zero or exceeded five.
    Gamma,
    /// The output black point was not finite or fell outside `-10000..=10000`.
    OutputBlack,
    /// The output white point was not finite or fell outside `-10000..=10000`.
    OutputWhite,
}

impl std::fmt::Display for LevelsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InputBlack => {
                formatter.write_str("levels input black must be finite and within -10000 to 10000")
            }
            Self::InputWhite => {
                formatter.write_str("levels input white must be finite and within -10000 to 10000")
            }
            Self::Gamma => {
                formatter.write_str("levels gamma must be finite, above 0 and at most 5")
            }
            Self::OutputBlack => {
                formatter.write_str("levels output black must be finite and within -10000 to 10000")
            }
            Self::OutputWhite => {
                formatter.write_str("levels output white must be finite and within -10000 to 10000")
            }
        }
    }
}

impl std::error::Error for LevelsError {}

/// Levels authoring resolved into the mapping the kernel applies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelsGrade {
    input_black: f32,
    input_white: f32,
    gamma: f32,
    output_black: f32,
    output_white: f32,
    channel: LevelsChannel,
    clip_output_black: LevelsClip,
    clip_output_white: LevelsClip,
}

impl LevelsGrade {
    /// Resolves the authored controls into the input, gamma and output mapping.
    ///
    /// # Errors
    ///
    /// One of the level variants when a point is not finite or falls outside
    /// `-10000..=10000`, and [`LevelsError::Gamma`] when gamma is not finite, is
    /// zero or below, or exceeds five.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_black: f32,
        input_white: f32,
        gamma: f32,
        output_black: f32,
        output_white: f32,
        channel: LevelsChannel,
        clip_output_black: LevelsClip,
        clip_output_white: LevelsClip,
    ) -> Result<Self, LevelsError> {
        for (point, error) in [
            (input_black, LevelsError::InputBlack),
            (input_white, LevelsError::InputWhite),
            (output_black, LevelsError::OutputBlack),
            (output_white, LevelsError::OutputWhite),
        ] {
            if !point.is_finite() || !(LEVELS_MIN_LEVEL..=LEVELS_MAX_LEVEL).contains(&point) {
                return Err(error);
            }
        }
        if !gamma.is_finite() || !(LEVELS_MIN_GAMMA < gamma && gamma <= LEVELS_MAX_GAMMA) {
            return Err(LevelsError::Gamma);
        }
        Ok(Self {
            input_black,
            input_white,
            gamma,
            output_black,
            output_white,
            channel,
            clip_output_black,
            clip_output_white,
        })
    }

    /// The authored input black point.
    #[must_use]
    pub const fn input_black(&self) -> f32 {
        self.input_black
    }

    /// The authored input white point.
    #[must_use]
    pub const fn input_white(&self) -> f32 {
        self.input_white
    }

    /// The authored gamma.
    #[must_use]
    pub const fn gamma(&self) -> f32 {
        self.gamma
    }

    /// The authored output black point.
    #[must_use]
    pub const fn output_black(&self) -> f32 {
        self.output_black
    }

    /// The authored output white point.
    #[must_use]
    pub const fn output_white(&self) -> f32 {
        self.output_white
    }

    /// The remapped channel.
    #[must_use]
    pub const fn channel(&self) -> LevelsChannel {
        self.channel
    }

    /// The clipping mode applied at the low end of the output.
    #[must_use]
    pub const fn clip_output_black(&self) -> LevelsClip {
        self.clip_output_black
    }

    /// The clipping mode applied at the high end of the output.
    #[must_use]
    pub const fn clip_output_white(&self) -> LevelsClip {
        self.clip_output_white
    }

    /// Whether the authored state is the declared neutral grade.
    ///
    /// The graph builder elides the operation for this state. The channel does
    /// not enter the answer: with the neutral points and no clipping, every
    /// channel returns its sample unchanged. The non-negative clamp of the gamma
    /// stage does not enter it either: a sample below the input black point is
    /// held at zero even at the neutral points, and a negative sample is not
    /// authored scene content.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.input_black == 0.0
            && self.input_white == 1.0
            && self.gamma == 1.0
            && self.output_black == 0.0
            && self.output_white == 1.0
            && self.clip_output_black == LevelsClip::Off
            && self.clip_output_white == LevelsClip::Off
    }

    /// Maps one sample through the input points, the gamma and the output
    /// points, then clips it as authored.
    #[must_use]
    pub fn transform(&self, value: f32) -> f32 {
        let span = self.input_white - self.input_black;
        let normalized = if span == 0.0 {
            if value <= self.input_black {
                0.0
            } else {
                1.0
            }
        } else {
            (value - self.input_black) / span
        };
        let shaped = normalized.max(0.0).powf(1.0 / self.gamma);
        let mut mapped = self.output_black + shaped * (self.output_white - self.output_black);
        if let Some(low) = self.clip_output_black.low_bound(self.output_black, self.output_white) {
            mapped = mapped.max(low);
        }
        if let Some(high) = self.clip_output_white.high_bound(self.output_black, self.output_white)
        {
            mapped = mapped.min(high);
        }
        mapped
    }

    /// Applies the effect to one pixel.
    ///
    /// In every colour mode a pixel without coverage is left untouched: the
    /// compositor short circuits zero coverage, so mapping one would make the
    /// CPU and the GPU disagree about a pixel that is never visible. The alpha
    /// mode is the exception and processes every pixel, because authoring
    /// coverage is the point of it.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if self.channel == LevelsChannel::Alpha {
            return [pixel[0], pixel[1], pixel[2], self.transform(pixel[3])];
        }
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let mut mapped = pixel;
        for (index, remapped) in self.channel.colour_mask().into_iter().enumerate() {
            if remapped {
                mapped[index] = self.transform(pixel[index]);
            }
        }
        mapped
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [input_black, input_white, gamma, output_black]` and
    /// `extra = [output_white, channel, clip_output_black, clip_output_white]`,
    /// with both menus as their one-based numbers.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.input_black,
                self.input_white,
                self.gamma,
                self.output_black,
            ],
            [
                self.output_white,
                self.channel.as_menu() as f32,
                self.clip_output_black.as_menu() as f32,
                self.clip_output_white.as_menu() as f32,
            ],
        )
    }
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether the grade ran. The neutral points are elided by the graph
/// builder, but the kernel still applies the documented mapping: the pre-gamma
/// clamp means the neutral grade is not a no-op for a negative extended-range
/// sample, and the GPU pass implements the same mapping.
pub fn levels_frame_f32(pixels: &mut [[f32; 4]], grade: &LevelsGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_levels_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_levels_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &LevelsGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // The neutral grade is elided by the graph builder, so no authored graph
    // reaches this mapping. It is still applied here rather than short circuited
    // because the pre-gamma clamp makes it change a negative extended-range
    // sample, and the GPU pass has to agree with the CPU kernel pixel for pixel.
    // Point grades are bandwidth bound: measured on a 1280x720 frame they run at
    // 300-890 MP/s, i.e. already at the single-thread DRAM stream rate, so
    // splitting the frame across cores buys nothing. The 4096-pixel chunks exist
    // purely for cancellation checkpoints.
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply(*pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

const CHECKPOINT_STRIDE: usize = 4_096;

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(
        input_black: f32,
        input_white: f32,
        gamma: f32,
        output_black: f32,
        output_white: f32,
    ) -> LevelsGrade {
        LevelsGrade::new(
            input_black,
            input_white,
            gamma,
            output_black,
            output_white,
            LevelsChannel::Rgb,
            LevelsClip::Off,
            LevelsClip::Off,
        )
        .expect("grade")
    }

    fn channelled(channel: LevelsChannel) -> LevelsGrade {
        LevelsGrade::new(
            0.0,
            1.0,
            2.0,
            0.0,
            1.0,
            channel,
            LevelsClip::Off,
            LevelsClip::Off,
        )
        .expect("grade")
    }

    fn clipped(clip_output_black: LevelsClip, clip_output_white: LevelsClip) -> LevelsGrade {
        LevelsGrade::new(
            0.0,
            1.0,
            1.0,
            0.0,
            1.0,
            LevelsChannel::Rgb,
            clip_output_black,
            clip_output_white,
        )
        .expect("grade")
    }

    fn close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= 1.0e-6,
            "expected {expected}, got {actual}"
        );
    }

    fn close_pixel(actual: [f32; 4], expected: [f32; 4]) {
        for (index, expected) in expected.into_iter().enumerate() {
            close(actual[index], expected);
        }
    }

    #[test]
    fn levels_outside_the_authored_range_are_rejected() {
        for value in [
            f32::NAN,
            f32::INFINITY,
            LEVELS_MAX_LEVEL + 0.5,
            LEVELS_MIN_LEVEL - 0.5,
        ] {
            assert_eq!(
                LevelsGrade::new(
                    value,
                    1.0,
                    1.0,
                    0.0,
                    1.0,
                    LevelsChannel::Rgb,
                    LevelsClip::Off,
                    LevelsClip::Off
                ),
                Err(LevelsError::InputBlack)
            );
            assert_eq!(
                LevelsGrade::new(
                    0.0,
                    value,
                    1.0,
                    0.0,
                    1.0,
                    LevelsChannel::Rgb,
                    LevelsClip::Off,
                    LevelsClip::Off
                ),
                Err(LevelsError::InputWhite)
            );
            assert_eq!(
                LevelsGrade::new(
                    0.0,
                    1.0,
                    1.0,
                    value,
                    1.0,
                    LevelsChannel::Rgb,
                    LevelsClip::Off,
                    LevelsClip::Off
                ),
                Err(LevelsError::OutputBlack)
            );
            assert_eq!(
                LevelsGrade::new(
                    0.0,
                    1.0,
                    1.0,
                    0.0,
                    value,
                    LevelsChannel::Rgb,
                    LevelsClip::Off,
                    LevelsClip::Off
                ),
                Err(LevelsError::OutputWhite)
            );
        }
    }

    #[test]
    fn gamma_outside_the_authored_range_is_rejected() {
        for value in [f32::NAN, f32::INFINITY, 0.0, -0.5, LEVELS_MAX_GAMMA + 0.5] {
            assert_eq!(
                LevelsGrade::new(
                    0.0,
                    1.0,
                    value,
                    0.0,
                    1.0,
                    LevelsChannel::Rgb,
                    LevelsClip::Off,
                    LevelsClip::Off
                ),
                Err(LevelsError::Gamma)
            );
        }
    }

    #[test]
    fn the_declared_endpoints_are_accepted() {
        for point in [LEVELS_MIN_LEVEL, 0.0, LEVELS_MAX_LEVEL] {
            let resolved = grade(point, LEVELS_MAX_LEVEL, 1.0, LEVELS_MIN_LEVEL, point);
            assert_eq!(resolved.input_black(), point);
            assert_eq!(resolved.input_white(), LEVELS_MAX_LEVEL);
            assert_eq!(resolved.output_black(), LEVELS_MIN_LEVEL);
            assert_eq!(resolved.output_white(), point);
            for gamma in [f32::MIN_POSITIVE, 1.0, LEVELS_MAX_GAMMA] {
                assert_eq!(grade(0.0, 1.0, gamma, 0.0, 1.0).gamma(), gamma);
            }
        }
    }

    #[test]
    fn the_neutral_grade_is_the_identity() {
        let neutral = grade(0.0, 1.0, 1.0, 0.0, 1.0);
        assert!(neutral.is_identity());
        for channel in LevelsChannel::ALL {
            let every_channel = LevelsGrade::new(
                0.0,
                1.0,
                1.0,
                0.0,
                1.0,
                channel,
                LevelsClip::Off,
                LevelsClip::Off,
            )
            .expect("grade");
            assert!(every_channel.is_identity());
        }
        assert!(!grade(0.1, 1.0, 1.0, 0.0, 1.0).is_identity());
        assert!(!grade(0.0, 1.1, 1.0, 0.0, 1.0).is_identity());
        assert!(!grade(0.0, 1.0, 1.1, 0.0, 1.0).is_identity());
        assert!(!grade(0.0, 1.0, 1.0, 0.1, 1.0).is_identity());
        assert!(!grade(0.0, 1.0, 1.0, 0.0, 1.1).is_identity());
        assert!(!clipped(LevelsClip::ClampDisplay, LevelsClip::Off).is_identity());
        assert!(!clipped(LevelsClip::Off, LevelsClip::ClampOutput).is_identity());
    }

    #[test]
    fn the_neutral_grade_passes_samples_through() {
        let neutral = grade(0.0, 1.0, 1.0, 0.0, 1.0);
        for value in [0.0, 0.37, 1.0, 2.5] {
            close(neutral.transform(value), value);
        }
        close_pixel(neutral.apply([0.2, 0.4, 0.6, 1.0]), [0.2, 0.4, 0.6, 1.0]);
        assert_eq!(neutral.apply([0.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn the_documented_gamma_clamp_holds_negative_samples_at_zero() {
        let neutral = grade(0.0, 1.0, 1.0, 0.0, 1.0);
        assert_eq!(neutral.transform(-0.4), 0.0);
        assert_eq!(neutral.transform(-1.0e-9), 0.0);
        assert_eq!(neutral.transform(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn the_channel_menu_selects_which_components_are_remapped() {
        let pixel = [0.25, 0.25, 0.25, 0.25];
        close_pixel(
            channelled(LevelsChannel::Rgb).apply(pixel),
            [0.5, 0.5, 0.5, 0.25],
        );
        close_pixel(
            channelled(LevelsChannel::Red).apply(pixel),
            [0.5, 0.25, 0.25, 0.25],
        );
        close_pixel(
            channelled(LevelsChannel::Green).apply(pixel),
            [0.25, 0.5, 0.25, 0.25],
        );
        close_pixel(
            channelled(LevelsChannel::Blue).apply(pixel),
            [0.25, 0.25, 0.5, 0.25],
        );
        close_pixel(
            channelled(LevelsChannel::Alpha).apply(pixel),
            [0.25, 0.25, 0.25, 0.5],
        );
    }

    #[test]
    fn the_channel_menu_numbering_matches_the_list() {
        assert_eq!(
            LevelsChannel::ALL.map(LevelsChannel::as_key),
            ["rgb", "red", "green", "blue", "alpha"]
        );
        for (index, channel) in LevelsChannel::ALL.into_iter().enumerate() {
            assert_eq!(channel.as_menu(), index as i64 + 1);
            assert_eq!(LevelsChannel::from_menu(channel.as_menu()), Some(channel));
            assert_eq!(LevelsChannel::from_key(channel.as_key()), Some(channel));
        }
        assert_eq!(LevelsChannel::from_menu(0), None);
        assert_eq!(LevelsChannel::from_menu(6), None);
        assert_eq!(LevelsChannel::from_key("luma"), None);
    }

    #[test]
    fn the_clip_menu_numbering_matches_the_list() {
        assert_eq!(
            LevelsClip::ALL.map(LevelsClip::as_key),
            ["off", "clamp_output", "clamp_display"]
        );
        for (index, clip) in LevelsClip::ALL.into_iter().enumerate() {
            assert_eq!(clip.as_menu(), index as i64 + 1);
            assert_eq!(LevelsClip::from_menu(clip.as_menu()), Some(clip));
            assert_eq!(LevelsClip::from_key(clip.as_key()), Some(clip));
        }
        assert_eq!(LevelsClip::from_menu(0), None);
        assert_eq!(LevelsClip::from_menu(4), None);
        assert_eq!(LevelsClip::from_key("clamp"), None);
    }

    #[test]
    fn the_degenerate_input_range_becomes_a_hard_step() {
        let stepped = grade(0.5, 0.5, 1.0, 0.0, 1.0);
        assert_eq!(stepped.transform(-1.0), 0.0);
        assert_eq!(stepped.transform(0.4), 0.0);
        assert_eq!(stepped.transform(0.5), 0.0);
        assert_eq!(stepped.transform(0.6), 1.0);
        assert_eq!(stepped.transform(2.0), 1.0);
        let stepped_with_gamma = grade(0.5, 0.5, 2.0, 0.0, 1.0);
        for value in [-1.0, 0.5, 0.6, 2.0] {
            assert!(stepped_with_gamma.transform(value).is_finite());
        }
        assert_eq!(stepped_with_gamma.transform(0.6), 1.0);
    }

    #[test]
    fn gamma_above_one_brightens() {
        close(grade(0.0, 1.0, 1.0, 0.0, 1.0).transform(0.25), 0.25);
        close(grade(0.0, 1.0, 2.0, 0.0, 1.0).transform(0.25), 0.5);
        close(grade(0.0, 1.0, 0.5, 0.0, 1.0).transform(0.25), 0.062_5);
        let shaped = grade(0.2, 0.8, 2.0, 0.0, 1.0);
        close(shaped.transform(0.0), 0.0);
        close(shaped.transform(0.2), 0.0);
        close(shaped.transform(0.5), 0.5f32.sqrt());
        close(shaped.transform(0.8), 1.0);
    }

    #[test]
    fn the_output_points_remap_the_shaped_sample() {
        close(grade(0.0, 1.0, 1.0, 0.2, 0.8).transform(0.5), 0.5);
        close(grade(0.0, 1.0, 1.0, 0.2, 0.8).transform(0.0), 0.2);
        close(grade(0.0, 1.0, 1.0, 0.2, 0.8).transform(1.0), 0.8);
        let inverted = grade(0.0, 1.0, 1.0, 1.0, 0.0);
        close(inverted.transform(0.25), 0.75);
    }

    #[test]
    fn clamp_output_clips_into_the_output_range() {
        let high = LevelsGrade::new(
            0.0,
            1.0,
            1.0,
            0.4,
            0.6,
            LevelsChannel::Rgb,
            LevelsClip::ClampOutput,
            LevelsClip::ClampOutput,
        )
        .expect("grade");
        close(high.transform(2.0), 0.6);
        close(high.transform(0.0), 0.4);
        close(high.transform(0.5), 0.5);
        let inverted = LevelsGrade::new(
            0.0,
            1.0,
            1.0,
            0.6,
            0.4,
            LevelsChannel::Rgb,
            LevelsClip::ClampOutput,
            LevelsClip::ClampOutput,
        )
        .expect("grade");
        close(inverted.transform(2.0), 0.4);
        close(inverted.transform(0.5), 0.5);
        let low_end_only = LevelsGrade::new(
            0.0,
            1.0,
            1.0,
            0.6,
            0.4,
            LevelsChannel::Rgb,
            LevelsClip::ClampOutput,
            LevelsClip::Off,
        )
        .expect("grade");
        close(low_end_only.transform(2.0), 0.4);
        close(low_end_only.transform(-1.0), 0.6);
        let high_end_only = LevelsGrade::new(
            0.0,
            1.0,
            1.0,
            0.4,
            0.6,
            LevelsChannel::Rgb,
            LevelsClip::Off,
            LevelsClip::ClampOutput,
        )
        .expect("grade");
        close(high_end_only.transform(2.0), 0.6);
        close(high_end_only.transform(-1.0), 0.4);
    }

    #[test]
    fn clamp_display_clips_into_the_display_range() {
        let display = clipped(LevelsClip::ClampDisplay, LevelsClip::ClampDisplay);
        assert_eq!(display.transform(2.5), 1.0);
        assert_eq!(display.transform(-0.4), 0.0);
        close(display.transform(0.37), 0.37);
        close_pixel(display.apply([2.5, -0.4, 0.9, 1.0]), [1.0, 0.0, 0.9, 1.0]);
        let low_end_only = clipped(LevelsClip::ClampDisplay, LevelsClip::Off);
        assert_eq!(low_end_only.transform(-0.4), 0.0);
        close(low_end_only.transform(2.5), 2.5);
        let high_end_only = clipped(LevelsClip::Off, LevelsClip::ClampDisplay);
        assert_eq!(high_end_only.transform(-0.4), 0.0);
        assert_eq!(high_end_only.transform(2.5), 1.0);
    }

    #[test]
    fn zero_coverage_pixels_are_untouched_in_colour_modes() {
        let pixel = [0.2, 0.3, 0.4, 0.0];
        for channel in [
            LevelsChannel::Rgb,
            LevelsChannel::Red,
            LevelsChannel::Green,
            LevelsChannel::Blue,
        ] {
            assert_eq!(channelled(channel).apply(pixel), pixel);
        }
        assert_eq!(
            channelled(LevelsChannel::Rgb).apply([0.0, 0.0, 0.0, 0.0]),
            [0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn the_alpha_channel_processes_zero_coverage_pixels() {
        let coverage = LevelsGrade::new(
            0.0,
            1.0,
            1.0,
            0.25,
            1.0,
            LevelsChannel::Alpha,
            LevelsClip::Off,
            LevelsClip::Off,
        )
        .expect("grade");
        close_pixel(coverage.apply([0.2, 0.3, 0.4, 0.0]), [0.2, 0.3, 0.4, 0.25]);
        close_pixel(coverage.apply([0.2, 0.3, 0.4, 0.5]), [0.2, 0.3, 0.4, 0.625]);
        close_pixel(coverage.apply([0.2, 0.3, 0.4, 1.0]), [0.2, 0.3, 0.4, 1.0]);
    }

    #[test]
    fn the_gpu_controls_pack_the_authored_grade() {
        let grade = LevelsGrade::new(
            0.2,
            0.85,
            2.2,
            0.05,
            0.95,
            LevelsChannel::Alpha,
            LevelsClip::ClampDisplay,
            LevelsClip::ClampOutput,
        )
        .expect("grade");
        assert_eq!(grade.gpu_controls().0, [0.2, 0.85, 2.2, 0.05]);
        assert_eq!(grade.gpu_controls().1, [0.95, 5.0, 3.0, 2.0]);
    }

    #[test]
    fn the_whole_frame_matches_the_point_kernel() {
        let grade = grade(0.2, 0.85, 1.0, 0.05, 0.95);
        let mut pixels = vec![
            [0.18, 0.18, 0.18, 1.0],
            [0.25, 0.55, 0.05, 1.0],
            [2.5, -0.4, 0.9, 1.0],
            [0.0, 0.0, 0.0, 0.0],
            [0.5, 0.9, 0.1, 1.0],
            [0.35, 0.35, 0.35, 0.25],
        ];
        let expected = pixels.iter().map(|pixel| grade.apply(*pixel)).collect::<Vec<_>>();
        assert!(levels_frame_f32(&mut pixels, &grade));
        assert_eq!(pixels, expected);
    }

    #[test]
    fn the_neutral_grade_still_holds_a_negative_sample_at_zero() {
        // The neutral points are elided by the graph builder, but the mapping is
        // not a mathematical no-op: the gamma clamp has no branch for negatives.
        let neutral = grade(0.0, 1.0, 1.0, 0.0, 1.0);
        let mut pixels = vec![[-0.4, 0.3, 0.4, 1.0], [0.2, 0.3, 0.4, 0.0]];
        assert!(levels_frame_f32(&mut pixels, &neutral));
        assert_eq!(pixels[0], [0.0, 0.3, 0.4, 1.0]);
        // A pixel without coverage is never authored by a colour mode.
        assert_eq!(pixels[1], [0.2, 0.3, 0.4, 0.0]);
    }

    #[test]
    fn checkpoints_run_once_per_chunk_and_once_at_the_end() {
        let grade = grade(0.0, 1.0, 2.0, 0.0, 1.0);
        let mut checkpoints = 0;
        let mut short = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_levels_f32_controlled(&mut short, &grade, &mut checkpoint).expect("infallible")
        );
        assert_eq!(checkpoints, 2);
        let mut long = vec![[0.25, 0.25, 0.25, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_levels_f32_controlled(&mut long, &grade, &mut checkpoint).expect("infallible")
        );
        assert_eq!(checkpoints, 3);
    }

    #[test]
    fn the_neutral_grade_still_checkpoints_once_per_chunk() {
        let neutral = grade(0.0, 1.0, 1.0, 0.0, 1.0);
        let mut pixels = [[0.25, 0.25, 0.25, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_levels_f32_controlled(&mut pixels, &neutral, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 3);
    }
}
