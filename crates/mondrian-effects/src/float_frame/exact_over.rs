//! Exact bounded dyadic arithmetic for Float32 Normal Over and Effect Mix.
//!
//! Every monomial has at most four finite Float32 factors: its unsigned
//! significand fits 96 bits, its quantum is at least 2^-596, and its magnitude
//! is below 2^512. Three such terms sum to a magnitude below 2^514. Therefore
//! 18 fixed 64-bit limbs (1152 bits) cover the entire input domain with 42 bits
//! of carry headroom. No allocation or runtime arbitrary-precision type is
//! needed. Mix has at most three factors and uses the same bounded sums;
//! Straight Mix rounds an exact dyadic quotient, never a rounded association
//! or alpha. Its aligned numerator/denominator and 24 quotient bits fit this
//! same capacity. The ordinary normalized Over path remains in coverage.rs.

use mondrian_core::ColorFrameAlpha;

#[derive(Clone, Copy)]
struct Term {
    magnitude: u128,
    exponent: i32,
    negative: bool,
}

impl Term {
    fn from_float(value: f32) -> Self {
        let bits = value.to_bits();
        let encoded_exponent = (bits >> 23) & 0xff;
        let fraction = bits & 0x007f_ffff;
        let mut magnitude = if encoded_exponent == 0 {
            fraction
        } else {
            fraction | 0x0080_0000
        };
        let mut exponent = if encoded_exponent == 0 {
            -149
        } else {
            encoded_exponent as i32 - 150
        };
        // All callers have validated finite input. Removing exact powers of
        // two makes the common u128 path insensitive to redundant zero bits.
        if magnitude != 0 {
            let trailing = magnitude.trailing_zeros();
            magnitude >>= trailing;
            exponent += trailing as i32;
        }
        Self {
            magnitude: u128::from(magnitude),
            exponent,
            negative: bits >> 31 != 0,
        }
    }

    fn multiply(self, other: Self) -> Self {
        // At most four 24-bit input significands participate in a monomial.
        Self {
            magnitude: self.magnitude * other.magnitude,
            exponent: self.exponent + other.exponent,
            negative: self.negative ^ other.negative,
        }
    }

    fn negate(mut self) -> Self {
        self.negative = !self.negative;
        self
    }
}

/// Input samples have already passed the complete typed-frame validation.
pub(super) fn sample(
    base: [f32; 4],
    base_alpha: ColorFrameAlpha,
    source: [f32; 4],
    source_alpha: ColorFrameAlpha,
    opacity: f32,
) -> [f32; 4] {
    let base_a = Term::from_float(base[3]);
    let source_a = Term::from_float(source[3]);
    let opacity = Term::from_float(opacity);
    let source_transmission = source_a.multiply(opacity);
    let mut output = [0.0; 4];
    for (channel, destination) in output[..3].iter_mut().enumerate() {
        let mut b = Term::from_float(base[channel]);
        if !base_alpha.is_premultiplied() {
            b = b.multiply(base_a);
        }
        let mut s = Term::from_float(source[channel]);
        if !source_alpha.is_premultiplied() {
            s = s.multiply(source_a);
        }
        // B + opacity*S - opacity*As*B. Do not round a product, form 1-As,
        // or discard a tiny residual before the single Float32 delivery.
        *destination = sum([
            b,
            s.multiply(opacity),
            b.multiply(source_transmission).negate(),
        ]);
    }
    output[3] = if base_alpha == ColorFrameAlpha::Opaque {
        1.0
    } else {
        sum([
            base_a,
            source_transmission,
            base_a.multiply(source_transmission).negate(),
        ])
    };
    output
}

/// Evaluate Mix after the complete input frames and amount were admitted.
/// None means exact nonzero associated emission at exact zero output alpha;
/// the public typed consumer owns this refusal and all finite-output checks.
#[inline]
pub(super) fn mix_sample(
    base: [f32; 4],
    base_alpha: ColorFrameAlpha,
    source: [f32; 4],
    source_alpha: ColorFrameAlpha,
    amount: f32,
    output_alpha: ColorFrameAlpha,
) -> Option<[f32; 4]> {
    if let Some(certified) =
        mix_sample_certified(base, base_alpha, source, source_alpha, amount, output_alpha)
    {
        return Some(certified);
    }
    mix_sample_exact(base, base_alpha, source, source_alpha, amount, output_alpha)
}

/// The bounded integer engine remains the fallback arithmetic authority.
fn mix_sample_exact(
    base: [f32; 4],
    base_alpha: ColorFrameAlpha,
    source: [f32; 4],
    source_alpha: ColorFrameAlpha,
    amount: f32,
    output_alpha: ColorFrameAlpha,
) -> Option<[f32; 4]> {
    let base_a = Term::from_float(base[3]);
    let source_a = Term::from_float(source[3]);
    let amount = Term::from_float(amount);
    let alpha = exact_sum([
        base_a,
        source_a.multiply(amount),
        base_a.multiply(amount).negate(),
    ]);
    let mut output = [0.0; 4];
    output[3] = alpha.round();
    for (channel, destination) in output[..3].iter_mut().enumerate() {
        let mut b = Term::from_float(base[channel]);
        if !base_alpha.is_premultiplied() {
            b = b.multiply(base_a);
        }
        let mut s = Term::from_float(source[channel]);
        if !source_alpha.is_premultiplied() {
            s = s.multiply(source_a);
        }
        let associated = exact_sum([b, s.multiply(amount), b.multiply(amount).negate()]);
        *destination = if output_alpha.is_premultiplied() {
            associated.round()
        } else if alpha.magnitude.bit_len() != 0 {
            round_quotient(&associated, &alpha)
        } else if associated.magnitude.bit_len() == 0 {
            0.0
        } else {
            return None;
        };
    }
    Some(output)
}

/// A closed interval containing the exact dyadic value. Float64 is used only
/// to enclose the result, never as an alternate rounding interpretation.
#[derive(Clone, Copy)]
struct MixInterval {
    lower: f64,
    upper: f64,
}

impl MixInterval {
    #[inline]
    fn point(value: f64) -> Self {
        // The integer sum makes exact cancellation canonical positive zero.
        let value = if value == 0.0 { 0.0 } else { value };
        Self { lower: value, upper: value }
    }

    #[inline]
    fn rounded(self) -> Option<f32> {
        if !self.lower.is_finite() || !self.upper.is_finite() {
            return None;
        }
        if self.lower == 0.0 && self.upper == 0.0 {
            return Some(0.0);
        }
        // An interval touching zero does not prove the sign of a nonzero
        // underflow or exact cancellation. Never certify negative zero for
        // a possible exact zero; the integer engine decides that boundary.
        if self.lower <= 0.0 && self.upper >= 0.0 {
            return None;
        }
        let lower = self.lower as f32;
        let upper = self.upper as f32;
        (lower.to_bits() == upper.to_bits()).then_some(lower)
    }

    #[inline]
    fn reciprocal(self) -> Option<Self> {
        if !self.lower.is_finite()
            || !self.upper.is_finite()
            || (self.lower <= 0.0 && self.upper >= 0.0)
        {
            return None;
        }
        let lower = 1.0 / self.upper;
        let upper = 1.0 / self.lower;
        if !lower.is_finite() || !upper.is_finite() {
            return None;
        }
        Some(Self { lower: lower.next_down(), upper: upper.next_up() })
    }

    #[inline]
    fn multiply_reciprocal(self, reciprocal: Self) -> Option<Self> {
        if self.lower == 0.0 && self.upper == 0.0 {
            return Some(Self::point(0.0));
        }
        if self.lower <= 0.0 && self.upper >= 0.0 {
            return None;
        }
        // Convert a negative reciprocal to positive and reverse/negate the
        // numerator. The two products then use exact interval monotonicity.
        let (numerator, reciprocal) = if reciprocal.upper < 0.0 {
            (
                Self { lower: -self.upper, upper: -self.lower },
                Self { lower: -reciprocal.upper, upper: -reciprocal.lower },
            )
        } else {
            (self, reciprocal)
        };
        let (lower, upper) = if numerator.lower > 0.0 {
            (
                numerator.lower * reciprocal.lower,
                numerator.upper * reciprocal.upper,
            )
        } else {
            (
                numerator.lower * reciprocal.upper,
                numerator.upper * reciprocal.lower,
            )
        };
        if !lower.is_finite() || !upper.is_finite() {
            return None;
        }
        Some(Self { lower: lower.next_down(), upper: upper.next_up() })
    }
}

/// Unit roundoff is 2^-53. This radius is 8u times the computed absolute
/// scale. Three ordinary rounded operations have error below 4u times the
/// real scale; the computed scale is at least (1-u)^3 times that scale.
const MIX_INTERVAL_RADIUS: f64 = 1.0 / 1_125_899_906_842_624.0; // 2^-50

#[inline]
fn mix_interval(base: f64, source: f64, amount: f64) -> MixInterval {
    if base == source {
        return MixInterval::point(base);
    }
    let difference = source - base;
    let value = base + amount * difference;
    let scale = base.abs() + amount * (base.abs() + source.abs());
    let radius = scale * MIX_INTERVAL_RADIUS;
    MixInterval {
        lower: (value - radius).next_down(),
        upper: (value + radius).next_up(),
    }
}

/// All inputs to these error-free transformations are finite normal Float64
/// or zero. Their nonzero quantum is at least 2^-447 and magnitude below
/// 2^258, so neither overflow nor Float64 underflow can lose a residual.
#[inline]
fn mix_two_sum(left: f64, right: f64) -> (f64, f64) {
    let sum = left + right;
    let right_virtual = sum - left;
    let left_virtual = sum - right_virtual;
    let right_error = right - right_virtual;
    let left_error = left - left_virtual;
    (sum, left_error + right_error)
}

/// Association has at most 48 significant bits; amount has at most 24.
/// Splitting after 27 Float64 significand bits gives two exact products of
/// at most 51/50 bits. TwoSum returns the exact product as high + residual.
/// This requires neither an FMA instruction nor a platform math callback.
#[inline]
fn mix_two_product(value: f64, amount: f64) -> (f64, f64) {
    let high = f64::from_bits(value.to_bits() & !((1_u64 << 26) - 1));
    let low = value - high;
    mix_two_sum(high * amount, low * amount)
}

#[inline]
fn mix_interval_compensated(base: f64, source: f64, amount: f64) -> MixInterval {
    if base == source {
        return MixInterval::point(base);
    }
    let (source_high, source_error) = mix_two_product(source, amount);
    let (base_high, base_error) = mix_two_product(base, amount);
    let (sum, first_error) = mix_two_sum(base, source_high);
    let (value, second_error) = mix_two_sum(sum, -base_high);
    if first_error == 0.0 && second_error == 0.0 && source_error == 0.0 && base_error == 0.0 {
        return MixInterval::point(value);
    }
    // Exact target = value + first_error + second_error + source_error -
    // base_error. Enclose the four-term tail with the same 8u proof, then
    // outward-round its addition to value. A huge cancellation is harmless
    // only when this actual interval proves the final Float32 rounding.
    let tail = ((first_error + second_error) + source_error) - base_error;
    let scale = ((first_error.abs() + second_error.abs()) + source_error.abs()) + base_error.abs();
    let radius = scale * MIX_INTERVAL_RADIUS;
    let lower_tail = (tail - radius).next_down();
    let upper_tail = (tail + radius).next_up();
    MixInterval {
        lower: (value + lower_tail).next_down(),
        upper: (value + upper_tail).next_up(),
    }
}

#[inline]
fn mix_sample_certified(
    base: [f32; 4],
    base_alpha: ColorFrameAlpha,
    source: [f32; 4],
    source_alpha: ColorFrameAlpha,
    amount: f32,
    output_alpha: ColorFrameAlpha,
) -> Option<[f32; 4]> {
    let amount = f64::from(amount);
    let base_a = f64::from(base[3]);
    let source_a = f64::from(source[3]);
    let mut alpha = mix_interval(base_a, source_a, amount);
    let rounded_alpha = match alpha.rounded() {
        Some(value) => value,
        None => {
            alpha = mix_interval_compensated(base_a, source_a, amount);
            alpha.rounded()?
        }
    };
    let reciprocal = if output_alpha.is_premultiplied() {
        None
    } else {
        Some(alpha.reciprocal()?)
    };
    let mut output = [0.0; 4];
    output[3] = rounded_alpha;
    for (channel, destination) in output[..3].iter_mut().enumerate() {
        // Multiplication of two admitted finite Float32 values is exact in
        // normal Float64 (at most 48 significant bits, quantum >= 2^-298).
        let base = f64::from(base[channel])
            * if base_alpha.is_premultiplied() {
                1.0
            } else {
                base_a
            };
        let source = f64::from(source[channel])
            * if source_alpha.is_premultiplied() {
                1.0
            } else {
                source_a
            };
        let associated = mix_interval(base, source, amount);
        let interval = match reciprocal {
            Some(reciprocal) => associated.multiply_reciprocal(reciprocal),
            None => Some(associated),
        };
        *destination = match interval.and_then(MixInterval::rounded) {
            Some(value) => value,
            None => {
                let associated = mix_interval_compensated(base, source, amount);
                match reciprocal {
                    Some(reciprocal) => associated.multiply_reciprocal(reciprocal)?.rounded()?,
                    None => associated.rounded()?,
                }
            }
        };
    }
    Some(output)
}

fn sum(terms: [Term; 3]) -> f32 {
    exact_sum(terms).round()
}

enum SumMagnitude {
    Small(u128),
    Wide(WideMagnitude),
}

struct ExactSum {
    magnitude: SumMagnitude,
    origin: i32,
    negative: bool,
}

impl ExactSum {
    fn round(&self) -> f32 {
        round_float(&self.magnitude, self.origin, self.negative)
    }
}

fn exact_sum(terms: [Term; 3]) -> ExactSum {
    let mut origin = i32::MAX;
    for term in &terms {
        if term.magnitude != 0 {
            origin = origin.min(term.exponent);
        }
    }
    // Exact cancellation has canonical positive zero. Signed nonzero
    // underflow is retained by the final Float32 rounding instead.
    if origin == i32::MAX {
        return ExactSum {
            magnitude: SumMagnitude::Small(0),
            origin: 0,
            negative: false,
        };
    }
    if let Some((magnitude, negative)) = sum_small(terms, origin) {
        return ExactSum {
            magnitude: SumMagnitude::Small(magnitude),
            origin,
            negative,
        };
    }
    let mut accumulator = WideSum {
        magnitude: WideMagnitude([0; LIMBS]),
        negative: false,
    };
    for term in terms {
        accumulator.add_term(term, origin);
    }
    ExactSum {
        magnitude: SumMagnitude::Wide(accumulator.magnitude),
        origin,
        negative: accumulator.negative,
    }
}

impl SumMagnitude {
    fn small_shifted(&self, shift: u32) -> Option<u128> {
        match self {
            Self::Small(0) => Some(0),
            Self::Small(value) if self.bit_len() + shift <= u128::BITS => Some(*value << shift),
            _ => None,
        }
    }
    fn wide_shifted(&self, shift: u32) -> WideMagnitude {
        match self {
            Self::Small(value) => WideMagnitude::from_term(
                Term { magnitude: *value, exponent: 0, negative: false },
                0,
            )
            .shifted(shift),
            Self::Wide(value) => value.shifted(shift),
        }
    }
}

fn compare_shifted(
    left: &SumMagnitude,
    left_shift: u32,
    right: &SumMagnitude,
    right_shift: u32,
) -> std::cmp::Ordering {
    if let (Some(left), Some(right)) = (
        left.small_shifted(left_shift),
        right.small_shifted(right_shift),
    ) {
        left.cmp(&right)
    } else {
        left.wide_shifted(left_shift).compare(&right.wide_shifted(right_shift))
    }
}

/// Round the exact signed rational once. Denominator is known nonzero.
fn round_quotient(numerator: &ExactSum, denominator: &ExactSum) -> f32 {
    let numerator_bits = numerator.magnitude.bit_len();
    if numerator_bits == 0 {
        return 0.0;
    }
    let denominator_bits = denominator.magnitude.bit_len();
    let negative = numerator.negative ^ denominator.negative;
    let sign = if negative { 0x8000_0000 } else { 0 };
    let mut exponent =
        numerator.origin - denominator.origin + numerator_bits as i32 - denominator_bits as i32;
    // Align only the leading bits. This exact comparison determines which
    // of the two possible binary exponents contains the rational quotient.
    let order = compare_shifted(
        &numerator.magnitude,
        denominator_bits.saturating_sub(numerator_bits),
        &denominator.magnitude,
        numerator_bits.saturating_sub(denominator_bits),
    );
    if order == std::cmp::Ordering::Less {
        exponent -= 1;
    }
    if exponent > 127 {
        return f32::from_bits(sign | 0x7f80_0000);
    }
    if exponent < -150 {
        return f32::from_bits(sign);
    }
    let subnormal = exponent < -126;
    let quantum = if subnormal { -149 } else { exponent - 23 };
    let shift = numerator.origin - denominator.origin - quantum;
    let (numerator_shift, denominator_shift) = if shift >= 0 {
        (shift as u32, 0)
    } else {
        (0, (-shift) as u32)
    };
    let mut significand = if let (Some(n), Some(d)) = (
        numerator.magnitude.small_shifted(numerator_shift),
        denominator.magnitude.small_shifted(denominator_shift),
    ) {
        let quotient = n / d;
        let remainder = n % d;
        // Compare r with d-r rather than forming 2*r, which can overflow.
        let order = remainder.cmp(&(d - remainder));
        quotient as u32
            + u32::from(
                order == std::cmp::Ordering::Greater
                    || (order == std::cmp::Ordering::Equal && quotient & 1 != 0),
            )
    } else {
        let mut remainder = numerator.magnitude.wide_shifted(numerator_shift);
        let divisor = denominator.magnitude.wide_shifted(denominator_shift);
        let mut quotient = 0_u32;
        // The chosen quantum bounds the exact integral quotient to 24 bits.
        // Wide alignment is needed only after the checked-u128 fast path.
        for bit in (0..24).rev() {
            let candidate = divisor.shifted(bit);
            if remainder.compare(&candidate) != std::cmp::Ordering::Less {
                remainder.subtract(&candidate);
                quotient |= 1 << bit;
            }
        }
        let mut complement = divisor;
        complement.subtract(&remainder);
        let order = remainder.compare(&complement);
        quotient
            + u32::from(
                order == std::cmp::Ordering::Greater
                    || (order == std::cmp::Ordering::Equal && quotient & 1 != 0),
            )
    };
    if subnormal {
        return f32::from_bits(sign | significand);
    }
    if significand == 0x0100_0000 {
        significand >>= 1;
        exponent += 1;
    }
    if exponent > 127 {
        return f32::from_bits(sign | 0x7f80_0000);
    }
    f32::from_bits(sign | (((exponent + 127) as u32) << 23) | (significand & 0x007f_ffff))
}

fn sum_small(terms: [Term; 3], origin: i32) -> Option<(u128, bool)> {
    let mut magnitude = 0_u128;
    let mut negative = false;
    for term in terms {
        if term.magnitude == 0 {
            continue;
        }
        let shift = (term.exponent - origin) as u32;
        let significant_bits = u128::BITS - term.magnitude.leading_zeros();
        if shift > u128::BITS || significant_bits > u128::BITS - shift {
            return None;
        }
        let value = term.magnitude << shift;
        if negative == term.negative {
            magnitude = magnitude.checked_add(value)?;
        } else if magnitude >= value {
            magnitude -= value;
        } else {
            magnitude = value - magnitude;
            negative = term.negative;
        }
    }
    Some((magnitude, negative))
}

const LIMBS: usize = 18;

#[derive(Clone, Copy)]
struct WideMagnitude([u64; LIMBS]);

impl WideMagnitude {
    fn from_term(term: Term, origin: i32) -> Self {
        let mut output = Self([0; LIMBS]);
        if term.magnitude == 0 {
            return output;
        }
        let shift = (term.exponent - origin) as usize;
        let word = shift / 64;
        let offset = shift % 64;
        let low = term.magnitude as u64;
        let high = (term.magnitude >> 64) as u64;
        output.0[word] = low << offset;
        if offset == 0 {
            if high != 0 {
                output.0[word + 1] = high;
            }
        } else {
            let middle = (low >> (64 - offset)) | (high << offset);
            let upper = high >> (64 - offset);
            if middle != 0 {
                output.0[word + 1] = middle;
            }
            if upper != 0 {
                output.0[word + 2] = upper;
            }
        }
        output
    }

    fn shifted(&self, shift: u32) -> Self {
        debug_assert!(
            self.bit_len() + shift <= LIMBS as u32 * 64,
            "Mix quotient alignment fits the proven fixed monomial capacity"
        );
        let mut output = Self([0; LIMBS]);
        let words = shift as usize / 64;
        let offset = shift % 64;
        for (index, &value) in self.0.iter().enumerate() {
            if value == 0 {
                continue;
            }
            output.0[index + words] |= value << offset;
            if offset != 0 {
                let high = value >> (64 - offset);
                if high != 0 {
                    output.0[index + words + 1] |= high;
                }
            }
        }
        output
    }

    fn compare(&self, other: &Self) -> std::cmp::Ordering {
        for index in (0..LIMBS).rev() {
            let order = self.0[index].cmp(&other.0[index]);
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    }

    fn add(&mut self, other: &Self) {
        let mut carry = 0_u128;
        for (destination, &source) in self.0.iter_mut().zip(&other.0) {
            let value = u128::from(*destination) + u128::from(source) + carry;
            *destination = value as u64;
            carry = value >> 64;
        }
        debug_assert_eq!(
            carry, 0,
            "finite Float32 monomial bound leaves 42 carry bits"
        );
    }

    fn subtract(&mut self, other: &Self) {
        let mut borrow = false;
        for (destination, &source) in self.0.iter_mut().zip(&other.0) {
            let (difference, first_borrow) = destination.overflowing_sub(source);
            let (difference, second_borrow) = difference.overflowing_sub(u64::from(borrow));
            *destination = difference;
            borrow = first_borrow || second_borrow;
        }
        debug_assert!(
            !borrow,
            "magnitude ordering is established before subtraction"
        );
    }
}

struct WideSum {
    magnitude: WideMagnitude,
    negative: bool,
}

impl WideSum {
    fn add_term(&mut self, term: Term, origin: i32) {
        if term.magnitude == 0 {
            return;
        }
        let mut value = WideMagnitude::from_term(term, origin);
        if self.negative == term.negative {
            self.magnitude.add(&value);
        } else if self.magnitude.compare(&value) != std::cmp::Ordering::Less {
            self.magnitude.subtract(&value);
        } else {
            value.subtract(&self.magnitude);
            self.magnitude = value;
            self.negative = term.negative;
        }
    }
}

// Both storage choices supply bits to exactly one IEEE Float32 rounding law.
trait Magnitude {
    fn bit_len(&self) -> u32;
    fn low_u32_after_shift(&self, shift: u32) -> u32;
    fn bit(&self, index: u32) -> bool;
    fn any_below(&self, end: u32) -> bool;
}

impl Magnitude for SumMagnitude {
    fn bit_len(&self) -> u32 {
        match self {
            Self::Small(v) => v.bit_len(),
            Self::Wide(v) => v.bit_len(),
        }
    }
    fn low_u32_after_shift(&self, shift: u32) -> u32 {
        match self {
            Self::Small(v) => v.low_u32_after_shift(shift),
            Self::Wide(v) => v.low_u32_after_shift(shift),
        }
    }
    fn bit(&self, index: u32) -> bool {
        match self {
            Self::Small(v) => v.bit(index),
            Self::Wide(v) => v.bit(index),
        }
    }
    fn any_below(&self, end: u32) -> bool {
        match self {
            Self::Small(v) => v.any_below(end),
            Self::Wide(v) => v.any_below(end),
        }
    }
}

impl Magnitude for u128 {
    fn bit_len(&self) -> u32 {
        u128::BITS - self.leading_zeros()
    }
    fn low_u32_after_shift(&self, shift: u32) -> u32 {
        if shift >= u128::BITS {
            0
        } else {
            (self >> shift) as u32
        }
    }
    fn bit(&self, index: u32) -> bool {
        index < u128::BITS && (self >> index) & 1 != 0
    }
    fn any_below(&self, end: u32) -> bool {
        if end >= u128::BITS {
            *self != 0
        } else {
            *self & ((1_u128 << end) - 1) != 0
        }
    }
}

impl Magnitude for WideMagnitude {
    fn bit_len(&self) -> u32 {
        for index in (0..LIMBS).rev() {
            if self.0[index] != 0 {
                return index as u32 * 64 + 64 - self.0[index].leading_zeros();
            }
        }
        0
    }
    fn low_u32_after_shift(&self, shift: u32) -> u32 {
        let word = shift as usize / 64;
        let offset = shift % 64;
        let low = self.0.get(word).map_or(0, |value| *value >> offset);
        let high = if offset > 32 {
            self.0.get(word + 1).map_or(0, |value| *value << (64 - offset))
        } else {
            0
        };
        (low | high) as u32
    }
    fn bit(&self, index: u32) -> bool {
        self.0
            .get(index as usize / 64)
            .is_some_and(|value| (*value >> (index % 64)) & 1 != 0)
    }
    fn any_below(&self, end: u32) -> bool {
        let word = (end as usize / 64).min(LIMBS);
        self.0[..word].iter().any(|value| *value != 0)
            || (!end.is_multiple_of(64)
                && self.0.get(word).is_some_and(|value| *value & ((1_u64 << (end % 64)) - 1) != 0))
    }
}

fn round_float(magnitude: &impl Magnitude, origin: i32, negative: bool) -> f32 {
    let bits = magnitude.bit_len();
    if bits == 0 {
        return 0.0;
    }
    let sign = if negative { 0x8000_0000 } else { 0 };
    let mut exponent = origin + bits as i32 - 1;
    if exponent > 127 {
        return f32::from_bits(sign | 0x7f80_0000);
    }
    let subnormal = exponent < -126;
    let quantum = if subnormal { -149 } else { exponent - 23 };
    let shift = quantum - origin;
    let mut significand = if shift > 0 {
        let shift = shift as u32;
        let mut value = magnitude.low_u32_after_shift(shift);
        let guard = magnitude.bit(shift - 1);
        let sticky = magnitude.any_below(shift - 1);
        if guard && (sticky || value & 1 != 0) {
            value += 1;
        }
        value
    } else {
        // The selected quantum and bit length guarantee at most 24 bits.
        magnitude.low_u32_after_shift(0) << (-shift as u32)
    };
    if subnormal {
        // A carry into bit 23 is exactly the smallest normal Float32.
        return f32::from_bits(sign | significand);
    }
    if significand == 0x0100_0000 {
        significand >>= 1;
        exponent += 1;
    }
    if exponent > 127 {
        return f32::from_bits(sign | 0x7f80_0000);
    }
    f32::from_bits(sign | (((exponent + 127) as u32) << 23) | (significand & 0x007f_ffff))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_four_factor_span_retains_low_term_after_large_cancellation() {
        let maximum = Term::from_float(f32::MAX);
        let maximum_product = maximum.multiply(maximum).multiply(maximum).multiply(maximum);
        let minimum = Term::from_float(f32::from_bits(1));
        let minimum_product = minimum.multiply(minimum).multiply(minimum).multiply(minimum);
        assert_eq!(maximum_product.magnitude.ilog2() + 1, 96);
        assert_eq!(minimum_product.exponent, -596);
        let terms = [maximum_product, minimum_product, maximum_product.negate()];
        assert!(sum_small(terms, -596).is_none());
        let mut value = WideSum {
            magnitude: WideMagnitude([0; LIMBS]),
            negative: false,
        };
        for term in terms {
            value.add_term(term, -596);
        }
        assert_eq!(value.magnitude.0[0], 1);
        assert!(value.magnitude.0[1..].iter().all(|word| *word == 0));
        assert!(!value.negative);
    }

    #[test]
    fn one_rounding_authority_keeps_ties_even_and_signed_nonzero_underflow() {
        assert_eq!(round_float(&1_u128, -150, false).to_bits(), 0);
        assert_eq!(round_float(&1_u128, -150, true).to_bits(), 0x8000_0000);
        assert_eq!(round_float(&3_u128, -150, false).to_bits(), 2);
        assert_eq!(round_float(&3_u128, -150, true).to_bits(), 0x8000_0002);
        assert_eq!(
            round_float(&0x0100_0001_u128, -24, false).to_bits(),
            0x3f80_0000
        );
        assert_eq!(
            round_float(&0x0100_0003_u128, -24, false).to_bits(),
            0x3f80_0002
        );
        let midpoint = Term {
            magnitude: 0x01ff_ffff,
            exponent: 103,
            negative: false,
        };
        let tiny = Term { magnitude: 1, exponent: -596, negative: true };
        let zero = Term::from_float(0.0);
        assert_eq!(sum([midpoint, zero, zero]).to_bits(), 0x7f80_0000);
        assert_eq!(sum([midpoint, tiny, zero]).to_bits(), 0x7f7f_ffff);
        assert_eq!(
            sum([midpoint.negate(), tiny.negate(), zero]).to_bits(),
            0xff7f_ffff
        );
        assert_eq!(sum([midpoint, midpoint.negate(), zero]).to_bits(), 0);
    }

    #[test]
    fn ordinary_pm_monomials_fit_checked_inline_storage() {
        let b = Term::from_float(-3.7);
        let s = Term::from_float(12.4);
        let opacity = Term::from_float(0.37);
        let source_a = Term::from_float(0.63);
        let terms = [
            b,
            s.multiply(opacity),
            b.multiply(source_a.multiply(opacity)).negate(),
        ];
        let origin = terms.iter().map(|term| term.exponent).min().expect("three nonzero terms");
        assert!(sum_small(terms, origin).is_some());
    }
}

#[cfg(test)]
mod mix_exact_division_contracts {
    use super::*;

    fn value(magnitude: u128, origin: i32, negative: bool) -> ExactSum {
        ExactSum {
            magnitude: SumMagnitude::Small(magnitude),
            origin,
            negative,
        }
    }

    #[test]
    fn mix_exact_quotient_keeps_halfway_even_signed_underflow_and_overflow_boundaries() {
        let one = value(1, 0, false);
        for (numerator, expected) in [
            (value(0x0100_0001, -24, false), 0x3f80_0000),
            (value(0x0100_0003, -24, false), 0x3f80_0002),
            (value(1, -150, false), 0),
            (value(1, -150, true), 0x8000_0000),
            (value(3, -150, false), 2),
            (value(3, -150, true), 0x8000_0002),
            (value(0x01ff_ffff, 103, false), 0x7f80_0000),
        ] {
            assert_eq!(round_quotient(&numerator, &one).to_bits(), expected);
        }
        let midpoint = Term {
            magnitude: 0x01ff_ffff,
            exponent: 103,
            negative: false,
        };
        let tiny = Term { magnitude: 1, exponent: -447, negative: true };
        let zero = Term::from_float(0.0);
        assert_eq!(
            round_quotient(&exact_sum([midpoint, tiny, zero]), &one).to_bits(),
            0x7f7f_ffff
        );
        assert_eq!(
            round_quotient(&value(1, -149, true), &value(1, 0, true)).to_bits(),
            1
        );
    }

    #[test]
    fn mix_exact_full_three_factor_span_retains_the_residual_before_division() {
        let huge = Term::from_float(f32::MAX);
        let huge = huge.multiply(huge).multiply(huge);
        let tiny = Term::from_float(f32::from_bits(1));
        let numerator = exact_sum([huge, tiny.multiply(tiny).multiply(tiny), huge.negate()]);
        let denominator = exact_sum([
            tiny.multiply(tiny),
            Term::from_float(0.0),
            Term::from_float(0.0),
        ]);
        assert!(
            matches!(&numerator.magnitude, SumMagnitude::Wide(_)),
            "exercise the actual bounded limb sum"
        );
        assert_eq!(round_quotient(&numerator, &denominator).to_bits(), 1);
    }

    #[test]
    fn mix_exact_ordinary_coverage_has_checked_u128_sums_and_division_alignment() {
        // This source profile proves the fixed ordinary corpus uses the
        // existing inline sum plus integer division, not the 24-step limb
        // fallback. It is not a release throughput measurement.
        for (left, right) in [
            ([-4.0, 8.0, 0.25, 0.25], [2.0, -1.0, 7.0, 0.75]),
            ([0.125, -2.0, 16.0, 0.5], [4.0, 0.0, -8.0, 0.25]),
            (
                [8.0, -4.0, 0.125, 1.0 / 65535.0],
                [-1.0, 3.0, 4.0, 1.0 / 32768.0],
            ),
        ] {
            for amount in [0.125, 0.4, 0.875] {
                let amount = Term::from_float(amount);
                let la = Term::from_float(left[3]);
                let ra = Term::from_float(right[3]);
                let alpha = exact_sum([la, ra.multiply(amount), la.multiply(amount).negate()]);
                assert!(matches!(&alpha.magnitude, SumMagnitude::Small(_)));
                for channel in 0..3 {
                    let b = Term::from_float(left[channel]).multiply(la);
                    let s = Term::from_float(right[channel]).multiply(ra);
                    let n = exact_sum([b, s.multiply(amount), b.multiply(amount).negate()]);
                    assert!(matches!(&n.magnitude, SumMagnitude::Small(_)));
                    if n.magnitude.bit_len() == 0 {
                        continue;
                    }
                    let nb = n.magnitude.bit_len();
                    let db = alpha.magnitude.bit_len();
                    let mut exponent = n.origin - alpha.origin + nb as i32 - db as i32;
                    if compare_shifted(
                        &n.magnitude,
                        db.saturating_sub(nb),
                        &alpha.magnitude,
                        nb.saturating_sub(db),
                    ) == std::cmp::Ordering::Less
                    {
                        exponent -= 1;
                    }
                    let quantum = if exponent < -126 { -149 } else { exponent - 23 };
                    let shift = n.origin - alpha.origin - quantum;
                    assert!(n.magnitude.small_shifted(shift.max(0) as u32).is_some());
                    assert!(alpha.magnitude.small_shifted((-shift).max(0) as u32).is_some());
                }
            }
        }
    }
}

#[cfg(test)]
mod mix_certified_interval_contracts {
    use super::*;

    #[test]
    fn mix_certified_measured_changing_coverage_corpus_never_needs_integer_division() {
        // The fixed independent Fraction expected bits are the exact eight
        // spatially changing HDR/signed patterns in the release cost runner.
        let patterns = [
            (
                [0xc0800000, 0x41000000, 0x3e800000, 0x3e800000],
                [0x40000000, 0xbf800000, 0x40e00000, 0x3f400000],
                [0x330e38e3, 0x40000000, 0x40980000, 0x3ee66666],
            ),
            (
                [0x41000000, 0xc0800000, 0x3e000000, 0x3f000000],
                [0xbf800000, 0x40400000, 0x40800000, 0x3e800000],
                [0x40b80000, 0xc0100000, 0x3f8c0000, 0x3ecccccd],
            ),
            (
                [0x3f800000, 0xbf800000, 0x42000000, 0x3f400000],
                [0xc1800000, 0x3f000000, 0xc2000000, 0x3e000000],
                [0xbf333334, 0xbf59999a, 0x41cccccd, 0x3f000000],
            ),
            (
                [0xc0000000, 0x41400000, 0x3c800000, 0x3e000000],
                [0x41000000, 0xc0000000, 0x41000000, 0x3f600000],
                [0x40c78788, 0x3ef0f0ef, 0x40d2e969, 0x3ed9999a],
            ),
            (
                [0x40000000, 0x40800000, 0xc1800000, 0x3f200000],
                [0xc0800000, 0x41800000, 0x3e800000, 0x3d800000],
                [0x3fd00000, 0x40980000, 0xc16fc000, 0x3ecccccd],
            ),
            (
                [0x3f000000, 0xc1000000, 0x42800000, 0x3d800000],
                [0x40400000, 0x3d800000, 0xc2800000, 0x3f200000],
                [0x402b2164, 0xbf7d37a7, 0xc23d37a7, 0x3e933333],
            ),
            (
                [0xc1800000, 0x3d800000, 0x40400000, 0x3f600000],
                [0x42000000, 0xc1000000, 0x3f800000, 0x3e800000],
                [0xc1051eb8, 0xbf9d1eb9, 0x402b851f, 0x3f200000],
            ),
            (
                [0x40800000, 0xc0400000, 0x3a800000, 0x3f100000],
                [0xc0400000, 0x41000000, 0x40000000, 0x3f500000],
                [0x3f10e7d9, 0x40195bc6, 0x3f7b4c14, 0x3f29999a],
            ),
        ];
        for (left, right, expected) in patterns {
            let actual = mix_sample_certified(
                left.map(f32::from_bits),
                ColorFrameAlpha::StraightCoverage,
                right.map(f32::from_bits),
                ColorFrameAlpha::StraightCoverage,
                f32::from_bits(0x3ecccccd),
                ColorFrameAlpha::StraightCoverage,
            )
            .expect("actual common cost corpus must be interval-certified");
            assert_eq!(actual.map(f32::to_bits), expected);
        }
    }

    #[test]
    fn mix_certified_independent_fraction_corpus_keeps_signed_subnormal_and_extended_domains() {
        // Independent rational image law and ordered-Float32 midpoint search.
        // Neither the production divider nor a Float64 reference creates these bits.
        let cases = [
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x00000001, 0x00000001, 0x00000000],
                [0x00000000, 0x7f7fffff, 0x00000001, 0x3f000000],
                0x00800000,
                [0x00000000, 0x7f7fffff, 0x00000001, 0x00400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x64800000, 0x40400000, 0x80000001, 0x00800000],
                [0x00800000, 0x80000001, 0x7f7fffff, 0x3f000000],
                0x3f7fffff,
                [0x1a000001, 0x00000002, 0x7f7fffff, 0x3effffff],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x00000000, 0x80000000, 0x3f400000],
                [0x80800000, 0xff7fffff, 0xbf800000, 0x1a000000],
                0x00000001,
                [0x80000001, 0x8faaaaaa, 0x80000000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x80800000, 0xe4800000, 0x1a000000],
                [0xbf800000, 0xc0400000, 0xbf800000, 0x3e800000],
                0x00000001,
                [0xe4800000, 0x9a400000, 0xe4800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x40400000, 0x00000001, 0x00000000],
                [0x40400000, 0x00000001, 0x00000000, 0x3f000000],
                0x3e000000,
                [0x40400000, 0x00000001, 0x00000000, 0x3d800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x64800000, 0xe4800000, 0x3e800000],
                [0x3e800000, 0xbf800000, 0x40400000, 0x3f800000],
                0x3e000000,
                [0x3dba2e8c, 0x6422e8ba, 0xe422e8ba, 0x3eb00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0xff7fffff, 0x3e800000, 0x00000000],
                [0x7f7fffff, 0x3f800000, 0x00000001, 0x3f400000],
                0x1a000000,
                [0x7f7fffff, 0x3f800000, 0x00000001, 0x19c00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x80000001, 0x40400000, 0x3f400000],
                [0xc0400000, 0x64800000, 0x80800000, 0x1a000000],
                0x3ecccccd,
                [0xc0400000, 0x3ee38e39, 0x40400000, 0x3ee66666],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0x00000000, 0xbe800000, 0x00800000],
                [0x3f800000, 0xc0400000, 0xbe800000, 0x3f000000],
                0x00800000,
                [0x3f800000, 0xbf800000, 0xbe800000, 0x00c00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x3f800000, 0x00800000, 0x00800000],
                [0xbe800000, 0x40400000, 0xbf800000, 0x3f400000],
                0x00000001,
                [0xb2bfffff, 0x3f800001, 0xb3bfffff, 0x00800001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0xe4800000, 0x00000000, 0x1a000000],
                [0xbf800000, 0x80000001, 0x40400000, 0x3f800000],
                0x3ecccccd,
                [0xbf800000, 0xbf400000, 0x40400000, 0x3ecccccd],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x64800000, 0xc0400000, 0xc0400000, 0x00000000],
                [0xe4800000, 0x00800000, 0x00800000, 0x3f800000],
                0x3e000000,
                [0xe4800000, 0x00800000, 0x00800000, 0x3e000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0xff7fffff, 0x80000001, 0x00800000],
                [0x40400000, 0x80000001, 0x00000000, 0x1a000000],
                0x3f7fffff,
                [0x40400000, 0xda000000, 0x80000000, 0x19ffffff],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x80000000, 0xe4800000, 0x00000000],
                [0x64800000, 0x40400000, 0x00800000, 0x1a000000],
                0x3f600000,
                [0x64800000, 0x40400000, 0x00800000, 0x19e00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0xe4800000, 0x80800000, 0x00000000],
                [0x7f7fffff, 0x80000000, 0x80800000, 0x00800000],
                0x3f600000,
                [0x7f7fffff, 0x00000000, 0x80800000, 0x00700000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0xc0400000, 0xff7fffff, 0x00000001],
                [0xe4800000, 0xff7fffff, 0x3f800000, 0x1a000000],
                0x3f600000,
                [0xe4800000, 0xff7fffff, 0xd9124924, 0x19e00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0xbe800000, 0x80000000, 0x00000001],
                [0x80000000, 0x00800000, 0xff7fffff, 0x00800000],
                0x1a000000,
                [0xbf800000, 0xbe800000, 0xe57fffff, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x80000001, 0xbf800000, 0x1a000000],
                [0x00000000, 0x00000000, 0x00000001, 0x00000000],
                0x3e000000,
                [0x80000001, 0x80000001, 0xbf800000, 0x19e00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x3e800000, 0x3e800000, 0x3f000000],
                [0xbf800000, 0x3f800000, 0x80800000, 0x00800000],
                0x1a000000,
                [0x00000001, 0x3e800000, 0x3e800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0xe4800000, 0x00000001, 0x3e800000],
                [0x00000001, 0x3e800000, 0x80800000, 0x3f800000],
                0x00800000,
                [0xc0400000, 0xe4800000, 0x00000001, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x80800000, 0x00000000, 0x00000001],
                [0x80000001, 0xe4800000, 0xbf800000, 0x1a000000],
                0x1a000000,
                [0x3e2aaaab, 0xe3aaaaab, 0xbeaaaaab, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x80000000, 0x3e800000, 0x3f800000],
                [0x3e800000, 0x40400000, 0xbe800000, 0x3f000000],
                0x3f7fffff,
                [0xd8ffffff, 0x403fffff, 0xbe7ffffc, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0xc0400000, 0x80800000, 0x00000000],
                [0x00800000, 0xe4800000, 0x00000001, 0x00000000],
                0x3e000000,
                [0x00000000, 0x00000000, 0x00000000, 0x00000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x80000001, 0xbf800000, 0x00800000],
                [0x80000000, 0xc0400000, 0xbf800000, 0x3f800000],
                0x3e000000,
                [0x00e00000, 0xc0400000, 0xbf800000, 0x3e000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0xc0400000, 0x80800000, 0x00000000],
                [0x7f7fffff, 0x80800000, 0x00000000, 0x3f400000],
                0x3f600000,
                [0x7f7fffff, 0x80800000, 0x00000000, 0x3f280000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x80800000, 0x3e800000, 0x1a000000],
                [0xbe800000, 0x80800000, 0x64800000, 0x00800000],
                0x3f600000,
                [0xa6600000, 0x80800000, 0x4c600000, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0x80000001, 0xc0400000, 0x3e800000],
                [0x00000000, 0x00800000, 0x3f800000, 0x3f800000],
                0x1a000000,
                [0xbe800000, 0x80000001, 0xc0400000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x7f7fffff, 0xff7fffff, 0x3e800000],
                [0x80000000, 0x00000001, 0x80800000, 0x00000000],
                0x1a000000,
                [0x80800000, 0x7f7fffff, 0xff7fffff, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x7f7fffff, 0xff7fffff, 0x3f400000],
                [0xc0400000, 0x00800000, 0xbe800000, 0x00000000],
                0x3f600000,
                [0xc0400000, 0x7f7fffff, 0xff7fffff, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x00000001, 0xff7fffff, 0x3f400000],
                [0x00000000, 0x40400000, 0x00800000, 0x3f800000],
                0x00800000,
                [0x80000001, 0x01800000, 0xff7fffff, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0xc0400000, 0xbf800000, 0x3e800000],
                [0xbe800000, 0x40400000, 0x00000001, 0x00000001],
                0x00000001,
                [0x80000001, 0xc0400000, 0xbf800000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x00800000, 0x00800000, 0x1a000000],
                [0xbf800000, 0x7f7fffff, 0x00800000, 0x3f800000],
                0x1a000000,
                [0xe4000000, 0x7effffff, 0x00800000, 0x1a800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x00000000, 0xe4800000, 0x1a000000],
                [0x40400000, 0x3e800000, 0xc0400000, 0x3f400000],
                0x00000001,
                [0xc0400000, 0x19400000, 0xe4800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0x40400000, 0x40400000, 0x00000001],
                [0x00000000, 0x80800000, 0xc0400000, 0x00000000],
                0x00800000,
                [0x00800000, 0x40400000, 0x40400000, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x3f800000, 0xe4800000, 0x00000000],
                [0x00000000, 0x3e800000, 0xbf800000, 0x3f800000],
                0x00000001,
                [0x00000000, 0x3e800000, 0xbf800000, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0x00000000, 0xe4800000, 0x1a000000],
                [0x80800000, 0x00800000, 0x64800000, 0x00000001],
                0x1a000000,
                [0xbf800000, 0x00000000, 0xe4800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0x80000000, 0x7f7fffff, 0x3e800000],
                [0xe4800000, 0xe4800000, 0x80000000, 0x00800000],
                0x3f7fffff,
                [0x3f800000, 0xb27fffff, 0x7f7fffff, 0x32800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x7f7fffff, 0x3f800000, 0x3e800000, 0x3f800000],
                [0x00000000, 0xc0400000, 0xbf800000, 0x1a000000],
                0x00000001,
                [0x7f7fffff, 0x3f800000, 0x3e800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0x80000001, 0x00000000, 0x3e800000],
                [0xbe800000, 0x7f7fffff, 0x7f7fffff, 0x3f000000],
                0x3f600000,
                [0xbe2aaaab, 0x7f6eeeee, 0x7f6eeeee, 0x3ef00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x3e800000, 0xff7fffff, 0x3e800000],
                [0x80000000, 0x00000000, 0xbf800000, 0x00000001],
                0x00800000,
                [0x00000001, 0x3e800000, 0xff7fffff, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x80800000, 0x3f800000, 0x00000000],
                [0x00800000, 0x00000000, 0xbe800000, 0x3f400000],
                0x3ecccccd,
                [0x00800000, 0x00000000, 0xbe800000, 0x3e99999a],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x00000001, 0x00000001, 0x3e800000],
                [0x00000001, 0x00800000, 0x00800000, 0x3e800000],
                0x3f7fffff,
                [0x00000001, 0x00800000, 0x00800000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0x00000001, 0xbe800000, 0x3f000000],
                [0xbe800000, 0x80000000, 0xe4800000, 0x1a000000],
                0x3ecccccd,
                [0x3f800000, 0x00000001, 0xbf6aaaab, 0x3e99999a],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0xbf800000, 0x80000000, 0x00000000],
                [0xff7fffff, 0x00000001, 0xe4800000, 0x3f800000],
                0x3ecccccd,
                [0xff7fffff, 0x00000001, 0xe4800000, 0x3ecccccd],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x3e800000, 0x80000001, 0x1a000000],
                [0x00000001, 0x00000000, 0xbf800000, 0x3f000000],
                0x00800000,
                [0xff7fffff, 0x3e800000, 0xa5800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0x80800000, 0x80800000, 0x00800000],
                [0x80000001, 0x80000000, 0x00800000, 0x00000001],
                0x3f7fffff,
                [0xbdaaaaab, 0x802aaaab, 0x002aaaaa, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0xff7fffff, 0xbe800000, 0x00000000],
                [0x00800000, 0x3f800000, 0x00800000, 0x3f800000],
                0x3f7fffff,
                [0x00800000, 0x3f800000, 0x00800000, 0x3f7fffff],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x64800000, 0xc0400000, 0x00000001],
                [0xff7fffff, 0x80800000, 0x80800000, 0x3e800000],
                0x00800000,
                [0xff7ffff7, 0x59fffff8, 0xb5bffffa, 0x00200001],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x3f800000, 0xbf800000, 0x3f000000],
                [0x3f800000, 0xff7fffff, 0x00000000, 0x3e800000],
                0x3f7fffff,
                [0x3f7ffffe, 0xff7ffffd, 0xb3ffffff, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x64800000, 0x7f7fffff, 0x3e800000],
                [0x80000001, 0x80000000, 0xbf800000, 0x00000000],
                0x3f600000,
                [0x80800000, 0x64800000, 0x7f7fffff, 0x3d000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0xff7fffff, 0x40400000, 0x3e800000],
                [0x40400000, 0x00000000, 0xff7fffff, 0x00800000],
                0x3f600000,
                [0x03aa0000, 0xff7fffff, 0xc2d9ffff, 0x3d000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x00800000, 0xbe800000, 0x3f400000],
                [0x64800000, 0xff7fffff, 0x00000000, 0x1a000000],
                0x1a000000,
                [0x19aaaaab, 0xb4aaaaaa, 0xbe800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x00000001, 0x00000001, 0x00000001],
                [0x00000000, 0xbf800000, 0x7f7fffff, 0x3f000000],
                0x3f7fffff,
                [0x8e800001, 0xbf800000, 0x7f7fffff, 0x3effffff],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x80000001, 0xc0400000, 0x1a000000],
                [0x80000000, 0x3f800000, 0x80000000, 0x3f000000],
                0x1a000000,
                [0x00000001, 0x3eaaaaab, 0xc0000000, 0x1a400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0x80000001, 0x80800000, 0x00000000],
                [0x00800000, 0x80000000, 0xbe800000, 0x00000001],
                0x00000001,
                [0x00800000, 0x00000000, 0xbe800000, 0x00000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x80000001, 0x80000001, 0x00000001],
                [0xe4800000, 0x80000001, 0x00000000, 0x1a000000],
                0x3f600000,
                [0xe4800000, 0x80000001, 0x80000000, 0x19e00000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0xe4800000, 0x3f800000, 0x3f800000],
                [0xe4800000, 0x80000001, 0xc0400000, 0x3f800000],
                0x00800000,
                [0xbe800000, 0xe4800000, 0x3f800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x00000001, 0x3f800000, 0x00000000],
                [0x80000000, 0x64800000, 0x00800000, 0x00000000],
                0x00000001,
                [0x00000000, 0x00000000, 0x00000000, 0x00000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0x00000000, 0x80800000, 0x3f400000],
                [0x00000000, 0x3e800000, 0xc0400000, 0x00000000],
                0x00000001,
                [0x40400000, 0x00000000, 0x80800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x40400000, 0x80000000, 0x3f000000],
                [0xbf800000, 0x7f7fffff, 0xe4800000, 0x3f800000],
                0x1a000000,
                [0xff7fffff, 0x5a7fffff, 0xbf800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x3f800000, 0x7f7fffff, 0x00000001],
                [0x3e800000, 0xbf800000, 0x40400000, 0x3e800000],
                0x3ecccccd,
                [0x3e7fff40, 0xbf800000, 0x4040000c, 0x3dcccccd],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0xc0400000, 0x80000000, 0x00000000],
                [0x7f7fffff, 0x00800000, 0x3e800000, 0x3f800000],
                0x3f600000,
                [0x7f7fffff, 0x00800000, 0x3e800000, 0x3f600000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x3e800000, 0xbf800000, 0x1a000000],
                [0x00800000, 0xc0400000, 0x80000001, 0x3f400000],
                0x00000001,
                [0x3e800000, 0x3e800000, 0xbf800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x80000001, 0x80000000, 0x00800000],
                [0x3f800000, 0x64800000, 0x64800000, 0x00000001],
                0x3f7fffff,
                [0x3f2aaaaa, 0x642aaaaa, 0x642aaaaa, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xff7fffff, 0x40400000, 0xbf800000, 0x00000000],
                [0xe4800000, 0x80000001, 0x80800000, 0x3f400000],
                0x00000001,
                [0xe4800000, 0x80000001, 0x80800000, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0x3f800000, 0x3e800000, 0x1a000000],
                [0x3f800000, 0x80000000, 0x80000000, 0x1a000000],
                0x00000001,
                [0x3e800000, 0x3f800000, 0x3e800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0xe4800000, 0x00000000, 0xbf800000],
                [0x00800000, 0x00800000, 0x80800000, 0xbf000000],
                0x3f600000,
                [0x3e638e39, 0xe3638e39, 0x80638e39, 0xbf100000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x3e800000, 0xe4800000, 0x7f7fffff],
                [0x00000000, 0x80000001, 0x3e800000, 0x3e800000],
                0x3f7fffff,
                [0x7f7fffff, 0x3e800000, 0xe4800000, 0x737fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000001, 0xe4800000, 0xbf800000, 0x00000000],
                [0xff7fffff, 0xc0400000, 0x80000001, 0x3e800000],
                0x1a000000,
                [0xff7fffff, 0xc0400000, 0x80000001, 0x19000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0x7f7fffff, 0xe4800000, 0xbf800000],
                [0x7f7fffff, 0x3f800000, 0x3e800000, 0x3f000000],
                0x1a000000,
                [0xd97fffff, 0x7f7fffff, 0xe4800000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x80000001, 0x80800000, 0x1a000000],
                [0x00000000, 0x00000001, 0x00000000, 0x7f7fffff],
                0x00800000,
                [0x59000000, 0x00000001, 0x80000000, 0x407fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00800000, 0x3f800000, 0x00000000, 0x00000001],
                [0xbf800000, 0x80800000, 0x7f7fffff, 0x00800000],
                0x1a000000,
                [0xa5800000, 0x3f800000, 0x657fffff, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0x3e800000, 0x80000000, 0x65000000],
                [0x00800000, 0xbe800000, 0x3f800000, 0x3f400000],
                0x1a000000,
                [0xbe800000, 0x3e800000, 0x00000000, 0x65000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0xbf800000, 0xe4800000, 0x00800000],
                [0x00800000, 0xbf800000, 0x80800000, 0xff7fffff],
                0x1a000000,
                [0x00800000, 0xbf800000, 0x0afffff9, 0xd9ffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xff7fffff, 0xff7fffff, 0x40400000, 0xbf000000],
                [0x3f800000, 0x40400000, 0x00000000, 0xbf000000],
                0x00800000,
                [0xff7fffff, 0xff7fffff, 0x40400000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xc0400000, 0xbe800000, 0x3e800000, 0x65000000],
                [0xbe800000, 0x80000000, 0x7f7fffff, 0xff7fffff],
                0x3e000000,
                [0xbe800000, 0x25600001, 0x7f7fffff, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80800000, 0xbe800000, 0x00800000, 0x3f400000],
                [0x64800000, 0xbf800000, 0x80000000, 0x3e800000],
                0x3f600000,
                [0x64333333, 0xbf466666, 0x00266666, 0x3ea00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0x3e800000, 0x7f7fffff, 0x65000000],
                [0x80000000, 0x80800000, 0x00800000, 0xff7fffff],
                0x3f600000,
                [0xc8924925, 0xa2924925, 0xe3924925, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0xbe800000, 0x64800000, 0x3e800000],
                [0x80800000, 0x80000001, 0xff7fffff, 0x00000000],
                0x3f7fffff,
                [0x64800000, 0xbe800000, 0x64800000, 0x32800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x80800000, 0x00000001, 0xbf000000],
                [0x40400000, 0xbf800000, 0x80800000, 0x80000001],
                0x00800000,
                [0x7f7fffff, 0x80800000, 0x00000001, 0xbf000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00800000, 0x00000000, 0x3e800000, 0xbf000000],
                [0x80000001, 0x00000001, 0xbe800000, 0x3f800000],
                0x3ecccccd,
                [0x81400001, 0x00000004, 0xbfdfffff, 0x3dccccce],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000000, 0x80000001, 0x00800000, 0x7f7fffff],
                [0x3f800000, 0xbf800000, 0x00000000, 0x3f000000],
                0x00800000,
                [0x00000000, 0x80000001, 0x00800000, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80000001, 0x00000000, 0x3f800000, 0x3f000000],
                [0xff7fffff, 0xbe800000, 0x00000000, 0xff7fffff],
                0x3e000000,
                [0xff7fffff, 0xbe800000, 0x80700000, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0x64800000, 0x00000000, 0x3f400000],
                [0x00000001, 0xc0400000, 0x3e800000, 0x65000000],
                0x00000001,
                [0xbf800000, 0x64800000, 0x19aaaaab, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0x40400000, 0x80000000, 0x3e800000],
                [0x80000000, 0x00000000, 0x7f7fffff, 0x3e800000],
                0x3f7fffff,
                [0x33800000, 0x34400000, 0x7f7ffffe, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00800000, 0xe4800000, 0xbf800000, 0x3f400000],
                [0x3f800000, 0x00000001, 0xbe800000, 0xbf800000],
                0x00800000,
                [0x802aaaab, 0xe4800000, 0xbf800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0x00000001, 0x80000000, 0xbf000000],
                [0x3e800000, 0x80000001, 0x80000000, 0x3f800000],
                0x1a000000,
                [0xbf800000, 0x00000001, 0x00000000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x3e800000, 0x40400000, 0x3f800000],
                [0x64800000, 0x3f800000, 0x80000001, 0x3f400000],
                0x00800000,
                [0x7f7fffff, 0x3e800000, 0x40400000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0x7f7fffff, 0x80000000, 0xbf000000],
                [0xbe800000, 0x00000000, 0xe4800000, 0x00000001],
                0x00000001,
                [0xbf800000, 0x7f7fffff, 0x00000000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00800000, 0xbe800000, 0xbe800000, 0x7f7fffff],
                [0x40400000, 0x3f800000, 0x00000001, 0x3f800000],
                0x3f7fffff,
                [0x0c400001, 0xbe800000, 0xbe800000, 0x737fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0x00000001, 0x3f800000, 0x1a000000],
                [0x00000000, 0x3e800000, 0x64800000, 0x80000001],
                0x00800000,
                [0x3f800000, 0x00000001, 0x3f800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0xbf800000, 0x3e800000, 0xbf000000],
                [0xe4800000, 0x3e800000, 0x00800000, 0xff7fffff],
                0x3f600000,
                [0xe4800000, 0x3e800000, 0x00809249, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xff7fffff, 0xe4800000, 0x7f7fffff, 0x00000001],
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x65000000],
                0x3ecccccd,
                [0x7f7fffff, 0x7f7fffff, 0x7f7fffff, 0x644ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80800000, 0xbf800000, 0x40400000, 0x3e800000],
                [0x40400000, 0x80000001, 0x3f800000, 0x3f400000],
                0x3f7fffff,
                [0x40400000, 0xb2aaaaab, 0x3f800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000000, 0x00000001, 0x00800000, 0x00000000],
                [0x00800000, 0x64800000, 0xbf800000, 0x1a000000],
                0x3e000000,
                [0x00800000, 0x64800000, 0xbf800000, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x40400000, 0x00800000, 0xbe800000, 0x3f800000],
                [0xbf800000, 0x64800000, 0x64800000, 0x00800000],
                0x3f7fffff,
                [0x40400000, 0x317fffff, 0xbe800000, 0x33800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0x00000001, 0x80800000, 0x3f400000],
                [0x00000000, 0x7f7fffff, 0xe4800000, 0x3f800000],
                0x3f7fffff,
                [0x32400000, 0x7f7ffffe, 0xe47fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0x3e800000, 0xc0400000, 0x3f400000],
                [0x64800000, 0x3e800000, 0x00000000, 0x80000001],
                0x1a000000,
                [0x3f800000, 0x3e800000, 0xc0400000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xff7fffff, 0x80000001, 0x80000001, 0x00000000],
                [0xff7fffff, 0x40400000, 0x00000000, 0xe5000000],
                0x00000001,
                [0xff7fffff, 0x40400000, 0x00000000, 0x9a800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000001, 0x40400000, 0x00000000, 0xff7fffff],
                [0xbe800000, 0x00800000, 0x7f7fffff, 0xbf000000],
                0x3e000000,
                [0x80009248, 0x40400000, 0x3d924925, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3f800000, 0xe4800000, 0xbe800000, 0xff7fffff],
                [0x00800000, 0x00000001, 0x80000001, 0xe5000000],
                0x3e000000,
                [0x3f800000, 0xe4800000, 0xbe800000, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0xff7fffff, 0x00800000, 0x65000000],
                [0xc0400000, 0x00000000, 0xbe800000, 0x80000001],
                0x3f600000,
                [0x3e800000, 0xff7fffff, 0x00800000, 0x63800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00800000, 0x80800000, 0xe4800000, 0x00800000],
                [0x00000000, 0x00000000, 0x00800000, 0x1a000000],
                0x3e000000,
                [0x00000000, 0x80000000, 0xcc600000, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80800000, 0x80800000, 0xbe800000, 0x3f000000],
                [0xbe800000, 0x80000001, 0x00000000, 0x65000000],
                0x1a000000,
                [0xbe2aaaab, 0x802aaaab, 0xbdaaaaab, 0x3fc00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0xff7fffff, 0x7f7fffff, 0x1a000000],
                [0xe4800000, 0x80800000, 0x00800000, 0x00000000],
                0x3f600000,
                [0x64800000, 0xff7fffff, 0x7f7fffff, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000001, 0xff7fffff, 0x80000001, 0x3f000000],
                [0x80800000, 0x3e800000, 0xff7fffff, 0x00000001],
                0x00800000,
                [0x00000001, 0xff7fffff, 0x80000009, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xe4800000, 0x7f7fffff, 0x00800000, 0xff7fffff],
                [0x00800000, 0xbe800000, 0xe4800000, 0x65000000],
                0x00800000,
                [0xe4800000, 0x7f7fffff, 0x0b000005, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xc0400000, 0x00800000, 0x64800000, 0x00800000],
                [0x3e800000, 0x3f800000, 0x3e800000, 0x00000001],
                0x00800000,
                [0xc0400000, 0x00800001, 0x64800000, 0x00800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbf800000, 0x3f800000, 0x64800000, 0x3f400000],
                [0x40400000, 0x3f800000, 0x3e800000, 0x3f400000],
                0x3ecccccd,
                [0x3f19999a, 0x3f800000, 0x6419999a, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80800000, 0x7f7fffff, 0x80000001, 0xff7fffff],
                [0x64800000, 0xbe800000, 0xbe800000, 0x3f800000],
                0x3e000000,
                [0xa3124925, 0x7f7fffff, 0x00012491, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0x40400000, 0x00800000, 0x3e800000],
                [0x80800000, 0xbe800000, 0x80800000, 0x3f000000],
                0x3ecccccd,
                [0xbddb6db7, 0x3f924924, 0x80124925, 0x3eb33333],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80000000, 0xff7fffff, 0x80800000, 0x00000001],
                [0x7f7fffff, 0x80000000, 0x64800000, 0xbf000000],
                0x3ecccccd,
                [0x7f7fffff, 0x35bfffff, 0x64800000, 0xbe4ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0x80800000, 0xc0400000, 0xe5000000],
                [0x64800000, 0x00800000, 0x64800000, 0x3f800000],
                0x3e000000,
                [0x3e36db6e, 0x80800000, 0xc0449249, 0xe4e00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x80000000, 0xff7fffff, 0x3f800000, 0x3e800000],
                [0x3f800000, 0x80000001, 0x80000000, 0x7f7fffff],
                0x00000001,
                [0x35ffffdf, 0xff7fffdf, 0x3f7fffe0, 0x3e800010],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000001, 0x40400000, 0x7f7fffff, 0x3f800000],
                [0x40400000, 0x3f800000, 0xc0400000, 0x00000000],
                0x00800000,
                [0x00000001, 0x40400000, 0x7f7fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0xff7fffff, 0xbf800000, 0xbf800000],
                [0xbe800000, 0x80000000, 0x00000001, 0xbf800000],
                0x3e000000,
                [0x7f5fffff, 0xff5fffff, 0xbf600000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0xbf800000, 0x3f800000, 0x3f400000],
                [0xbf800000, 0xe4800000, 0x7f7fffff, 0x3f400000],
                0x1a000000,
                [0xbe800000, 0xbfc00000, 0x59ffffff, 0x3f400000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x3e800000, 0xff7fffff, 0x3f800000, 0x80000001],
                [0x3f800000, 0xff7fffff, 0xbe800000, 0xbf800000],
                0x00000001,
                [0x3f200000, 0xff7fffff, 0x3ec00000, 0x80000002],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0xe4800000, 0x80800000, 0x80000001],
                [0xbe800000, 0xc0400000, 0x80000001, 0xbf800000],
                0x00000001,
                [0x64000000, 0xe4000000, 0x80400000, 0x80000002],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0x00800000, 0x64800000, 0xbf000000],
                [0xe4800000, 0x80800000, 0x3f800000, 0x3f400000],
                0x3f7fffff,
                [0xe4800000, 0x80800001, 0xd82aaaac, 0x3f3fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0xbe800000, 0xff7fffff, 0x00800000],
                [0x80800000, 0x40400000, 0xff7fffff, 0x3f800000],
                0x00800000,
                [0x64000000, 0x3fb00000, 0xff7fffff, 0x01000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0xe4800000, 0x80000001, 0xbf800000],
                [0xff7fffff, 0x7f7fffff, 0x3e800000, 0x65000000],
                0x00800000,
                [0x66100000, 0xe6100000, 0xa5000000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x7f7fffff, 0x00000001, 0x3f800000, 0xe5000000],
                [0xc0400000, 0x80000001, 0x80000000, 0x65000000],
                0x3f7fffff,
                [0xf3800001, 0x80000001, 0xb3800001, 0x64fffffe],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xc0400000, 0x00000001, 0x64800000, 0x3f000000],
                [0x00000001, 0xc0400000, 0xbf800000, 0x00800000],
                0x3e000000,
                [0xc0400000, 0x806db6da, 0x64800000, 0x3ee00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x00000001, 0x00800000, 0x7f7fffff, 0x00000001],
                [0x40400000, 0x00800000, 0x00800000, 0x3f800000],
                0x1a000000,
                [0x40400000, 0x00800000, 0x5a7fffff, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0xbe800000, 0xe4800000, 0x40400000, 0x00000001],
                [0x3f800000, 0x3e800000, 0xbe800000, 0x1a000000],
                0x3e000000,
                [0x3f800000, 0xc0d80000, 0xbe800000, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0x00000000, 0x00800000, 0x00000001],
                [0xbe800000, 0x64800000, 0xff7fffff, 0x00000001],
                0x00800000,
                [0x64800000, 0x25800000, 0xc07fffff, 0x00000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::StraightFloat,
                [0x64800000, 0x00000001, 0xe4800000, 0x3e800000],
                [0x00800000, 0x80000000, 0x7f7fffff, 0x00000000],
                0x3f7fffff,
                [0x64800000, 0x00000001, 0xe4800000, 0x32800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x7f7fffff, 0xff7fffff, 0x1a000000],
                [0x00000001, 0xe4800000, 0x40400000, 0x3f400000],
                0x3e000000,
                [0x00000001, 0x7f5fffff, 0xff5fffff, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x00800000, 0x00000000, 0x7f7fffff],
                [0x00000000, 0xff7fffff, 0x7f7fffff, 0x00000001],
                0x3e000000,
                [0x40280000, 0xfdffffff, 0x7dffffff, 0x7f5fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x3e800000, 0xc0400000, 0x3f800000],
                [0x40400000, 0x3f800000, 0xff7fffff, 0x3f800000],
                0x3ecccccd,
                [0x3fe66666, 0x3f0ccccd, 0xfecccccc, 0x3f800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x00000000, 0x3f800000, 0x3f000000],
                [0xbe800000, 0x00800000, 0x80000001, 0x3f000000],
                0x3f7fffff,
                [0xbe7ffff3, 0x00800000, 0x33800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x80000000, 0x00800000, 0x3f400000],
                [0x00000000, 0xe4800000, 0x64800000, 0x00000000],
                0x00800000,
                [0x00000000, 0xa5800000, 0x25800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x80000001, 0xff7fffff, 0xe5000000],
                [0xbe800000, 0x80000000, 0x80800000, 0x3f400000],
                0x3e000000,
                [0xc02a0000, 0x80000001, 0xff5fffff, 0xe4e00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00800000, 0xc0400000, 0xff7fffff],
                [0x7f7fffff, 0x00800000, 0x80000000, 0xbf000000],
                0x1a000000,
                [0x59ffffff, 0x00800000, 0xc0400000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0xc0400000, 0xe4800000, 0x3f000000],
                [0x80800000, 0x80000000, 0xbf800000, 0x1a000000],
                0x1a000000,
                [0xff7fffff, 0xc0400000, 0xe4800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0xc0400000, 0x80000000, 0x00800000],
                [0x7f7fffff, 0xe4800000, 0x80000000, 0x00800000],
                0x1a000000,
                [0x59ffffff, 0xc0600000, 0x00000000, 0x00800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x40400000, 0x00800000, 0x3e800000],
                [0x3e800000, 0xbf800000, 0x64800000, 0x3e800000],
                0x00000001,
                [0x3f800000, 0x40400000, 0x1a000000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x80000000, 0x3f800000, 0xbf800000],
                [0x40400000, 0x40400000, 0x3f800000, 0xe5000000],
                0x3e000000,
                [0x7f5fffff, 0x3ec00000, 0x3f800000, 0xe3800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0xff7fffff, 0x64800000, 0x65000000],
                [0x7f7fffff, 0xbe800000, 0x00800000, 0x3f400000],
                0x3f600000,
                [0x7f5fffff, 0xfdffffff, 0x63000000, 0x63800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x7f7fffff, 0x7f7fffff, 0x00800000],
                [0x7f7fffff, 0x80800000, 0x00800000, 0x3e800000],
                0x00800000,
                [0x4087ffff, 0x7f7fffff, 0x7f7fffff, 0x00a00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x00000001, 0x80000000, 0xff7fffff],
                [0xbe800000, 0xe4800000, 0xff7fffff, 0x3f000000],
                0x3ecccccd,
                [0x3fd99999, 0xe3cccccd, 0xfecccccc, 0xff199999],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x80000000, 0xe4800000, 0xbf000000],
                [0x00800000, 0x40400000, 0x00800000, 0xff7fffff],
                0x3e000000,
                [0x3f600000, 0x3ec00000, 0xe4600000, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x80800000, 0x80800000, 0x00800000],
                [0x00000000, 0x64800000, 0x00000001, 0x80000001],
                0x3ecccccd,
                [0xbfe66666, 0x63cccccd, 0x804ccccc, 0x004ccccc],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x3f800000, 0x00000001, 0xbf000000],
                [0x64800000, 0x40400000, 0x64800000, 0xbf800000],
                0x3e000000,
                [0x63000000, 0x3fa00000, 0x63000000, 0xbf100000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x80000001, 0xff7fffff, 0x3f800000],
                [0xff7fffff, 0x3e800000, 0xff7fffff, 0x00000001],
                0x3f7fffff,
                [0xff7ffffd, 0x3e7fffff, 0xff7fffff, 0x33800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x7f7fffff, 0x00800000, 0x1a000000],
                [0xe4800000, 0x80800000, 0x7f7fffff, 0x3f000000],
                0x00800000,
                [0x3f800000, 0x7f7fffff, 0x407fffff, 0x1a000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x80800000, 0x00000001, 0x3f400000],
                [0x40400000, 0xbf800000, 0x00000000, 0xe5000000],
                0x3ecccccd,
                [0x3f99999a, 0xbecccccd, 0x00000001, 0xe44ccccd],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x3e800000, 0x3f800000, 0x3f400000],
                [0x40400000, 0xbf800000, 0xe4800000, 0xff7fffff],
                0x00000001,
                [0x64800000, 0x3e800000, 0x3f800000, 0x3f3ffff8],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x80000001, 0x64800000, 0xbf800000],
                [0x80000000, 0x64800000, 0x64800000, 0x80000001],
                0x3ecccccd,
                [0x3fe66666, 0x63cccccd, 0x64800000, 0xbf19999a],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x80000001, 0x64800000, 0xe5000000],
                [0x3e800000, 0x64800000, 0x00800000, 0x00000001],
                0x3f7fffff,
                [0x737fffff, 0x647fffff, 0x58800000, 0xd9000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0xc0400000, 0x80000000, 0x65000000],
                [0x00000001, 0xe4800000, 0x80000000, 0xe5000000],
                0x1a000000,
                [0x00000001, 0xc0600000, 0x00000000, 0x65000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0xbe800000, 0x00000001, 0x3e800000],
                [0xbe800000, 0xff7fffff, 0x00800000, 0x7f7fffff],
                0x3f7fffff,
                [0x58800000, 0xff7ffffe, 0x00800000, 0x7f7ffffe],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x80000000, 0x80800000, 0xbf000000],
                [0x40400000, 0x80000000, 0xe4800000, 0x3f800000],
                0x00000001,
                [0x00000003, 0x00000000, 0x9a000000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x40400000, 0xff7fffff, 0x1a000000],
                [0x80800000, 0x3e800000, 0xff7fffff, 0x00800000],
                0x3f600000,
                [0x7dffffff, 0x3f180000, 0xff7fffff, 0x18800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xbe800000, 0x80800000, 0x65000000],
                [0xe4800000, 0x80000000, 0x40400000, 0x3f000000],
                0x3f600000,
                [0xe4600000, 0xbd000000, 0x40280000, 0x63800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0xe4800000, 0x3f800000, 0x3f800000],
                [0x40400000, 0x64800000, 0x64800000, 0x7f7fffff],
                0x1a000000,
                [0xff7fffff, 0xe4800000, 0x3fc00000, 0x59ffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x00000000, 0x3e800000, 0x1a000000],
                [0x00800000, 0x7f7fffff, 0xc0400000, 0x80000001],
                0x00000001,
                [0x00000000, 0x34ffffff, 0x3e800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x7f7fffff, 0xbe800000, 0x3e800000],
                [0x3e800000, 0x3e800000, 0x7f7fffff, 0xe5000000],
                0x3ecccccd,
                [0x6419999a, 0x7f199999, 0x7ecccccc, 0xe44ccccd],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0xbe800000, 0xc0400000, 0xe5000000],
                [0xe4800000, 0x00800000, 0x3e800000, 0x3f400000],
                0x00000001,
                [0x9a000000, 0xbe800000, 0xc0400000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0x00800000, 0x80000001, 0x65000000],
                [0x00800000, 0xe4800000, 0x80000000, 0xbf000000],
                0x3ecccccd,
                [0xff199999, 0xe3cccccd, 0x80000001, 0x64999999],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xbe800000, 0x00800000, 0x00800000],
                [0xbe800000, 0xc0400000, 0xbe800000, 0x1a000000],
                0x00000001,
                [0x80000000, 0xbe800000, 0x00800000, 0x00800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x80800000, 0x40400000, 0x3f000000],
                [0x00000000, 0xc0400000, 0x80800000, 0xbf800000],
                0x00800000,
                [0x80800000, 0x81800000, 0x40400000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x80000000, 0x80000000, 0xbf800000],
                [0xbf800000, 0xbe800000, 0x3e800000, 0xbf000000],
                0x3f7fffff,
                [0xbf7fffff, 0xbe7fffff, 0x3e7fffff, 0xbf000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xff7fffff, 0x00000001, 0x00000000],
                [0xff7fffff, 0x40400000, 0x80800000, 0x3f400000],
                0x1a000000,
                [0xd9ffffff, 0xff7fffff, 0x00000001, 0x19c00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x3f800000, 0x7f7fffff, 0x7f7fffff],
                [0x7f7fffff, 0xff7fffff, 0x64800000, 0xff7fffff],
                0x1a000000,
                [0x59ffffff, 0xd9ffffff, 0x7f7fffff, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x40400000, 0xe4800000, 0x3e800000],
                [0x00800000, 0x64800000, 0x80000000, 0x3f400000],
                0x3f600000,
                [0x00800000, 0x64600000, 0xe3000000, 0x3f300000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x3e800000, 0xbf800000, 0x7f7fffff],
                [0x00000000, 0x80000000, 0x80000000, 0x3f400000],
                0x3f600000,
                [0x80100000, 0x3d000000, 0xbe000000, 0x7dffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x40400000, 0x7f7fffff, 0xe5000000],
                [0x80000001, 0x3e800000, 0x80000000, 0x00000001],
                0x00000001,
                [0x64800000, 0x40400000, 0x7f7fffff, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0xe4800000, 0xbf800000, 0x00800000],
                [0x3f800000, 0x40400000, 0x00000001, 0x3f000000],
                0x00800000,
                [0x64800000, 0xe4800000, 0xbf800000, 0x00c00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x40400000, 0xff7fffff, 0xbf800000],
                [0xe4800000, 0x3e800000, 0xbe800000, 0x7f7fffff],
                0x00800000,
                [0xa5800000, 0x40400000, 0xff7fffff, 0x403fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00000001, 0x00000000, 0x65000000],
                [0xbf800000, 0xe4800000, 0x3f800000, 0x00000001],
                0x00000001,
                [0x80000000, 0x9a000000, 0x00000001, 0x65000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0x00800000, 0x80000001, 0x00000000],
                [0x80000000, 0xff7fffff, 0xc0400000, 0x3f000000],
                0x00800000,
                [0xe4800000, 0xc07fffff, 0x81400000, 0x00400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x3e800000, 0xbe800000, 0xe5000000],
                [0xe4800000, 0x64800000, 0x80800000, 0x3f800000],
                0x1a000000,
                [0xbf000000, 0x3f400000, 0xbe800000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x7f7fffff, 0x64800000, 0x7f7fffff],
                [0xbe800000, 0x00000000, 0x00800000, 0x1a000000],
                0x3e000000,
                [0xbe800000, 0x7f5fffff, 0x64600000, 0x7f5fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0xc0400000, 0xc0400000, 0x80000001],
                [0x00000001, 0x80000000, 0x00000000, 0x00000000],
                0x00800000,
                [0x80000001, 0xc0400000, 0xc0400000, 0x80000001],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x00800000, 0x64800000, 0x3e800000],
                [0x00000001, 0x3f800000, 0xbe800000, 0xff7fffff],
                0x3e000000,
                [0x7f5fffff, 0x3e000000, 0x64600000, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0xff7fffff, 0x3e800000, 0x1a000000],
                [0x3f800000, 0xbf800000, 0xc0400000, 0x1a000000],
                0x1a000000,
                [0xc0400000, 0xff7fffff, 0x3e800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x00800000, 0x3f800000, 0xe5000000],
                [0x80800000, 0x00000000, 0xbe800000, 0x00000001],
                0x3f600000,
                [0x7dffffff, 0x00100000, 0xbdc00000, 0xe3800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xbf800000, 0x00800000, 0xe5000000],
                [0x40400000, 0x40400000, 0x3e800000, 0xbf000000],
                0x1a000000,
                [0x1ac00000, 0xbf800000, 0x19000000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00800000, 0x3f800000, 0xff7fffff],
                [0x3e800000, 0x00000001, 0x3e800000, 0x1a000000],
                0x1a000000,
                [0x19000000, 0x00800000, 0x3f800000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x40400000, 0x40400000, 0x00000001],
                [0x80000001, 0x80000001, 0x64800000, 0x80000001],
                0x1a000000,
                [0xc0400000, 0x40400000, 0x40600000, 0x00000001],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00000000, 0x80800000, 0xbf000000],
                [0x80000000, 0x64800000, 0x3e800000, 0xbf800000],
                0x3e000000,
                [0x00000001, 0x63000000, 0x3d000000, 0xbf100000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0xbf800000, 0xbe800000, 0xff7fffff],
                [0x80000000, 0x00800000, 0x80800000, 0x7f7fffff],
                0x1a000000,
                [0xbf800000, 0xbf800000, 0xbe800000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xff7fffff, 0x00800000, 0x3f000000],
                [0x00000001, 0xc0400000, 0x7f7fffff, 0x00800000],
                0x3ecccccd,
                [0x804ccccc, 0xff199999, 0x7ecccccc, 0x3e99999a],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x00000001, 0x3e800000, 0xbf800000],
                [0xbf800000, 0x64800000, 0x80800000, 0x7f7fffff],
                0x3e000000,
                [0xc0300000, 0x63000000, 0x3e600000, 0x7dffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00800000, 0xe4800000, 0x7f7fffff],
                [0xe4800000, 0x3f800000, 0xbe800000, 0x65000000],
                0x3f600000,
                [0xe4600000, 0x3f600000, 0xe3000000, 0x7dffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xe4800000, 0x3f800000, 0x1a000000],
                [0xc0400000, 0x64800000, 0x40400000, 0x00800000],
                0x3e000000,
                [0xbec00000, 0xe4400000, 0x3fa00000, 0x19e00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x00000000, 0xc0400000, 0xbf800000],
                [0x3e800000, 0x00000001, 0xc0400000, 0x00000000],
                0x00000001,
                [0x80800000, 0x00000000, 0xc0400000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0xc0400000, 0xbf800000, 0x3f400000],
                [0x00800000, 0xc0400000, 0x00000000, 0xe5000000],
                0x00800000,
                [0xe4800000, 0xc0400000, 0xbf800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0xbe800000, 0x40400000, 0x3e800000],
                [0x00000001, 0x80000001, 0xff7fffff, 0x80000001],
                0x00800000,
                [0x40400000, 0xbe800000, 0xbf7ffffc, 0x3e800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xe4800000, 0xbe800000, 0x1a000000],
                [0xbe800000, 0x00000001, 0x64800000, 0x65000000],
                0x00000001,
                [0xbe800000, 0xe4800000, 0xbe800000, 0x1ac00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x00000001, 0xbf800000, 0xe5000000],
                [0x00800000, 0xe4800000, 0x3e800000, 0x65000000],
                0x3f600000,
                [0xe3800000, 0xe4600000, 0x63800000, 0x64c00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xbf800000, 0x80800000, 0x1a000000],
                [0x00800000, 0x40400000, 0xc0400000, 0xbf800000],
                0x00000001,
                [0x99000000, 0x9a000000, 0x80000003, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x80000000, 0x00000001, 0x7f7fffff],
                [0x00000000, 0x00000001, 0x40400000, 0x3f000000],
                0x1a000000,
                [0xb4ffffff, 0x00000000, 0x34ffffff, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0xe4800000, 0x40400000, 0x3e800000],
                [0x80000001, 0x00000000, 0x3f800000, 0x3f000000],
                0x3f7fffff,
                [0xd7800000, 0xd7800000, 0x3f800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x80000001, 0x00800000, 0x80000001],
                [0x3e800000, 0x80800000, 0x3e800000, 0x3f400000],
                0x3e000000,
                [0x3d000000, 0x80100000, 0x3d000000, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x80000000, 0xc0400000, 0x00000000],
                [0x00000001, 0xbf800000, 0x00000000, 0x1a000000],
                0x3e000000,
                [0x00000000, 0xbe000000, 0x00000000, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0x3e800000, 0x00000000, 0x3f000000],
                [0xbf800000, 0x7f7fffff, 0x7f7fffff, 0x3f000000],
                0x3f600000,
                [0xe2800000, 0x7f5fffff, 0x7f5fffff, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x40400000, 0x00000001, 0xe5000000],
                [0x80800000, 0x80000001, 0xff7fffff, 0xff7fffff],
                0x3f600000,
                [0xe3800000, 0xe4400000, 0xff5fffff, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x80000000, 0xbe800000, 0x7f7fffff],
                [0xbf800000, 0x80000000, 0x00000001, 0x00000000],
                0x3f600000,
                [0xbf5fffff, 0x00000000, 0xfcffffff, 0x7dffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x3f800000, 0x64800000, 0x00800000],
                [0x64800000, 0x64800000, 0x7f7fffff, 0xbf000000],
                0x3ecccccd,
                [0x63cccccd, 0x63cccccd, 0x7ecccccc, 0xbe4ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x00000000, 0x80800000, 0xbf800000],
                [0xbe800000, 0x64800000, 0x64800000, 0x00000000],
                0x1a000000,
                [0x99000000, 0x3f000000, 0x3f000000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x00000000, 0x3e800000, 0xbf000000],
                [0x80000000, 0x00800000, 0x00000001, 0x80000001],
                0x00000001,
                [0x80400000, 0x00000000, 0xbe000000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0xff7fffff, 0xbe800000, 0x00000001],
                [0x40400000, 0xbf800000, 0x00800000, 0xe5000000],
                0x1a000000,
                [0x1a800000, 0xb4ffffff, 0x80000000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0xff7fffff, 0x80800000, 0x00000000],
                [0x64800000, 0xc0400000, 0xbe800000, 0x65000000],
                0x00000001,
                [0x1a000000, 0x80000003, 0x80000000, 0x1a800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x00000001, 0xbe800000, 0x00000000],
                [0x3f800000, 0x7f7fffff, 0x00800000, 0x1a000000],
                0x3e000000,
                [0x3e000000, 0x7dffffff, 0x00100000, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x00000000, 0xbe800000, 0x00800000],
                [0x80000001, 0x3e800000, 0x7f7fffff, 0x3f400000],
                0x3f600000,
                [0x80000001, 0x3e600000, 0x7f5fffff, 0x3f280000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0xbe800000, 0xbe800000, 0x00000001],
                [0xff7fffff, 0x80800000, 0x80000000, 0xbf800000],
                0x3e000000,
                [0xfdffffff, 0x80100000, 0x80000000, 0xbe000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0xc0400000, 0x40400000, 0x00800000],
                [0x80800000, 0x80800000, 0x80800000, 0x00000000],
                0x00000001,
                [0x00000000, 0x81400000, 0x01400000, 0x00800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0xe4800000, 0x3e800000, 0x3f800000],
                [0x64800000, 0x00800000, 0x00000000, 0x80000001],
                0x1a000000,
                [0xff7fffff, 0xe4800000, 0x3e800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x40400000, 0x40400000, 0x3f400000],
                [0x80800000, 0xbe800000, 0x7f7fffff, 0x3f800000],
                0x3e000000,
                [0x7f27ffff, 0x3ff80000, 0x7dffffff, 0x3f480000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0xff7fffff, 0x00000000, 0xbf800000],
                [0x3f800000, 0x80800000, 0xc0400000, 0xbf000000],
                0x1a000000,
                [0xbf800000, 0x7f7fffff, 0x9ac00000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x00800000, 0xbe800000, 0xbf800000],
                [0x3e800000, 0xbf800000, 0xe4800000, 0x3e800000],
                0x3f7fffff,
                [0x3e7ffffe, 0xbf7fffff, 0xe47fffff, 0x3e7ffffb],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00000000, 0x80000000, 0x3e800000],
                [0x3f800000, 0x00800000, 0x7f7fffff, 0x00000000],
                0x3ecccccd,
                [0x3eb9999a, 0x00333333, 0x7ecccccc, 0x3e19999a],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x00800000, 0x80000000, 0x1a000000],
                [0x64800000, 0xff7fffff, 0x00000000, 0xff7fffff],
                0x00800000,
                [0x25800000, 0xc07fffff, 0x00000000, 0xc07fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xbf800000, 0x80000000, 0xbf800000],
                [0x80000000, 0x3e800000, 0x00000001, 0x00000001],
                0x3ecccccd,
                [0x00000000, 0x3f333333, 0x00000000, 0xbf199999],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x7f7fffff, 0x3e800000, 0x00000001],
                [0x80800000, 0xe4800000, 0x64800000, 0x80000001],
                0x3f600000,
                [0x80700000, 0xe4600000, 0x64600000, 0x80000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0xbe800000, 0x40400000, 0xe5000000],
                [0xff7fffff, 0xbe800000, 0xbf800000, 0x00000000],
                0x3f7fffff,
                [0xff7ffffe, 0x58000000, 0xd9c00000, 0xd9000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xc0400000, 0x80000001, 0x00000000],
                [0xbe800000, 0x80000001, 0x00800000, 0x3e800000],
                0x3ecccccd,
                [0xbdcccccd, 0x80000000, 0x00333333, 0x3dcccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x00800000, 0x3e800000, 0xff7fffff],
                [0xbf800000, 0x80000001, 0x7f7fffff, 0x00800000],
                0x3f600000,
                [0xbf600000, 0xbeffffff, 0x7f57ffff, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0x3f800000, 0xbe800000, 0x3f800000],
                [0xbe800000, 0xbf800000, 0xe4800000, 0x00000001],
                0x00000001,
                [0xff7fffff, 0x3f800000, 0xbe800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x3e800000, 0x64800000, 0x3e800000],
                [0x40400000, 0xbf800000, 0xe4800000, 0x00000001],
                0x3f7fffff,
                [0x403fffff, 0xbf7fffff, 0xe47fffff, 0x32800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x00000001, 0x3e800000, 0x1a000000],
                [0xbe800000, 0x80000001, 0x80000000, 0x00800000],
                0x1a000000,
                [0x99000000, 0x80000000, 0x19000000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x40400000, 0x80000001, 0x80000001],
                [0x00000001, 0xff7fffff, 0x40400000, 0x1a000000],
                0x1a000000,
                [0x00000000, 0xd9ffffff, 0x1ac00000, 0x80000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x80000000, 0x3f800000, 0x00000001],
                [0x40400000, 0xe4800000, 0xbf800000, 0xbf800000],
                0x1a000000,
                [0x1b000000, 0xbf000000, 0x9a000000, 0x9a000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x7f7fffff, 0x3f800000, 0x00800000],
                [0xbe800000, 0x00000000, 0x3e800000, 0xbf800000],
                0x3f7fffff,
                [0xbe7fffff, 0x347fffff, 0x3e7fffff, 0xbf7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00000001, 0xc0400000, 0xff7fffff],
                [0x00000001, 0x80000001, 0xbf800000, 0x3f400000],
                0x3f600000,
                [0xb37fffff, 0xb37fffff, 0x7ebfffff, 0xfdffffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xff7fffff, 0x80800000, 0x3f000000],
                [0xbf800000, 0x64800000, 0x3f800000, 0x3f000000],
                0x3f600000,
                [0xbf640000, 0xfd7fffff, 0x3f600000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x80000001, 0x80000000, 0xbf000000],
                [0x3e800000, 0xbe800000, 0x3f800000, 0xe5000000],
                0x3e000000,
                [0x3d000000, 0xbd000000, 0x3e000000, 0xe3800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00000001, 0x80000001, 0xff7fffff],
                [0xbe800000, 0xbe800000, 0xbf800000, 0xff7fffff],
                0x00000001,
                [0x7e7fffff, 0xb4ffffff, 0x34ffffff, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x3e800000, 0x80800000, 0x80000001],
                [0x7f7fffff, 0x64800000, 0x80800000, 0x80000001],
                0x3f600000,
                [0x7f5fffff, 0x64600000, 0x80700000, 0x80000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x80000001, 0x80000001, 0x00800000],
                [0x00000000, 0x00000001, 0xbf800000, 0x3f000000],
                0x3ecccccd,
                [0x80000000, 0x00000000, 0xbecccccd, 0x3e4ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x7f7fffff, 0x40400000, 0x3f800000],
                [0x64800000, 0x3e800000, 0x40400000, 0xe5000000],
                0x00000001,
                [0x64800000, 0x7f7fffff, 0x40400000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x00000001, 0x64800000, 0x00000000],
                [0x00000000, 0xc0400000, 0x3e800000, 0xbf800000],
                0x3f600000,
                [0x00000000, 0xc0280000, 0x3e600000, 0xbf600000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x00800000, 0x80000001, 0x7f7fffff],
                [0x3f800000, 0x3e800000, 0x00000001, 0x80000001],
                0x3e000000,
                [0x7e5fffff, 0x4061ffff, 0xb4dfffff, 0x7f5fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0xc0400000, 0xe4800000, 0x80000001],
                [0x00800000, 0xbf800000, 0x80800000, 0x3f000000],
                0x1a000000,
                [0x00000000, 0x9a000000, 0x1a000000, 0x19800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xbe800000, 0x64800000, 0x3f400000],
                [0xe4800000, 0x7f7fffff, 0x80800000, 0xbf800000],
                0x3ecccccd,
                [0xe3cccccd, 0x7ecccccc, 0x63e66666, 0x3d4cccca],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0xbe800000, 0xff7fffff, 0x00800000],
                [0x80800000, 0xc0400000, 0x7f7fffff, 0x3f400000],
                0x3e000000,
                [0x01200000, 0xbec00000, 0x7dffffff, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0xbe800000, 0x00000001, 0x00000000],
                [0x80000000, 0x80800000, 0x80000001, 0xbf000000],
                0x3ecccccd,
                [0x00000000, 0x80333333, 0x80000000, 0xbe4ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0x00800000, 0xff7fffff, 0x1a000000],
                [0xbf800000, 0x00800000, 0x7f7fffff, 0x3e800000],
                0x1a000000,
                [0xd9ffffff, 0x00000000, 0x347fffff, 0x1a200000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0xbf800000, 0x3e800000, 0x80000001],
                [0x00000000, 0x64800000, 0xff7fffff, 0x3f800000],
                0x3f7fffff,
                [0x80000000, 0x647fffff, 0xff7ffffe, 0x3f7fffff],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x80000000, 0x80000001, 0x3f800000],
                [0x7f7fffff, 0x80000000, 0x40400000, 0x3f400000],
                0x3ecccccd,
                [0x7ecccccc, 0x00000000, 0x3f99999a, 0x3f666666],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x00000000, 0x7f7fffff, 0x80000001],
                [0x40400000, 0x3e800000, 0x80800000, 0x3f000000],
                0x3f600000,
                [0x40280000, 0x3e600000, 0xb37fffff, 0x3ee00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0xc0400000, 0xbe800000, 0x00000001],
                [0x00000001, 0xe4800000, 0xff7fffff, 0x3f400000],
                0x3e000000,
                [0x19e00000, 0xe3000000, 0xfdffffff, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0xbe800000, 0x80000001, 0x80000001],
                [0x3e800000, 0x64800000, 0xbf800000, 0xbf000000],
                0x3e000000,
                [0x3d000000, 0x63000000, 0xbe000000, 0xbd800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0x80000001, 0x3e800000, 0x3f800000],
                [0x64800000, 0x3e800000, 0x00000000, 0x3f000000],
                0x3e000000,
                [0xe4400000, 0x3d000000, 0x3e600000, 0x3f700000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x00800000, 0x64800000, 0x3e800000],
                [0x80800000, 0xbe800000, 0x80000000, 0x00800000],
                0x00000001,
                [0x80200000, 0x00200000, 0x63800000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xbe800000, 0xff7fffff, 0x1a000000],
                [0x00000000, 0xbf800000, 0xff7fffff, 0x80000001],
                0x3f600000,
                [0x97800000, 0xbf600000, 0xff5fffff, 0x18800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x3e800000, 0x80000000, 0x80000001],
                [0x64800000, 0x80000001, 0xe4800000, 0x00800000],
                0x00800000,
                [0x25800000, 0x80000000, 0xa5800000, 0x80000001],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00000000, 0x80000000, 0xe5000000],
                [0xbe800000, 0x00000001, 0x80800000, 0x7f7fffff],
                0x1a000000,
                [0x9a900000, 0x00000000, 0x80000000, 0xe4fffffc],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00800000, 0xc0400000, 0x00800000],
                [0x3e800000, 0x00000001, 0xc0400000, 0xe5000000],
                0x3ecccccd,
                [0x3dcccccd, 0x00000000, 0xbf99999a, 0xe44ccccd],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00800000, 0x00800000, 0x65000000],
                [0x00000000, 0xbf800000, 0x7f7fffff, 0x80000001],
                0x3f600000,
                [0x19000000, 0xbf600000, 0x7f5fffff, 0x63800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x80000000, 0x7f7fffff, 0x3f800000],
                [0x7f7fffff, 0x40400000, 0xe4800000, 0xff7fffff],
                0x3ecccccd,
                [0x7ecccccc, 0x3f99999a, 0x7f199999, 0xfecccccc],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0xe4800000, 0x00000001, 0x3f400000],
                [0x40400000, 0xc0400000, 0x7f7fffff, 0xe5000000],
                0x1a000000,
                [0xbf400000, 0xe4400000, 0x59ffffff, 0xbe800000],
            ),
            (
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x3f800000, 0x00000000, 0xbf000000],
                [0xbe800000, 0x00000001, 0x80000001, 0x80000001],
                0x00800000,
                [0xe4000000, 0xbf000000, 0x80000000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0x7f7fffff, 0x3f800000, 0x00000001],
                [0xbe800000, 0xbe800000, 0xe4800000, 0xbf000000],
                0x00800000,
                [0xbf800000, 0x7f7fffff, 0x3f800000, 0x803fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00000000, 0x64800000, 0xff7fffff],
                [0xbe800000, 0x80000001, 0x80000000, 0x00000001],
                0x00800000,
                [0xbe800000, 0x80000000, 0x64800000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x00800000, 0x00000000, 0x3f000000],
                [0x80000000, 0x80800000, 0x80800000, 0x7f7fffff],
                0x3ecccccd,
                [0x3fe66666, 0xbfcccccc, 0xbfcccccc, 0x7ecccccc],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x40400000, 0xbf800000, 0x3f800000],
                [0x80800000, 0x00000000, 0x00800000, 0x3f000000],
                0x3f7fffff,
                [0x33800000, 0x34400000, 0xb3800000, 0x3f000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x3e800000, 0xe4800000, 0x65000000],
                [0x64800000, 0x3f800000, 0x7f7fffff, 0xbf000000],
                0x00000001,
                [0x3f800000, 0x3e800000, 0xe4800000, 0x65000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0x00000000, 0x00000001, 0x3e800000],
                [0x00000000, 0x3e800000, 0xff7fffff, 0x00000001],
                0x3e000000,
                [0xbf600000, 0x00000000, 0xb37fffff, 0x3e600000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0xff7fffff, 0xc0400000, 0x00000000],
                [0x64800000, 0xbf800000, 0xff7fffff, 0x3e800000],
                0x3ecccccd,
                [0x62cccccd, 0xff199999, 0xfdcccccc, 0x3dcccccd],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xbf800000, 0x80000001, 0x65000000],
                [0x40400000, 0xbf800000, 0x00000000, 0xbf000000],
                0x3e000000,
                [0xbe400000, 0xbf500000, 0x80000001, 0x64e00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00000000, 0xbe800000, 0xff7fffff],
                [0x80000000, 0x80000001, 0x3f800000, 0xbf000000],
                0x00800000,
                [0xbe800000, 0x00000000, 0xbe800000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0xbe800000, 0xe4800000, 0x3f400000],
                [0x7f7fffff, 0x40400000, 0x64800000, 0xbf800000],
                0x00000001,
                [0x403ffffe, 0xbe800000, 0xe4800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x3e800000, 0x00800000, 0xff7fffff],
                [0x7f7fffff, 0x40400000, 0x00000000, 0x1a000000],
                0x00800000,
                [0x1affffff, 0x3e800000, 0x00800000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0xff7fffff, 0xbe800000, 0x00000001],
                [0xc0400000, 0xbf800000, 0x00800000, 0xe5000000],
                0x3f7fffff,
                [0x65bfffff, 0xf37fffff, 0xb2800000, 0xe4ffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x3f800000, 0xbf800000, 0xbf800000],
                [0x80000001, 0x64800000, 0x80000001, 0xbf000000],
                0x00000001,
                [0x3f800000, 0x3f800000, 0xbf800000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0x00000001, 0xe4800000, 0x3e800000],
                [0x40400000, 0x3e800000, 0x64800000, 0x3e800000],
                0x3ecccccd,
                [0x3e99999a, 0x3ccccccd, 0xe4000000, 0x3e800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x3e800000, 0x80000000, 0xe5000000],
                [0xbe800000, 0x00000001, 0xff7fffff, 0x65000000],
                0x00800000,
                [0xa5000000, 0x3e800000, 0xe5ffffff, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0x00000001, 0x00800000, 0xe5000000],
                [0xbe800000, 0x00000001, 0x80000001, 0x3f800000],
                0x3f600000,
                [0xbe600000, 0x00000001, 0x000fffff, 0xe3800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x00000000, 0x80000001, 0xe5000000],
                [0x40400000, 0x7f7fffff, 0xbe800000, 0x00000001],
                0x3ecccccd,
                [0x7f199999, 0x344ccccc, 0x80000001, 0xe4999999],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x64800000, 0x3f800000, 0xe5000000],
                [0x80800000, 0x80000000, 0x80800000, 0x65000000],
                0x1a000000,
                [0x3e800000, 0x64800000, 0x3f800000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x80000001, 0xc0400000, 0xe5000000],
                [0xbf800000, 0x3e800000, 0xbf800000, 0xbf800000],
                0x00000001,
                [0x807fffff, 0x80000001, 0xc0400000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x3f800000, 0x40400000, 0xff7fffff],
                [0xe4800000, 0x00000001, 0x00000001, 0x3f000000],
                0x3f7fffff,
                [0xe3ffffff, 0x33800000, 0x34400000, 0xf37fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbf800000, 0x80800000, 0x80800000, 0xbf000000],
                [0x80800000, 0x3f800000, 0x00000000, 0xbf000000],
                0x1a000000,
                [0xbf800000, 0x99800000, 0x80800000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0xff7fffff, 0x64800000, 0x00800000],
                [0x80000000, 0xe4800000, 0x80800000, 0xbf000000],
                0x3e000000,
                [0x80700000, 0xff5fffff, 0x64600000, 0xbd800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x3e800000, 0x80000000, 0x3f800000],
                [0x00000001, 0x00800000, 0x7f7fffff, 0x00000001],
                0x3f600000,
                [0x80100000, 0x3d000000, 0x34dfffff, 0x3e000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x80800000, 0x64800000, 0x3f000000],
                [0xbe800000, 0x7f7fffff, 0x00800000, 0x3f800000],
                0x3e000000,
                [0x7f5fffff, 0x7dffffff, 0x64600000, 0x3f100000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x80800000, 0x3f800000, 0x3f400000],
                [0x80000000, 0x80800000, 0xbf800000, 0xbf800000],
                0x00000001,
                [0x00000001, 0x80800000, 0x3f800000, 0x3f400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x80800000, 0x64800000, 0x7f7fffff],
                [0x3f800000, 0x3f800000, 0x3e800000, 0x00800000],
                0x3e000000,
                [0xc0280000, 0x80600000, 0x64600000, 0x7f5fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x40400000, 0x80800000, 0xe5000000],
                [0xbe800000, 0x40400000, 0xbf800000, 0x3f800000],
                0x3f7fffff,
                [0x58800000, 0x40400000, 0xbf7fffff, 0xd9000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x3f800000, 0x80000000, 0xe5000000],
                [0x40400000, 0x7f7fffff, 0xbe800000, 0x3f800000],
                0x1a000000,
                [0xc0400000, 0x59ffffff, 0x99000000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x80000001, 0x00800000, 0x3f000000],
                [0x00800000, 0xbf800000, 0x64800000, 0x3f800000],
                0x3f600000,
                [0x3ec00000, 0xbf600000, 0x64600000, 0x3f700000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0x00000001, 0x3e800000, 0xbf800000],
                [0x00800000, 0x80000000, 0x80000000, 0x3f800000],
                0x3f7fffff,
                [0x58800000, 0x00000000, 0x32800000, 0x3f7ffffe],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x3f800000, 0xe4800000, 0x3f800000],
                [0x64800000, 0xbe800000, 0x80000000, 0x3e800000],
                0x3f600000,
                [0x63600000, 0x3d900000, 0xe3000000, 0x3eb00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x40400000, 0x40400000, 0xbf800000],
                [0x7f7fffff, 0x3f800000, 0x80000001, 0x00800000],
                0x3e000000,
                [0x3f380000, 0x40280000, 0x40280000, 0xbf600000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0xe4800000, 0x64800000, 0x1a000000],
                [0x80800000, 0xff7fffff, 0x40400000, 0xbf800000],
                0x00800000,
                [0xc0400000, 0xe4800000, 0x64800000, 0x1a000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0xe4800000, 0x64800000, 0x7f7fffff],
                [0xc0400000, 0x3f800000, 0x80000001, 0xff7fffff],
                0x1a000000,
                [0x5abfffff, 0xe4800004, 0x64800000, 0x7f7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00800000, 0xff7fffff, 0x3f800000, 0x80000001],
                [0x00800000, 0x80800000, 0x00000000, 0x3f400000],
                0x00000001,
                [0x00800000, 0xff7fffff, 0x3f800000, 0x80000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0xc0400000, 0x80000000, 0xe5000000],
                [0x3f800000, 0x00000000, 0x40400000, 0xff7fffff],
                0x00000001,
                [0xbe800010, 0xc0400000, 0xb5bfffff, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0x00800000, 0xc0400000, 0x00000000],
                [0xe4800000, 0xbf800000, 0x80000000, 0x3f400000],
                0x3ecccccd,
                [0xe4666666, 0xbe99999a, 0xbfe66666, 0x3e99999a],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x64800000, 0xe4800000, 0xbf800000, 0x3f800000],
                [0x80800000, 0xc0400000, 0x80800000, 0xbf000000],
                0x3f600000,
                [0x63000000, 0xe3000000, 0xbe000000, 0xbea00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x7f7fffff, 0x80000001, 0xbf000000],
                [0x00000001, 0x80000000, 0x00000001, 0xe5000000],
                0x00000001,
                [0x80000000, 0x7f7fffff, 0x80000001, 0xbf000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0x7f7fffff, 0xc0400000, 0xbf000000],
                [0xbf800000, 0x64800000, 0xbe800000, 0xbf000000],
                0x00800000,
                [0x7f7fffff, 0x7f7fffff, 0xc0400000, 0xbf000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x40400000, 0x40400000, 0xe4800000, 0x00000000],
                [0x00000001, 0xbf800000, 0x7f7fffff, 0xbf000000],
                0x3e000000,
                [0x40280000, 0x402c0000, 0xfd7fffff, 0xbd800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x40400000, 0x80800000, 0xe5000000],
                [0x40400000, 0x00000001, 0x64800000, 0x80000001],
                0x1a000000,
                [0x80800000, 0x40400000, 0x80800000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80800000, 0x80800000, 0x40400000, 0xe5000000],
                [0x80000001, 0x7f7fffff, 0x7f7fffff, 0x00000001],
                0x00000001,
                [0x80800000, 0x80800000, 0x40400000, 0xe5000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0xbf800000, 0xbf800000, 0x3f400000],
                [0x80000001, 0xbf800000, 0x64800000, 0x80000001],
                0x3f7fffff,
                [0xb4400000, 0xb3800000, 0xb3800000, 0x33400000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0xe4800000, 0x00800000, 0x80000001],
                [0xe4800000, 0xe4800000, 0x00000000, 0xbf800000],
                0x3ecccccd,
                [0x63cccccd, 0xe34ccccc, 0x004ccccd, 0xbecccccd],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0xc0400000, 0x40400000, 0x00000001],
                [0x3e800000, 0x00800000, 0x80800000, 0x65000000],
                0x3f600000,
                [0x63a00000, 0xbec00000, 0x3ec00000, 0x64e00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0x7f7fffff, 0x64800000, 0x00800000],
                [0xbe800000, 0xe4800000, 0x00800000, 0xff7fffff],
                0x00000001,
                [0x3f800001, 0x7f7fffff, 0x64800000, 0xb4ffffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x00000001, 0xe4800000, 0x00000000],
                [0x40400000, 0x80000001, 0xbe800000, 0xff7fffff],
                0x00800000,
                [0xc16fffff, 0x00000005, 0xe4800000, 0xc07fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xe4800000, 0xff7fffff, 0x00000000, 0x7f7fffff],
                [0xbe800000, 0xbf800000, 0xff7fffff, 0x1a000000],
                0x3f7fffff,
                [0xd8800000, 0xf37fffff, 0xd9fffffe, 0x737fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x7f7fffff, 0xc0400000, 0x64800000, 0x00000000],
                [0x40400000, 0x80000000, 0x3e800000, 0x1a000000],
                0x1a000000,
                [0x7f7fffff, 0xc0400000, 0x64800000, 0x00000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xff7fffff, 0xff7fffff, 0xff7fffff, 0xff7fffff],
                [0x40400000, 0x64800000, 0xbf800000, 0x00800000],
                0x3e000000,
                [0xff5fffff, 0xff5fffff, 0xff5fffff, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x00000001, 0x80800000, 0x3e800000],
                [0x3e800000, 0xe4800000, 0x80800000, 0xff7fffff],
                0x00800000,
                [0xbf9fffff, 0x657fffff, 0x013fffff, 0xc06fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0x00800000, 0xbe800000, 0x7f7fffff],
                [0x00800000, 0x3e800000, 0x80800000, 0x65000000],
                0x3f7fffff,
                [0x25ffffff, 0x63ffffff, 0xb2800000, 0x737fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x7f7fffff, 0x7f7fffff, 0x65000000],
                [0x80800000, 0x3e800000, 0x00800000, 0x3f400000],
                0x3ecccccd,
                [0x80266667, 0x7f199999, 0x7f199999, 0x6499999a],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xbe800000, 0x80000001, 0xbe800000, 0xbf800000],
                [0x80000001, 0xc0400000, 0xe4800000, 0x3f400000],
                0x1a000000,
                [0xbe800000, 0x9a900000, 0xbf200000, 0xbf800000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000001, 0x00000000, 0xc0400000, 0x00800000],
                [0x00000000, 0xc0400000, 0xbe800000, 0x00000000],
                0x3f7fffff,
                [0x80000000, 0x00000000, 0xb4400000, 0x00000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000000, 0xbe800000, 0xbe800000, 0x3f400000],
                [0xc0400000, 0x3e800000, 0xc0400000, 0x1a000000],
                0x3f600000,
                [0x9aa80000, 0xbd000000, 0xbd000000, 0x3dc00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0xe4800000, 0xbf800000, 0x3e800000],
                [0x00000000, 0x80000000, 0x80000001, 0x65000000],
                0x3f600000,
                [0x3e000000, 0xe3000000, 0xbe000000, 0x64e00000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0xc0400000, 0x40400000, 0x40400000, 0x00000000],
                [0x3e800000, 0xbe800000, 0x3f800000, 0x7f7fffff],
                0x3ecccccd,
                [0x7dcccccc, 0xfdcccccc, 0x7ecccccc, 0x7ecccccc],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00000000, 0xbe800000, 0x3e800000],
                [0x00000000, 0x7f7fffff, 0x40400000, 0x80000001],
                0x3f600000,
                [0x00000000, 0xb4dfffff, 0xbd000000, 0x3d000000],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x00000001, 0x00800000, 0x00000000, 0xff7fffff],
                [0x00000000, 0xff7fffff, 0x00000001, 0x3f400000],
                0x3e000000,
                [0x00000001, 0xfdbfffff, 0x00000000, 0xff5fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x80000000, 0xc0400000, 0xe4800000, 0x1a000000],
                [0x80000001, 0x40400000, 0xbf800000, 0x3f000000],
                0x3f7fffff,
                [0x80000000, 0x3fbffffe, 0xd8800000, 0x3effffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3e800000, 0x7f7fffff, 0x3f800000, 0xff7fffff],
                [0xbf800000, 0x00800000, 0x3e800000, 0x3f400000],
                0x1a000000,
                [0x3e800000, 0x7f7fffff, 0x3f800000, 0xff7fffff],
            ),
            (
                ColorFrameAlpha::PremultipliedFloat,
                ColorFrameAlpha::StraightFloat,
                ColorFrameAlpha::PremultipliedFloat,
                [0x3f800000, 0xe4800000, 0x00000000, 0x00000001],
                [0x80800000, 0x80000001, 0x7f7fffff, 0x3f800000],
                0x00000001,
                [0x3f800000, 0xe4800000, 0x34ffffff, 0x00000002],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x7f7fffff, 0x00800000, 0x3f800000],
                [0x80000000, 0xbf800000, 0xe4800000, 0x00000001],
                0x00000001,
                [0x00000001, 0x7f7fffff, 0x00800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x7f7fffff, 0xff7fffff, 0x3f800000],
                [0xbe800000, 0x00800000, 0x40400000, 0x3f400000],
                0x3e000000,
                [0xff6739ce, 0x7f6739ce, 0xff6739ce, 0x3f780000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x7f7fffff, 0x80000001, 0x3f800000],
                [0xff7fffff, 0x7f7fffff, 0x00800000, 0x3f000000],
                0x3f600000,
                [0xff471c71, 0x7f7fffff, 0x00638e39, 0x3f100000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x00000000, 0xbe800000, 0x3f800000],
                [0xbe800000, 0x00000000, 0x80800000, 0x00800000],
                0x00800000,
                [0x80000000, 0x00000000, 0xbe800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0x00800000, 0x00000000, 0x3f800000],
                [0xe4800000, 0x80800000, 0x3e800000, 0x00800000],
                0x1a000000,
                [0xbf800000, 0x00800000, 0x00000000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0x3e800000, 0x00000001, 0x3f800000],
                [0x80000000, 0x64800000, 0x80800000, 0x00000000],
                0x3f600000,
                [0x40400000, 0x3e800000, 0x00000001, 0x3e000000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0x3e800000, 0x80800000, 0x3f800000],
                [0x00000001, 0x80000001, 0xbf800000, 0x3e800000],
                0x3ecccccd,
                [0x006db6dc, 0x3e5b6db7, 0xbe124925, 0x3f333333],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x00000001, 0xbe800000, 0x3f800000],
                [0xe4800000, 0xbf800000, 0xbf800000, 0x00000001],
                0x00800000,
                [0x80800000, 0x00000001, 0xbe800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x7f7fffff, 0xbe800000, 0x00000000, 0x3f800000],
                [0x64800000, 0xbf800000, 0x80000001, 0x00000001],
                0x1a000000,
                [0x7f7fffff, 0xbe800000, 0x80000000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x00000000, 0x3f800000, 0x3f800000],
                [0x80800000, 0x00000000, 0x80800000, 0x3f000000],
                0x1a000000,
                [0x3e800000, 0x00000000, 0x3f800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x3f800000, 0x7f7fffff, 0x3f800000],
                [0xbf800000, 0xc0400000, 0x3f800000, 0x00000000],
                0x1a000000,
                [0x00000000, 0x3f800000, 0x7f7fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x80000001, 0x00000000, 0x3f800000],
                [0xff7fffff, 0x3f800000, 0x00000000, 0x00000001],
                0x3ecccccd,
                [0xb4aaaaaa, 0x80000000, 0x00000000, 0x3f19999a],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x3f800000, 0x3f800000, 0x3f800000],
                [0x00800000, 0x64800000, 0x80800000, 0x3f000000],
                0x3e000000,
                [0xc0333333, 0x62888889, 0x3f6eeeef, 0x3f700000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x40400000, 0x80800000, 0x3f800000],
                [0x40400000, 0x80000000, 0x00000000, 0x00000000],
                0x00000001,
                [0xc0400000, 0x40400000, 0x80800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x40400000, 0xbe800000, 0x3f800000],
                [0x3f800000, 0x7f7fffff, 0x00800000, 0x00000001],
                0x3f600000,
                [0xe4800000, 0x4040000e, 0xbe800000, 0x3e000000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x3e800000, 0xe4800000, 0x3f800000],
                [0xff7fffff, 0x40400000, 0xe4800000, 0x00000000],
                0x3f7fffff,
                [0x80000001, 0x3e800000, 0xe4800000, 0x33800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x64800000, 0x00800000, 0x3e800000, 0x3f800000],
                [0xbf800000, 0x80000000, 0x80800000, 0x00000001],
                0x3ecccccd,
                [0x64800000, 0x00800000, 0x3e800000, 0x3f19999a],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0xbe800000, 0xff7fffff, 0x3f800000],
                [0x80800000, 0xbf800000, 0x80000000, 0x1a000000],
                0x3e000000,
                [0x40400000, 0xbe800000, 0xff7fffff, 0x3f600000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x7f7fffff, 0xe4800000, 0x3f800000],
                [0x00800000, 0x80000001, 0xc0400000, 0x3f000000],
                0x3e000000,
                [0x3e6eeeef, 0x7f6eeeee, 0xe46eeeef, 0x3f700000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x64800000, 0xe4800000, 0x3f800000],
                [0x40400000, 0xbf800000, 0x64800000, 0x3f800000],
                0x3f7fffff,
                [0x403fffff, 0x58800000, 0x647ffffe, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80800000, 0x80800000, 0x00000001, 0x3f800000],
                [0x00800000, 0x80800000, 0x00000000, 0x00000000],
                0x3e000000,
                [0x80800000, 0x80800000, 0x00000001, 0x3f600000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0xe4800000, 0x64800000, 0x3f800000],
                [0x3e800000, 0xc0400000, 0x64800000, 0x00000000],
                0x00800000,
                [0xbf800000, 0xe4800000, 0x64800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0x3e800000, 0x00000000, 0x3f800000],
                [0xff7fffff, 0xff7fffff, 0xe4800000, 0x3f000000],
                0x3ecccccd,
                [0xfe7fffff, 0xfe7fffff, 0xe3800000, 0x3f4ccccd],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x00000001, 0xbf800000, 0x3f800000],
                [0xbf800000, 0x80800000, 0x00000000, 0x00000001],
                0x3f7fffff,
                [0x80ffffff, 0x00000001, 0xbf800000, 0x33800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0xff7fffff, 0x00800000, 0x3f800000],
                [0xe4800000, 0x3e800000, 0x3e800000, 0x3f400000],
                0x3f7fffff,
                [0xe47fffff, 0xf3aaaaaa, 0x3e7fffff, 0x3f400000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x3e800000, 0x40400000, 0x3f800000],
                [0x00000001, 0xe4800000, 0xe4800000, 0x00000001],
                0x3ecccccd,
                [0xff7fffff, 0x3e800000, 0x40400000, 0x3f19999a],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x64800000, 0x00800000, 0x3f800000],
                [0x3e800000, 0x00000001, 0x80000000, 0x3e800000],
                0x3f7fffff,
                [0x3e7ffffc, 0x597ffffd, 0x00000002, 0x3e800002],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0xe4800000, 0x80000001, 0x3f800000],
                [0x80000000, 0xbe800000, 0x3e800000, 0x3f400000],
                0x1a000000,
                [0x00000000, 0xe4800000, 0x18c00000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x7f7fffff, 0xff7fffff, 0x3f800000],
                [0xbf800000, 0x3e800000, 0x3f800000, 0x3e800000],
                0x3f600000,
                [0xbf22e8ba, 0x7eba2e8b, 0xfeba2e8b, 0x3eb00000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x3e800000, 0xff7fffff, 0x3f800000],
                [0x40400000, 0xff7fffff, 0x40400000, 0x1a000000],
                0x00000001,
                [0x00000001, 0x3e800000, 0xff7fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x80800000, 0x00000000, 0x3f800000],
                [0x80800000, 0x80000001, 0x00800000, 0x00000000],
                0x3f7fffff,
                [0x00000000, 0x80800000, 0x00000000, 0x33800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0x80800000, 0x3f800000, 0x3f800000],
                [0xbf800000, 0x64800000, 0x80000001, 0x00000001],
                0x00800000,
                [0x00800000, 0x80800000, 0x3f800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x3f800000, 0xbf800000, 0x3f800000],
                [0x80000001, 0x3f800000, 0x80800000, 0x3f000000],
                0x3ecccccd,
                [0xe4400000, 0x3f800000, 0xbf400000, 0x3f4ccccd],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x7f7fffff, 0xc0400000, 0x3f800000],
                [0x80000000, 0x3e800000, 0x80000001, 0x3f800000],
                0x3f600000,
                [0x00000000, 0x7dffffff, 0xbec00000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x7f7fffff, 0x00000000, 0x3f800000],
                [0x40400000, 0x00800000, 0x00000000, 0x3e800000],
                0x1a000000,
                [0x3e800000, 0x7f7fffff, 0x00000000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x00800000, 0x00000001, 0x3f800000],
                [0xbe800000, 0x7f7fffff, 0x3e800000, 0x3f400000],
                0x3f600000,
                [0xbf30a3d7, 0x7f570a3d, 0x3e570a3d, 0x3f480000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0xff7fffff, 0x80000001, 0x3f800000],
                [0x80800000, 0x3f800000, 0x80000000, 0x00000001],
                0x00000001,
                [0x00000001, 0xff7fffff, 0x80000001, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x80000000, 0xff7fffff, 0x3f800000],
                [0xbe800000, 0xc0400000, 0x7f7fffff, 0x3e800000],
                0x1a000000,
                [0x98000000, 0x99c00000, 0xff7fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0x00000001, 0xc0400000, 0x3f800000],
                [0x3e800000, 0x3e800000, 0x7f7fffff, 0x00000001],
                0x00000001,
                [0x40400000, 0x00000001, 0xc0400000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0x00000000, 0x00000001, 0x3f800000],
                [0x80800000, 0xe4800000, 0xbe800000, 0x1a000000],
                0x1a000000,
                [0xbe800000, 0x99800000, 0x00000001, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3f800000, 0x80800000, 0x40400000, 0x3f800000],
                [0x00000000, 0xff7fffff, 0xff7fffff, 0x00000001],
                0x00800000,
                [0x3f800000, 0x80800004, 0x40400000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x80000001, 0x00000000, 0x3f800000],
                [0xbe800000, 0x00800000, 0x00000001, 0x3f000000],
                0x3f7fffff,
                [0xbe7ffffe, 0x007fffff, 0x00000001, 0x3f000000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0x3e800000, 0x00000001, 0x3f800000],
                [0x40400000, 0xff7fffff, 0x80000001, 0x3f000000],
                0x3f600000,
                [0x40155555, 0xff471c71, 0x80000001, 0x3f100000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x64800000, 0xe4800000, 0x3f800000],
                [0xbe800000, 0x00000001, 0x7f7fffff, 0x3f800000],
                0x3f600000,
                [0xbe600000, 0x63000000, 0x7f5fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x00800000, 0xbe800000, 0x3f800000],
                [0x7f7fffff, 0x3e800000, 0x7f7fffff, 0x00800000],
                0x3ecccccd,
                [0x402aaaaa, 0x00955555, 0x401aaaaa, 0x3f19999a],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbe800000, 0x40400000, 0x7f7fffff, 0x3f800000],
                [0xbf800000, 0xbe800000, 0x7f7fffff, 0x3f400000],
                0x3ecccccd,
                [0xbf000000, 0x3ff55555, 0x7f7fffff, 0x3f666666],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000000, 0x40400000, 0x00800000, 0x3f800000],
                [0xbf800000, 0x00800000, 0xbe800000, 0x3e800000],
                0x3e000000,
                [0xbd0d3dcb, 0x4039611a, 0xbc0d3dcb, 0x3f680000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x3f800000, 0xbf800000, 0x3f800000],
                [0xbe800000, 0xbe800000, 0x80000000, 0x3f800000],
                0x3e000000,
                [0xff5fffff, 0x3f580000, 0xbf600000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x3e800000, 0x80800000, 0x00000000, 0x3f800000],
                [0x3f800000, 0x80000000, 0x00800000, 0x1a000000],
                0x00800000,
                [0x3e800000, 0x80800000, 0x00000000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x7f7fffff, 0x00800000, 0x3e800000, 0x3f800000],
                [0x80800000, 0x3e800000, 0x3e800000, 0x00000000],
                0x3ecccccd,
                [0x7f7fffff, 0x00800000, 0x3e800000, 0x3f19999a],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xe4800000, 0x40400000, 0x3f800000, 0x3f800000],
                [0xbf800000, 0x80000000, 0x3f800000, 0x1a000000],
                0x00000001,
                [0xe4800000, 0x40400000, 0x3f800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000001, 0x00000001, 0x80000001, 0x3f800000],
                [0x00000001, 0x80000000, 0x80000001, 0x1a000000],
                0x1a000000,
                [0x80000001, 0x00000001, 0x80000001, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x40400000, 0x7f7fffff, 0x3f800000],
                [0x00000001, 0x7f7fffff, 0x80000000, 0x3f400000],
                0x00000001,
                [0xc0400000, 0x40400001, 0x7f7fffff, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0x3e800000, 0x3f800000, 0x3f800000],
                [0x3f800000, 0xe4800000, 0x64800000, 0x00000001],
                0x3ecccccd,
                [0x00000002, 0x3e800000, 0x3f800000, 0x3f19999a],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x80000001, 0x3e800000, 0x3f800000],
                [0x64800000, 0x3e800000, 0xbe800000, 0x3f400000],
                0x00800000,
                [0xc0400000, 0x0017ffff, 0x3e800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0xc0400000, 0x80000000, 0x3f800000],
                [0xbe800000, 0x80000001, 0x00800000, 0x00000001],
                0x00800000,
                [0x00000001, 0xc0400000, 0x00000000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0x7f7fffff, 0x40400000, 0x3f800000],
                [0xbf800000, 0x80000001, 0x80000000, 0x3f800000],
                0x3e000000,
                [0xc0300000, 0x7f5fffff, 0x40280000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00800000, 0x64800000, 0xc0400000, 0x3f800000],
                [0x80000001, 0x00000001, 0x00000001, 0x00000000],
                0x3e000000,
                [0x00800000, 0x64800000, 0xc0400000, 0x3f600000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xc0400000, 0xbe800000, 0x00000001, 0x3f800000],
                [0x80000000, 0xc0400000, 0xbe800000, 0x3f000000],
                0x1a000000,
                [0xc0400000, 0xbe800000, 0x98800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x00000001, 0xbe800000, 0xc0400000, 0x3f800000],
                [0x00000001, 0x7f7fffff, 0x00000001, 0x3f000000],
                0x3f7fffff,
                [0x00000001, 0x7f7ffffd, 0xb4bfffff, 0x3f000000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x80000000, 0x80800000, 0x64800000, 0x3f800000],
                [0xc0400000, 0x64800000, 0x80000000, 0x00000001],
                0x00000001,
                [0x80000000, 0x80800000, 0x64800000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xbf800000, 0x40400000, 0x00800000, 0x3f800000],
                [0xbf800000, 0x40400000, 0xbf800000, 0x3f400000],
                0x3f600000,
                [0xbf800000, 0x40400000, 0xbf570a3d, 0x3f480000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0x40400000, 0xbf800000, 0x80000001, 0x3f800000],
                [0x3e800000, 0xbe800000, 0x64800000, 0x3e800000],
                0x1a000000,
                [0x40400000, 0xbf800000, 0x3e000000, 0x3f800000],
            ),
            (
                ColorFrameAlpha::Opaque,
                ColorFrameAlpha::StraightCoverage,
                ColorFrameAlpha::StraightCoverage,
                [0xff7fffff, 0x7f7fffff, 0xc0400000, 0x3f800000],
                [0x80000000, 0x3f800000, 0x00800000, 0x3e800000],
                0x3f7fffff,
                [0xf47ffffc, 0x747ffffc, 0xb53ffffe, 0x3e800002],
            ),
        ];
        let mut certified = 0;
        for (left_alpha, right_alpha, output_alpha, left, right, amount, expected) in cases {
            let left = left.map(f32::from_bits);
            let right = right.map(f32::from_bits);
            let amount = f32::from_bits(amount);
            if let Some(value) =
                mix_sample_certified(left, left_alpha, right, right_alpha, amount, output_alpha)
            {
                certified += 1;
                assert_eq!(
                    value.map(f32::to_bits),
                    expected,
                    "certified exact domain result"
                );
            }
            let actual = mix_sample(left, left_alpha, right, right_alpha, amount, output_alpha)
                .expect("finite exact result");
            assert_eq!(
                actual.map(f32::to_bits),
                expected,
                "certified or bounded exact fallback"
            );
        }
        assert!(
            certified > 0,
            "exercise the actual certified path, not only fallback"
        );
    }

    #[test]
    fn mix_certified_uncertain_midpoints_zero_sign_and_emission_use_exact_fallback() {
        let midpoint = 1.0 + 2.0_f64.powi(-24);
        assert_eq!(
            MixInterval {
                lower: midpoint.next_down(),
                upper: midpoint.next_up()
            }
            .rounded(),
            None
        );
        assert_eq!(
            MixInterval { lower: -f64::from_bits(1), upper: -0.0 }.rounded(),
            None
        );
        assert_eq!(
            MixInterval::point(-0.0).rounded().expect("proved exact zero").to_bits(),
            0
        );
        assert_eq!(
            MixInterval { lower: -1.0, upper: 1.0 }.reciprocal().map(|_| ()),
            None
        );
        let left = [1.0, 0.0, 0.0, 1.0];
        let right = [0.0, 0.0, 0.0, -1.0];
        assert!(mix_sample_certified(
            left,
            ColorFrameAlpha::StraightFloat,
            right,
            ColorFrameAlpha::StraightFloat,
            0.5,
            ColorFrameAlpha::StraightFloat
        )
        .is_none());
        assert!(
            mix_sample(
                left,
                ColorFrameAlpha::StraightFloat,
                right,
                ColorFrameAlpha::StraightFloat,
                0.5,
                ColorFrameAlpha::StraightFloat
            )
            .is_none(),
            "exact zero-alpha emission remains refusal"
        );
        // A representable exact halfway value is still decided by the integer
        // authority if outward interval rounding crosses its tie-even cell.
        let left = [1.0, 0.0, 0.0, 1.0];
        let right = [f32::from_bits(0x3f800001), 0.0, 0.0, 1.0];
        assert!(mix_sample_certified(
            left,
            ColorFrameAlpha::StraightCoverage,
            right,
            ColorFrameAlpha::StraightCoverage,
            0.5,
            ColorFrameAlpha::StraightCoverage
        )
        .is_none());
        assert_eq!(
            mix_sample(
                left,
                ColorFrameAlpha::StraightCoverage,
                right,
                ColorFrameAlpha::StraightCoverage,
                0.5,
                ColorFrameAlpha::StraightCoverage
            )
            .expect("exact tie")[0]
                .to_bits(),
            0x3f800000
        );
    }
}
