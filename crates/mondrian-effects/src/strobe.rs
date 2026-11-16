//! Strobe: flash the frame to a colour, or drop its alpha, on a periodic or
//! randomised schedule.
//!
//! The effect is a pure point operation. Whether a frame is flashing is decided
//! while the graph is built, from the authored schedule and the frame time, so
//! the kernel only applies the operator blend. There is no halo, no scratch
//! plane, and no ordering constraint beyond the point-op chain.
//!
//! Timing. One cycle lasts `duration + interval` seconds: the strobe is active
//! while `phase < duration` and idle for the remaining `interval`. The interval
//! is the gap between flashes, not the period between their starts, so a
//! duration equal to zero never flashes. Random flashes are drawn once per
//! cycle from a deterministic hash of `random_seed` and the cycle index, which
//! keeps a single flash stable across its frames and reproducible on rebuild.
//!
//! Colour mode blends the operator result over the source by `weight`; mask
//! mode drops the source alpha by the same weight and leaves the colour alone.

use std::hash::{Hash, Hasher};

use thiserror::Error;

/// Documented flash colour.
pub const STROBE_DEFAULT_COLOR: [f32; 3] = [1.0, 1.0, 1.0];

/// Documented blend with the original image.
pub const STROBE_DEFAULT_BLEND_WITH_ORIGINAL: f32 = 0.0;
/// Documented minimum blend with the original image.
pub const STROBE_MIN_BLEND_WITH_ORIGINAL: f32 = 0.0;
/// Documented maximum blend with the original image.
pub const STROBE_MAX_BLEND_WITH_ORIGINAL: f32 = 100.0;

/// Documented flash duration in seconds.
pub const STROBE_DEFAULT_DURATION: f32 = 0.5;
/// Documented minimum flash duration in seconds.
pub const STROBE_MIN_DURATION: f32 = 0.0;
/// Documented maximum flash duration in seconds.
pub const STROBE_MAX_DURATION: f32 = 32_000.0;

/// Documented gap between flashes in seconds.
pub const STROBE_DEFAULT_INTERVAL: f32 = 1.0;
/// Documented minimum gap between flashes in seconds.
pub const STROBE_MIN_INTERVAL: f32 = 0.0;
/// Documented maximum gap between flashes in seconds.
pub const STROBE_MAX_INTERVAL: f32 = 32_000.0;

/// Documented probability that a cycle flashes.
pub const STROBE_DEFAULT_RANDOM_PROBABILITY: f32 = 0.0;
/// Documented minimum probability that a cycle flashes.
pub const STROBE_MIN_RANDOM_PROBABILITY: f32 = 0.0;
/// Documented maximum probability that a cycle flashes.
pub const STROBE_MAX_RANDOM_PROBABILITY: f32 = 100.0;

/// Documented random seed.
pub const STROBE_DEFAULT_RANDOM_SEED: f32 = 0.0;
/// Documented minimum random seed.
pub const STROBE_MIN_RANDOM_SEED: f32 = 0.0;
/// Documented maximum random seed.
pub const STROBE_MAX_RANDOM_SEED: f32 = 100_000.0;

/// Quantisation scale of the bitwise operators, which are defined on 8-bit
/// channels.
const BYTE_SCALE: f32 = 255.0;

/// What the flash does to the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrobeMode {
    /// Replace the source colour with the flash colour, blended by strength.
    ColorOnly,
    /// Drop the source alpha to the flash strength and keep the colour.
    MasksLayer,
}

impl StrobeMode {
    /// Every mode, in menu order.
    pub const ALL: [Self; 2] = [Self::ColorOnly, Self::MasksLayer];

    /// Stable menu key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::ColorOnly => "color_only",
            Self::MasksLayer => "masks_layer",
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "color_only" => Some(Self::ColorOnly),
            "masks_layer" => Some(Self::MasksLayer),
            _ => None,
        }
    }

    /// Stable numeric tag used by the graph, plan, and shader.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::ColorOnly => 1,
            Self::MasksLayer => 2,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ColorOnly => "仅颜色",
            Self::MasksLayer => "蒙版图层",
        }
    }
}

/// How the flash colour is combined with the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrobeOperator {
    /// Replace the source with the flash colour.
    Copy,
    /// Add the flash colour.
    Add,
    /// Subtract the flash colour.
    Subtract,
    /// Multiply by the flash colour.
    Multiply,
    /// Absolute difference.
    Difference,
    /// Bitwise AND on 8-bit channels.
    And,
    /// Bitwise OR on 8-bit channels.
    Or,
    /// Bitwise XOR on 8-bit channels.
    Xor,
    /// Per-channel maximum.
    Lighter,
    /// Per-channel minimum.
    Darker,
    /// Per-channel minimum; a second historical entry for [`Self::Darker`].
    Minimum,
    /// Per-channel maximum; a second historical entry for [`Self::Lighter`].
    Maximum,
    /// Screen: `1 - (1 - b)(1 - f)`.
    Screen,
}

impl StrobeOperator {
    /// Every operator, in menu order.
    pub const ALL: [Self; 13] = [
        Self::Copy,
        Self::Add,
        Self::Subtract,
        Self::Multiply,
        Self::Difference,
        Self::And,
        Self::Or,
        Self::Xor,
        Self::Lighter,
        Self::Darker,
        Self::Minimum,
        Self::Maximum,
        Self::Screen,
    ];

    /// Stable menu key.
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Multiply => "multiply",
            Self::Difference => "difference",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Lighter => "lighter",
            Self::Darker => "darker",
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
            Self::Screen => "screen",
        }
    }

    /// Resolves a menu key.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "copy" => Some(Self::Copy),
            "add" => Some(Self::Add),
            "subtract" => Some(Self::Subtract),
            "multiply" => Some(Self::Multiply),
            "difference" => Some(Self::Difference),
            "and" => Some(Self::And),
            "or" => Some(Self::Or),
            "xor" => Some(Self::Xor),
            "lighter" => Some(Self::Lighter),
            "darker" => Some(Self::Darker),
            "minimum" => Some(Self::Minimum),
            "maximum" => Some(Self::Maximum),
            "screen" => Some(Self::Screen),
            _ => None,
        }
    }

    /// Stable numeric tag used by the graph, plan, and shader.
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Copy => 1,
            Self::Add => 2,
            Self::Subtract => 3,
            Self::Multiply => 4,
            Self::Difference => 5,
            Self::And => 6,
            Self::Or => 7,
            Self::Xor => 8,
            Self::Lighter => 9,
            Self::Darker => 10,
            Self::Minimum => 11,
            Self::Maximum => 12,
            Self::Screen => 13,
        }
    }

    /// Localized menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Copy => "复制",
            Self::Add => "相加",
            Self::Subtract => "相减",
            Self::Multiply => "相乘",
            Self::Difference => "差值",
            Self::And => "与",
            Self::Or => "或",
            Self::Xor => "异或",
            Self::Lighter => "变亮",
            Self::Darker => "变暗",
            Self::Minimum => "最小值",
            Self::Maximum => "最大值",
            Self::Screen => "滤色",
        }
    }
}

/// The eight authored strobe values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrobeParameters {
    /// Flash colour.
    pub color: [f32; 3],
    /// How much of the original image survives, 0..=100.
    pub blend_with_original: f32,
    /// Flash duration in seconds.
    pub duration: f32,
    /// Idle gap between flashes in seconds.
    pub interval: f32,
    /// Probability that one cycle flashes, 0..=100.
    pub random_probability: f32,
    /// What the flash does to the source.
    pub strobe_mode: StrobeMode,
    /// How the flash colour is combined with the source.
    pub operator: StrobeOperator,
    /// Seed of the deterministic random schedule.
    pub random_seed: f32,
}

impl Default for StrobeParameters {
    fn default() -> Self {
        Self {
            color: STROBE_DEFAULT_COLOR,
            blend_with_original: STROBE_DEFAULT_BLEND_WITH_ORIGINAL,
            duration: STROBE_DEFAULT_DURATION,
            interval: STROBE_DEFAULT_INTERVAL,
            random_probability: STROBE_DEFAULT_RANDOM_PROBABILITY,
            strobe_mode: StrobeMode::ColorOnly,
            operator: StrobeOperator::Copy,
            random_seed: STROBE_DEFAULT_RANDOM_SEED,
        }
    }
}

/// One parameter failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("strobe.{parameter} must be finite and within its documented range")]
pub struct StrobeError {
    parameter: &'static str,
}

impl StrobeError {
    /// Stable snake_case id of the offending parameter.
    pub const fn parameter(self) -> &'static str {
        self.parameter
    }
}

/// The three fused point-op uniform cells produced by
/// [`StrobeGrade::gpu_controls`].
pub type StrobeGpuControls = ([f32; 4], [f32; 4], [f32; 4]);

/// A validated strobe configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrobeGrade {
    parameters: StrobeParameters,
}

impl StrobeGrade {
    /// Validates one authored configuration.
    pub fn new(parameters: StrobeParameters) -> Result<Self, StrobeError> {
        let numeric = [
            (
                "blend_with_original",
                parameters.blend_with_original,
                STROBE_MIN_BLEND_WITH_ORIGINAL,
                STROBE_MAX_BLEND_WITH_ORIGINAL,
            ),
            (
                "duration",
                parameters.duration,
                STROBE_MIN_DURATION,
                STROBE_MAX_DURATION,
            ),
            (
                "interval",
                parameters.interval,
                STROBE_MIN_INTERVAL,
                STROBE_MAX_INTERVAL,
            ),
            (
                "random_probability",
                parameters.random_probability,
                STROBE_MIN_RANDOM_PROBABILITY,
                STROBE_MAX_RANDOM_PROBABILITY,
            ),
            (
                "random_seed",
                parameters.random_seed,
                STROBE_MIN_RANDOM_SEED,
                STROBE_MAX_RANDOM_SEED,
            ),
        ];
        for (parameter, value, minimum, maximum) in numeric {
            if !value.is_finite() || value < minimum || value > maximum {
                return Err(StrobeError { parameter });
            }
        }
        for component in parameters.color {
            if !component.is_finite() || !(0.0..=1.0).contains(&component) {
                return Err(StrobeError { parameter: "color" });
            }
        }
        Ok(Self { parameters })
    }

    /// Documented defaults.
    pub fn documented_defaults() -> Self {
        Self::new(StrobeParameters::default()).expect("the documented strobe defaults are in range")
    }

    /// The authored values.
    pub const fn parameters(&self) -> &StrobeParameters {
        &self.parameters
    }

    /// How much of the flash reaches the frame, 0..=1.
    pub fn weight(&self) -> f32 {
        1.0 - self.parameters.blend_with_original / 100.0
    }

    /// Whether the configuration leaves every pixel untouched.
    pub fn is_identity(&self) -> bool {
        self.weight() <= 0.0
    }

    /// Whether the schedule is flashing at `time_seconds`.
    pub fn flash_active(&self, time_seconds: f64) -> bool {
        if !time_seconds.is_finite() {
            return false;
        }
        if self.parameters.duration <= 0.0 {
            return false;
        }
        let duration = f64::from(self.parameters.duration);
        let cycle = duration + f64::from(self.parameters.interval);
        if cycle <= 0.0 {
            return false;
        }
        if time_seconds.rem_euclid(cycle) >= duration {
            return false;
        }
        if self.parameters.random_probability >= 100.0 {
            return true;
        }
        if self.parameters.random_probability <= 0.0 {
            return true;
        }
        let cycle_index = (time_seconds / cycle).floor() as i64;
        cycle_noise(self.parameters.random_seed, cycle_index) * 100.0
            < self.parameters.random_probability
    }

    /// The grade to apply at `time_seconds`, or `None` when the frame is not
    /// flashing or the blend makes the flash invisible.
    pub fn at_time(&self, time_seconds: f64) -> Option<Self> {
        if self.is_identity() || !self.flash_active(time_seconds) {
            return None;
        }
        Some(*self)
    }

    /// Relative cost of one point pass.
    pub const fn cost(&self) -> u8 {
        1
    }

    /// Packs the grade into the fused point-op uniform cells.
    ///
    /// The cells are `[flash rgb, weight]`, `[mode tag, operator tag, 0, 0]`,
    /// and one unused cell.
    pub fn gpu_controls(&self) -> StrobeGpuControls {
        let params = [
            self.parameters.color[0],
            self.parameters.color[1],
            self.parameters.color[2],
            self.weight(),
        ];
        let extra0 = [
            f32::from(self.parameters.strobe_mode.as_u8()),
            f32::from(self.parameters.operator.as_u8()),
            0.0,
            0.0,
        ];
        (params, extra0, [0.0; 4])
    }

    /// Feeds the whole configuration into a graph or plan fingerprint.
    pub(crate) fn hash_state<H: Hasher>(&self, state: &mut H) {
        for component in self.parameters.color {
            component.to_bits().hash(state);
        }
        for value in [
            self.parameters.blend_with_original,
            self.parameters.duration,
            self.parameters.interval,
            self.parameters.random_probability,
            self.parameters.random_seed,
        ] {
            value.to_bits().hash(state);
        }
        self.parameters.strobe_mode.as_u8().hash(state);
        self.parameters.operator.as_u8().hash(state);
    }
}

/// Applies the strobe to one tightly packed frame.
pub fn strobe_frame_f32(
    pixels: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &StrobeGrade,
) -> bool {
    apply_strobe_f32_controlled(pixels, width, height, grade, &mut || {
        Ok::<_, std::convert::Infallible>(())
    })
    .unwrap_or(false)
}

/// Cancellation-aware strobe kernel over one tightly packed frame.
pub(crate) fn apply_strobe_f32_controlled<E>(
    working: &mut [[f32; 4]],
    width: usize,
    height: usize,
    grade: &StrobeGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let Some(pixel_count) = width.checked_mul(height) else {
        return Ok(false);
    };
    if pixel_count == 0 || working.len() < pixel_count || width == 0 || height == 0 {
        return Ok(false);
    }
    if grade.is_identity() {
        return Ok(true);
    }
    checkpoint()?;

    let parameters = &grade.parameters;
    let weight = grade.weight();
    let fade = 1.0 - weight;
    let operator = parameters.operator;
    let mode = parameters.strobe_mode;
    let color = parameters.color;

    crate::parallel::for_each_row_band(working, width, 0..height, |_, band| {
        for pixel in band.iter_mut() {
            if !crate::coverage::has_positive_coverage(pixel[3].clamp(0.0, 1.0)) {
                continue;
            }
            match mode {
                StrobeMode::ColorOnly => {
                    for (component, flash) in pixel[..3].iter_mut().zip(color.iter()) {
                        let combined = apply_operator(operator, *component, *flash);
                        *component += (combined - *component) * weight;
                    }
                }
                StrobeMode::MasksLayer => {
                    pixel[3] *= fade;
                }
            }
        }
    });

    checkpoint()?;
    Ok(true)
}

/// Combines one source channel with one flash channel.
pub(crate) fn apply_operator(operator: StrobeOperator, base: f32, flash: f32) -> f32 {
    match operator {
        StrobeOperator::Copy => flash,
        StrobeOperator::Add => (base + flash).clamp(0.0, 1.0),
        StrobeOperator::Subtract => (base - flash).clamp(0.0, 1.0),
        StrobeOperator::Multiply => base * flash,
        StrobeOperator::Difference => (base - flash).abs(),
        StrobeOperator::And => dequantize(quantize(base) & quantize(flash)),
        StrobeOperator::Or => dequantize(quantize(base) | quantize(flash)),
        StrobeOperator::Xor => dequantize(quantize(base) ^ quantize(flash)),
        StrobeOperator::Lighter | StrobeOperator::Maximum => base.max(flash),
        StrobeOperator::Darker | StrobeOperator::Minimum => base.min(flash),
        StrobeOperator::Screen => 1.0 - (1.0 - base) * (1.0 - flash),
    }
}

/// Rounds one channel to the 8-bit grid used by the bitwise operators.
fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * BYTE_SCALE).round() as u8
}

/// Maps an 8-bit channel back into the working range.
fn dequantize(value: u8) -> f32 {
    f32::from(value) / BYTE_SCALE
}

/// Deterministic per-cycle noise in `0..1`, stable across frames and builds.
fn cycle_noise(seed: f32, cycle_index: i64) -> f32 {
    let mut state = u64::from(seed.to_bits()) ^ 0x9E37_79B9_7F4A_7C15;
    state ^= (cycle_index as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / 16_777_216.0
}

#[cfg(test)]
mod tests {
    #[allow(clippy::field_reassign_with_default)]
    fn solid_frame(value: [f32; 4], width: usize, height: usize) -> Vec<[f32; 4]> {
        vec![value; width * height]
    }

    use super::*;

    #[test]
    fn documented_defaults_validate_and_are_not_identity() {
        let grade = StrobeGrade::documented_defaults();
        assert_eq!(grade.parameters().color, STROBE_DEFAULT_COLOR);
        assert_eq!(grade.parameters().duration, 0.5);
        assert_eq!(grade.weight(), 1.0);
        assert!(!grade.is_identity());
        assert_eq!(grade.cost(), 1);
    }

    #[test]
    fn operator_menu_is_thirteen_unique_entries() {
        assert_eq!(StrobeOperator::ALL.len(), 13);
        let mut keys = Vec::new();
        for operator in StrobeOperator::ALL {
            assert!(!operator.label().is_empty());
            assert_eq!(StrobeOperator::from_key(operator.as_key()), Some(operator));
            keys.push(operator.as_key());
        }
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 13);
        assert_eq!(StrobeMode::ALL.len(), 2);
        for mode in StrobeMode::ALL {
            assert_eq!(StrobeMode::from_key(mode.as_key()), Some(mode));
        }
    }

    #[test]
    fn flash_window_covers_the_first_duration_of_each_cycle() {
        let grade = StrobeGrade::documented_defaults();
        assert!(grade.flash_active(0.0));
        assert!(grade.flash_active(0.49));
        assert!(!grade.flash_active(0.5));
        assert!(!grade.flash_active(1.2));
        assert!(grade.flash_active(1.5));
        assert!(grade.flash_active(3.0));
    }

    #[test]
    fn gap_between_windows_leaves_the_frame_alone() {
        let grade = StrobeGrade::documented_defaults();
        assert_eq!(grade.at_time(0.0), Some(grade));
        assert_eq!(grade.at_time(0.75), None);
    }

    #[test]
    fn zero_duration_never_flashes() {
        let parameters = StrobeParameters { duration: 0.0, ..Default::default() };
        let grade = StrobeGrade::new(parameters).expect("zero duration is in range");
        assert!(!grade.flash_active(0.0));
        assert_eq!(grade.at_time(0.0), None);
    }

    #[test]
    fn random_probability_100_always_flashes_and_zero_follows_the_window() {
        let certain = StrobeParameters { random_probability: 100.0, ..Default::default() };
        let grade = StrobeGrade::new(certain).expect("certain probability is in range");
        assert!(grade.flash_active(0.1));
        assert!(grade.flash_active(1.6));
        assert!(!grade.flash_active(0.75));
    }

    #[test]
    fn random_decision_is_stable_within_a_cycle() {
        let parameters = StrobeParameters {
            random_probability: 50.0,
            random_seed: 7.0,
            duration: 0.2,
            interval: 0.8,
            ..Default::default()
        };
        let grade = StrobeGrade::new(parameters).expect("the fixture is in range");
        let mut decisions = Vec::new();
        for frame in 0..10 {
            let time = 3.0 + f64::from(frame) * 0.01;
            decisions.push(grade.flash_active(time));
        }
        assert!(
            decisions.windows(2).all(|pair| pair[0] == pair[1]),
            "one cycle must not change its decision between frames: {decisions:?}"
        );
        let again = StrobeGrade::new(parameters).expect("the fixture is in range");
        assert_eq!(again.flash_active(3.05), grade.flash_active(3.05));
    }

    #[test]
    fn copy_operator_replaces_the_frame_at_full_weight() {
        let mut grade = StrobeGrade::documented_defaults();
        grade.parameters.color = [0.2, 0.4, 0.6];
        let mut frame = solid_frame([0.9, 0.8, 0.7, 1.0], 3, 2);
        assert!(strobe_frame_f32(&mut frame, 3, 2, &grade));
        for pixel in frame {
            assert!((pixel[0] - 0.2).abs() < 1e-6);
            assert!((pixel[1] - 0.4).abs() < 1e-6);
            assert!((pixel[2] - 0.6).abs() < 1e-6);
            assert_eq!(pixel[3], 1.0);
        }
    }

    #[test]
    fn blend_half_mixes_toward_the_flash_colour() {
        let parameters = StrobeParameters {
            color: [1.0, 0.0, 0.0],
            blend_with_original: 50.0,
            ..Default::default()
        };
        let grade = StrobeGrade::new(parameters).expect("the fixture is in range");
        assert!((grade.weight() - 0.5).abs() < 1e-6);
        let mut frame = solid_frame([0.0, 1.0, 0.0, 1.0], 2, 1);
        assert!(strobe_frame_f32(&mut frame, 2, 1, &grade));
        for pixel in frame {
            assert!((pixel[0] - 0.5).abs() < 1e-6);
            assert!((pixel[1] - 0.5).abs() < 1e-6);
            assert!((pixel[2] - 0.0).abs() < 1e-6);
        }
    }

    #[test]
    fn masks_layer_scales_alpha() {
        let parameters = StrobeParameters {
            strobe_mode: StrobeMode::MasksLayer,
            blend_with_original: 25.0,
            ..Default::default()
        };
        let grade = StrobeGrade::new(parameters).expect("the fixture is in range");
        let mut frame = solid_frame([0.3, 0.4, 0.5, 0.8], 2, 1);
        assert!(strobe_frame_f32(&mut frame, 2, 1, &grade));
        for pixel in frame {
            assert!((pixel[0] - 0.3).abs() < 1e-6);
            assert!((pixel[3] - 0.2).abs() < 1e-6);
        }
    }

    #[test]
    fn transparent_pixels_stay_untouched() {
        let mut frame = solid_frame([0.25, 0.5, 0.75, 0.0], 2, 1);
        let grade = StrobeGrade::documented_defaults();
        assert!(strobe_frame_f32(&mut frame, 2, 1, &grade));
        for pixel in frame {
            assert_eq!(pixel, [0.25, 0.5, 0.75, 0.0]);
        }
    }

    #[test]
    fn add_and_subtract_clamp() {
        assert_eq!(apply_operator(StrobeOperator::Add, 0.8, 0.5), 1.0);
        assert_eq!(apply_operator(StrobeOperator::Subtract, 0.2, 0.5), 0.0);
        assert!((apply_operator(StrobeOperator::Add, 0.2, 0.5) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn bitwise_operators_quantize_to_eight_bits() {
        // 0.5 quantises to 128 (0x80) and 0.75 to 191 (0xBF).
        let and = apply_operator(StrobeOperator::And, 0.5, 0.75);
        assert!((and - 128.0 / 255.0).abs() < 1e-6);
        let or = apply_operator(StrobeOperator::Or, 0.5, 0.75);
        assert!((or - 191.0 / 255.0).abs() < 1e-6);
        let xor = apply_operator(StrobeOperator::Xor, 0.5, 0.75);
        assert!((xor - 63.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn screen_operator_matches_the_formula() {
        let mixed = apply_operator(StrobeOperator::Screen, 0.5, 0.5);
        assert!((mixed - 0.75).abs() < 1e-6);
        assert_eq!(apply_operator(StrobeOperator::Lighter, 0.2, 0.6), 0.6);
        assert_eq!(apply_operator(StrobeOperator::Darker, 0.2, 0.6), 0.2);
        assert_eq!(
            apply_operator(StrobeOperator::Maximum, 0.2, 0.6),
            apply_operator(StrobeOperator::Lighter, 0.2, 0.6)
        );
        assert_eq!(
            apply_operator(StrobeOperator::Minimum, 0.2, 0.6),
            apply_operator(StrobeOperator::Darker, 0.2, 0.6)
        );
    }

    #[test]
    fn gpu_controls_pack_the_authored_slots() {
        let parameters = StrobeParameters {
            color: [0.1, 0.2, 0.3],
            blend_with_original: 40.0,
            strobe_mode: StrobeMode::MasksLayer,
            operator: StrobeOperator::Screen,
            ..Default::default()
        };
        let grade = StrobeGrade::new(parameters).expect("the fixture is in range");
        let (params, extra0, extra1) = grade.gpu_controls();
        assert_eq!(params[0..3], [0.1, 0.2, 0.3]);
        assert!((params[3] - 0.6).abs() < 1e-6);
        assert_eq!(extra0, [2.0, 13.0, 0.0, 0.0]);
        assert_eq!(extra1, [0.0; 4]);
    }

    #[test]
    fn out_of_range_parameters_are_rejected() {
        let parameters = StrobeParameters { duration: -1.0, ..Default::default() };
        let error = StrobeGrade::new(parameters).expect_err("negative duration is invalid");
        assert_eq!(error.parameter(), "duration");

        let parameters = StrobeParameters { color: [1.2, 0.0, 0.0], ..Default::default() };
        let error = StrobeGrade::new(parameters).expect_err("colours live in 0..=1");
        assert_eq!(error.parameter(), "color");

        let parameters = StrobeParameters { random_probability: 101.0, ..Default::default() };
        let error = StrobeGrade::new(parameters).expect_err("probability is a percentage");
        assert_eq!(error.parameter(), "random_probability");
    }

    #[test]
    fn identity_grade_leaves_the_frame_untouched() {
        let parameters = StrobeParameters { blend_with_original: 100.0, ..Default::default() };
        let grade = StrobeGrade::new(parameters).expect("the fixture is in range");
        assert!(grade.is_identity());
        assert_eq!(grade.at_time(0.0), None);
        let mut frame = solid_frame([0.4, 0.5, 0.6, 1.0], 2, 2);
        assert!(strobe_frame_f32(&mut frame, 2, 2, &grade));
        for pixel in frame {
            assert_eq!(pixel, [0.4, 0.5, 0.6, 1.0]);
        }
    }

    #[test]
    fn empty_or_short_frames_are_rejected() {
        let grade = StrobeGrade::documented_defaults();
        let mut empty: Vec<[f32; 4]> = Vec::new();
        assert!(!strobe_frame_f32(&mut empty, 0, 0, &grade));
        let mut short = solid_frame([0.5; 4], 2, 1);
        assert!(!strobe_frame_f32(&mut short, 4, 4, &grade));
    }

    #[test]
    fn hash_state_separates_schedules() {
        use std::collections::hash_map::DefaultHasher;

        let grade = StrobeGrade::documented_defaults();
        let mut shifted = grade;
        shifted.parameters.interval = 2.0;
        let mut left = DefaultHasher::new();
        grade.hash_state(&mut left);
        let mut right = DefaultHasher::new();
        shifted.hash_state(&mut right);
        assert_ne!(left.finish(), right.finish());
    }
}
