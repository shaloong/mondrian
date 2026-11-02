//! The color balance effect from the built-in effect list.
//!
//! The list authors nine balance amounts — shadow, midtone and highlight, each
//! with a red, green and blue amount — plus one `preserve_luminosity` switch.
//! Color balance is a point operation, so it runs on the fused GPU point path
//! as well as the CPU kernel.
//!
//! # Documented reading
//!
//! The list names the effect but does not define the maths, so this module fixes
//! one reading and documents it here. Straight alpha, float32:
//!
//! 1. With `l = 0.2126r + 0.7152g + 0.0722b` the Rec.709 luma of the *source*
//!    sample, the three region weights are `ws = clamp(1 - 2l, 0, 1)`,
//!    `wh = clamp(2l - 1, 0, 1)` and `wm = 1 - ws - wh`, so pure black is all
//!    shadow, pure white all highlight, and the three weights always sum to one.
//!    The weights are piecewise linear on purpose: the kernel and the shader
//!    then evaluate the same expression, while a smootherstep difference
//!    quotient would amplify rounding into visible banding.
//! 2. `amount[channel] = (shadow[channel] * ws + midtone[channel] * wm +
//!    highlight[channel] * wh) / 100 * 0.5`, so a full slider shifts its
//!    channel by `0.5` at the region it targets. The weight is shared by all
//!    three channels and is evaluated from the source luma, so the three
//!    channels cannot drift apart.
//! 3. `rgb' = clamp(rgb + amount, 0, 1)`. The clamp happens once, on the final
//!    value, so no channel is clipped ahead of the others.
//! 4. With `preserve_luminosity` on, `l' = 0.2126r' + 0.7152g' + 0.0722b'` and,
//!    when `l' > 1e-6`, `rgb'' = rgb' * (l / l')`. The rescale is not clamped
//!    again: keeping the source luma has priority, so a sample may sit slightly
//!    above 1 after a positive highlight lift.
//! 5. Alpha is passed through, and a pixel with `alpha <= 0` is returned
//!    unchanged, because the compositor short circuits zero coverage and
//!    grading such a pixel would only make the CPU and the GPU disagree about a
//!    pixel that is never visible.
//!
//! `preserve_luminosity` alone is an identity: the nine amounts are zero, so
//! `rgb'` equals `rgb` (up to the final clamp) and there is no shift for the
//! rescale to undo. [`ColorBalanceGrade::is_identity`] therefore depends on the
//! nine amounts only, and the graph builder elides the operation for an
//! unconfigured effect whatever the switch says.
//!
//! # Uniform packing
//!
//! [`ColorBalanceGrade::gpu_controls`] returns
//! `params = [shadow_r, shadow_g, shadow_b, midtone_r]`,
//! `color = [midtone_g, midtone_b, highlight_r, highlight_g]` and
//! `extra0 = [highlight_b, preserve number, 0, 0]`. Nine amounts and the one
//! flag fit exactly in three `vec4`s; `extra1` is unused. The shader repeats
//! the same multiplication order, so the CPU and the GPU agree on the final
//! sample.

/// Lowest authored balance amount, in percent.
pub const COLOR_BALANCE_MIN_BALANCE: f32 = -100.0;

/// Highest authored balance amount, in percent.
pub const COLOR_BALANCE_MAX_BALANCE: f32 = 100.0;

/// The authored balance amount that leaves a channel untouched.
pub const COLOR_BALANCE_DEFAULT_BALANCE: f32 = 0.0;

/// Rec.709 luma coefficients, shared verbatim with the fused WGSL branch.
const REC_709_LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// A full slider shifts its channel by half of the unit range.
const FULL_SLIDER_SHIFT: f32 = 0.5;

/// A luma this small cannot rescale the sample without dividing by it.
const LUMA_EPSILON: f32 = 1.0e-6;

/// Why a color-balance grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorBalanceError {
    /// One of the nine balance amounts is not finite or falls outside `-100..=100`.
    Balance,
}

impl std::fmt::Display for ColorBalanceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Balance => write!(
                formatter,
                "color balance amounts must be finite and within -100..=100"
            ),
        }
    }
}

impl std::error::Error for ColorBalanceError {}

/// A resolved color-balance grade.
///
/// The nine authored amounts stay in their authored `-100..=100` domain so a
/// round trip through the effect list preserves what the user typed; the kernel
/// and [`ColorBalanceGrade::gpu_controls`] resolve them to a channel shift when
/// they run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorBalanceGrade {
    shadow: [f32; 3],
    midtone: [f32; 3],
    highlight: [f32; 3],
    preserve_luminosity: bool,
}

impl ColorBalanceGrade {
    /// Resolves the authored controls, rejecting amounts outside `-100..=100`.
    ///
    /// # Errors
    ///
    /// [`ColorBalanceError::Balance`] when any of the nine amounts is not finite
    /// or falls outside `-100..=100`.
    pub fn new(
        shadow: [f32; 3],
        midtone: [f32; 3],
        highlight: [f32; 3],
        preserve_luminosity: bool,
    ) -> Result<Self, ColorBalanceError> {
        for balance in shadow.into_iter().chain(midtone).chain(highlight) {
            if !balance.is_finite()
                || !(COLOR_BALANCE_MIN_BALANCE..=COLOR_BALANCE_MAX_BALANCE).contains(&balance)
            {
                return Err(ColorBalanceError::Balance);
            }
        }
        Ok(Self { shadow, midtone, highlight, preserve_luminosity })
    }

    /// The authored shadow amounts, in red-green-blue order.
    #[must_use]
    pub const fn shadow(&self) -> [f32; 3] {
        self.shadow
    }

    /// The authored midtone amounts, in red-green-blue order.
    #[must_use]
    pub const fn midtone(&self) -> [f32; 3] {
        self.midtone
    }

    /// The authored highlight amounts, in red-green-blue order.
    #[must_use]
    pub const fn highlight(&self) -> [f32; 3] {
        self.highlight
    }

    /// Whether the authored state asks for the luminosity rescale.
    #[must_use]
    pub const fn preserve_luminosity(&self) -> bool {
        self.preserve_luminosity
    }

    /// Whether the authored state leaves the frame unchanged.
    ///
    /// The switch alone is an identity: with all nine amounts zero there is no
    /// shift to rescale, so only the amounts decide, and the graph builder
    /// elides the operation when every one of them is zero.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.shadow[0] == 0.0
            && self.shadow[1] == 0.0
            && self.shadow[2] == 0.0
            && self.midtone[0] == 0.0
            && self.midtone[1] == 0.0
            && self.midtone[2] == 0.0
            && self.highlight[0] == 0.0
            && self.highlight[1] == 0.0
            && self.highlight[2] == 0.0
    }

    /// Applies the effect to one straight-alpha pixel.
    ///
    /// A pixel without coverage is returned unchanged: the compositor short
    /// circuits zero coverage, so mapping one would make the CPU and the GPU
    /// disagree about a pixel that is never visible.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let source_luma = luma([pixel[0], pixel[1], pixel[2]]);
        let shadows = (1.0 - 2.0 * source_luma).clamp(0.0, 1.0);
        let highlights = (2.0 * source_luma - 1.0).clamp(0.0, 1.0);
        let midtones = 1.0 - shadows - highlights;
        let mut rgb = [0.0_f32; 3];
        for (channel, value) in rgb.iter_mut().enumerate() {
            let amount = (self.shadow[channel] * shadows
                + self.midtone[channel] * midtones
                + self.highlight[channel] * highlights)
                / 100.0
                * FULL_SLIDER_SHIFT;
            *value = (pixel[channel] + amount).clamp(0.0, 1.0);
        }
        if self.preserve_luminosity {
            let balanced_luma = luma(rgb);
            if balanced_luma > LUMA_EPSILON {
                let scale = source_luma / balanced_luma;
                for value in &mut rgb {
                    *value *= scale;
                }
            }
        }
        [rgb[0], rgb[1], rgb[2], pixel[3]]
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [shadow_r, shadow_g, shadow_b, midtone_r]`,
    /// `color = [midtone_g, midtone_b, highlight_r, highlight_g]` and
    /// `extra0 = [highlight_b, preserve number, 0, 0]`. The shader repeats the
    /// kernel's expression in the same order, so the two sides resolve the same
    /// shift.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4], [f32; 4]) {
        (
            [
                self.shadow[0],
                self.shadow[1],
                self.shadow[2],
                self.midtone[0],
            ],
            [
                self.midtone[1],
                self.midtone[2],
                self.highlight[0],
                self.highlight[1],
            ],
            [
                self.highlight[2],
                if self.preserve_luminosity { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
        )
    }
}

/// Rec.709 luma of a straight-alpha RGB sample.
///
/// The expression is shared verbatim with the fused WGSL branch so the two
/// sides agree on the region weights.
fn luma(rgb: [f32; 3]) -> f32 {
    REC_709_LUMA[0] * rgb[0] + REC_709_LUMA[1] * rgb[1] + REC_709_LUMA[2] * rgb[2]
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether the grade ran. The graph builder elides an identity grade, so
/// a compiled graph that reaches this kernel always maps the frame.
pub fn color_balance_frame_f32(pixels: &mut [[f32; 4]], grade: &ColorBalanceGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_color_balance_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_color_balance_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &ColorBalanceGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // Color balance is a point grade over the three colour channels. Point
    // grades are bandwidth bound, so splitting the frame across cores buys
    // nothing; the 4096-pixel chunks exist purely for cancellation
    // checkpoints.
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
        shadow: [f32; 3],
        midtone: [f32; 3],
        highlight: [f32; 3],
        preserve_luminosity: bool,
    ) -> ColorBalanceGrade {
        ColorBalanceGrade::new(shadow, midtone, highlight, preserve_luminosity).expect("grade")
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

    fn frame(width: usize, height: usize) -> Vec<[f32; 4]> {
        (0..width * height)
            .map(|index| {
                let channel = |offset: usize| ((index * (offset + 3)) % 17) as f32 / 16.0;
                [
                    channel(0),
                    channel(1),
                    channel(2),
                    if index % 5 == 0 { 0.0 } else { 1.0 },
                ]
            })
            .collect()
    }

    #[test]
    fn zero_amounts_are_an_identity() {
        let grade = grade([0.0; 3], [0.0; 3], [0.0; 3], false);
        assert!(grade.is_identity());
        for pixel in [
            [0.0, 0.0, 0.0, 1.0],
            [0.25, 0.5, 0.75, 1.0],
            [1.0, 1.0, 1.0, 0.5],
        ] {
            let output = grade.apply(pixel);
            for (actual, expected) in output.into_iter().zip(pixel) {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
    }

    #[test]
    fn preserve_luminosity_alone_is_an_identity() {
        let grade = grade([0.0; 3], [0.0; 3], [0.0; 3], true);
        assert!(grade.is_identity());
        let pixel = [0.2, 0.4, 0.8, 1.0];
        let output = grade.apply(pixel);
        for (actual, expected) in output.into_iter().zip(pixel) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn shadow_red_lifts_black_by_half_a_step() {
        let grade = grade([100.0, 0.0, 0.0], [0.0; 3], [0.0; 3], false);
        assert!(!grade.is_identity());
        close_pixel(grade.apply([0.0, 0.0, 0.0, 1.0]), [0.5, 0.0, 0.0, 1.0]);
        // White sits entirely in the highlight region, so the shadow amount
        // cannot touch it.
        close_pixel(grade.apply([1.0, 1.0, 1.0, 1.0]), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn highlight_blue_pulls_white_down_by_half_a_step() {
        let grade = grade([0.0; 3], [0.0; 3], [0.0, 0.0, -100.0], false);
        assert!(!grade.is_identity());
        close_pixel(grade.apply([1.0, 1.0, 1.0, 1.0]), [1.0, 1.0, 0.5, 1.0]);
        // Black sits entirely in the shadow region.
        close_pixel(grade.apply([0.0, 0.0, 0.0, 1.0]), [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn midtone_weights_peak_at_mid_grey() {
        let grade = grade([0.0; 3], [80.0, -40.0, 0.0], [0.0; 3], false);
        let output = grade.apply([0.5, 0.5, 0.5, 1.0]);
        // A mid-grey sample sits on the midtone plateau, so the shift is the
        // documented `slider / 100 * 0.5`.
        close(output[0], 0.5 + 80.0 / 100.0 * 0.5);
        close(output[1], 0.5 - 40.0 / 100.0 * 0.5);
        close(output[2], 0.5);
    }

    #[test]
    fn preserve_luminosity_keeps_the_source_luma() {
        let grade = grade(
            [40.0, -20.0, 0.0],
            [10.0, 20.0, -30.0],
            [-10.0, 0.0, 60.0],
            true,
        );
        let source = [0.4, 0.6, 0.3, 1.0];
        let source_luma = luma([source[0], source[1], source[2]]);
        let output = grade.apply(source);
        let output_luma = luma([output[0], output[1], output[2]]);
        assert!((output_luma - source_luma).abs() < 1.0e-5);
        // The rescale is allowed to leave the unit range, so the invariant is
        // the luma and not a clamp.
        assert_eq!(output[3], 1.0);
    }

    #[test]
    fn zero_coverage_pixels_are_returned_untouched() {
        let grade = grade([100.0, -100.0, 50.0], [10.0; 3], [-20.0; 3], true);
        for alpha in [0.0, -0.5] {
            let pixel = [0.3, 0.7, 0.2, alpha];
            let output = grade.apply(pixel);
            for (actual, expected) in output.into_iter().zip(pixel) {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
    }

    #[test]
    fn the_alpha_channel_is_never_rewritten() {
        let grade = grade([100.0, 0.0, 0.0], [0.0; 3], [0.0; 3], true);
        let output = grade.apply([0.5, 0.5, 0.5, 0.25]);
        assert_eq!(output[3].to_bits(), 0.25_f32.to_bits());
    }

    #[test]
    fn new_rejects_non_finite_and_out_of_range_amounts() {
        for balance in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -100.5, 100.5] {
            assert_eq!(
                ColorBalanceGrade::new([balance, 0.0, 0.0], [0.0; 3], [0.0; 3], false),
                Err(ColorBalanceError::Balance)
            );
            assert_eq!(
                ColorBalanceGrade::new([0.0; 3], [0.0; 3], [0.0, balance, 0.0], true),
                Err(ColorBalanceError::Balance)
            );
        }
        assert!(ColorBalanceGrade::new([0.0; 3], [0.0; 3], [0.0; 3], false).is_ok());
        assert!(
            ColorBalanceGrade::new([-100.0, 0.0, 0.0], [0.0; 3], [0.0, 0.0, 100.0], true).is_ok()
        );
    }

    #[test]
    fn the_checkpoint_error_cancels_the_frame() {
        let mut pixels = frame(4, 2);
        let source = pixels.clone();
        let mut called = false;
        let mut checkpoint = || {
            called = true;
            Err("cancelled")
        };
        assert_eq!(
            apply_color_balance_f32_controlled(
                &mut pixels,
                &grade([100.0, 0.0, 0.0], [0.0; 3], [0.0; 3], false),
                &mut checkpoint,
            ),
            Err("cancelled")
        );
        assert!(called);
        for (pixel, expected) in pixels.iter().zip(source) {
            for (actual, expected) in pixel.iter().zip(expected) {
                assert_eq!(actual.to_bits(), expected.to_bits());
            }
        }
    }

    #[test]
    fn the_checkpoint_cadence_is_bounded() {
        let mut pixels = vec![[0.5, 0.5, 0.5, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut calls = 0_usize;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert_eq!(
            apply_color_balance_f32_controlled(
                &mut pixels,
                &grade([0.0; 3], [10.0, 0.0, 0.0], [0.0; 3], false),
                &mut checkpoint,
            ),
            Ok(true)
        );
        // One call per chunk plus the trailing checkpoint.
        assert_eq!(calls, 3);
    }

    #[test]
    fn the_frame_kernel_matches_the_pixel_kernel() {
        let grade = grade(
            [30.0, -10.0, 5.0],
            [0.0, 25.0, -15.0],
            [-20.0, 0.0, 40.0],
            true,
        );
        for (width, height) in [(1, 1), (3, 2), (17, 5)] {
            let mut pixels = frame(width, height);
            let expected: Vec<[f32; 4]> = pixels.iter().map(|pixel| grade.apply(*pixel)).collect();
            assert!(color_balance_frame_f32(&mut pixels, &grade));
            for (actual, expected) in pixels.iter().zip(expected) {
                for (actual, expected) in actual.iter().zip(expected) {
                    assert_eq!(actual.to_bits(), expected.to_bits());
                }
            }
        }
    }

    #[test]
    fn gpu_controls_pack_the_grade() {
        let grade = grade([1.0, -2.0, 3.0], [4.0, -5.0, 6.0], [-7.0, 8.0, -9.0], true);
        assert_eq!(
            grade.gpu_controls(),
            (
                [1.0, -2.0, 3.0, 4.0],
                [-5.0, 6.0, -7.0, 8.0],
                [-9.0, 1.0, 0.0, 0.0],
            )
        );
        let plain = ColorBalanceGrade::new([0.0; 3], [0.0; 3], [0.0; 3], false).expect("grade");
        assert_eq!(plain.gpu_controls().2, [0.0; 4]);
    }
}
