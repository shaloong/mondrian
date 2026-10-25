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

/// Bar zero begins at quarter note zero. Determine the bar from the exact
/// sample/reference ratio, before CLAP beat-tick rounding can cross a boundary.
pub(super) fn bar_position(
    sample: i64,
    rate: u32,
    reference: MusicalTimeReference,
) -> Result<Option<(i32, BeatTime)>, BeatPositionError> {
    let Some(signature) = reference.time_signature() else {
        return Ok(None);
    };
    if rate == 0 {
        return Err(BeatPositionError::ZeroSampleRate);
    }
    let zero = reference.quarter_note_zero();
    let duration = reference.quarter_note_duration();
    let difference = i128::from(sample) * i128::from(zero.denominator())
        - i128::from(zero.numerator()) * i128::from(rate);
    let numerator = BeatMagnitude::from_u128(difference.unsigned_abs())
        .checked_mul(duration.denominator() as u64)
        .and_then(|value| value.checked_mul(u64::from(signature.denominator())))
        .ok_or(BeatPositionError::OutOfRange)?;
    let denominator = BeatMagnitude::from_u128(u128::from(rate))
        .checked_mul(zero.denominator() as u64)
        .and_then(|value| value.checked_mul(duration.numerator() as u64))
        .and_then(|value| value.checked_mul(4 * u64::from(signature.numerator())))
        .ok_or(BeatPositionError::OutOfRange)?;
    let number = numerator
        .signed_ratio(denominator, difference < 0, true)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or(BeatPositionError::OutOfRange)?;
    // The i32 bar and u16 signature need at most 80 bits after tick scaling.
    let start =
        i128::from(number) * 4 * i128::from(signature.numerator()) * i128::from(BeatTime::FACTOR);
    let ticks = BeatMagnitude::from_u128(start.unsigned_abs())
        .rounded_signed_ratio(
            BeatMagnitude::from_u128(u128::from(signature.denominator())),
            start < 0,
        )
        .ok_or(BeatPositionError::OutOfRange)?;
    Ok(Some((number, BeatTime::from_bits(ticks))))
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
        // A division remainder is below the at-most-176-bit denominator;
        // doubling it cannot reach the end of this 256-bit scratch.
        debug_assert_eq!(carry, 0);
    }

    fn rounded_signed_ratio(self, denominator: Self, negative: bool) -> Option<i64> {
        self.signed_ratio(denominator, negative, false)
    }

    fn signed_ratio(self, denominator: Self, negative: bool, floor: bool) -> Option<i64> {
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
        let increment = if floor {
            negative && remainder.compare(Self::ZERO) != Ordering::Equal
        } else {
            let mut distance_to_next = denominator;
            distance_to_next.subtract(remainder);
            remainder.compare(distance_to_next) != Ordering::Less
        };
        if increment {
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

    #[test]
    fn bar_floor_precedes_fixed_point_rounding_and_checks_clap_specific_limits() {
        let meter = mondrian_core::MusicalTimeSignature::new(7, 8).unwrap();
        let music = reference((0, 1), (1, 2)).with_time_signature(Some(meter));
        for (sample, number, bits) in [
            (0, 0, 0),
            (-1, -1, -(7_i64 << 30)),
            (83_999, 0, 0),
            (84_000, 1, 7_i64 << 30),
            (-84_000, -1, -(7_i64 << 30)),
            (-84_001, -2, -(7_i64 << 31)),
        ] {
            let (bar, start) = bar_position(sample, 48_000, music).unwrap().unwrap();
            assert_eq!((bar, start.to_bits()), (number, bits));
        }
        // Both exact positions round onto the following CLAP beat boundary.
        // Bar selection must still use the original rational position.
        let near = reference((-8_070_450_532_247_928_831, 1_i64 << 61), (1, 1))
            .with_time_signature(Some(meter));
        assert_eq!(position(0, 48_000, near).unwrap().to_bits(), 7_i64 << 30);
        assert_eq!(bar_position(0, 48_000, near).unwrap().unwrap().0, 0);
        let tiny_negative = reference((1, 1_i64 << 62), (1, 1)).with_time_signature(Some(meter));
        assert_eq!(position(0, 48_000, tiny_negative).unwrap().to_bits(), 0);
        let (bar, start) = bar_position(0, 48_000, tiny_negative).unwrap().unwrap();
        assert_eq!((bar, start.to_bits()), (-1, -(7_i64 << 30)));
        let odd = reference((713, 7), (3, 4)).with_time_signature(Some(
            mondrian_core::MusicalTimeSignature::new(5, 7).unwrap(),
        ));
        let (bar, start) = bar_position(99_317, 44_100, odd).unwrap().unwrap();
        assert_eq!((bar, start.to_bits()), (-47, -288_376_375_589));
        let unit_bar = reference((0, 1), (1, 1)).with_time_signature(Some(
            mondrian_core::MusicalTimeSignature::new(1, 4).unwrap(),
        ));
        for number in [i32::MIN, i32::MAX] {
            let (bar, start) = bar_position(i64::from(number), 1, unit_bar).unwrap().unwrap();
            assert_eq!((bar, start.to_bits()), (number, i64::from(number) << 31));
        }
        for sample in [i64::from(i32::MIN) - 1, i64::from(i32::MAX) + 1] {
            assert!(position(sample, 1, unit_bar).is_ok());
            assert_eq!(bar_position(sample, 1, unit_bar), Err(BeatPositionError::OutOfRange));
        }
        let narrow = reference((0, 1), (1, 1)).with_time_signature(Some(
            mondrian_core::MusicalTimeSignature::new(1, u16::MAX).unwrap(),
        ));
        assert!(position(1_000_000, 1, narrow).is_ok());
        assert_eq!(
            bar_position(1_000_000, 1, narrow),
            Err(BeatPositionError::OutOfRange)
        );
        assert_eq!(
            bar_position(0, 0, music),
            Err(BeatPositionError::ZeroSampleRate)
        );
        assert_eq!(
            bar_position(0, 48_000, reference((0, 1), (1, 2))).unwrap(),
            None
        );
    }

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
