//! The leave-colour effect from the built-in effect list.
//!
//! The list authors five controls: the amount of colour left behind, the colour
//! that is kept, a tolerance, an edge softness and a match menu. The effect is a
//! point operation, so it runs on the fused GPU point path as well as the CPU
//! kernel.
//!
//! # Documented reading
//!
//! The list names the effect but does not define the maths, so this module fixes
//! one reading and documents it here. With `c` the current straight-alpha colour
//! sample, `k` the authored colour to keep, `tolerance` and `softness` the two
//! authored authoring percentages divided by 100, the match menu is read as:
//!
//! - `rgb`: `d = sqrt((cr-kr)² + (cg-kg)² + (cb-kb)²) / sqrt(3)`, the Euclidean
//!   distance in the unit cube normalised so the opposite corner is at one.
//! - `hue`: `d = 2 * min(|dh|, 1 - |dh|)` with `dh` the wrapped difference of
//!   the two normalised hues in `0..1`, so the full hue circle is also at one.
//!
//! The hue itself is derived with the usual maximum/minimum difference: with
//! `delta = max(rgb) - min(rgb)`, a grey sample (`delta == 0`) has hue zero, a
//! red maximal sample has `((g - b) / delta) / 6` wrapped into `0..1`, a green
//! maximal sample has `((b - r) / delta + 2) / 6` and a blue maximal sample has
//! `((r - g) / delta + 4) / 6`. The CPU kernel and the fused WGSL branch use the
//! same expression so the two sides agree at the boundary.
//!
//! # Retained weight
//!
//! `keep = 1 - smoothstep(tolerance, tolerance + softness + 1e-4, d)`.
//! `smoothstep(edge0, edge1, x)` is `t * t * (3 - 2t)` with `t` the clamped
//! `(x - edge0) / (edge1 - edge0)`, and degenerates to a hard step at `edge0`
//! when `edge1 <= edge0`. The explicit `1e-4` keeps the soft edge strictly wider
//! than the tolerance, so a zero softness is the documented near-hard edge and
//! the two sides never evaluate a degenerate ramp.
//!
//! # Output
//!
//! `grey = dot(rgb, (0.2126, 0.7152, 0.0722))` (Rec.709 luma, like the rest of
//! the engine), `amount = amount / 100` and
//! `decolor = amount * (1 - keep)`; the output is
//! `mix(rgb, vec3(grey), decolor)` and the source alpha is preserved. An amount
//! of zero is the exact identity, so [`LeaveColorGrade::is_identity`] is true
//! and the graph builder elides the operation. The result is never clamped, so
//! an HDR sample keeps its extended range where it is not decoloured.
//!
//! A pixel without coverage is returned unchanged: the compositor short circuits
//! zero coverage, so writing into one would make the CPU and the GPU disagree
//! about a pixel that is never visible. The effect never authors coverage.

/// Number of match menu options the list declares.
pub const LEAVE_COLOR_MATCH_COUNT: usize = 2;

/// Highest authored percentage (`0` to `100`).
pub const LEAVE_COLOR_MAX_PERCENT: f32 = 100.0;

/// Lowest authored percentage.
pub const LEAVE_COLOR_MIN_PERCENT: f32 = 0.0;

/// Match criterion used to decide how close a sample is to the kept colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveColorMatch {
    /// Euclidean distance in the RGB cube, normalised to `0..=1`.
    Rgb,
    /// Circular distance between the two hues, normalised to `0..=1`.
    Hue,
}

impl LeaveColorMatch {
    /// Every match criterion in menu order.
    pub const ALL: [Self; LEAVE_COLOR_MATCH_COUNT] = [Self::Rgb, Self::Hue];

    /// The key the effect list stores for this criterion.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Hue => "hue",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|criterion| criterion.as_key() == key)
    }

    /// The one-based menu number the list declares for this criterion.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Resolves a one-based menu number, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Rgb),
            2 => Some(Self::Hue),
            _ => None,
        }
    }

    /// The zero-based number the fused point shader receives.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Rgb => 0,
            Self::Hue => 1,
        }
    }

    /// The Chinese label the effect list declares for this criterion.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rgb => "使用 RGB",
            Self::Hue => "使用色相",
        }
    }
}

/// Why a leave-colour grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveColorError {
    /// The decolour amount is not a finite number within `0..=100`.
    Amount,
    /// The kept colour is not a finite colour.
    Colour,
    /// The tolerance is not a finite number within `0..=100`.
    Tolerance,
    /// The edge softness is not a finite number within `0..=100`.
    Softness,
}

impl std::fmt::Display for LeaveColorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amount => write!(
                formatter,
                "leave color amount must be finite and within 0..=100"
            ),
            Self::Colour => write!(formatter, "leave color kept colour must be a finite colour"),
            Self::Tolerance => write!(
                formatter,
                "leave color tolerance must be finite and within 0..=100"
            ),
            Self::Softness => write!(
                formatter,
                "leave color edge softness must be finite and within 0..=100"
            ),
        }
    }
}

impl std::error::Error for LeaveColorError {}

/// A resolved leave-colour grade.
///
/// The authoring percentages stay in the authored `0..=100` domain so a round
/// trip through the effect list preserves what the user typed; the kernel and
/// [`LeaveColorGrade::gpu_controls`] scale them by `1 / 100` when they run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LeaveColorGrade {
    amount: f32,
    colour: [f32; 3],
    tolerance: f32,
    softness: f32,
    match_mode: LeaveColorMatch,
}

impl LeaveColorGrade {
    /// Resolves the authored controls, rejecting values outside `0..=100`.
    ///
    /// # Errors
    ///
    /// [`LeaveColorError::Amount`], [`LeaveColorError::Tolerance`] or
    /// [`LeaveColorError::Softness`] when that percentage is not finite or falls
    /// outside `0..=100`, and [`LeaveColorError::Colour`] when one of the kept
    /// colour's channels is not finite. The match criterion is an enum, so it
    /// cannot be out of range.
    pub fn new(
        amount: f32,
        colour: [f32; 3],
        tolerance: f32,
        softness: f32,
        match_mode: LeaveColorMatch,
    ) -> Result<Self, LeaveColorError> {
        for (value, error) in [
            (amount, LeaveColorError::Amount),
            (tolerance, LeaveColorError::Tolerance),
            (softness, LeaveColorError::Softness),
        ] {
            if !value.is_finite()
                || !(LEAVE_COLOR_MIN_PERCENT..=LEAVE_COLOR_MAX_PERCENT).contains(&value)
            {
                return Err(error);
            }
        }
        if colour.iter().any(|channel| !channel.is_finite()) {
            return Err(LeaveColorError::Colour);
        }
        Ok(Self { amount, colour, tolerance, softness, match_mode })
    }

    /// The authored decolour amount in `0..=100`.
    #[must_use]
    pub const fn amount(&self) -> f32 {
        self.amount
    }

    /// The authored colour that is kept, as straight-alpha RGB.
    #[must_use]
    pub const fn colour(&self) -> [f32; 3] {
        self.colour
    }

    /// The authored tolerance in `0..=100`.
    #[must_use]
    pub const fn tolerance(&self) -> f32 {
        self.tolerance
    }

    /// The authored edge softness in `0..=100`.
    #[must_use]
    pub const fn softness(&self) -> f32 {
        self.softness
    }

    /// The authored match criterion.
    #[must_use]
    pub const fn match_mode(&self) -> LeaveColorMatch {
        self.match_mode
    }

    /// Whether the authored state leaves the frame unchanged.
    ///
    /// A zero decolour amount mixes the source onto itself, so the graph builder
    /// can elide the operation. Every other control only shapes which samples the
    /// amount applies to.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.amount == 0.0
    }

    /// Applies the effect to one straight-alpha pixel.
    ///
    /// A pixel without coverage is returned unchanged: the compositor short
    /// circuits zero coverage, so mapping one would make the CPU and the GPU
    /// disagree about a pixel that is never visible. The effect never authors
    /// coverage, so the output alpha is the source alpha.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let rgb = [pixel[0], pixel[1], pixel[2]];
        let keep = self.keep(rgb);
        let grey = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
        let decolour = (self.amount / LEAVE_COLOR_MAX_PERCENT) * (1.0 - keep);
        [
            rgb[0] + (grey - rgb[0]) * decolour,
            rgb[1] + (grey - rgb[1]) * decolour,
            rgb[2] + (grey - rgb[2]) * decolour,
            pixel[3],
        ]
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [amount / 100, tolerance / 100, softness / 100, match number]`
    /// with the match criterion as its zero-based number, and
    /// `color = [kr, kg, kb, 0]`. Both extra vectors are unused by the branch.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.amount / LEAVE_COLOR_MAX_PERCENT,
                self.tolerance / LEAVE_COLOR_MAX_PERCENT,
                self.softness / LEAVE_COLOR_MAX_PERCENT,
                self.match_mode.as_u32() as f32,
            ],
            [self.colour[0], self.colour[1], self.colour[2], 0.0],
        )
    }

    /// How much of the source colour the authored tolerance keeps.
    fn keep(&self, rgb: [f32; 3]) -> f32 {
        let tolerance = self.tolerance / LEAVE_COLOR_MAX_PERCENT;
        let softness = self.softness / LEAVE_COLOR_MAX_PERCENT;
        // The epsilon keeps the soft edge wider than the tolerance even when the
        // authored softness is zero, so the ramp never degenerates.
        let edge = tolerance + softness + 1.0e-4;
        1.0 - smoothstep(tolerance, edge, self.distance(rgb))
    }

    /// The match distance between one sample and the kept colour.
    fn distance(&self, rgb: [f32; 3]) -> f32 {
        match self.match_mode {
            LeaveColorMatch::Rgb => {
                let red = rgb[0] - self.colour[0];
                let green = rgb[1] - self.colour[1];
                let blue = rgb[2] - self.colour[2];
                (red * red + green * green + blue * blue).sqrt() / 3.0_f32.sqrt()
            }
            LeaveColorMatch::Hue => {
                let difference = (hue(rgb) - hue(self.colour)).abs();
                2.0 * difference.min(1.0 - difference)
            }
        }
    }
}

/// Normalised hue of a straight-alpha RGB sample in `0..1`.
///
/// The expression is shared verbatim with the fused WGSL branch: a grey sample
/// is hue zero, and the three maximal-channel arms add the sector offset. The
/// red arm wraps a negative sector into the top of the circle instead of using a
/// remainder, which is the form the shader can evaluate identically.
fn hue(rgb: [f32; 3]) -> f32 {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    let delta = maximum - minimum;
    if delta == 0.0 {
        return 0.0;
    }
    if maximum == rgb[0] {
        let sector = (rgb[1] - rgb[2]) / delta;
        if sector < 0.0 {
            (sector + 6.0) / 6.0
        } else {
            sector / 6.0
        }
    } else if maximum == rgb[1] {
        ((rgb[2] - rgb[0]) / delta + 2.0) / 6.0
    } else {
        ((rgb[0] - rgb[1]) / delta + 4.0) / 6.0
    }
}

/// Hermite ramp from zero at `edge0` to one at `edge1`.
///
/// A degenerate edge is a hard step at `edge0`, matching the documented reading
/// and the shader. The fused WGSL branch spells the same expression out instead
/// of calling the builtin, so both sides run one formula.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x <= edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether the grade ran. A zero amount is elided by the graph builder,
/// so a compiled graph always reaches this kernel with a real amount, and the
/// kernel always applies the mapping.
pub fn leave_color_frame_f32(pixels: &mut [[f32; 4]], grade: &LeaveColorGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_leave_color_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_leave_color_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &LeaveColorGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // Leave colour is a point grade over the three colour channels; alpha is
    // passed through and a pixel without coverage is left alone. Point grades
    // are bandwidth bound: measured on a 1280x720 frame they run at the single
    // thread DRAM stream rate, so splitting the frame across cores buys nothing.
    // The 4096-pixel chunks exist purely for cancellation checkpoints.
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
        amount: f32,
        colour: [f32; 3],
        tolerance: f32,
        softness: f32,
        match_mode: LeaveColorMatch,
    ) -> LeaveColorGrade {
        LeaveColorGrade::new(amount, colour, tolerance, softness, match_mode).expect("grade")
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

    const RED: [f32; 3] = [1.0, 0.0, 0.0];

    #[test]
    fn menu_numbers_are_one_based_and_round_trip() {
        assert_eq!(LeaveColorMatch::ALL.len(), LEAVE_COLOR_MATCH_COUNT);
        for (index, criterion) in LeaveColorMatch::ALL.into_iter().enumerate() {
            assert_eq!(criterion.as_u32() as usize, index);
            assert_eq!(criterion.as_menu() as usize, index + 1);
            assert_eq!(
                LeaveColorMatch::from_menu(criterion.as_menu()),
                Some(criterion)
            );
            assert_eq!(
                LeaveColorMatch::from_key(criterion.as_key()),
                Some(criterion)
            );
        }
        assert_eq!(LeaveColorMatch::from_menu(0), None);
        assert_eq!(
            LeaveColorMatch::from_menu(LEAVE_COLOR_MATCH_COUNT as u32 + 1),
            None
        );
        assert_eq!(LeaveColorMatch::from_key("mystery"), None);
    }

    #[test]
    fn the_kept_colour_survives_a_full_decolour() {
        let exact = [1.0, 0.0, 0.0, 1.0];
        let rgb = grade(100.0, RED, 20.0, 0.0, LeaveColorMatch::Rgb);
        assert_eq!(
            rgb.apply(exact).map(f32::to_bits),
            exact.map(f32::to_bits),
            "an exactly matching colour must not be decoloured"
        );
        // A colour on the far side of the cube is fully grey under Rec.709.
        let mapped = rgb.apply([0.0, 1.0, 0.0, 1.0]);
        close_pixel(mapped, [0.7152, 0.7152, 0.7152, 1.0]);
        // A darker tint of the kept colour is only recognised by the hue
        // criterion: the RGB distance also measures the brightness difference.
        let hue_mode = grade(100.0, RED, 20.0, 0.0, LeaveColorMatch::Hue);
        let tint = [0.4, 0.0, 0.0, 0.5];
        assert_eq!(
            hue_mode.apply(tint).map(f32::to_bits),
            tint.map(f32::to_bits),
            "the hue criterion ignores the brightness difference"
        );
    }

    #[test]
    fn the_retained_weight_falls_off_with_the_distance() {
        let grade = grade(100.0, RED, 10.0, 40.0, LeaveColorMatch::Rgb);
        let samples = [
            [0.9, 0.1, 0.1],
            [0.7, 0.2, 0.2],
            [0.4, 0.3, 0.3],
            [0.0, 0.5, 0.5],
        ];
        let mut previous = 1.0_f32;
        for sample in samples {
            let keep = grade.keep(sample);
            assert!(keep <= previous, "keep must not rise with the distance");
            previous = keep;
        }
        assert!(previous < 1.0e-6, "a far sample must be fully decoloured");
    }

    #[test]
    fn the_hue_weight_falls_off_with_the_hue_distance() {
        let grade = grade(100.0, RED, 10.0, 40.0, LeaveColorMatch::Hue);
        let samples = [
            [1.0, 0.5, 0.2],
            [1.0, 0.8, 0.2],
            [0.8, 1.0, 0.2],
            [0.2, 1.0, 0.8],
        ];
        let mut previous = 1.0_f32;
        for sample in samples {
            let keep = grade.keep(sample);
            assert!(keep < previous, "the hue ramp must keep falling");
            previous = keep;
        }
    }

    #[test]
    fn a_zero_tolerance_only_keeps_the_exact_colour() {
        let grade = grade(100.0, RED, 0.0, 0.0, LeaveColorMatch::Rgb);
        assert_eq!(grade.keep(RED), 1.0);
        assert_eq!(grade.keep([0.5, 0.5, 0.5]), 0.0);
        let mapped = grade.apply([0.5, 0.5, 0.5, 1.0]);
        close_pixel(mapped, [0.5, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn a_hundred_tolerance_keeps_every_colour() {
        let grade = grade(100.0, RED, 100.0, 0.0, LeaveColorMatch::Rgb);
        for pixel in [
            [0.0, 1.0, 0.0, 1.0],
            [0.25, 0.5, 0.75, 0.5],
            [1.0, 1.0, 1.0, 1.0],
        ] {
            assert_eq!(
                grade.apply(pixel).map(f32::to_bits),
                pixel.map(f32::to_bits),
                "the full tolerance must keep every sample"
            );
        }
    }

    #[test]
    fn the_edge_softness_widens_the_retained_ramp() {
        let hard = grade(100.0, RED, 20.0, 0.0, LeaveColorMatch::Rgb);
        let soft = grade(100.0, RED, 20.0, 30.0, LeaveColorMatch::Rgb);
        let sample = [0.4, 0.0, 0.0];
        let distance = hard.distance(sample);
        assert!(distance > 0.2 && distance < 0.5);
        assert_eq!(hard.keep(sample), 0.0);
        let widened = soft.keep(sample);
        assert!(
            widened > 0.0 && widened < 1.0,
            "the soft edge must ramp, got {widened}"
        );
    }

    #[test]
    fn rgb_and_hue_disagree_on_a_dark_tint_of_the_kept_colour() {
        let rgb = grade(100.0, RED, 20.0, 0.0, LeaveColorMatch::Rgb);
        let hue = grade(100.0, RED, 20.0, 0.0, LeaveColorMatch::Hue);
        let sample = [0.25, 0.0, 0.0, 1.0];
        assert!(rgb.distance([0.25, 0.0, 0.0]) > 0.2);
        assert_eq!(hue.distance([0.25, 0.0, 0.0]), 0.0);
        let decoloured = rgb.apply(sample);
        let kept = hue.apply(sample);
        assert_eq!(kept.map(f32::to_bits), sample.map(f32::to_bits));
        close(decoloured[0], 0.25 * 0.2126);
        assert!(decoloured[0] < kept[0]);
    }

    #[test]
    fn a_zero_amount_is_the_identity() {
        let grade = grade(0.0, [0.2, 0.4, 0.6], 15.0, 25.0, LeaveColorMatch::Hue);
        assert!(grade.is_identity());
        for pixel in [[0.3, 0.6, 0.9, 1.0], [2.5, -0.4, 0.1, 0.25]] {
            assert_eq!(
                grade.apply(pixel).map(f32::to_bits),
                pixel.map(f32::to_bits),
                "a zero amount must not touch the frame"
            );
        }
    }

    #[test]
    fn transparent_pixels_are_left_untouched() {
        let grade = grade(100.0, RED, 15.0, 0.0, LeaveColorMatch::Rgb);
        let pixels = [[1.4, -0.3, 0.2, 0.0], [0.5, 0.5, 0.5, -0.5]];
        for pixel in pixels {
            assert_eq!(
                grade.apply(pixel).map(f32::to_bits),
                pixel.map(f32::to_bits),
                "a pixel without coverage must not be authored"
            );
        }
    }

    #[test]
    fn alpha_is_preserved() {
        for match_mode in LeaveColorMatch::ALL {
            let grade = grade(75.0, [0.2, 0.4, 0.6], 15.0, 10.0, match_mode);
            for alpha in [0.5, 1.0, 2.0] {
                let mapped = grade.apply([0.3, 0.6, 0.9, alpha]);
                assert_eq!(
                    mapped[3].to_bits(),
                    alpha.to_bits(),
                    "{} must not author alpha",
                    match_mode.as_key()
                );
            }
        }
    }

    #[test]
    fn extended_range_samples_stay_finite() {
        let grade = grade(100.0, [0.5, 0.5, 0.5], 15.0, 10.0, LeaveColorMatch::Hue);
        let mapped = grade.apply([2.5, -0.4, 0.9, 1.0]);
        for channel in &mapped[..3] {
            assert!(channel.is_finite(), "HDR input must stay finite");
        }
        assert_eq!(mapped[3], 1.0);
    }

    #[test]
    fn grade_rejects_non_finite_and_out_of_range_values() {
        assert_eq!(
            LeaveColorGrade::new(f32::NAN, RED, 0.0, 0.0, LeaveColorMatch::Rgb),
            Err(LeaveColorError::Amount)
        );
        assert_eq!(
            LeaveColorGrade::new(101.0, RED, 0.0, 0.0, LeaveColorMatch::Rgb),
            Err(LeaveColorError::Amount)
        );
        assert_eq!(
            LeaveColorGrade::new(
                100.0,
                [f32::INFINITY, 0.0, 0.0],
                0.0,
                0.0,
                LeaveColorMatch::Rgb
            ),
            Err(LeaveColorError::Colour)
        );
        assert_eq!(
            LeaveColorGrade::new(100.0, RED, -1.0, 0.0, LeaveColorMatch::Rgb),
            Err(LeaveColorError::Tolerance)
        );
        assert_eq!(
            LeaveColorGrade::new(100.0, RED, 0.0, 101.0, LeaveColorMatch::Rgb),
            Err(LeaveColorError::Softness)
        );
        assert!(LeaveColorGrade::new(
            LEAVE_COLOR_MIN_PERCENT,
            RED,
            LEAVE_COLOR_MAX_PERCENT,
            LEAVE_COLOR_MAX_PERCENT,
            LeaveColorMatch::Hue
        )
        .is_ok());
    }

    #[test]
    fn gpu_controls_pack_the_amount_and_the_kept_colour() {
        let grade = grade(50.0, [0.25, 0.5, 0.75], 20.0, 30.0, LeaveColorMatch::Hue);
        let (params, color) = grade.gpu_controls();
        assert_eq!(params, [0.5, 0.2, 0.3, 1.0]);
        assert_eq!(color, [0.25, 0.5, 0.75, 0.0]);
    }

    #[test]
    fn the_frame_kernel_matches_the_pixel_mapping() {
        let grade = grade(80.0, [0.1, 0.9, 0.4], 25.0, 35.0, LeaveColorMatch::Hue);
        let mut pixels = vec![
            [0.2, 0.4, 0.6, 1.0],
            [0.9, 0.1, 0.5, 0.25],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let expected = pixels.iter().map(|pixel| grade.apply(*pixel)).collect::<Vec<_>>();
        assert!(leave_color_frame_f32(&mut pixels, &grade));
        for (actual, expected) in pixels.iter().zip(&expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    #[test]
    fn checkpoints_run_once_per_chunk_and_once_at_the_end() {
        let grade = grade(100.0, RED, 15.0, 0.0, LeaveColorMatch::Rgb);
        let mut checkpoints = 0;
        let mut short = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_leave_color_f32_controlled(&mut short, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 2);
        let mut long = vec![[0.25, 0.25, 0.25, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_leave_color_f32_controlled(&mut long, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 3);
    }
}
