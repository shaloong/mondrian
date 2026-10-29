//! Fixed-width exact projection for the SDK's anchored musical timeline.
//!
//! This private arithmetic has no storage owner: all intermediates are stack
//! values. With signed i64 sample/zero numerators, positive i64 denominators
//! and duration numerator, and a nonzero u32 rate, the numerator needs at most
//! 190 bits and denominator at most 158 bits. Constant meter projection needs
//! at most 207 bits. Four fixed limbs suffice; no general bigint is required.

use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Wide([u64; 4]);

impl Wide {
    fn from_u128(value: u128) -> Self {
        Self([value as u64, (value >> 64) as u64, 0, 0])
    }

    fn product(left: u128, right: u64) -> Self {
        let low = u128::from(left as u64) * u128::from(right);
        // The product of two u64 values plus the preceding high carry fits
        // u128: (2^64-1)^2 + (2^64-2) < 2^128.
        let high = (left >> 64) * u128::from(right) + (low >> 64);
        Self([low as u64, high as u64, (high >> 64) as u64, 0])
    }

    fn is_zero(self) -> bool {
        self.0 == [0; 4]
    }

    fn bits(self) -> u32 {
        for (index, limb) in self.0.into_iter().enumerate().rev() {
            if limb != 0 {
                return index as u32 * 64 + 64 - limb.leading_zeros();
            }
        }
        0
    }

    fn compare(self, other: Self) -> Ordering {
        for (left, right) in self.0.into_iter().zip(other.0).rev() {
            match left.cmp(&right) {
                Ordering::Equal => {}
                order => return order,
            }
        }
        Ordering::Equal
    }

    fn shift_left(self, shift: u32) -> Self {
        debug_assert!(self.is_zero() || self.bits() + shift <= 256);
        let words = (shift / 64) as usize;
        let bits = shift % 64;
        let mut result = [0; 4];
        for (index, limb) in self.0.into_iter().enumerate() {
            let destination = index + words;
            if destination < 4 {
                result[destination] |= limb << bits;
                if bits != 0 && destination + 1 < 4 {
                    result[destination + 1] |= limb >> (64 - bits);
                }
            }
        }
        Self(result)
    }

    fn subtract(self, other: Self) -> Self {
        debug_assert!(self.compare(other) != Ordering::Less);
        let mut result = [0; 4];
        let mut borrow = false;
        for (index, result) in result.iter_mut().enumerate() {
            let (difference, first) = self.0[index].overflowing_sub(other.0[index]);
            let (difference, second) = difference.overflowing_sub(u64::from(borrow));
            *result = difference;
            borrow = first || second;
        }
        debug_assert!(!borrow);
        Self(result)
    }

    // All meter products are at most 207 bits, below this fixed scratch width.
    fn multiply(self, factor: u64) -> Self {
        let mut result = [0; 4];
        let mut carry = 0_u128;
        for (target, limb) in result.iter_mut().zip(self.0) {
            let product = u128::from(limb) * u128::from(factor) + carry;
            *target = product as u64;
            carry = product >> 64;
        }
        debug_assert_eq!(carry, 0);
        Self(result)
    }

    fn add(self, other: Self) -> Self {
        let mut result = [0; 4];
        let mut carry = 0_u128;
        for (index, target) in result.iter_mut().enumerate() {
            let sum = u128::from(self.0[index]) + u128::from(other.0[index]) + carry;
            *target = sum as u64;
            carry = sum >> 64;
        }
        debug_assert_eq!(carry, 0);
        Self(result)
    }

    fn remainder(self, denominator: Self) -> Self {
        let mut remainder = Self::from_u128(0);
        for bit in (0..self.bits() as usize).rev() {
            remainder = remainder.shift_left(1);
            remainder.0[0] |= (self.0[bit / 64] >> (bit % 64)) & 1;
            if remainder.compare(denominator) != Ordering::Less {
                remainder = remainder.subtract(denominator);
            }
        }
        remainder
    }
}

/// Round one exact positive ratio to binary64, nearest with ties to even.
///
/// Callers prove a nonzero denominator and a normal finite result (or zero).
/// Normalization shifts the smaller operand only to the other's magnitude;
/// it never computes the potentially 210-bit `N << (52 - exponent)`.
fn nearest_even(numerator: Wide, denominator: Wide, negative: bool) -> f64 {
    debug_assert!(!denominator.is_zero());
    if numerator.is_zero() {
        // Mathematical zero has no authored negative-zero sign.
        return 0.0;
    }
    let mut exponent = numerator.bits() as i32 - denominator.bits() as i32;
    let smaller = if exponent >= 0 {
        numerator.compare(denominator.shift_left(exponent as u32)) == Ordering::Less
    } else {
        numerator
            .shift_left((-exponent) as u32)
            .compare(denominator)
            == Ordering::Less
    };
    if smaller {
        exponent -= 1;
    }
    let (normalized_numerator, normalized_denominator) = if exponent >= 0 {
        (numerator, denominator.shift_left(exponent as u32))
    } else {
        (numerator.shift_left((-exponent) as u32), denominator)
    };
    debug_assert!(normalized_numerator.compare(normalized_denominator) != Ordering::Less);
    let mut remainder = normalized_numerator.subtract(normalized_denominator);
    debug_assert!(remainder.compare(normalized_denominator) == Ordering::Less);
    // The leading normalized bit is exactly one. Generate its following 52
    // bits with long division, maintaining 0 <= remainder < denominator.
    // The normalized divisor is <=207 bits, hence 2*remainder is <=208 bits.
    let mut significand = 1_u64;
    for _ in 0..52 {
        remainder = remainder.shift_left(1);
        significand <<= 1;
        if remainder.compare(normalized_denominator) != Ordering::Less {
            remainder = remainder.subtract(normalized_denominator);
            significand |= 1;
        }
    }
    // Exact remainder comparison includes every discarded bit: no floating
    // division, intermediate rounding, or approximate sticky bit is involved.
    let halfway = remainder.shift_left(1).compare(normalized_denominator);
    if halfway == Ordering::Greater || (halfway == Ordering::Equal && significand & 1 != 0) {
        significand += 1;
        if significand == 1_u64 << 53 {
            significand >>= 1;
            exponent += 1;
        }
    }
    debug_assert!((-1022..=1023).contains(&exponent));
    let sign = u64::from(negative) << 63;
    let biased_exponent = (exponent + 1023) as u64;
    let fraction = significand & ((1_u64 << 52) - 1);
    f64::from_bits(sign | (biased_exponent << 52) | fraction)
}

pub(super) fn quarter_notes(
    sample: i64,
    rate: u32,
    zero_numerator: i64,
    zero_denominator: i64,
    duration_numerator: i64,
    duration_denominator: i64,
) -> Option<f64> {
    if rate == 0 || zero_denominator <= 0 || duration_numerator <= 0 || duration_denominator <= 0 {
        return None;
    }
    // |sample*b - a*rate| < 2^127 for the admitted scalar widths, so this
    // exact cancellation cannot overflow signed i128, including i64::MIN.
    let difference = i128::from(sample) * i128::from(zero_denominator)
        - i128::from(zero_numerator) * i128::from(rate);
    let numerator = Wide::product(difference.unsigned_abs(), duration_denominator as u64);
    let denominator = Wide::product(
        u128::from(rate) * zero_denominator as u128,
        duration_numerator as u64,
    );
    // Nonzero N/D > 2^-158. Independently,
    // |sample/rate - a/b| < 2^64 and d/c < 2^63, so |Q| < 2^127.
    // Rounding can reach 2^127, still a normal finite binary64 value.
    Some(nearest_even(numerator, denominator, difference < 0))
}

// Only the validated parent DTO supplies these strictly positive scalars.
pub(super) fn tempo(duration_numerator: i64, duration_denominator: i64) -> f64 {
    debug_assert!(duration_numerator > 0 && duration_denominator > 0);
    nearest_even(
        Wide::from_u128(60 * duration_denominator as u128),
        Wide::from_u128(duration_numerator as u128),
        false,
    )
}

/// Exact Euclidean bar floor, then one binary64 rounding. A quotient is never
/// narrowed to CLAP's i32 bar number: VST3 has no corresponding integer field.
#[allow(clippy::too_many_arguments)]
pub(super) fn bar_start(
    sample: i64,
    rate: u32,
    zero_numerator: i64,
    zero_denominator: i64,
    duration_numerator: i64,
    duration_denominator: i64,
    meter_numerator: u16,
    meter_denominator: u16,
) -> Option<f64> {
    if rate == 0
        || zero_denominator <= 0
        || duration_numerator <= 0
        || duration_denominator <= 0
        || meter_numerator == 0
        || meter_denominator == 0
    {
        return None;
    }
    let difference = i128::from(sample) * i128::from(zero_denominator)
        - i128::from(zero_numerator) * i128::from(rate);
    let numerator = Wide::product(difference.unsigned_abs(), duration_denominator as u64)
        .multiply(u64::from(meter_denominator));
    let denominator = Wide::product(
        u128::from(rate) * zero_denominator as u128,
        duration_numerator as u64,
    );
    let bar_divisor = denominator.multiply(4 * u64::from(meter_numerator));
    let remainder = numerator.remainder(bar_divisor);
    let start = if difference < 0 && !remainder.is_zero() {
        numerator.add(bar_divisor.subtract(remainder))
    } else {
        numerator.subtract(remainder)
    };
    Some(nearest_even(
        start,
        denominator.multiply(u64::from(meter_denominator)),
        difference < 0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bar_starts_match_independent_fraction_goldens() {
        // Literals from Python Fraction: floor(Q / (4*n/d)) * (4*n/d),
        // followed by its independent correctly rounded binary64 conversion.
        for (s, r, a, b, c, d, n, e, bits) in [
            (0, 48_000, 0, 1, 1, 2, 7, 8, 0),
            (-1, 48_000, 0, 1, 1, 2, 7, 8, 0xc00c_0000_0000_0000),
            (83_999, 48_000, 0, 1, 1, 2, 7, 8, 0),
            (84_000, 48_000, 0, 1, 1, 2, 7, 8, 0x400c_0000_0000_0000),
            (
                0,
                48_000,
                -8_070_450_532_247_928_831,
                1_i64 << 61,
                1,
                1,
                7,
                8,
                0,
            ),
            (0, 48_000, 1, 1_i64 << 62, 1, 1, 7, 8, 0xc00c_0000_0000_0000),
            (
                i64::MAX,
                u32::MAX,
                i64::MIN,
                1,
                1,
                i64::MAX,
                u16::MAX,
                u16::MAX,
                0x47d0_0000_0010_0000,
            ),
            (99_317, 44_100, 713, 7, 3, 4, 5, 7, 0xc060_c924_9249_2492),
        ] {
            assert_eq!(bar_start(s, r, a, b, c, d, n, e).unwrap().to_bits(), bits);
        }
    }

    #[test]
    fn anchored_music_rounds_literal_halfway_neighbors_and_carry_to_even() {
        // Each tuple is an exact positive quarter-note position. The author
        // anchor is its negative at sample zero; expected bits are independent
        // binary64 literals, not values produced by this projection helper.
        for (numerator, denominator, bits) in [
            ((1_i64 << 53) + 1, 1_i64 << 53, 0x3ff0_0000_0000_0000),
            ((1_i64 << 53) + 3, 1_i64 << 53, 0x3ff0_0000_0000_0002),
            ((1_i64 << 54) - 1, 1_i64 << 53, 0x4000_0000_0000_0000),
            ((1_i64 << 54) + 1, 1_i64 << 54, 0x3ff0_0000_0000_0000),
            ((1_i64 << 54) + 3, 1_i64 << 54, 0x3ff0_0000_0000_0001),
        ] {
            for negative in [false, true] {
                let zero = if negative { numerator } else { -numerator };
                let actual = quarter_notes(0, 1, zero, denominator, 1, 1).unwrap();
                assert_eq!(actual.to_bits(), bits | (u64::from(negative) << 63));
            }
        }
    }

    #[test]
    fn anchored_music_cancels_exactly_near_signed_sample_limits() {
        let maximum = i64::MAX;
        for (sample, rate, a, b, c, d, bits) in [
            (i64::MIN, 1, i64::MIN, 1, 1, 1, 0),
            (i64::MIN, 1, i64::MIN + 1, 1, 1, 1, 0xbff0_0000_0000_0000),
            (
                maximum - 32,
                48_000,
                maximum - 33,
                48_000,
                1,
                1,
                0x3ef5_d867_c3ec_e2a5,
            ),
            (maximum, 1, i64::MIN, 1, 1, maximum, 0x47e0_0000_0000_0000),
            (i64::MIN, 1, maximum, 1, 1, maximum, 0xc7e0_0000_0000_0000),
        ] {
            let actual = quarter_notes(sample, rate, a, b, c, d).unwrap();
            assert_eq!(actual.to_bits(), bits, "sample={sample}, rate={rate}");
            assert!(actual.is_finite());
        }
    }

    #[test]
    fn anchored_music_supports_190_bit_numerators_and_158_bit_denominators() {
        let maximum = i64::MAX;
        for (sample, rate, a, b, c, d, bits) in [
            (
                maximum - 32,
                u32::MAX,
                -maximum,
                maximum - 1,
                1,
                maximum,
                0x45d0_0000_0030_0000,
            ),
            (
                i64::MIN,
                u32::MAX,
                maximum - 2,
                maximum,
                maximum - 4,
                maximum - 6,
                0xc1e0_0000_0030_0000,
            ),
            // Exact s*b-a*r == 1, with a 158-bit unreduced denominator.
            (
                4_294_967_293,
                u32::MAX,
                9_223_372_032_559_808_510,
                maximum,
                maximum,
                1,
                0x3610_0000_0010_0000,
            ),
        ] {
            let actual = quarter_notes(sample, rate, a, b, c, d).unwrap();
            assert_eq!(actual.to_bits(), bits);
            assert!(actual.is_normal());
        }
    }

    #[test]
    fn anchored_music_validates_scalars_without_a_partial_numeric_domain() {
        for (a, b, c, d) in [
            (0, 0, 1, 1),
            (0, -1, 1, 1),
            (0, 1, 0, 1),
            (0, 1, -1, 1),
            (0, 1, 1, 0),
            (0, 1, 1, -1),
        ] {
            assert_eq!(quarter_notes(i64::MAX, u32::MAX, a, b, c, d), None);
        }
        assert_eq!(quarter_notes(0, 0, 0, 1, 1, 1), None);
        assert_eq!(quarter_notes(-10, 2, -5, 1, 7, 11).unwrap().to_bits(), 0);
        assert_eq!(
            quarter_notes(17, 44_100, 2, 3, 7, 11),
            quarter_notes(17, 44_100, 4, 6, 14, 22)
        );
        assert_eq!(tempo(3, 4).to_bits(), 0x4054_0000_0000_0000); // 80 BPM
    }

    #[test]
    fn anchored_music_wide_arithmetic_carries_and_borrows_across_limb_boundaries() {
        let value = Wide::product(u128::MAX, u64::MAX);
        assert_eq!(value, Wide([1, u64::MAX, u64::MAX - 1, 0]));
        assert_eq!(
            Wide([0, 0, 1, 0]).subtract(Wide([1, 0, 0, 0])),
            Wide([u64::MAX, u64::MAX, 0, 0])
        );
        assert_eq!(Wide::from_u128(1).shift_left(127), Wide([0, 1 << 63, 0, 0]));
        assert_eq!(Wide::from_u128(1).shift_left(190).bits(), 191);
    }
}
