//! The broadcast-safe effect from the built-in effect list.
//!
//! The list authors three controls: the broadcast locale, the way a colour is
//! made safe and the maximum signal amplitude in IRE. Broadcast safe is a point
//! operation, so it runs on the fused GPU point path as well as the CPU kernel.
//!
//! # Documented reading
//!
//! The list names the effect but does not define the maths, so this module fixes
//! one reading and documents it here. The list has no black-level control and
//! the engine works in a full-range float32 scene-linear pipeline, so the locale
//! only selects how the authored IRE amplitude is normalised into the unit
//! signal range:
//!
//! - `ntsc`: the NTSC composite standard carries a 7.5 IRE setup above black, so
//!   `limit = (ire - 7.5) / (100 - 7.5)` (the documented default 110 IRE is
//!   `1.1081`).
//! - `pal`: the PAL standard has no setup, so `limit = ire / 100` (the documented
//!   default 110 IRE is `1.1`).
//!
//! Both expressions are evaluated once by [`BroadcastSafeGrade::limit`] and
//! travel into the fused point uniform as a plain amplitude; the shader never
//! repeats the IRE conversion, so the two sides cannot disagree about rounding.
//!
//! # Safety modes
//!
//! With `c` the current straight-alpha colour sample, `Y = 0.299r + 0.587g +
//! 0.114b` the Rec.601 luma of the broadcast standard (deliberately not the
//! Rec.709 luma used elsewhere in the engine), `maxc = max(r, g, b)` and `limit`
//! the amplitude resolved above:
//!
//! - `reduce_luminance`: when `Y > limit`, `rgb *= limit / Y`. The single ratio
//!   keeps the hue and pulls the whole sample toward black.
//! - `reduce_saturation`: when `maxc > limit`, mix toward the luma with
//!   `t = (maxc - limit) / (maxc - Y)` and `rgb = rgb + (Y - rgb) * clamp(t, 0, 1)`.
//!   That weight is the one that lands the brightest channel exactly on `limit`,
//!   and mixing toward the luma leaves the luma itself untouched. `maxc - Y <=
//!   1e-6` is treated as already safe, so a grey sample is never degenerately
//!   scaled. A sample whose luma already exceeds the limit cannot be rescued by
//!   desaturation, so the weight clamps to one and the sample keeps its luma
//!   while losing all of its saturation; `clamp_rgb` is the hard ceiling.
//! - `key_out_unsafe`: an unsafe sample gets `alpha = 0`, a safe sample is left
//!   exactly as it arrived.
//! - `key_out_safe`: a safe sample gets `alpha = 0`, an unsafe sample is left
//!   exactly as it arrived.
//! - `clamp_rgb`: `rgb = clamp(rgb, 0, limit)` per channel. AE does not document
//!   this mode, but the list declares a fifth menu position, so this module fills
//!   it with the most conservative reading of "pull the signal back" and declares
//!   that reading here. The lower bound of zero also lifts negative HDR samples.
//!
//! # Unsafe samples
//!
//! The list does not define which samples the two keying modes call unsafe, so
//! this module declares one reading: a sample is unsafe when its brightest
//! channel exceeds `limit`, i.e. `maxc > limit`. That is exactly the test
//! `reduce_saturation` and `clamp_rgb` use, and it contains the `Y > limit` test
//! of `reduce_luminance`, so "unsafe" covers every sample that any of the
//! reducing modes would have to change.
//!
//! # Output
//!
//! Straight alpha throughout: a sample is never premultiplied. A sample with
//! `alpha <= 0` is returned unchanged, because the compositor short circuits
//! zero coverage and mapping such a pixel would only make the CPU and the GPU
//! disagree about a pixel that is never visible. The two keying modes author
//! coverage, but they only lower it, so no coverage repair pass is needed. Every
//! other mode passes the source alpha through. Nothing is clamped, so an HDR
//! sample keeps its extended range wherever it is already safe.
//!
//! Safety is content dependent rather than parameter dependent, so
//! [`BroadcastSafeGrade::is_identity`] is always false and the graph builder
//! appends the operation unconditionally.

/// Number of broadcast locales the list declares.
pub const BROADCAST_SAFE_LOCALE_COUNT: usize = 2;

/// Number of colour-safety modes the list declares.
pub const BROADCAST_SAFE_MODE_COUNT: usize = 5;

/// Lowest authored maximum signal amplitude, in IRE.
pub const BROADCAST_SAFE_MIN_IRE: f32 = 90.0;

/// Highest authored maximum signal amplitude, in IRE.
pub const BROADCAST_SAFE_MAX_IRE: f32 = 120.0;

/// The 7.5 IRE setup NTSC carries above the black level.
const NTSC_SETUP_IRE: f32 = 7.5;

/// The full-scale amplitude of the IRE scale.
const FULL_SCALE_IRE: f32 = 100.0;

/// Luma coefficients of the Rec.601 broadcast standard.
const REC_601_LUMA: [f32; 3] = [0.299, 0.587, 0.114];

/// A chroma this small cannot be pulled toward the luma without dividing by it.
const CHROMA_EPSILON: f32 = 1.0e-6;

/// Broadcast standard the authored amplitude is normalised against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastLocale {
    /// NTSC: 7.5 IRE setup, so the usable amplitude is `92.5` IRE wide.
    Ntsc,
    /// PAL: no setup, so the authored IRE is the usable amplitude in percent.
    Pal,
}

impl BroadcastLocale {
    /// Every locale in menu order.
    pub const ALL: [Self; BROADCAST_SAFE_LOCALE_COUNT] = [Self::Ntsc, Self::Pal];

    /// The key the effect list stores for this locale.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Ntsc => "ntsc",
            Self::Pal => "pal",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|locale| locale.as_key() == key)
    }

    /// The one-based menu number the list declares for this locale.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Resolves a one-based menu number, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::Ntsc),
            2 => Some(Self::Pal),
            _ => None,
        }
    }

    /// The zero-based number the fused point shader receives.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Ntsc => 0,
            Self::Pal => 1,
        }
    }

    /// The label the effect list declares for this locale.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Ntsc => "NTSC",
            Self::Pal => "PAL",
        }
    }
}

/// How a sample is made safe when it exceeds the authored amplitude.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastSafeMode {
    /// Scale the whole sample toward black until its luma reaches the limit.
    ReduceLuminance,
    /// Mix the sample toward its luma so its brightest channel is pulled down.
    ReduceSaturation,
    /// Key out the unsafe samples by lowering their alpha to zero.
    KeyOutUnsafe,
    /// Key out the safe samples by lowering their alpha to zero.
    KeyOutSafe,
    /// Clamp each channel into `0..=limit`.
    ClampRgb,
}

impl BroadcastSafeMode {
    /// Every safety mode in menu order.
    pub const ALL: [Self; BROADCAST_SAFE_MODE_COUNT] = [
        Self::ReduceLuminance,
        Self::ReduceSaturation,
        Self::KeyOutUnsafe,
        Self::KeyOutSafe,
        Self::ClampRgb,
    ];

    /// The key the effect list stores for this mode.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::ReduceLuminance => "reduce_luminance",
            Self::ReduceSaturation => "reduce_saturation",
            Self::KeyOutUnsafe => "key_out_unsafe",
            Self::KeyOutSafe => "key_out_safe",
            Self::ClampRgb => "clamp_rgb",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_key() == key)
    }

    /// The one-based menu number the list declares for this mode.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Resolves a one-based menu number, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::ReduceLuminance),
            2 => Some(Self::ReduceSaturation),
            3 => Some(Self::KeyOutUnsafe),
            4 => Some(Self::KeyOutSafe),
            5 => Some(Self::ClampRgb),
            _ => None,
        }
    }

    /// The zero-based number the fused point shader receives.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::ReduceLuminance => 0,
            Self::ReduceSaturation => 1,
            Self::KeyOutUnsafe => 2,
            Self::KeyOutSafe => 3,
            Self::ClampRgb => 4,
        }
    }

    /// The Chinese label the effect list declares for this mode.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReduceLuminance => "降低明亮度",
            Self::ReduceSaturation => "降低饱和度",
            Self::KeyOutUnsafe => "抠出不安全区域",
            Self::KeyOutSafe => "抠出安全区域",
            Self::ClampRgb => "逐通道钳位",
        }
    }
}

/// Why a broadcast-safe grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroadcastSafeError {
    /// The maximum signal amplitude is not a finite IRE value within `90..=120`.
    Ire,
}

impl std::fmt::Display for BroadcastSafeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ire => write!(
                formatter,
                "broadcast safe maximum signal amplitude must be finite and within 90..=120 IRE"
            ),
        }
    }
}

impl std::error::Error for BroadcastSafeError {}

/// A resolved broadcast-safe grade.
///
/// The authored IRE amplitude stays in the authored `90..=120` domain so a round
/// trip through the effect list preserves what the user typed; the kernel and
/// [`BroadcastSafeGrade::gpu_controls`] resolve it to a signal `limit` when they
/// run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BroadcastSafeGrade {
    locale: BroadcastLocale,
    mode: BroadcastSafeMode,
    ire: f32,
}

impl BroadcastSafeGrade {
    /// Resolves the authored controls, rejecting amplitudes outside `90..=120`.
    ///
    /// # Errors
    ///
    /// [`BroadcastSafeError::Ire`] when the authored amplitude is not finite or
    /// falls outside `90..=120` IRE. The locale and the safety mode are enums, so
    /// neither can be out of range.
    pub fn new(
        locale: BroadcastLocale,
        mode: BroadcastSafeMode,
        ire: f32,
    ) -> Result<Self, BroadcastSafeError> {
        if !ire.is_finite() || !(BROADCAST_SAFE_MIN_IRE..=BROADCAST_SAFE_MAX_IRE).contains(&ire) {
            return Err(BroadcastSafeError::Ire);
        }
        Ok(Self { locale, mode, ire })
    }

    /// The authored broadcast locale.
    #[must_use]
    pub const fn locale(&self) -> BroadcastLocale {
        self.locale
    }

    /// The authored colour-safety mode.
    #[must_use]
    pub const fn mode(&self) -> BroadcastSafeMode {
        self.mode
    }

    /// The authored maximum signal amplitude in `90..=120` IRE.
    #[must_use]
    pub const fn ire(&self) -> f32 {
        self.ire
    }

    /// The unit-range amplitude `limit` the authored IRE resolves to.
    ///
    /// NTSC normalises against its 7.5 IRE setup, PAL against full scale; the
    /// kernel and the fused point shader both consume this one value, so the IRE
    /// conversion runs exactly once.
    #[must_use]
    pub fn limit(&self) -> f32 {
        match self.locale {
            BroadcastLocale::Ntsc => {
                (self.ire - NTSC_SETUP_IRE) / (FULL_SCALE_IRE - NTSC_SETUP_IRE)
            }
            BroadcastLocale::Pal => self.ire / FULL_SCALE_IRE,
        }
    }

    /// Whether the authored state leaves the frame unchanged.
    ///
    /// Broadcast safety is content dependent: the most permissive settings still
    /// rewrite a sample that exceeds them, so the graph builder must never elide
    /// the operation.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        false
    }

    /// Applies the effect to one straight-alpha pixel.
    ///
    /// A pixel without coverage is returned unchanged: the compositor short
    /// circuits zero coverage, so mapping one would make the CPU and the GPU
    /// disagree about a pixel that is never visible. The two keying modes lower
    /// alpha to zero for the samples they reject and leave the rgb untouched;
    /// every other mode passes the source alpha through.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4]) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let limit = self.limit();
        let rgb = [pixel[0], pixel[1], pixel[2]];
        match self.mode {
            BroadcastSafeMode::ReduceLuminance => {
                let luma = luma(rgb);
                if luma > limit {
                    let scale = limit / luma;
                    [rgb[0] * scale, rgb[1] * scale, rgb[2] * scale, pixel[3]]
                } else {
                    pixel
                }
            }
            BroadcastSafeMode::ReduceSaturation => {
                let luma = luma(rgb);
                let maximum = maximum_channel(rgb);
                if maximum > limit {
                    let chroma = maximum - luma;
                    if chroma <= CHROMA_EPSILON {
                        return pixel;
                    }
                    let amount = ((maximum - limit) / chroma).clamp(0.0, 1.0);
                    [
                        rgb[0] + (luma - rgb[0]) * amount,
                        rgb[1] + (luma - rgb[1]) * amount,
                        rgb[2] + (luma - rgb[2]) * amount,
                        pixel[3],
                    ]
                } else {
                    pixel
                }
            }
            BroadcastSafeMode::KeyOutUnsafe => {
                if is_unsafe(rgb, limit) {
                    [rgb[0], rgb[1], rgb[2], 0.0]
                } else {
                    pixel
                }
            }
            BroadcastSafeMode::KeyOutSafe => {
                if is_unsafe(rgb, limit) {
                    pixel
                } else {
                    [rgb[0], rgb[1], rgb[2], 0.0]
                }
            }
            BroadcastSafeMode::ClampRgb => [
                rgb[0].clamp(0.0, limit),
                rgb[1].clamp(0.0, limit),
                rgb[2].clamp(0.0, limit),
                pixel[3],
            ],
        }
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [mode number, limit, 0, 0]` with the safety mode as its
    /// zero-based number and the amplitude already resolved by
    /// [`BroadcastSafeGrade::limit`], and `extra0 = [locale number, 0, 0, 0]`.
    /// The shader reads only `params`, but the locale index stays in the payload
    /// so a locale change is visible in the uniform (and in the operation hash)
    /// even when both locales happen to resolve to the same limit.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [self.mode.as_u32() as f32, self.limit(), 0.0, 0.0],
            [self.locale.as_u32() as f32, 0.0, 0.0, 0.0],
        )
    }
}

/// Rec.601 luma of a straight-alpha RGB sample.
///
/// The expression is shared verbatim with the fused WGSL branch so the two sides
/// agree on the limit boundary.
fn luma(rgb: [f32; 3]) -> f32 {
    REC_601_LUMA[0] * rgb[0] + REC_601_LUMA[1] * rgb[1] + REC_601_LUMA[2] * rgb[2]
}

/// The brightest colour channel of a straight-alpha RGB sample.
fn maximum_channel(rgb: [f32; 3]) -> f32 {
    rgb[0].max(rgb[1]).max(rgb[2])
}

/// Whether a sample exceeds the legal signal amplitude.
///
/// The predicate is documented on the module: the brightest channel is compared
/// with the limit, which is the test the reducing modes share.
fn is_unsafe(rgb: [f32; 3], limit: f32) -> bool {
    maximum_channel(rgb) > limit
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether the grade ran. The graph builder appends the operation
/// unconditionally, so a compiled graph always reaches this kernel and the
/// mapping always applies.
pub fn broadcast_safe_frame_f32(pixels: &mut [[f32; 4]], grade: &BroadcastSafeGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_broadcast_safe_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_broadcast_safe_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &BroadcastSafeGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // Broadcast safety is a point grade over the three colour channels, with a
    // coverage key in the two keying modes. Point grades are bandwidth bound:
    // measured on a 1280x720 frame they run at the single thread DRAM stream
    // rate, so splitting the frame across cores buys nothing. The 4096-pixel
    // chunks exist purely for cancellation checkpoints.
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

    fn grade(locale: BroadcastLocale, mode: BroadcastSafeMode, ire: f32) -> BroadcastSafeGrade {
        BroadcastSafeGrade::new(locale, mode, ire).expect("grade")
    }

    fn pal(mode: BroadcastSafeMode) -> BroadcastSafeGrade {
        grade(BroadcastLocale::Pal, mode, 110.0)
    }

    fn ntsc(mode: BroadcastSafeMode) -> BroadcastSafeGrade {
        grade(BroadcastLocale::Ntsc, mode, 110.0)
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
    fn menu_numbers_are_one_based_and_round_trip() {
        assert_eq!(BroadcastLocale::ALL.len(), BROADCAST_SAFE_LOCALE_COUNT);
        for (index, locale) in BroadcastLocale::ALL.into_iter().enumerate() {
            assert_eq!(locale.as_u32() as usize, index);
            assert_eq!(locale.as_menu() as usize, index + 1);
            assert_eq!(BroadcastLocale::from_menu(locale.as_menu()), Some(locale));
            assert_eq!(BroadcastLocale::from_key(locale.as_key()), Some(locale));
        }
        assert_eq!(BroadcastLocale::from_menu(0), None);
        assert_eq!(
            BroadcastLocale::from_menu(BROADCAST_SAFE_LOCALE_COUNT as u32 + 1),
            None
        );
        assert_eq!(BroadcastLocale::from_key("mystery"), None);

        assert_eq!(BroadcastSafeMode::ALL.len(), BROADCAST_SAFE_MODE_COUNT);
        for (index, mode) in BroadcastSafeMode::ALL.into_iter().enumerate() {
            assert_eq!(mode.as_u32() as usize, index);
            assert_eq!(mode.as_menu() as usize, index + 1);
            assert_eq!(BroadcastSafeMode::from_menu(mode.as_menu()), Some(mode));
            assert_eq!(BroadcastSafeMode::from_key(mode.as_key()), Some(mode));
        }
        assert_eq!(BroadcastSafeMode::from_menu(0), None);
        assert_eq!(
            BroadcastSafeMode::from_menu(BROADCAST_SAFE_MODE_COUNT as u32 + 1),
            None
        );
        assert_eq!(BroadcastSafeMode::from_key("mystery"), None);
    }

    #[test]
    fn the_locale_selects_the_documented_limit_mapping() {
        // The default 110 IRE normalises to 102.5 / 92.5 on NTSC and to 1.1 on
        // PAL, and the two standards differ by more than a rounding step.
        close(ntsc(BroadcastSafeMode::ClampRgb).limit(), 102.5 / 92.5);
        close(pal(BroadcastSafeMode::ClampRgb).limit(), 1.1);
        assert!(
            ntsc(BroadcastSafeMode::ClampRgb).limit() > pal(BroadcastSafeMode::ClampRgb).limit()
        );

        // At 100 IRE the setup cancels out, so both standards agree exactly.
        close(
            grade(BroadcastLocale::Ntsc, BroadcastSafeMode::ClampRgb, 100.0).limit(),
            1.0,
        );
        close(
            grade(BroadcastLocale::Pal, BroadcastSafeMode::ClampRgb, 100.0).limit(),
            1.0,
        );

        // A sample between the two limits is safe on NTSC and unsafe on PAL,
        // which is the observable difference between the two locales.
        let sample = [1.105, 0.0, 0.0, 1.0];
        assert_eq!(
            ntsc(BroadcastSafeMode::ClampRgb).apply(sample).map(f32::to_bits),
            sample.map(f32::to_bits)
        );
        close_pixel(
            pal(BroadcastSafeMode::ClampRgb).apply(sample),
            [1.1, 0.0, 0.0, 1.0],
        );
    }

    #[test]
    fn reducing_luminance_scales_an_over_limit_sample_toward_black() {
        let grade = pal(BroadcastSafeMode::ReduceLuminance);
        let sample = [1.5, 1.2, 1.0, 0.5];
        let luma = luma([sample[0], sample[1], sample[2]]);
        assert!(luma > grade.limit());
        let mapped = grade.apply(sample);
        // The single ratio lands the result exactly on the limit and keeps the
        // hue, so every channel is scaled by the same factor.
        close(luma_of(mapped), grade.limit());
        close(mapped[0] / sample[0], mapped[1] / sample[1]);
        close(mapped[1] / sample[1], mapped[2] / sample[2]);
        assert_eq!(mapped[3], 0.5);
        // A sample already under the limit is the exact identity.
        let safe = [0.4, 0.3, 0.2, 1.0];
        assert_eq!(grade.apply(safe).map(f32::to_bits), safe.map(f32::to_bits));
    }

    #[test]
    fn reducing_saturation_mixes_an_over_limit_sample_toward_its_luma() {
        let grade = pal(BroadcastSafeMode::ReduceSaturation);
        let sample = [1.5, 0.2, 0.2, 0.8];
        let luma = luma([sample[0], sample[1], sample[2]]);
        let limit = grade.limit();
        assert!(maximum_channel([sample[0], sample[1], sample[2]]) > limit);
        let mapped = grade.apply(sample);
        // Mixing toward the luma preserves it exactly and lands the brightest
        // channel on the limit, which is the weight this mode solves for.
        close(luma_of(mapped), luma);
        close(maximum_channel([mapped[0], mapped[1], mapped[2]]), limit);
        close(mapped[1], mapped[2]);
        assert!(mapped[1] > sample[1], "the dim channels rise toward grey");
        assert_eq!(mapped[3], 0.8);
    }

    #[test]
    fn a_chroma_within_the_epsilon_is_already_safe() {
        let grade = pal(BroadcastSafeMode::ReduceSaturation);
        let grey = [1.3, 1.3, 1.3, 1.0];
        assert_eq!(
            grade.apply(grey).map(f32::to_bits),
            grey.map(f32::to_bits),
            "a grey sample has no chroma to remove"
        );
        // When the luma already exceeds the limit desaturation cannot rescue the
        // sample: the weight clamps to one, so it loses all of its saturation and
        // keeps a luma that is still over the limit.
        let over_bright = [2.0, 2.0, 0.5, 1.0];
        let limit = grade.limit();
        let mapped = grade.apply(over_bright);
        let grey = luma([2.0, 2.0, 0.5]);
        close(mapped[0], grey);
        close(mapped[1], grey);
        close(mapped[2], grey);
        assert!(grey > limit);
        assert_eq!(mapped[3], 1.0);
    }

    #[test]
    fn clamping_rewrites_each_channel_independently() {
        let grade = pal(BroadcastSafeMode::ClampRgb);
        close_pixel(grade.apply([1.4, -0.2, 0.8, 1.0]), [1.1, 0.0, 0.8, 1.0]);
        // The HDR headroom of each channel is clipped to the same ceiling.
        close_pixel(grade.apply([3.0, 2.0, 1.0, 0.5]), [1.1, 1.1, 1.0, 0.5]);
    }

    #[test]
    fn a_sample_exactly_at_the_limit_is_left_alone() {
        let limit = pal(BroadcastSafeMode::ClampRgb).limit();
        let boundary = [limit, limit, limit, 1.0];
        // Clamping and desaturation are exact at the boundary: both compare with
        // the limit and neither rewrites an equal sample.
        for mode in [
            BroadcastSafeMode::ClampRgb,
            BroadcastSafeMode::ReduceSaturation,
        ] {
            assert_eq!(
                pal(mode).apply(boundary).map(f32::to_bits),
                boundary.map(f32::to_bits),
                "{mode:?} must not rewrite a sample at the limit"
            );
        }
        // The luminance path divides only when the luma is strictly greater, so
        // the boundary sample can move by one rounding step at most.
        close_pixel(
            pal(BroadcastSafeMode::ReduceLuminance).apply(boundary),
            boundary,
        );
    }

    #[test]
    fn the_two_keying_modes_invert_each_other() {
        let unsafe_sample = [2.0, 0.0, 0.0, 1.0];
        let safe_sample = [0.4, 0.2, 0.1, 1.0];
        let boundary = [1.1, 1.1, 1.1, 1.0];

        let key_unsafe = pal(BroadcastSafeMode::KeyOutUnsafe);
        // The keyed sample keeps its rgb and loses its coverage.
        assert_eq!(
            key_unsafe.apply(unsafe_sample).map(f32::to_bits),
            [2.0_f32, 0.0, 0.0, 0.0].map(f32::to_bits)
        );
        assert_eq!(
            key_unsafe.apply(safe_sample).map(f32::to_bits),
            safe_sample.map(f32::to_bits)
        );
        assert_eq!(
            key_unsafe.apply(boundary).map(f32::to_bits),
            boundary.map(f32::to_bits)
        );

        let key_safe = pal(BroadcastSafeMode::KeyOutSafe);
        assert_eq!(
            key_safe.apply(unsafe_sample).map(f32::to_bits),
            unsafe_sample.map(f32::to_bits)
        );
        assert_eq!(
            key_safe.apply(safe_sample).map(f32::to_bits),
            [0.4_f32, 0.2, 0.1, 0.0].map(f32::to_bits)
        );
        assert_eq!(
            key_safe.apply(boundary).map(f32::to_bits),
            [1.1_f32, 1.1, 1.1, 0.0].map(f32::to_bits)
        );
    }

    #[test]
    fn a_sample_without_coverage_is_returned_unchanged() {
        for mode in BroadcastSafeMode::ALL {
            let grade = pal(mode);
            for sample in [
                [5.0, 5.0, 5.0, 0.0],
                [5.0, 0.0, 0.0, -1.0],
                [0.0, 0.0, 0.0, 0.0],
            ] {
                assert_eq!(
                    grade.apply(sample).map(f32::to_bits),
                    sample.map(f32::to_bits),
                    "{mode:?} must skip a zero coverage sample"
                );
            }
        }
    }

    #[test]
    fn alpha_is_preserved_by_every_mode_that_does_not_key() {
        for (mode, sample) in [
            (BroadcastSafeMode::ReduceLuminance, [1.5, 1.2, 1.0, 0.25]),
            (BroadcastSafeMode::ReduceSaturation, [1.5, 0.2, 0.2, 0.25]),
            (BroadcastSafeMode::ClampRgb, [2.0, 0.0, 0.0, 0.25]),
        ] {
            assert_eq!(pal(mode).apply(sample)[3], 0.25, "{mode:?}");
        }
    }

    #[test]
    fn an_hdr_sample_is_reduced_without_being_clamped_to_unit_range() {
        let sample = [3.0, 1.0, 0.5, 1.0];
        let mapped = pal(BroadcastSafeMode::ReduceLuminance).apply(sample);
        close(luma_of(mapped), 1.1);
        assert!(
            mapped[0] > 1.0,
            "the luminance mode scales, it does not clamp: {mapped:?}"
        );
        // The clamping mode is the one with a hard ceiling.
        close_pixel(
            pal(BroadcastSafeMode::ClampRgb).apply(sample),
            [1.1, 1.0, 0.5, 1.0],
        );
    }

    #[test]
    fn out_of_range_amplitudes_are_rejected() {
        for ire in [f32::NAN, f32::INFINITY, 89.0, 121.0] {
            assert_eq!(
                BroadcastSafeGrade::new(
                    BroadcastLocale::Ntsc,
                    BroadcastSafeMode::ReduceLuminance,
                    ire,
                ),
                Err(BroadcastSafeError::Ire)
            );
        }
        assert!(BroadcastSafeGrade::new(
            BroadcastLocale::Pal,
            BroadcastSafeMode::ClampRgb,
            BROADCAST_SAFE_MIN_IRE
        )
        .is_ok());
        assert!(BroadcastSafeGrade::new(
            BroadcastLocale::Pal,
            BroadcastSafeMode::ClampRgb,
            BROADCAST_SAFE_MAX_IRE
        )
        .is_ok());
    }

    #[test]
    fn the_controls_pack_the_fused_point_uniform() {
        let ntsc_grade = grade(
            BroadcastLocale::Ntsc,
            BroadcastSafeMode::KeyOutUnsafe,
            BROADCAST_SAFE_MIN_IRE,
        );
        let (params, extra0) = ntsc_grade.gpu_controls();
        assert_eq!(params[0], 2.0);
        close(params[1], 82.5 / 92.5);
        assert_eq!(params[2], 0.0);
        assert_eq!(params[3], 0.0);
        assert_eq!(extra0, [0.0; 4]);

        let pal_grade = grade(
            BroadcastLocale::Pal,
            BroadcastSafeMode::ClampRgb,
            BROADCAST_SAFE_MAX_IRE,
        );
        let (params, extra0) = pal_grade.gpu_controls();
        assert_eq!(params[0], 4.0);
        close(params[1], 1.2);
        assert_eq!(extra0[0], 1.0);
        assert_eq!(extra0[1..], [0.0, 0.0, 0.0]);
    }

    #[test]
    fn safety_is_never_an_identity() {
        for mode in BroadcastSafeMode::ALL {
            assert!(
                !pal(mode).is_identity(),
                "{mode:?} still rewrites samples that exceed the limit"
            );
        }
    }

    #[test]
    fn the_frame_kernel_matches_the_per_pixel_grade() {
        let grade = grade(
            BroadcastLocale::Ntsc,
            BroadcastSafeMode::ReduceSaturation,
            100.0,
        );
        let mut pixels = vec![
            [0.2, 0.4, 0.6, 1.0],
            [1.9, 0.1, 0.4, 0.25],
            [0.0, 0.0, 0.0, 0.0],
            [1.2, 1.2, 1.2, 0.5],
        ];
        let expected = pixels.iter().map(|pixel| grade.apply(*pixel)).collect::<Vec<_>>();
        assert!(broadcast_safe_frame_f32(&mut pixels, &grade));
        for (actual, expected) in pixels.iter().zip(&expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    #[test]
    fn checkpoints_run_once_per_chunk_and_once_at_the_end() {
        let grade = pal(BroadcastSafeMode::ClampRgb);
        let mut checkpoints = 0;
        let mut short = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_broadcast_safe_f32_controlled(&mut short, &grade, &mut checkpoint)
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
            apply_broadcast_safe_f32_controlled(&mut long, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 3);
    }

    fn luma_of(pixel: [f32; 4]) -> f32 {
        luma([pixel[0], pixel[1], pixel[2]])
    }
}
