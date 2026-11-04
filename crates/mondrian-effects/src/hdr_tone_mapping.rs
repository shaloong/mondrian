//! The HDR tone-mapping effect from the built-in effect list.
//!
//! The list authors three controls: the tone-mapping mode, an exposure gain and
//! the display gamma. Tone mapping is a point operation, so it runs on the fused
//! GPU point path as well as the CPU kernel.
//!
//! # Documented reading
//!
//! The list names the effect but does not define the maths beyond the two menu
//! labels, so this module fixes one reading and documents it here. Every channel
//! is mapped independently, in a fixed order:
//!
//! ```text
//! x = max(rgb * gain, 0)
//! y = Reinhard ? x / (1 + x) : hable(x) / hable(11.2)
//! out = y ^ (1 / gamma)
//! ```
//!
//! The exposure sits before the curve and the negative clamp sits before both,
//! so an authored gain can never turn a negative channel into a NaN. The gamma is
//! a display gamma: its reciprocal is the exponent, which is why the two menu
//! modes look the way they do on a display.
//!
//! # The two curves
//!
//! - `reinhard`: `y = x / (1 + x)`. It compresses every positive value strictly
//!   below itself and converges to one as `x` grows.
//! - `hable`: the filmic curve
//!   `hable(v) = (v * (2.51v + 0.03)) / (v * (2.43v + 0.59) + 0.14)`, normalised
//!   by `hable(11.2)` so the documented white point maps to one.
//!
//! The white-point normalisation is resolved once, at compile time, by
//! [`HDR_TONE_MAPPING_WHITE_POINT_INVERSE`] and shipped to the shader as the
//! fourth component of the `params` cell. The shader multiplies by it instead of
//! re-deriving `hable(11.2)`, so the two sides cannot drift apart about where
//! white is.
//!
//! Above the white point the filmic curve would keep rising past one (it
//! converges to about 1.02), so the normalised value is pinned to one: the
//! output of this effect is display-referred, and a value above the white point
//! is white. That ceiling is the one deliberate reading on top of the formula,
//! and the shader applies the same `min`.
//!
//! # Output
//!
//! Straight alpha throughout: a sample is never premultiplied and the source
//! alpha is passed through untouched. A sample with `alpha <= 0` is returned
//! unchanged, because the compositor short circuits zero coverage and mapping
//! such a pixel would only make the CPU and the GPU disagree about a pixel that
//! is never visible. Every channel is finite for a finite input and the output
//! stays inside `0..=1`.
//!
//! # No identity
//!
//! There is no identity configuration: Reinhard compresses every positive value
//! even at `gain = 1`, and the filmic curve does the same. [`HdrToneMappingGrade::is_identity`]
//! is therefore always false and the graph builder appends the operation
//! unconditionally, exactly like `builtin.vr_sphere_to_plane`. The documented
//! defaults are the first thing the effect does to a frame, which is intended:
//! the user added the effect.

/// Number of tone-mapping modes the list declares.
pub const HDR_TONE_MAPPING_MODE_COUNT: usize = 2;

/// Lowest authored exposure gain.
pub const HDR_TONE_MAPPING_MIN_GAIN: f32 = 0.01;

/// Highest authored exposure gain.
pub const HDR_TONE_MAPPING_MAX_GAIN: f32 = 100.0;

/// Lowest authored display gamma.
pub const HDR_TONE_MAPPING_MIN_GAMMA: f32 = 0.1;

/// Highest authored display gamma.
pub const HDR_TONE_MAPPING_MAX_GAMMA: f32 = 10.0;

/// The white point of the Hable filmic curve.
const HABLE_WHITE_POINT: f32 = 11.2;

/// Shoulder strength `A` of the Hable filmic curve.
const HABLE_A: f32 = 2.51;

/// Linear strength `B` of the Hable filmic curve.
const HABLE_B: f32 = 0.03;

/// Linear angle `C` of the Hable filmic curve.
const HABLE_C: f32 = 2.43;

/// Toe strength `D` of the Hable filmic curve.
const HABLE_D: f32 = 0.59;

/// Toe numerator `E` of the Hable filmic curve.
const HABLE_E: f32 = 0.14;

/// `1 / hable(11.2)`, the normalisation that maps the white point to one.
///
/// Precomputed on the CPU and passed to the fused point shader as the fourth
/// component of the `params` cell, so neither the kernel nor the shader
/// re-derives the white point.
pub const HDR_TONE_MAPPING_WHITE_POINT_INVERSE: f32 = 1.0 / hable(HABLE_WHITE_POINT);

/// The tone-mapping curve a sample is pushed through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ToneMapMode {
    /// Reinhard: `x / (1 + x)`, a plain global compression.
    Reinhard = 1,
    /// Hable: the filmic rational curve, normalised at its 11.2 white point.
    Hable = 2,
}

impl ToneMapMode {
    /// Every mode in menu order.
    pub const ALL: [Self; HDR_TONE_MAPPING_MODE_COUNT] = [Self::Reinhard, Self::Hable];

    /// The key the effect list stores for this mode.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Reinhard => "reinhard",
            Self::Hable => "hable",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }

    /// The one-based menu position, which is also the `mode_code` the fused
    /// point shader compares against.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self as u32
    }

    /// Resolves a one-based menu position, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Reinhard),
            2 => Some(Self::Hable),
            _ => None,
        }
    }

    /// The label the effect list declares for this mode.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Reinhard => "Reinhard",
            Self::Hable => "Hable",
        }
    }
}

/// Why an HDR tone-mapping grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrToneMappingError {
    /// The exposure gain is not a finite value within `0.01..=100`.
    Gain,
    /// The display gamma is not a finite value within `0.1..=10`.
    Gamma,
}

impl std::fmt::Display for HdrToneMappingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gain => write!(
                formatter,
                "hdr tone mapping gain must be finite and within 0.01..=100"
            ),
            Self::Gamma => write!(
                formatter,
                "hdr tone mapping gamma must be finite and within 0.1..=10"
            ),
        }
    }
}

impl std::error::Error for HdrToneMappingError {}

/// A resolved HDR tone-mapping grade.
///
/// The authored controls stay in their authored domains so a round trip through
/// the effect list preserves what the user typed; the kernel and
/// [`HdrToneMappingGrade::gpu_controls`] resolve the gamma and the white point
/// when they run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HdrToneMappingGrade {
    mode: ToneMapMode,
    gain: f32,
    gamma: f32,
}

impl HdrToneMappingGrade {
    /// Resolves the authored controls, rejecting out-of-range exposure values.
    ///
    /// # Errors
    ///
    /// [`HdrToneMappingError::Gain`] when the exposure gain is not finite or
    /// falls outside `0.01..=100`, and [`HdrToneMappingError::Gamma`] when the
    /// display gamma is not finite or falls outside `0.1..=10`. The mode is an
    /// enum, so it cannot be out of range.
    pub fn new(mode: ToneMapMode, gain: f32, gamma: f32) -> Result<Self, HdrToneMappingError> {
        if !gain.is_finite()
            || !(HDR_TONE_MAPPING_MIN_GAIN..=HDR_TONE_MAPPING_MAX_GAIN).contains(&gain)
        {
            return Err(HdrToneMappingError::Gain);
        }
        if !gamma.is_finite()
            || !(HDR_TONE_MAPPING_MIN_GAMMA..=HDR_TONE_MAPPING_MAX_GAMMA).contains(&gamma)
        {
            return Err(HdrToneMappingError::Gamma);
        }
        Ok(Self { mode, gain, gamma })
    }

    /// The authored tone-mapping curve.
    #[must_use]
    pub const fn mode(&self) -> ToneMapMode {
        self.mode
    }

    /// The authored exposure gain in `0.01..=100`.
    #[must_use]
    pub const fn gain(&self) -> f32 {
        self.gain
    }

    /// The authored display gamma in `0.1..=10`.
    #[must_use]
    pub const fn gamma(&self) -> f32 {
        self.gamma
    }

    /// Whether the authored state leaves the frame unchanged.
    ///
    /// Tone mapping is never an identity: both curves compress every positive
    /// value even at the most permissive settings, so the graph builder must
    /// never elide the operation.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        false
    }

    /// Applies the effect to one straight-alpha pixel.
    ///
    /// A pixel without coverage is returned unchanged: the compositor short
    /// circuits zero coverage, so mapping one would make the CPU and the GPU
    /// disagree about a pixel that is never visible. The source alpha is passed
    /// through untouched.
    #[must_use]
    pub fn apply_pixel(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let inverse_gamma = 1.0 / self.gamma;
        let mut mapped = [0.0_f32; 4];
        for (channel, sample) in pixel[..3].iter().enumerate() {
            // The order is fixed: exposure, then the negative clamp, then the
            // curve, then the display encoding.
            let x = (sample * self.gain).max(0.0);
            let y = match self.mode {
                ToneMapMode::Reinhard => x / (1.0 + x),
                ToneMapMode::Hable => (hable(x) * HDR_TONE_MAPPING_WHITE_POINT_INVERSE).min(1.0),
            };
            mapped[channel] = y.powf(inverse_gamma);
        }
        mapped[3] = pixel[3];
        mapped
    }

    /// Applies the effect to every pixel of one frame, in place.
    ///
    /// The frame is rewritten in place: the operation reads one pixel, writes the
    /// same pixel and never samples a neighbour, so no scratch frame is needed.
    pub fn apply_to_frame(&self, frame: &mut [[f32; 4]]) {
        let _ = hdr_tone_mapping_frame_f32(frame, self);
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [mode code, gain, gamma, 1 / hable(11.2)]` with the mode code
    /// one-based (1 is Reinhard, 2 is Hable) and the white-point inverse
    /// precomputed by [`HDR_TONE_MAPPING_WHITE_POINT_INVERSE`]; the extras are
    /// zero. The shader never re-derives the white point.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.mode.as_menu() as f32,
                self.gain,
                self.gamma,
                HDR_TONE_MAPPING_WHITE_POINT_INVERSE,
            ],
            [0.0; 4],
        )
    }
}

/// The Hable filmic curve.
///
/// The expression is shared verbatim with the fused WGSL branch; the constant
/// invocations here are evaluated at compile time.
const fn hable(v: f32) -> f32 {
    (v * (HABLE_A * v + HABLE_B)) / (v * (HABLE_C * v + HABLE_D) + HABLE_E)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_hdr_tone_mapping_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &HdrToneMappingGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // Tone mapping is a point grade over the three colour channels. Point grades
    // are bandwidth bound: measured on a 1280x720 frame they run at the single
    // thread DRAM stream rate, so splitting the frame across cores buys nothing.
    // The 4096-pixel chunks exist purely for cancellation checkpoints.
    for chunk in pixels.chunks_mut(CHECKPOINT_STRIDE) {
        checkpoint()?;
        for pixel in chunk.iter_mut() {
            *pixel = grade.apply_pixel(*pixel);
        }
    }
    checkpoint()?;
    Ok(true)
}

const CHECKPOINT_STRIDE: usize = 4_096;

/// Applies the effect to a frame region with the default, infallible checkpoint.
///
/// Returns `false` when the frame could not be rewritten; the kernel has no
/// interior failure mode, so this is only reachable through a checkpoint.
#[must_use]
pub fn hdr_tone_mapping_frame_f32(pixels: &mut [[f32; 4]], grade: &HdrToneMappingGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_hdr_tone_mapping_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(mode: ToneMapMode, gain: f32, gamma: f32) -> HdrToneMappingGrade {
        HdrToneMappingGrade::new(mode, gain, gamma).expect("grade")
    }

    fn reinhard() -> HdrToneMappingGrade {
        grade(ToneMapMode::Reinhard, 1.0, 1.0)
    }

    fn hable_grade() -> HdrToneMappingGrade {
        grade(ToneMapMode::Hable, 1.0, 1.0)
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

    /// The per-channel value the CPU kernel produces for one input channel.
    fn map_channel(grade: &HdrToneMappingGrade, value: f32) -> f32 {
        grade.apply_pixel([value, 0.0, 0.0, 1.0])[0]
    }

    #[test]
    fn mode_menu_numbers_are_one_based_and_round_trip() {
        assert_eq!(ToneMapMode::ALL.len(), HDR_TONE_MAPPING_MODE_COUNT);
        for (index, mode) in ToneMapMode::ALL.into_iter().enumerate() {
            assert_eq!(mode.as_menu() as usize, index + 1);
            assert_eq!(ToneMapMode::from_menu(mode.as_menu()), Some(mode));
            assert_eq!(ToneMapMode::from_key(mode.as_key()), Some(mode));
        }
        // The menu is one-based and declared for 1..=2 only, so the two
        // out-of-range positions the list test names are unrepresentable.
        assert_eq!(ToneMapMode::from_menu(0), None);
        assert_eq!(ToneMapMode::from_menu(3), None);
        assert_eq!(ToneMapMode::from_key("mystery"), None);
        assert_eq!(ToneMapMode::Reinhard.label(), "Reinhard");
        assert_eq!(ToneMapMode::Hable.label(), "Hable");
    }

    #[test]
    fn reinhard_maps_the_known_points() {
        // 0.25 -> 0.2, 1 -> 0.5 and 4 -> 0.8 are the three documented samples.
        close(map_channel(&reinhard(), 0.25), 0.2);
        close(map_channel(&reinhard(), 1.0), 0.5);
        close(map_channel(&reinhard(), 4.0), 0.8);
        for mode in ToneMapMode::ALL {
            assert!(
                !grade(mode, 1.0, 1.0).is_identity(),
                "{mode:?} compresses even at the documented defaults"
            );
        }
    }

    #[test]
    fn hable_maps_the_white_point_to_one() {
        // The curve is normalised at its 11.2 white point, so the documented
        // white input maps to exactly one.
        for channel in 0..3 {
            let mut pixel = [0.0_f32; 4];
            pixel[channel] = HABLE_WHITE_POINT;
            pixel[3] = 1.0;
            let mapped = hable_grade().apply_pixel(pixel);
            assert!(
                (mapped[channel] - 1.0).abs() <= 1.0e-5,
                "channel {channel} maps the white point to {}",
                mapped[channel]
            );
        }
        // The uniform carries the precomputed inverse as its fourth component, so
        // the shader never re-derives the white point.
        let (params, extra0) = hable_grade().gpu_controls();
        assert_eq!(params[0], 2.0);
        assert_eq!(params[1], 1.0);
        assert_eq!(params[2], 1.0);
        assert_eq!(params[3], HDR_TONE_MAPPING_WHITE_POINT_INVERSE);
        close(params[3] * hable(HABLE_WHITE_POINT), 1.0);
        assert_eq!(extra0, [0.0; 4]);
    }

    #[test]
    fn hable_is_strictly_increasing_below_the_white_point() {
        let grade = hable_grade();
        let mut previous = map_channel(&grade, 0.01);
        for step in 1..=20 {
            let value = step as f32 * 0.5;
            let mapped = map_channel(&grade, value);
            assert!(
                mapped > previous,
                "hable({value}) = {mapped} must exceed {previous}"
            );
            previous = mapped;
        }
    }

    #[test]
    fn gamma_scales_the_display_encoding_after_the_curve() {
        // The exponent is 1 / gamma and it is applied after the curve, so a
        // gamma below one darkens the mid-tones and a gamma above one lifts
        // them; gamma = 1 is the curve itself.
        let base = map_channel(&reinhard(), 0.25);
        close(base, 0.2);
        close(
            map_channel(&grade(ToneMapMode::Reinhard, 1.0, 1.0), 0.25),
            base,
        );
        let darker = map_channel(&grade(ToneMapMode::Reinhard, 1.0, 0.5), 0.25);
        let brighter = map_channel(&grade(ToneMapMode::Reinhard, 1.0, 2.2), 0.25);
        close(darker, base.powf(2.0));
        close(brighter, base.powf(1.0 / 2.2));
        assert!(darker < base, "gamma 0.5 must darken: {darker} < {base}");
        assert!(brighter > base, "gamma 2.2 must lift: {brighter} > {base}");
    }

    #[test]
    fn gain_scales_the_exposure_before_the_curve() {
        // Doubling the gain is doubling the input: Reinhard only sees x.
        let single = grade(ToneMapMode::Reinhard, 1.0, 1.0);
        let double = grade(ToneMapMode::Reinhard, 2.0, 1.0);
        close(map_channel(&double, 0.5), map_channel(&single, 1.0));
        close(map_channel(&double, 0.5), 0.5);
        // A tiny gain still compresses: no gain is an identity.
        let faint = map_channel(
            &grade(ToneMapMode::Reinhard, HDR_TONE_MAPPING_MIN_GAIN, 1.0),
            1.0,
        );
        assert!(faint > 0.0 && faint < 1.0);
    }

    #[test]
    fn negative_channels_are_clamped_to_zero() {
        let sample = [-1.5, -0.2, 0.0, 1.0];
        for mode in ToneMapMode::ALL {
            let mapped = grade(mode, 1.0, 1.0).apply_pixel(sample);
            assert_eq!(
                mapped.map(f32::to_bits),
                [0.0_f32, 0.0, 0.0, 1.0].map(f32::to_bits),
                "{mode:?} must lift a negative channel to zero without a NaN"
            );
        }
        // An HDR read with one negative channel keeps the other two.
        let mapped = reinhard().apply_pixel([2.0, -0.5, 0.5, 1.0]);
        close(mapped[0], 2.0 / 3.0);
        assert_eq!(mapped[1], 0.0);
        close(mapped[2], 1.0 / 3.0);
    }

    #[test]
    fn alpha_is_preserved_and_zero_coverage_pixels_are_bit_identical() {
        // Every visible alpha level passes through unchanged.
        for alpha in [0.0_f32, 0.37, 0.5, 1.0] {
            for mode in ToneMapMode::ALL {
                let mapped = grade(mode, 1.0, 1.0).apply_pixel([0.6, 0.3, 1.4, alpha]);
                assert_eq!(mapped[3].to_bits(), alpha.to_bits(), "{mode:?}");
            }
        }
        // A pixel without coverage is returned bit-identically, including its
        // extended-range colour: the compositor never shows it.
        for mode in ToneMapMode::ALL {
            let sample = [-4.0, 12.5, 0.25, 0.0];
            let mapped = grade(mode, 2.0, 2.0).apply_pixel(sample);
            assert_eq!(mapped.map(f32::to_bits), sample.map(f32::to_bits));
            let negative = [12.5, 0.25, 1.0, -0.5];
            assert_eq!(
                grade(mode, 1.0, 1.0).apply_pixel(negative).map(f32::to_bits),
                negative.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn extreme_inputs_stay_finite_and_within_the_display_range() {
        // gain 100 x white = 100 is the largest exposure the list can author.
        for mode in ToneMapMode::ALL {
            for gamma in [HDR_TONE_MAPPING_MIN_GAMMA, 1.0, HDR_TONE_MAPPING_MAX_GAMMA] {
                let mapped = grade(mode, HDR_TONE_MAPPING_MAX_GAIN, gamma).apply_pixel([1.0; 4]);
                for channel in &mapped[..3] {
                    assert!(channel.is_finite(), "{mode:?} produced {channel}");
                    assert!(
                        *channel >= 0.0 && *channel <= 1.0,
                        "{mode:?} at gamma {gamma} produced {channel}"
                    );
                }
            }
        }
        // The filmic curve pins everything above its white point to one.
        let above_white = map_channel(&hable_grade(), 1.0e6);
        assert!(above_white.is_finite());
        assert_eq!(above_white, 1.0);
    }

    #[test]
    fn out_of_range_controls_are_rejected() {
        for gain in [f32::NAN, f32::INFINITY, 0.0, 101.0] {
            assert_eq!(
                HdrToneMappingGrade::new(ToneMapMode::Reinhard, gain, 1.0),
                Err(HdrToneMappingError::Gain)
            );
        }
        for gamma in [f32::NAN, f32::INFINITY, 0.0, 11.0] {
            assert_eq!(
                HdrToneMappingGrade::new(ToneMapMode::Hable, 1.0, gamma),
                Err(HdrToneMappingError::Gamma)
            );
        }
        for gain in [HDR_TONE_MAPPING_MIN_GAIN, HDR_TONE_MAPPING_MAX_GAIN] {
            assert!(
                HdrToneMappingGrade::new(ToneMapMode::Reinhard, gain, 1.0).is_ok(),
                "gain {gain} is inside the declared range"
            );
        }
        for gamma in [HDR_TONE_MAPPING_MIN_GAMMA, HDR_TONE_MAPPING_MAX_GAMMA] {
            assert!(
                HdrToneMappingGrade::new(ToneMapMode::Reinhard, 1.0, gamma).is_ok(),
                "gamma {gamma} is inside the declared range"
            );
        }
    }

    #[test]
    fn checkpoints_run_once_per_chunk_and_once_at_the_end() {
        let grade = reinhard();
        let mut checkpoints = 0;
        let mut short = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_hdr_tone_mapping_f32_controlled(&mut short, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 2);
        // The frame entry point rewrites the same pixels the per-pixel grade
        // would, including the coverage skip.
        let mut frame = vec![
            [0.25, 1.0, 4.0, 1.0],
            [-1.0, 0.5, 2.0, 0.5],
            [9.0, 9.0, 9.0, 0.0],
        ];
        let expected = frame.iter().map(|pixel| grade.apply_pixel(*pixel)).collect::<Vec<_>>();
        grade.apply_to_frame(&mut frame);
        for (actual, expected) in frame.iter().zip(&expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
        close_pixel(frame[2], [9.0, 9.0, 9.0, 0.0]);
        let mut long = vec![[0.25, 0.25, 0.25, 1.0]; CHECKPOINT_STRIDE + 1];
        let mut checkpoints = 0;
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_hdr_tone_mapping_f32_controlled(&mut long, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 3);
    }
}
