//! Exact CLAP beat ticks at the existing signed sample/reference boundary.

use clack_host::utils::BeatTime;
use mondrian_core::MusicalTimeReference;
use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BeatPositionError {
    ZeroSampleRate,
    OutOfRange,
}

/// Project the one canonical sample coordinate to 2^31 ticks per quarter note.
///
/// For zero a/b and duration c/d, the exact ratio is
/// `(sample * b - a * rate) * d * 2^31 / (rate * b * c)`.
/// Round once to nearest, with half ties away from zero, before checking i64.
pub(super) fn position(
    sample: i64,
    rate: u32,
    reference: MusicalTimeReference,
) -> Result<BeatTime, BeatPositionError> {
    if rate == 0 {
        return Err(BeatPositionError::ZeroSampleRate);
    }
    let zero = reference.quarter_note_zero();
    let duration = reference.quarter_note_duration();
    // Core TimelineTime has numerator in i64::MIN..=i64::MAX and denominator
    // in 1..=i64::MAX; MusicalTimeReference also makes duration.numerator > 0.
    // Even i64::MIN has magnitude only 2^63: |sample * b| < 2^126 and
    // |a * rate| < 2^95, so their signed difference fits i128 without loss.
    let difference = i128::from(sample) * i128::from(zero.denominator())
        - i128::from(zero.numerator()) * i128::from(rate);
    // The numerator needs at most 221 bits; the denominator at most 158.
    // Four fixed limbs cover every valid Core reference, not just small times.
    let numerator = BeatMagnitude::from_u128(difference.unsigned_abs())
        .checked_mul(duration.denominator() as u64)
        .and_then(|value| value.checked_mul(BeatTime::FACTOR as u64))
        .ok_or(BeatPositionError::OutOfRange)?;
    let denominator = BeatMagnitude::from_u128(u128::from(rate))
        .checked_mul(zero.denominator() as u64)
        .and_then(|value| value.checked_mul(duration.numerator() as u64))
        .ok_or(BeatPositionError::OutOfRange)?;
    numerator
        .rounded_signed_ratio(denominator, difference < 0)
        .map(BeatTime::from_bits)
        .ok_or(BeatPositionError::OutOfRange)
}

/// Fixed scratch for this one ratio. No arbitrary-width storage or allocator.
#[derive(Clone, Copy)]
struct BeatMagnitude([u64; 4]);

impl BeatMagnitude {
    const ZERO: Self = Self([0; 4]);

    fn from_u128(value: u128) -> Self {
        Self([value as u64, (value >> 64) as u64, 0, 0])
    }

    fn as_u128(self) -> Option<u128> {
        (self.0[2] == 0 && self.0[3] == 0)
            .then_some(u128::from(self.0[0]) | (u128::from(self.0[1]) << 64))
    }

    fn checked_mul(self, factor: u64) -> Option<Self> {
        let mut result = Self::ZERO;
        let mut carry = 0_u128;
        for (target, limb) in result.0.iter_mut().zip(self.0) {
            let product = u128::from(limb) * u128::from(factor) + carry;
            *target = product as u64;
            carry = product >> 64;
        }
        (carry == 0).then_some(result)
    }

    fn compare(self, other: Self) -> Ordering {
        for (left, right) in self.0.iter().zip(other.0.iter()).rev() {
            match left.cmp(right) {
                Ordering::Equal => {}
                order => return order,
            }
        }
        Ordering::Equal
    }

    fn subtract(&mut self, other: Self) {
        let mut borrow = false;
        for (target, other) in self.0.iter_mut().zip(other.0) {
            let (difference, first_borrow) = target.overflowing_sub(other);
            let (difference, second_borrow) = difference.overflowing_sub(u64::from(borrow));
            *target = difference;
            borrow = first_borrow || second_borrow;
        }
        debug_assert!(!borrow);
    }

    fn push_bit(&mut self, bit: u64) {
        let mut carry = bit;
        for limb in &mut self.0 {
            let next_carry = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = next_carry;
        }
        // A division remainder is below the at-most-158-bit denominator;
        // doubling it cannot reach the end of this 256-bit scratch.
        debug_assert_eq!(carry, 0);
    }

    fn rounded_signed_ratio(self, denominator: Self, negative: bool) -> Option<i64> {
        let limit = if negative {
            1_u64 << 63
        } else {
            i64::MAX as u64
        };
        let (mut quotient, remainder) =
            if let (Some(dividend), Some(divisor)) = (self.as_u128(), denominator.as_u128()) {
                // Common small coordinates need no multi-limb division. The
                // remainder enters the same rounding and signed-range check below.
                let quotient = dividend / divisor;
                if quotient > u128::from(limit) {
                    return None;
                }
                (quotient as u64, Self::from_u128(dividend % divisor))
            } else {
                let mut remainder = Self::ZERO;
                let mut quotient = 0_u64;
                // Bounded binary division. Stop as soon as the monotonically growing
                // quotient cannot fit the final signed magnitude; never saturate it.
                for bit in (0..256).rev() {
                    remainder.push_bit((self.0[bit / 64] >> (bit % 64)) & 1);
                    let quotient_bit = if remainder.compare(denominator) != Ordering::Less {
                        remainder.subtract(denominator);
                        1
                    } else {
                        0
                    };
                    quotient = quotient.checked_mul(2)?.checked_add(quotient_bit)?;
                    if quotient > limit {
                        return None;
                    }
                }
                (quotient, remainder)
            };
        let mut distance_to_next = denominator;
        distance_to_next.subtract(remainder);
        if remainder.compare(distance_to_next) != Ordering::Less {
            quotient = quotient.checked_add(1)?;
            if quotient > limit {
                return None;
            }
        }
        if negative {
            if quotient == 1_u64 << 63 {
                Some(i64::MIN)
            } else {
                Some(-(quotient as i64))
            }
        } else {
            Some(quotient as i64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::TimelineTime;

    fn reference(zero: (i64, i64), duration: (i64, i64)) -> MusicalTimeReference {
        MusicalTimeReference::new(
            TimelineTime::new(zero.0, zero.1).unwrap(),
            TimelineTime::new(duration.0, duration.1).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn beat_position_matches_signed_grid_golden_values() {
        for (sample, rate, zero, expected) in [
            (-120_007, 44_100, -91, 29_431_271_512),
            (-120_007, 44_100, 713, -299_440_510_010),
            (-1, 44_100, -91, 37_222_984_971),
            (99_317, 44_100, 713, -285_200_307_331),
            (-120_007, 48_000, -91, 30_064_353_506),
            (-1, 48_000, 713, -291_648_791_276),
            (0, 48_000, -91, 37_223_049_899),
            (99_317, 48_000, 713, -285_724_241_805),
        ] {
            assert_eq!(
                position(sample, rate, reference((zero, 7), (3, 4))).unwrap().to_bits(),
                expected
            );
        }
    }

    #[test]
    fn beat_position_small_ratios_match_independent_i128_projection() {
        for rate in [1, 44_100, 48_000] {
            for sample in [-120_007, -1, 0, 1, 99_317] {
                for zero in [-91, 0, 713] {
                    for (duration_numerator, duration_denominator) in [(1, 3), (3, 4), (17, 9)] {
                        let music =
                            reference((zero, 7), (duration_numerator, duration_denominator));
                        let numerator = (i128::from(sample) * 7
                            - i128::from(zero) * i128::from(rate))
                            * i128::from(duration_denominator)
                            * (1_i128 << 31);
                        let denominator = i128::from(rate) * 7 * i128::from(duration_numerator);
                        let magnitude = numerator.abs();
                        let rounded = (magnitude + denominator / 2) / denominator;
                        let expected = i64::try_from(rounded * numerator.signum()).unwrap();
                        assert_eq!(position(sample, rate, music).unwrap().to_bits(), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn beat_position_rounds_half_ticks_away_from_zero() {
        for sign in [-1, 1] {
            for (denominator, magnitude) in [
                ((1_i64 << 32) - 1, 1),
                (1_i64 << 32, 1),
                ((1_i64 << 32) + 1, 0),
            ] {
                assert_eq!(
                    position(0, 48_000, reference((-sign, denominator), (1, 1))).unwrap().to_bits(),
                    sign * magnitude
                );
            }
        }
    }

    #[test]
    fn beat_position_checks_signed_limits_after_rounding() {
        let duration = (1_i64 << 31, 1);
        for sample in [i64::MIN, i64::MAX] {
            assert_eq!(
                position(sample, 1, reference((0, 1), duration)).unwrap().to_bits(),
                sample
            );
        }
        for (sample, zero, expected) in [
            (i64::MAX, (1, 2), i64::MAX),
            (i64::MAX, (-1, 3), i64::MAX),
            (i64::MIN, (-1, 2), i64::MIN),
            (i64::MIN, (1, 3), i64::MIN),
        ] {
            assert_eq!(
                position(sample, 1, reference(zero, duration)).unwrap().to_bits(),
                expected
            );
        }
        for (sample, zero) in [
            (i64::MAX, (-1, 2)),
            (i64::MAX, (-1, 1)),
            (i64::MIN, (1, 2)),
            (i64::MIN, (1, 1)),
        ] {
            assert_eq!(
                position(sample, 1, reference(zero, duration)),
                Err(BeatPositionError::OutOfRange)
            );
        }
    }

    #[test]
    fn beat_position_full_width_products_retain_low_bits() {
        let music = reference((i64::MIN, i64::MAX), (i64::MAX, i64::MAX - 1));
        for (sample, expected) in [
            (i64::MAX, 4_611_686_021_648_613_375),
            (i64::MIN, -4_611_686_017_353_646_080),
        ] {
            assert_eq!(
                position(sample, u32::MAX, music).unwrap().to_bits(),
                expected
            );
        }
        assert_eq!(
            position(0, 48_000, reference((i64::MIN, 1), (1, 3))),
            Err(BeatPositionError::OutOfRange)
        );
    }

    #[test]
    fn beat_position_preserves_near_cancellation_at_large_sample_origins() {
        for rate in [44_100, 48_000] {
            for sample in [-(1_i64 << 47), 1_i64 << 47] {
                for offset in [-1, 1] {
                    let music = reference((sample - offset, i64::from(rate)), (1, i64::from(rate)));
                    assert_eq!(
                        position(sample, rate, music).unwrap().to_bits(),
                        offset * (1_i64 << 31)
                    );
                }
            }
        }
    }

    #[test]
    fn beat_position_rejects_zero_rate() {
        assert_eq!(
            position(0, 0, reference((0, 1), (1, 1))),
            Err(BeatPositionError::ZeroSampleRate)
        );
    }
}
