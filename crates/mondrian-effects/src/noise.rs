//! Film-style noise (FLM-02).
//!
//! The effect perturbs every pixel with a hash of its integer position, so the
//! grain is deterministic: the same frame always receives the same grain, which
//! keeps the render cache identity honest and lets the CPU and GPU paths be
//! compared pixel-by-pixel.
//!
//! Two grain types are declared by the list:
//!
//! * `rgb` perturbs every colour channel independently, which adds coloured
//!   speckles.
//! * `hls` perturbs the three channels by the same amount, so the differences
//!   between the channels are preserved and only the luminance jitters.
//!
//! The authored `amount` is a percentage of the full signed range; one unit of
//! noise therefore spans `[-1, 1]` in the working scene-linear space. Coverage
//! is never touched, and the sampled colour is only clamped when the author
//! enables clipping, which keeps extended-range highlights intact by default.

use std::fmt;

/// The largest authored noise amount, as a percentage of the full range.
pub const NOISE_MAX_AMOUNT_PERCENT: f32 = 100.0;

/// Salt mixed into the red channel hash of the coloured grain.
pub const NOISE_SALT_RED: u32 = 0x9E37_79B9u32;

/// Salt mixed into the green channel hash of the coloured grain.
pub const NOISE_SALT_GREEN: u32 = 0x85EB_CA6Bu32;

/// Salt mixed into the blue channel hash of the coloured grain.
pub const NOISE_SALT_BLUE: u32 = 0x27D4_EB2Fu32;

/// Salt mixed into the shared luminance hash of the monochrome grain.
pub const NOISE_SALT_LUMINANCE: u32 = 0xC2B2_AE35u32;

/// The grain shape declared by the effect list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseType {
    /// Independent per-channel speckles.
    Rgb,
    /// A shared luminance jitter that keeps the chroma differences intact.
    Hls,
}

impl NoiseType {
    /// Maps the authored menu value onto a grain type.
    ///
    /// The list declares the menu as `0-1`, so `0` is the coloured grain and
    /// `1` is the luminance grain.
    #[must_use]
    pub fn from_menu(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::Rgb),
            1 => Some(Self::Hls),
            _ => None,
        }
    }

    /// The stable authoring key of this grain type.
    #[must_use]
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Hls => "hls",
        }
    }

    /// Resolves the stable authoring key back into a grain type.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "rgb" => Some(Self::Rgb),
            "hls" => Some(Self::Hls),
            _ => None,
        }
    }

    /// The packed value used by the GPU uniform and by the hash tag.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Rgb => 0,
            Self::Hls => 1,
        }
    }
}

/// Why a [`NoiseGrade`] could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseError {
    /// The authored amount is not a finite value inside the declared range.
    Amount,
}

impl fmt::Display for NoiseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Amount => write!(
                formatter,
                "noise amount must be finite and between 0 and {NOISE_MAX_AMOUNT_PERCENT}"
            ),
        }
    }
}

impl std::error::Error for NoiseError {}

/// The integer hash shared by the CPU kernel and the fused GPU pass.
///
/// The arithmetic is deliberately `u32` only: wrap-around multiplication,
/// shifts and exclusive ors behave identically in Rust and WGSL, so both paths
/// produce bit-identical grain.
#[must_use]
pub fn noise_hash(x: u32, y: u32, salt: u32) -> u32 {
    let mut hash = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B) ^ salt;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x7FEB_352D);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x846C_A68B);
    hash ^ (hash >> 16)
}

/// Resolves one signed noise sample in `[-1, 1]` for a pixel and salt.
#[must_use]
pub fn noise_sample(x: u32, y: u32, salt: u32) -> f32 {
    let bits = (noise_hash(x, y, salt) >> 8) & 0xFFFF;
    bits as f32 / 65_535.0 * 2.0 - 1.0
}

/// A validated grain configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseGrade {
    amount: f32,
    noise_type: NoiseType,
    clip_output: bool,
}

impl NoiseGrade {
    /// Builds a grain configuration.
    ///
    /// `amount` is a fraction of the full signed range in `[0, 1]`; the graph
    /// builder converts the authored percentage before calling this.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError::Amount`] when the amount is not finite or falls
    /// outside `[0, 1]`.
    pub fn new(amount: f32, noise_type: NoiseType, clip_output: bool) -> Result<Self, NoiseError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(NoiseError::Amount);
        }
        Ok(Self { amount, noise_type, clip_output })
    }

    /// The grain amount as a fraction of the full signed range.
    #[must_use]
    pub fn amount(&self) -> f32 {
        self.amount
    }

    /// The declared grain type.
    #[must_use]
    pub fn noise_type(&self) -> NoiseType {
        self.noise_type
    }

    /// Whether the sampled colour is clipped into `[0, 1]`.
    #[must_use]
    pub fn clips_output(&self) -> bool {
        self.clip_output
    }

    /// Whether the effect leaves the frame untouched.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.amount <= 1.0e-6
    }

    /// The signed grain applied to one channel of a pixel.
    fn channel_noise(&self, x: u32, y: u32, salt: u32) -> f32 {
        self.amount * noise_sample(x, y, salt)
    }

    /// Applies the grain to one straight-alpha scene-linear pixel.
    ///
    /// A pixel with no coverage is returned untouched: it contributes nothing
    /// to the composite, and the compositor short-circuits the effect chain on
    /// zero coverage, so perturbing it would make the two paths disagree.
    #[must_use]
    pub fn apply(&self, pixel: [f32; 4], x: usize, y: usize) -> [f32; 4] {
        if pixel[3] <= 0.0 {
            return pixel;
        }
        let column = u32::try_from(x).unwrap_or(u32::MAX);
        let row = u32::try_from(y).unwrap_or(u32::MAX);
        let noise = match self.noise_type {
            NoiseType::Rgb => [
                self.channel_noise(column, row, NOISE_SALT_RED),
                self.channel_noise(column, row, NOISE_SALT_GREEN),
                self.channel_noise(column, row, NOISE_SALT_BLUE),
            ],
            NoiseType::Hls => {
                let delta = self.channel_noise(column, row, NOISE_SALT_LUMINANCE);
                [delta, delta, delta]
            }
        };
        let mut colour = [
            pixel[0] + noise[0],
            pixel[1] + noise[1],
            pixel[2] + noise[2],
        ];
        if self.clip_output {
            for value in &mut colour {
                *value = value.clamp(0.0, 1.0);
            }
        }
        [colour[0], colour[1], colour[2], pixel[3]]
    }

    /// The packed controls used by the fused GPU pass.
    ///
    /// `params` is `[amount, type, clip, 0]`; the grain salts are compile-time
    /// constants shared with the shader.
    #[must_use]
    pub fn gpu_controls(&self) -> [f32; 4] {
        [
            self.amount,
            f32::from(self.noise_type.as_u8()),
            if self.clip_output { 1.0 } else { 0.0 },
            0.0,
        ]
    }
}

/// Applies the grain to a whole frame.
///
/// Returns `false` when the frame geometry is unusable.
pub fn noise_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: NoiseGrade,
) -> bool {
    let Some(expected) = width.checked_mul(height) else {
        return false;
    };
    if width == 0 || height == 0 || pixels.len() != expected {
        return false;
    }
    for (index, pixel) in pixels.iter_mut().enumerate() {
        *pixel = grade.apply(*pixel, index % width, index / width);
    }
    true
}

/// Applies the grain to a frame region, honouring the checkpoint budget.
///
/// # Errors
///
/// Propagates the checkpoint error unchanged.
pub(crate) fn apply_noise_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: NoiseGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(expected) = width.checked_mul(height) else {
        return Ok(false);
    };
    if width == 0 || height == 0 || pixels.len() != expected {
        return Ok(false);
    }
    for (index, pixel) in pixels.iter_mut().enumerate() {
        *pixel = grade.apply(*pixel, index % width, index / width);
        if index % 4096 == 0 {
            checkpoint()?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grade(amount: f32, noise_type: NoiseType, clip: bool) -> NoiseGrade {
        NoiseGrade::new(amount, noise_type, clip).expect("valid grade")
    }

    #[test]
    fn the_amount_range_is_enforced() {
        assert!(NoiseGrade::new(-0.01, NoiseType::Rgb, false).is_err());
        assert!(NoiseGrade::new(1.01, NoiseType::Rgb, false).is_err());
        assert!(NoiseGrade::new(f32::NAN, NoiseType::Rgb, false).is_err());
        assert!(NoiseGrade::new(f32::INFINITY, NoiseType::Rgb, false).is_err());
        assert!(NoiseGrade::new(0.0, NoiseType::Rgb, false).is_ok());
        assert!(NoiseGrade::new(1.0, NoiseType::Rgb, false).is_ok());
    }

    #[test]
    fn the_menu_maps_onto_the_declared_grain_types() {
        assert_eq!(NoiseType::from_menu(0), Some(NoiseType::Rgb));
        assert_eq!(NoiseType::from_menu(1), Some(NoiseType::Hls));
        assert_eq!(NoiseType::from_menu(2), None);
        assert_eq!(NoiseType::from_key("hls"), Some(NoiseType::Hls));
        assert_eq!(NoiseType::from_key("rgb"), Some(NoiseType::Rgb));
        assert_eq!(NoiseType::from_key("other"), None);
    }

    #[test]
    fn a_zero_amount_is_the_identity() {
        let frame = [[0.25_f32, 0.5, 0.75, 0.4], [2.0, -0.5, 1.5, 1.0]];
        let mut applied = frame;
        assert!(noise_frame_f32(
            &mut applied,
            2,
            1,
            grade(0.0, NoiseType::Rgb, false)
        ));
        assert_eq!(applied, frame);
        assert!(grade(0.0, NoiseType::Hls, true).is_identity());
        assert!(!grade(0.01, NoiseType::Hls, true).is_identity());
    }

    #[test]
    fn the_hash_is_deterministic_and_position_dependent() {
        let first = noise_sample(3, 4, NOISE_SALT_RED);
        assert_eq!(first, noise_sample(3, 4, NOISE_SALT_RED));
        assert!((0.0..=1.0).contains(&(first.abs())));
        assert_ne!(first, noise_sample(4, 4, NOISE_SALT_RED));
        assert_ne!(first, noise_sample(3, 4, NOISE_SALT_LUMINANCE));
    }

    #[test]
    fn noise_samples_are_spread_over_the_signed_range() {
        let mut negative = 0;
        let mut positive = 0;
        for y in 0..64u32 {
            for x in 0..64u32 {
                if noise_sample(x, y, NOISE_SALT_RED) < 0.0 {
                    negative += 1;
                } else {
                    positive += 1;
                }
            }
        }
        assert!(
            negative > 1_500 && positive > 1_500,
            "{negative} / {positive}"
        );
    }

    #[test]
    fn pixels_without_coverage_are_untouched() {
        let empty = [0.0_f32, 0.0, 0.0, 0.0];
        assert_eq!(grade(1.0, NoiseType::Rgb, false).apply(empty, 5, 6), empty);
        assert_eq!(grade(1.0, NoiseType::Hls, true).apply(empty, 5, 6), empty);
    }

    #[test]
    fn coverage_and_alpha_are_preserved() {
        let source = [0.4_f32, 0.4, 0.4, 0.25];
        let applied = grade(0.5, NoiseType::Rgb, false).apply(source, 7, 9);
        assert_eq!(applied[3], 0.25);
    }

    #[test]
    fn coloured_grain_perturbs_the_channels_independently() {
        let source = [0.5_f32, 0.5, 0.5, 1.0];
        let applied = grade(0.5, NoiseType::Rgb, false).apply(source, 11, 13);
        let deltas = [applied[0] - 0.5, applied[1] - 0.5, applied[2] - 0.5];
        assert_ne!(deltas[0], deltas[1]);
        assert_ne!(deltas[1], deltas[2]);
    }

    #[test]
    fn luminance_grain_keeps_the_chroma_differences() {
        let source = [0.2_f32, 0.5, 0.8, 1.0];
        let applied = grade(0.5, NoiseType::Hls, false).apply(source, 2, 3);
        let deltas = [applied[0] - 0.2, applied[1] - 0.5, applied[2] - 0.8];
        assert!((deltas[0] - deltas[1]).abs() < 1.0e-6);
        assert!((deltas[1] - deltas[2]).abs() < 1.0e-6);
        assert_ne!(deltas[0], 0.0);
    }

    #[test]
    fn clipping_only_happens_when_the_author_asks_for_it() {
        let source = [0.0_f32, 1.0, 0.5, 1.0];
        let mut saw_extended = false;
        let mut saw_clipped = false;
        for x in 0..64usize {
            let raw = grade(1.0, NoiseType::Rgb, false).apply(source, x, 0);
            let clipped = grade(1.0, NoiseType::Rgb, true).apply(source, x, 0);
            if raw[0] < 0.0 || raw[1] > 1.0 {
                saw_extended = true;
                assert!(clipped[0] >= 0.0 && clipped[1] <= 1.0);
            }
            if clipped[0] >= 0.0 && clipped[1] <= 1.0 {
                saw_clipped = true;
            }
        }
        assert!(saw_extended && saw_clipped);
    }

    #[test]
    fn the_frame_entry_rejects_mismatched_geometry() {
        let mut frame = [[0.0_f32; 4]; 6];
        assert!(!noise_frame_f32(
            &mut frame,
            2,
            2,
            grade(0.1, NoiseType::Rgb, false)
        ));
        assert!(!noise_frame_f32(
            &mut frame,
            0,
            6,
            grade(0.1, NoiseType::Rgb, false)
        ));
        assert!(noise_frame_f32(
            &mut frame,
            3,
            2,
            grade(0.1, NoiseType::Rgb, false)
        ));
    }

    #[test]
    fn the_controlled_entry_matches_the_frame_entry() {
        let mut frame = [[0.1_f32, 0.2, 0.3, 0.5]; 24];
        let mut controlled = frame;
        let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
        assert!(apply_noise_f32_controlled(
            &mut controlled,
            6,
            4,
            grade(0.4, NoiseType::Hls, false),
            &mut checkpoint
        )
        .unwrap_or(false));
        assert!(noise_frame_f32(
            &mut frame,
            6,
            4,
            grade(0.4, NoiseType::Hls, false)
        ));
        assert_eq!(controlled, frame);
    }

    #[test]
    fn the_gpu_controls_carry_the_amount_type_and_clipping() {
        let controls = grade(0.25, NoiseType::Hls, true).gpu_controls();
        assert_eq!(controls[0], 0.25);
        assert_eq!(controls[1], 1.0);
        assert_eq!(controls[2], 1.0);
        assert_eq!(controls[3], 0.0);
    }
}
