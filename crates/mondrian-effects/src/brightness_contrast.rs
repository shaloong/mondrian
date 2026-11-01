//! The brightness and contrast effect from the built-in effect list.
//!
//! The list authors three controls: a brightness offset, a contrast scale and a
//! legacy toggle. All of them are point operations, so the effect runs on the
//! fused GPU point path as well as the CPU kernel.
//!
//! # Documented reading
//!
//! The list does not define how the two sliders map onto arithmetic, so this
//! module fixes one reading and documents it here. Contrast becomes a gain that
//! is exactly one at zero and monotone on both sides of it:
//!
//! - `gain = 1 / (1 - contrast / 200)` while `contrast >= 0`, which reaches two
//!   at `+100`, so the distance from the pivot doubles.
//! - `gain = 1 + contrast / 100` while `contrast < 0`, which reaches zero at
//!   `-100`, so the whole frame collapses onto the brightness offset.
//!
//! Brightness becomes the offset `brightness / 150 * 0.5`, so the full positive
//! control raises a mid grey pixel by half of the working domain.
//!
//! The two compose as `out.rgb = pixel.rgb * gain + bias`. The default reading
//! never clamps, so extended-range highlights and negative samples keep their
//! sign and magnitude. The legacy toggle keeps the same arithmetic and then
//! clamps the result into `[0, 1]`, which is what the display encoded effect it
//! mirrors did implicitly; it is documented here rather than being a second
//! mapping.
//!
//! # Domain
//!
//! The working frame is scene linear and the controls are working-domain
//! numbers: the offset is a fraction of full scale and the gain is a plain
//! scale, so nothing is re-encoded. Alpha is never authored by this effect: it
//! is returned unchanged, and a pixel without coverage is left completely
//! untouched. The compositor short circuits zero coverage, so writing into such
//! a pixel would make the CPU and the GPU disagree about a pixel that is never
//! visible.

/// Highest authored brightness (`-150` to `150`).
pub const BRIGHTNESS_CONTRAST_MAX_BRIGHTNESS: f32 = 150.0;

/// Lowest authored brightness.
pub const BRIGHTNESS_CONTRAST_MIN_BRIGHTNESS: f32 = -150.0;

/// Highest authored contrast (`-100` to `100`).
pub const BRIGHTNESS_CONTRAST_MAX_CONTRAST: f32 = 100.0;

/// Lowest authored contrast.
pub const BRIGHTNESS_CONTRAST_MIN_CONTRAST: f32 = -100.0;

/// Authoring value rejected by [`BrightnessContrastGrade::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrightnessContrastError {
    /// Brightness was not finite or fell outside `-150..=150`.
    Brightness,
    /// Contrast was not finite or fell outside `-100..=100`.
    Contrast,
}

impl std::fmt::Display for BrightnessContrastError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Brightness => {
                formatter.write_str("brightness must be finite and within -150 to 150")
            }
            Self::Contrast => formatter.write_str("contrast must be finite and within -100 to 100"),
        }
    }
}

impl std::error::Error for BrightnessContrastError {}

/// Brightness and contrast authoring resolved into the gain and offset the
/// kernel applies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrightnessContrastGrade {
    brightness: f32,
    contrast: f32,
    legacy: bool,
}

impl BrightnessContrastGrade {
    /// Resolves the authored controls into the scale and the offset.
    ///
    /// # Errors
    ///
    /// [`BrightnessContrastError::Brightness`] when brightness is not finite or
    /// falls outside `-150..=150`, and [`BrightnessContrastError::Contrast`]
    /// when contrast is not finite or falls outside `-100..=100`.
    pub fn new(
        brightness: f32,
        contrast: f32,
        legacy: bool,
    ) -> Result<Self, BrightnessContrastError> {
        if !brightness.is_finite()
            || !(BRIGHTNESS_CONTRAST_MIN_BRIGHTNESS..=BRIGHTNESS_CONTRAST_MAX_BRIGHTNESS)
                .contains(&brightness)
        {
            return Err(BrightnessContrastError::Brightness);
        }
        if !contrast.is_finite()
            || !(BRIGHTNESS_CONTRAST_MIN_CONTRAST..=BRIGHTNESS_CONTRAST_MAX_CONTRAST)
                .contains(&contrast)
        {
            return Err(BrightnessContrastError::Contrast);
        }
        Ok(Self { brightness, contrast, legacy })
    }

    /// The authored brightness.
    #[must_use]
    pub const fn brightness(&self) -> f32 {
        self.brightness
    }

    /// The authored contrast.
    #[must_use]
    pub const fn contrast(&self) -> f32 {
        self.contrast
    }

    /// Whether the shifted result is clamped into the display domain.
    #[must_use]
    pub const fn legacy(&self) -> bool {
        self.legacy
    }

    /// Whether the grade leaves every visible pixel unchanged.
    ///
    /// The legacy toggle does not enter the answer: a clamp with nothing to
    /// clamp is not a change.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.brightness == 0.0 && self.contrast == 0.0
    }

    /// The scale applied to the colour channels.
    #[must_use]
    pub fn gain(&self) -> f32 {
        if self.contrast >= 0.0 {
            1.0 / (1.0 - self.contrast / 200.0)
        } else {
            1.0 + self.contrast / 100.0
        }
    }

    /// The offset added to the colour channels.
    #[must_use]
    pub fn bias(&self) -> f32 {
        self.brightness / BRIGHTNESS_CONTRAST_MAX_BRIGHTNESS * 0.5
    }

    /// Applies the effect to one pixel.
    ///
    /// A pixel without coverage is left untouched: the compositor short circuits
    /// zero coverage, so shifting one would make the CPU and the GPU disagree
    /// about a pixel that is never visible.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let gain = self.gain();
        let bias = self.bias();
        let shifted = [
            pixel[0] * gain + bias,
            pixel[1] * gain + bias,
            pixel[2] * gain + bias,
        ];
        let rgb = if self.legacy {
            [
                shifted[0].clamp(0.0, 1.0),
                shifted[1].clamp(0.0, 1.0),
                shifted[2].clamp(0.0, 1.0),
            ]
        } else {
            shifted
        };
        [rgb[0], rgb[1], rgb[2], pixel[3]]
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [gain, bias, legacy, 0]`, `extra = [0, 0, 0, 0]`.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.gain(),
                self.bias(),
                if self.legacy { 1.0 } else { 0.0 },
                0.0,
            ],
            [0.0; 4],
        )
    }
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether anything was written: the identity grade is not applied.
pub fn brightness_contrast_frame_f32(
    pixels: &mut [[f32; 4]],
    grade: &BrightnessContrastGrade,
) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_brightness_contrast_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_brightness_contrast_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &BrightnessContrastGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    if grade.is_identity() {
        return Ok(false);
    }
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

    fn grade(brightness: f32, contrast: f32, legacy: bool) -> BrightnessContrastGrade {
        BrightnessContrastGrade::new(brightness, contrast, legacy).expect("grade")
    }

    #[test]
    fn brightness_outside_the_authored_range_is_rejected() {
        assert_eq!(
            BrightnessContrastGrade::new(f32::NAN, 0.0, false),
            Err(BrightnessContrastError::Brightness)
        );
        assert_eq!(
            BrightnessContrastGrade::new(150.5, 0.0, false),
            Err(BrightnessContrastError::Brightness)
        );
        assert_eq!(
            BrightnessContrastGrade::new(-150.5, 0.0, false),
            Err(BrightnessContrastError::Brightness)
        );
    }

    #[test]
    fn contrast_outside_the_authored_range_is_rejected() {
        assert_eq!(
            BrightnessContrastGrade::new(0.0, f32::NAN, false),
            Err(BrightnessContrastError::Contrast)
        );
        assert_eq!(
            BrightnessContrastGrade::new(0.0, 100.5, false),
            Err(BrightnessContrastError::Contrast)
        );
        assert_eq!(
            BrightnessContrastGrade::new(0.0, -100.5, false),
            Err(BrightnessContrastError::Contrast)
        );
    }

    #[test]
    fn the_declared_endpoints_are_accepted() {
        for brightness in [
            BRIGHTNESS_CONTRAST_MIN_BRIGHTNESS,
            0.0,
            BRIGHTNESS_CONTRAST_MAX_BRIGHTNESS,
        ] {
            for contrast in [
                BRIGHTNESS_CONTRAST_MIN_CONTRAST,
                0.0,
                BRIGHTNESS_CONTRAST_MAX_CONTRAST,
            ] {
                assert!(BrightnessContrastGrade::new(brightness, contrast, true).is_ok());
            }
        }
    }

    #[test]
    fn the_neutral_setting_is_the_identity() {
        assert!(grade(0.0, 0.0, false).is_identity());
        // The toggle alone is not a change, only a clamp of an unshifted frame.
        assert!(grade(0.0, 0.0, true).is_identity());
        assert!(!grade(0.1, 0.0, false).is_identity());
        assert!(!grade(0.0, 0.1, false).is_identity());
    }

    #[test]
    fn the_gain_passes_through_one_and_reaches_the_endpoints() {
        assert_eq!(grade(0.0, 0.0, false).gain(), 1.0);
        assert_eq!(
            grade(0.0, BRIGHTNESS_CONTRAST_MAX_CONTRAST, false).gain(),
            2.0
        );
        assert_eq!(
            grade(0.0, BRIGHTNESS_CONTRAST_MIN_CONTRAST, false).gain(),
            0.0
        );
        let half = grade(0.0, 50.0, false).gain();
        assert!((half - 4.0 / 3.0).abs() <= 1.0e-6, "{half}");
    }

    #[test]
    fn the_gain_is_monotone_across_zero_contrast() {
        let contrasts = [-100.0_f32, -50.0, -10.0, 0.0, 10.0, 50.0, 100.0];
        let mut previous = f32::NEG_INFINITY;
        for contrast in contrasts {
            let gain = grade(0.0, contrast, false).gain();
            assert!(
                gain > previous,
                "contrast {contrast} did not raise the gain"
            );
            previous = gain;
        }
    }

    #[test]
    fn the_bias_raises_mid_grey_by_half_at_the_top() {
        assert_eq!(
            grade(BRIGHTNESS_CONTRAST_MAX_BRIGHTNESS, 0.0, false).bias(),
            0.5
        );
        assert_eq!(
            grade(BRIGHTNESS_CONTRAST_MIN_BRIGHTNESS, 0.0, false).bias(),
            -0.5
        );
        assert_eq!(grade(0.0, 0.0, false).bias(), 0.0);
        assert_eq!(grade(75.0, 0.0, false).bias(), 0.25);
    }

    #[test]
    fn brightness_and_contrast_compose_as_a_gain_and_an_offset() {
        let grade = grade(75.0, 50.0, false);
        let pixel = [0.2_f32, 0.4, 0.6, 1.0];
        let gain = grade.gain();
        let bias = grade.bias();
        let shifted = grade.apply(pixel);
        for channel in 0..3 {
            let expected = pixel[channel] * gain + bias;
            assert!(
                (shifted[channel] - expected).abs() <= 1.0e-6,
                "channel {channel}: {} != {expected}",
                shifted[channel]
            );
        }
        assert_eq!(shifted[3], 1.0);
    }

    #[test]
    fn the_default_reading_keeps_extended_range_values() {
        let grade = grade(0.0, 100.0, false);
        assert_eq!(grade.apply([2.5, -0.4, 0.9, 1.0]), [5.0, -0.8, 1.8, 1.0]);
    }

    #[test]
    fn the_legacy_toggle_clamps_the_shifted_value() {
        let modern = grade(0.0, 100.0, false);
        let legacy = grade(0.0, 100.0, true);
        assert_eq!(modern.apply([2.5, -0.4, 0.9, 1.0]), [5.0, -0.8, 1.8, 1.0]);
        assert_eq!(legacy.apply([2.5, -0.4, 0.9, 1.0]), [1.0, 0.0, 1.0, 1.0]);
        // A value that stays inside the domain after the gain is identical in
        // both readings.
        assert_eq!(
            legacy.apply([0.1, 0.2, 0.25, 0.5]),
            modern.apply([0.1, 0.2, 0.25, 0.5])
        );
    }

    #[test]
    fn alpha_is_never_modified() {
        let grade = grade(150.0, 100.0, true);
        for alpha in [0.25_f32, 0.5, 1.0] {
            assert_eq!(grade.apply([0.9, 2.0, -1.0, alpha])[3], alpha);
        }
    }

    #[test]
    fn a_pixel_without_coverage_is_left_untouched() {
        let grade = grade(150.0, 100.0, true);
        assert_eq!(grade.apply([0.9, 2.0, -1.0, 0.0]), [0.9, 2.0, -1.0, 0.0]);
        assert_eq!(grade.apply([0.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn the_gpu_controls_carry_the_gain_the_bias_and_the_legacy_flag() {
        let authored = grade(75.0, 50.0, true);
        let (params, extra) = authored.gpu_controls();
        assert_eq!(params, [authored.gain(), authored.bias(), 1.0, 0.0]);
        assert_eq!(extra, [0.0; 4]);
        assert_eq!(grade(0.0, 0.0, false).gpu_controls().0[2], 0.0);
    }

    #[test]
    fn the_whole_frame_entry_matches_the_point_kernel() {
        let grade = grade(-30.0, -40.0, false);
        let mut pixels = vec![
            [0.0_f32, 0.0, 0.0, 1.0],
            [0.5, 0.25, 0.75, 0.5],
            [1.5, -0.25, 0.5, 0.0],
            [0.9, 0.9, 0.9, 1.0],
        ];
        let expected: Vec<[f32; 4]> = pixels.iter().map(|pixel| grade.apply(*pixel)).collect();
        assert!(brightness_contrast_frame_f32(&mut pixels, &grade));
        assert_eq!(pixels, expected);
    }

    #[test]
    fn the_identity_is_not_applied_to_the_frame() {
        let mut pixels = vec![[0.25_f32, 0.5, 1.5, 1.0]; 4];
        let before = pixels.clone();
        assert!(!brightness_contrast_frame_f32(
            &mut pixels,
            &grade(0.0, 0.0, true)
        ));
        assert_eq!(pixels, before);
    }

    #[test]
    fn the_kernel_checkpoints_every_chunk_and_finishes_with_one() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 4];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(apply_brightness_contrast_f32_controlled(
            &mut pixels,
            &grade(10.0, 20.0, false),
            &mut checkpoint
        )
        .expect("applied"));
        // One call opens the single chunk of four pixels, one closes the frame.
        assert_eq!(calls, 2);

        let mut many = vec![[0.5_f32, 0.5, 0.5, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(apply_brightness_contrast_f32_controlled(
            &mut many,
            &grade(10.0, 20.0, false),
            &mut checkpoint
        )
        .expect("applied"));
        assert_eq!(calls, 3);
    }

    #[test]
    fn the_identity_never_checkpoints() {
        let mut pixels = vec![[0.5_f32, 0.5, 0.5, 1.0]; 4];
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(!apply_brightness_contrast_f32_controlled(
            &mut pixels,
            &grade(0.0, 0.0, false),
            &mut checkpoint
        )
        .expect("not applied"));
        assert_eq!(calls, 0);
    }
}
