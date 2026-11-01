//! The arithmetic effect from the built-in effect list.
//!
//! The list authors five controls: an operator menu, one constant for each of
//! red, green and blue, and one clipping switch. Every operator is a point
//! operation, so the effect runs on the fused GPU point path as well as the CPU
//! kernel.
//!
//! # Documented reading
//!
//! The list names the menu but does not define the arithmetic, so this module
//! fixes one reading and documents it here. With `c` the current colour sample,
//! `v` the authored value for that channel divided by 255, `k = round(c * 255)`
//! and `vk = round(v * 255)` the menu is read as:
//!
//! - `and`, `or`, `xor`: `(k & vk) / 255`, `(k | vk) / 255` and `(k ^ vk) / 255`.
//!   The three bitwise operators first reduce the sample to the eight-bit value
//!   `k = round(clamp(c, 0, 1) * 255)`, i.e. the sample an eight-bit-per-channel
//!   buffer would hold, combine those integers and divide by 255 in floating
//!   point. Their result is therefore always inside `[0, 1]` and the clipping
//!   switch cannot change it. Holding the sample at the display range before the
//!   bitwise operator is the same pre-clamp an eight-bit buffer would apply; a
//!   bitwise operator on an out-of-range integer has no documented meaning.
//! - `add`, `subtract`, `multiply`: `c + v`, `c - v` and `c * v` in float32.
//! - `difference`: `abs(c - v)`.
//! - `min`, `max`: `min(c, v)` and `max(c, v)`.
//! - `block_above`: `c > v ? 0 : c`, i.e. samples above the authored value are
//!   blacked out and the rest pass through.
//! - `block_below`: `c < v ? 0 : c`, the mirror image.
//! - `slice`: `c > v ? 1 : 0` applied to every colour channel independently, so
//!   a pixel that is above the authored value on two channels and below it on
//!   the third keeps those two channels at 1.0 and drops the third to 0.0. The
//!   list's "the other two colour channels are forced to 1.0" is read this way:
//!   forcing them literally would make the result depend on which channel is
//!   evaluated last, and `slice` only authors RGB, never alpha.
//! - `screen`: `1 - (1 - c) * (1 - v)`.
//!
//! # Clipping
//!
//! `剪切 == 1` (the default) clamps every colour result into `[0, 1]`;
//! `剪切 == 0` leaves the result untouched. The display encoded effect this list
//! mirrors wraps an out-of-range result at eight bits instead. That wrapping is
//! deliberately not implemented: the working frame is scene linear float32, and
//! wrapping a negative or extended-range sample would corrupt the very values the
//! pipeline exists to keep. The switch alone decides whether a result is
//! clamped.
//!
//! # Defaults
//!
//! The default menu entry is `and` with all three values zero, and `and` with
//! zero is zero, so the default grade turns the frame black. The default state is
//! therefore not the identity: [`ArithmeticGrade::is_identity`] always returns
//! `false` and the graph builder appends the operation unconditionally.
//!
//! # Domain
//!
//! The working frame is scene linear and the values are working-domain numbers
//! scaled by 255: no sample is re-encoded, and a value of 255 means 1.0 in the
//! working domain. A pixel without coverage is returned unchanged because the
//! compositor short circuits zero coverage, so writing into one would make the
//! CPU and the GPU disagree about a pixel that is never visible. The effect
//! never authors coverage: output alpha is the source alpha.

/// Number of operator menu options the list declares.
pub const ARITHMETIC_OPERATOR_COUNT: usize = 13;

/// Highest authored per-channel value (`0` to `255`).
pub const ARITHMETIC_MAX_VALUE: f32 = 255.0;

/// Lowest authored per-channel value.
pub const ARITHMETIC_MIN_VALUE: f32 = 0.0;

/// Operator menu of the arithmetic effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticOperator {
    /// Bitwise and of the eight-bit sample and value.
    And,
    /// Bitwise or of the eight-bit sample and value.
    Or,
    /// Bitwise exclusive or of the eight-bit sample and value.
    Xor,
    /// Sum of the sample and the value.
    Add,
    /// Sample minus the value.
    Subtract,
    /// Product of the sample and the value.
    Multiply,
    /// Absolute difference of the sample and the value.
    Difference,
    /// Smaller of the sample and the value.
    Minimum,
    /// Larger of the sample and the value.
    Maximum,
    /// Samples above the value are blacked out.
    BlockAbove,
    /// Samples below the value are blacked out.
    BlockBelow,
    /// Samples above the value become white, the rest black.
    Slice,
    /// Screen blend of the sample and the value.
    Screen,
}

impl ArithmeticOperator {
    /// Every operator in menu order.
    pub const ALL: [Self; ARITHMETIC_OPERATOR_COUNT] = [
        Self::And,
        Self::Or,
        Self::Xor,
        Self::Add,
        Self::Subtract,
        Self::Multiply,
        Self::Difference,
        Self::Minimum,
        Self::Maximum,
        Self::BlockAbove,
        Self::BlockBelow,
        Self::Slice,
        Self::Screen,
    ];

    /// The key the effect list stores for this operator.
    #[must_use]
    pub const fn as_key(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Multiply => "multiply",
            Self::Difference => "difference",
            Self::Minimum => "min",
            Self::Maximum => "max",
            Self::BlockAbove => "block_above",
            Self::BlockBelow => "block_below",
            Self::Slice => "slice",
            Self::Screen => "screen",
        }
    }

    /// Resolves a stored key, or `None` when the key is not a declared option.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|operator| operator.as_key() == key)
    }

    /// The one-based menu number the list declares for this operator.
    #[must_use]
    pub const fn as_menu(self) -> u32 {
        self.as_u32() + 1
    }

    /// Resolves a one-based menu number, or `None` when it is out of range.
    #[must_use]
    pub const fn from_menu(menu: u32) -> Option<Self> {
        match menu {
            1 => Some(Self::And),
            2 => Some(Self::Or),
            3 => Some(Self::Xor),
            4 => Some(Self::Add),
            5 => Some(Self::Subtract),
            6 => Some(Self::Multiply),
            7 => Some(Self::Difference),
            8 => Some(Self::Minimum),
            9 => Some(Self::Maximum),
            10 => Some(Self::BlockAbove),
            11 => Some(Self::BlockBelow),
            12 => Some(Self::Slice),
            13 => Some(Self::Screen),
            _ => None,
        }
    }

    /// The zero-based operator number the fused point shader receives.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::And => 0,
            Self::Or => 1,
            Self::Xor => 2,
            Self::Add => 3,
            Self::Subtract => 4,
            Self::Multiply => 5,
            Self::Difference => 6,
            Self::Minimum => 7,
            Self::Maximum => 8,
            Self::BlockAbove => 9,
            Self::BlockBelow => 10,
            Self::Slice => 11,
            Self::Screen => 12,
        }
    }

    /// The Chinese label the effect list declares for this operator.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::And => "与",
            Self::Or => "或",
            Self::Xor => "异或",
            Self::Add => "相加",
            Self::Subtract => "相减",
            Self::Multiply => "相乘",
            Self::Difference => "差值",
            Self::Minimum => "最小值",
            Self::Maximum => "最大值",
            Self::BlockAbove => "上界",
            Self::BlockBelow => "下界",
            Self::Slice => "限制",
            Self::Screen => "滤色",
        }
    }
}

/// Why an arithmetic grade could not be resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    /// The red value is not a finite number within `0..=255`.
    Red,
    /// The green value is not a finite number within `0..=255`.
    Green,
    /// The blue value is not a finite number within `0..=255`.
    Blue,
}

impl std::fmt::Display for ArithmeticError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let channel = match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        };
        write!(
            formatter,
            "arithmetic {channel} value must be finite and within 0..=255"
        )
    }
}

impl std::error::Error for ArithmeticError {}

/// A resolved arithmetic grade.
///
/// The three constants stay in the authored `0..=255` domain so a round trip
/// through the effect list preserves what the user typed; the arithmetic scales
/// them by `1 / 255` when it runs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArithmeticGrade {
    operator: ArithmeticOperator,
    red: f32,
    green: f32,
    blue: f32,
    clipping: bool,
}

impl ArithmeticGrade {
    /// Resolves the authored controls, rejecting a value outside `0..=255`.
    ///
    /// # Errors
    ///
    /// [`ArithmeticError::Red`], [`ArithmeticError::Green`] or
    /// [`ArithmeticError::Blue`] when that value is not finite or falls outside
    /// `0..=255`. The operator is an enum and the clipping switch is a boolean,
    /// so neither can be out of range.
    pub fn new(
        operator: ArithmeticOperator,
        red: f32,
        green: f32,
        blue: f32,
        clipping: bool,
    ) -> Result<Self, ArithmeticError> {
        for (value, error) in [
            (red, ArithmeticError::Red),
            (green, ArithmeticError::Green),
            (blue, ArithmeticError::Blue),
        ] {
            if !value.is_finite() || !(ARITHMETIC_MIN_VALUE..=ARITHMETIC_MAX_VALUE).contains(&value)
            {
                return Err(error);
            }
        }
        Ok(Self { operator, red, green, blue, clipping })
    }

    /// The authored operator.
    #[must_use]
    pub const fn operator(&self) -> ArithmeticOperator {
        self.operator
    }

    /// The authored red value in `0..=255`.
    #[must_use]
    pub const fn red(&self) -> f32 {
        self.red
    }

    /// The authored green value in `0..=255`.
    #[must_use]
    pub const fn green(&self) -> f32 {
        self.green
    }

    /// The authored blue value in `0..=255`.
    #[must_use]
    pub const fn blue(&self) -> f32 {
        self.blue
    }

    /// Whether the result is clamped into `[0, 1]`.
    #[must_use]
    pub const fn clipping(&self) -> bool {
        self.clipping
    }

    /// Whether the authored state leaves the frame unchanged.
    ///
    /// Always `false`. The default menu entry is `and` with all three values
    /// zero, and `and` with zero is zero, so the default grade turns the frame
    /// black instead of passing it through. No menu entry with zero values is a
    /// no-op either, and the list declares no neutral value, so the graph builder
    /// always appends the operation.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        false
    }

    /// Applies the effect to one pixel.
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
        let values = [
            self.red / ARITHMETIC_MAX_VALUE,
            self.green / ARITHMETIC_MAX_VALUE,
            self.blue / ARITHMETIC_MAX_VALUE,
        ];
        let mut mapped = pixel;
        for channel in 0..3 {
            mapped[channel] = self.colour(pixel[channel], values[channel]);
        }
        mapped
    }

    /// The uniform payload for the fused point shader.
    ///
    /// `params = [operator, red / 255, green / 255, blue / 255]` with the
    /// operator as its zero-based number, and
    /// `extra0 = [clipping ? 1 : 0, 0, 0, 0]`. The shader quantises the three
    /// values back to the authored `0..=255` domain for the bitwise operators.
    #[must_use]
    pub fn gpu_controls(&self) -> ([f32; 4], [f32; 4]) {
        (
            [
                self.operator.as_u32() as f32,
                self.red / ARITHMETIC_MAX_VALUE,
                self.green / ARITHMETIC_MAX_VALUE,
                self.blue / ARITHMETIC_MAX_VALUE,
            ],
            [if self.clipping { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
        )
    }

    /// Combines one colour sample with the authored value for that channel.
    fn colour(&self, sample: f32, value: f32) -> f32 {
        let combined = match self.operator {
            ArithmeticOperator::And => {
                f32::from(quantise(sample) & quantise(value)) / ARITHMETIC_MAX_VALUE
            }
            ArithmeticOperator::Or => {
                f32::from(quantise(sample) | quantise(value)) / ARITHMETIC_MAX_VALUE
            }
            ArithmeticOperator::Xor => {
                f32::from(quantise(sample) ^ quantise(value)) / ARITHMETIC_MAX_VALUE
            }
            ArithmeticOperator::Add => sample + value,
            ArithmeticOperator::Subtract => sample - value,
            ArithmeticOperator::Multiply => sample * value,
            ArithmeticOperator::Difference => (sample - value).abs(),
            ArithmeticOperator::Minimum => sample.min(value),
            ArithmeticOperator::Maximum => sample.max(value),
            ArithmeticOperator::BlockAbove => {
                if sample > value {
                    0.0
                } else {
                    sample
                }
            }
            ArithmeticOperator::BlockBelow => {
                if sample < value {
                    0.0
                } else {
                    sample
                }
            }
            ArithmeticOperator::Slice => {
                if sample > value {
                    1.0
                } else {
                    0.0
                }
            }
            ArithmeticOperator::Screen => 1.0 - (1.0 - sample) * (1.0 - value),
        };
        if self.clipping {
            combined.clamp(0.0, 1.0)
        } else {
            combined
        }
    }
}

/// Reduces a sample to the eight-bit integer the bitwise operators combine.
fn quantise(sample: f32) -> u8 {
    (sample.clamp(0.0, 1.0) * ARITHMETIC_MAX_VALUE).round() as u8
}

/// Applies the effect to every pixel of one frame, in place.
///
/// Returns whether the grade ran. The default state is not the identity, so a
/// compiled graph always reaches this kernel, and the kernel always applies the
/// mapping.
pub fn arithmetic_frame_f32(pixels: &mut [[f32; 4]], grade: &ArithmeticGrade) -> bool {
    let mut checkpoint = || Ok::<(), std::convert::Infallible>(());
    apply_arithmetic_f32_controlled(pixels, grade, &mut checkpoint).unwrap_or(false)
}

/// Applies the effect to a frame region, honouring the execution checkpoint.
///
/// # Errors
///
/// Propagates whatever the checkpoint returns.
pub(crate) fn apply_arithmetic_f32_controlled<E>(
    pixels: &mut [[f32; 4]],
    grade: &ArithmeticGrade,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    // Arithmetic is a point grade over the three colour channels; alpha is
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

    fn grade(operator: ArithmeticOperator, red: f32, green: f32, blue: f32) -> ArithmeticGrade {
        ArithmeticGrade::new(operator, red, green, blue, true).expect("grade")
    }

    fn unclipped(operator: ArithmeticOperator, red: f32, green: f32, blue: f32) -> ArithmeticGrade {
        ArithmeticGrade::new(operator, red, green, blue, false).expect("grade")
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
        assert_eq!(ArithmeticOperator::ALL.len(), ARITHMETIC_OPERATOR_COUNT);
        for (index, operator) in ArithmeticOperator::ALL.into_iter().enumerate() {
            assert_eq!(operator.as_u32() as usize, index);
            assert_eq!(operator.as_menu() as usize, index + 1);
            assert_eq!(
                ArithmeticOperator::from_menu(operator.as_menu()),
                Some(operator)
            );
            assert_eq!(
                ArithmeticOperator::from_key(operator.as_key()),
                Some(operator)
            );
        }
        assert_eq!(ArithmeticOperator::from_menu(0), None);
        assert_eq!(
            ArithmeticOperator::from_menu(ARITHMETIC_OPERATOR_COUNT as u32 + 1),
            None
        );
        assert_eq!(ArithmeticOperator::from_key("mystery"), None);
    }

    #[test]
    fn and_combines_the_eight_bit_samples() {
        let grade = grade(ArithmeticOperator::And, 204.0, 0.0, 51.0);
        let mapped = grade.apply([51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0]);
        close_pixel(mapped, [0.0, 0.0, 17.0 / 255.0, 1.0]);
    }

    #[test]
    fn or_combines_the_eight_bit_samples() {
        let grade = grade(ArithmeticOperator::Or, 51.0, 0.0, 51.0);
        let mapped = grade.apply([51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0]);
        close_pixel(mapped, [51.0 / 255.0, 102.0 / 255.0, 187.0 / 255.0, 1.0]);
    }

    #[test]
    fn xor_combines_the_eight_bit_samples() {
        let grade = grade(ArithmeticOperator::Xor, 51.0, 0.0, 51.0);
        let mapped = grade.apply([51.0 / 255.0, 102.0 / 255.0, 153.0 / 255.0, 1.0]);
        close_pixel(mapped, [0.0, 102.0 / 255.0, 170.0 / 255.0, 1.0]);
    }

    #[test]
    fn add_sums_the_sample_and_the_value() {
        let grade = unclipped(ArithmeticOperator::Add, 51.0, 102.0, 0.0);
        let mapped = grade.apply([0.2, 0.4, 0.6, 1.0]);
        close_pixel(mapped, [0.4, 0.8, 0.6, 1.0]);
    }

    #[test]
    fn subtract_differences_the_value_from_the_sample() {
        let grade = unclipped(ArithmeticOperator::Subtract, 102.0, 0.0, 51.0);
        let mapped = grade.apply([0.2, 0.4, 0.6, 1.0]);
        close_pixel(mapped, [-0.2, 0.4, 0.4, 1.0]);
    }

    #[test]
    fn multiply_scales_the_sample_by_the_value() {
        let grade = unclipped(ArithmeticOperator::Multiply, 51.0, 102.0, 153.0);
        let mapped = grade.apply([0.2, 0.4, 0.6, 1.0]);
        close_pixel(mapped, [0.04, 0.16, 0.36, 1.0]);
    }

    #[test]
    fn difference_takes_the_absolute_gap() {
        let grade = unclipped(ArithmeticOperator::Difference, 102.0, 0.0, 255.0);
        let mapped = grade.apply([0.2, 0.4, 0.6, 1.0]);
        close_pixel(mapped, [0.2, 0.4, 0.4, 1.0]);
    }

    #[test]
    fn minimum_and_maximum_pick_the_extreme() {
        let sample = [0.2, 0.4, 0.6, 1.0];
        let minimum = grade(ArithmeticOperator::Minimum, 102.0, 0.0, 255.0);
        close_pixel(minimum.apply(sample), [0.2, 0.0, 0.6, 1.0]);
        let maximum = grade(ArithmeticOperator::Maximum, 102.0, 0.0, 255.0);
        close_pixel(maximum.apply(sample), [0.4, 0.4, 1.0, 1.0]);
    }

    #[test]
    fn block_above_blacks_out_samples_over_the_value() {
        let grade = grade(ArithmeticOperator::BlockAbove, 128.0, 128.0, 128.0);
        let mapped = grade.apply([0.8, 0.2, 0.5, 1.0]);
        close_pixel(mapped, [0.0, 0.2, 0.5, 1.0]);
    }

    #[test]
    fn block_below_blacks_out_samples_under_the_value() {
        let grade = grade(ArithmeticOperator::BlockBelow, 128.0, 128.0, 128.0);
        let mapped = grade.apply([0.8, 0.2, 0.5, 1.0]);
        close_pixel(mapped, [0.8, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn slice_forces_the_other_two_colour_channels_white() {
        // Only red is below the authored 100/255; the other two channels are
        // above it and are therefore pushed to 1.0.
        let grade = grade(ArithmeticOperator::Slice, 100.0, 100.0, 100.0);
        let mapped = grade.apply([0.2, 0.8, 0.8, 1.0]);
        close_pixel(mapped, [0.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn screen_blends_the_sample_with_the_value() {
        let grade = grade(ArithmeticOperator::Screen, 102.0, 102.0, 102.0);
        let mapped = grade.apply([0.5, 0.8, 0.2, 1.0]);
        close_pixel(mapped, [0.7, 0.88, 0.52, 1.0]);
    }

    #[test]
    fn bitwise_operators_read_an_eight_bit_sample() {
        // 0.5 quantises up to 128, so an or with zero lifts the sample and an
        // and with 127 drops it.
        let lifted = grade(ArithmeticOperator::Or, 0.0, 0.0, 0.0);
        let mapped = lifted.apply([0.5, 0.599, 0.0, 1.0]);
        close_pixel(mapped, [128.0 / 255.0, 153.0 / 255.0, 0.0, 1.0]);
        let dropped = grade(ArithmeticOperator::And, 127.0, 255.0, 255.0);
        let mapped = dropped.apply([0.5, 0.599, 1.0, 1.0]);
        close_pixel(mapped, [0.0, 153.0 / 255.0, 1.0, 1.0]);
    }

    #[test]
    fn bitwise_operators_hold_extended_range_samples_at_the_eight_bit_range() {
        // The sample is reduced to an eight-bit value before the operator, so
        // the result stays inside [0, 1] even with clipping off.
        let grade = unclipped(ArithmeticOperator::And, 255.0, 255.0, 255.0);
        let mapped = grade.apply([-0.4, 2.5, 0.0, 1.0]);
        close_pixel(mapped, [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn clipping_on_clamps_negative_and_extended_range_results() {
        let grade = grade(ArithmeticOperator::Add, 153.0, 26.0, 128.0);
        let mapped = grade.apply([1.4, -0.35, 0.5, 1.0]);
        close_pixel(mapped, [1.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn clipping_off_keeps_negative_and_extended_range_results() {
        let grade = unclipped(ArithmeticOperator::Add, 153.0, 26.0, 128.0);
        let mapped = grade.apply([1.4, -0.35, 0.5, 1.0]);
        close_pixel(
            mapped,
            [
                1.4 + 153.0 / 255.0,
                -0.35 + 26.0 / 255.0,
                0.5 + 128.0 / 255.0,
                1.0,
            ],
        );
    }

    #[test]
    fn transparent_pixels_are_left_untouched() {
        let grade = grade(ArithmeticOperator::Add, 255.0, 255.0, 255.0);
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
    fn alpha_is_preserved_for_every_operator() {
        for operator in ArithmeticOperator::ALL {
            let grade = grade(operator, 51.0, 102.0, 153.0);
            for alpha in [0.5, 1.0, 2.0] {
                let mapped = grade.apply([0.3, 0.6, 0.9, alpha]);
                assert_eq!(
                    mapped[3].to_bits(),
                    alpha.to_bits(),
                    "{} must not author alpha",
                    operator.as_key()
                );
            }
        }
    }

    #[test]
    fn defaults_become_black_and_are_not_the_identity() {
        let defaults = grade(ArithmeticOperator::And, 0.0, 0.0, 0.0);
        assert!(!defaults.is_identity());
        let mut pixels = vec![[0.5, 0.2, 0.9, 1.0], [0.0, 0.0, 0.0, 0.5]];
        assert!(arithmetic_frame_f32(&mut pixels, &defaults));
        close_pixel(pixels[0], [0.0, 0.0, 0.0, 1.0]);
        close_pixel(pixels[1], [0.0, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn grade_rejects_non_finite_and_out_of_range_values() {
        assert_eq!(
            ArithmeticGrade::new(ArithmeticOperator::Add, f32::NAN, 0.0, 0.0, true),
            Err(ArithmeticError::Red)
        );
        assert_eq!(
            ArithmeticGrade::new(
                ArithmeticOperator::Add,
                ARITHMETIC_MAX_VALUE + 1.0,
                0.0,
                0.0,
                true
            ),
            Err(ArithmeticError::Red)
        );
        assert_eq!(
            ArithmeticGrade::new(ArithmeticOperator::Add, 0.0, -1.0, 0.0, true),
            Err(ArithmeticError::Green)
        );
        assert_eq!(
            ArithmeticGrade::new(ArithmeticOperator::Add, 0.0, 0.0, f32::INFINITY, true),
            Err(ArithmeticError::Blue)
        );
        assert!(ArithmeticGrade::new(
            ArithmeticOperator::Add,
            ARITHMETIC_MIN_VALUE,
            ARITHMETIC_MAX_VALUE,
            0.0,
            true
        )
        .is_ok());
    }

    #[test]
    fn gpu_controls_pack_the_operator_and_the_values() {
        let grade = grade(ArithmeticOperator::Or, 51.0, 102.0, 153.0);
        let (params, extra0) = grade.gpu_controls();
        assert_eq!(params, [1.0, 0.2, 0.4, 0.6]);
        assert_eq!(extra0, [1.0, 0.0, 0.0, 0.0]);
        let unclipped = unclipped(ArithmeticOperator::Or, 51.0, 102.0, 153.0);
        let (_, extra0) = unclipped.gpu_controls();
        assert_eq!(extra0, [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn the_frame_kernel_matches_the_pixel_mapping() {
        let grade = grade(ArithmeticOperator::Screen, 51.0, 102.0, 153.0);
        let mut pixels = vec![
            [0.2, 0.4, 0.6, 1.0],
            [0.9, 0.1, 0.5, 0.25],
            [0.0, 0.0, 0.0, 0.0],
        ];
        let expected = pixels.iter().map(|pixel| grade.apply(*pixel)).collect::<Vec<_>>();
        assert!(arithmetic_frame_f32(&mut pixels, &grade));
        for (actual, expected) in pixels.iter().zip(&expected) {
            assert_eq!(actual.map(f32::to_bits), expected.map(f32::to_bits));
        }
    }

    #[test]
    fn checkpoints_run_once_per_chunk_and_once_at_the_end() {
        let grade = grade(ArithmeticOperator::Add, 51.0, 102.0, 153.0);
        let mut checkpoints = 0;
        let mut short = vec![[0.25, 0.25, 0.25, 1.0]; 4];
        let mut checkpoint = || {
            checkpoints += 1;
            Ok::<(), std::convert::Infallible>(())
        };
        assert!(
            apply_arithmetic_f32_controlled(&mut short, &grade, &mut checkpoint)
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
            apply_arithmetic_f32_controlled(&mut long, &grade, &mut checkpoint)
                .expect("infallible")
        );
        assert_eq!(checkpoints, 3);
    }
}
