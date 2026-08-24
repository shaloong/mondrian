use serde::{Deserialize, Serialize};

/// Largest absolute Timeline-rate multiplier admitted by editorial shuttle.
pub const MAX_EDITORIAL_SHUTTLE_MULTIPLIER: u16 = 32;

/// Exact signed Timeline phase rate owned by one Playback Session.
///
/// Positive values move forward, negative values move in reverse, and pause is
/// represented by [`crate::TransportState`] rather than an invalid zero rate.
/// The value is always reduced and bounded so phase arithmetic remains exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlaybackRate {
    numerator: i16,
    denominator: u16,
}

impl PlaybackRate {
    /// Normal forward playback.
    pub const FORWARD_1X: Self = Self { numerator: 1, denominator: 1 };
    /// Normal reverse playback.
    pub const REVERSE_1X: Self = Self { numerator: -1, denominator: 1 };

    /// Validate and reduce an exact signed playback rate.
    pub fn new(numerator: i16, denominator: u16) -> Result<Self, PlaybackRateError> {
        if numerator == 0 {
            return Err(PlaybackRateError::ZeroNumerator);
        }
        if denominator == 0 {
            return Err(PlaybackRateError::ZeroDenominator);
        }
        let magnitude = numerator.unsigned_abs();
        if magnitude > denominator.saturating_mul(MAX_EDITORIAL_SHUTTLE_MULTIPLIER) {
            return Err(PlaybackRateError::MagnitudeTooLarge {
                numerator,
                denominator,
                maximum_multiplier: MAX_EDITORIAL_SHUTTLE_MULTIPLIER,
            });
        }
        let divisor = gcd_u16(magnitude, denominator);
        let reduced_magnitude = magnitude / divisor;
        let reduced_numerator = i16::try_from(reduced_magnitude)
            .map_err(|_| PlaybackRateError::MagnitudeUnrepresentable)?;
        Ok(Self {
            numerator: if numerator.is_negative() {
                -reduced_numerator
            } else {
                reduced_numerator
            },
            denominator: denominator / divisor,
        })
    }

    /// Signed reduced numerator.
    pub const fn numerator(self) -> i16 {
        self.numerator
    }

    /// Positive reduced denominator.
    pub const fn denominator(self) -> u16 {
        self.denominator
    }

    /// Whether Timeline phase moves forward.
    pub const fn is_forward(self) -> bool {
        self.numerator > 0
    }

    /// Whether Timeline phase moves in reverse.
    pub const fn is_reverse(self) -> bool {
        self.numerator < 0
    }

    /// Only forward 1x may hand clock authority to ordinary realtime audio.
    pub const fn supports_realtime_audio(self) -> bool {
        self.numerator == 1 && self.denominator == 1
    }

    /// Absolute reduced numerator used by checked phase arithmetic.
    pub(crate) const fn magnitude_numerator(self) -> u16 {
        self.numerator.unsigned_abs()
    }

    pub(crate) fn next_shuttle_rate(self, direction: PlaybackShuttleDirection) -> Self {
        let same_direction = matches!(
            (self.is_forward(), direction),
            (true, PlaybackShuttleDirection::Forward) | (false, PlaybackShuttleDirection::Reverse)
        );
        let current_multiplier = if same_direction && self.denominator == 1 {
            self.magnitude_numerator()
        } else {
            0
        };
        let multiplier = if current_multiplier == 0 {
            1
        } else {
            current_multiplier.saturating_mul(2).min(MAX_EDITORIAL_SHUTTLE_MULTIPLIER)
        };
        // The multiplier is capped at 32 above, so this conversion is exact.
        let numerator = multiplier as i16;
        Self {
            numerator: match direction {
                PlaybackShuttleDirection::Forward => numerator,
                PlaybackShuttleDirection::Reverse => -numerator,
            },
            denominator: 1,
        }
    }
}

impl Default for PlaybackRate {
    fn default() -> Self {
        Self::FORWARD_1X
    }
}

/// Directional J/L intent entering the Playback Session Interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaybackShuttleDirection {
    /// J: reverse, doubling a running reverse rate up to the bounded maximum.
    Reverse,
    /// L: forward, doubling a running forward rate up to the bounded maximum.
    Forward,
}

/// Invalid exact Playback rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PlaybackRateError {
    /// Pause is a Transport State and cannot be encoded as a zero phase rate.
    #[error("playback rate numerator must be non-zero")]
    ZeroNumerator,
    /// Exact playback rates require a positive denominator.
    #[error("playback rate denominator must be non-zero")]
    ZeroDenominator,
    /// Rate exceeds the bounded editorial shuttle envelope.
    #[error(
        "playback rate {numerator}/{denominator} exceeds {maximum_multiplier}x editorial limit"
    )]
    MagnitudeTooLarge {
        /// Requested numerator.
        numerator: i16,
        /// Requested denominator.
        denominator: u16,
        /// Maximum absolute multiplier.
        maximum_multiplier: u16,
    },
    /// Reduced magnitude cannot be represented by the public value type.
    #[error("playback rate magnitude cannot be represented")]
    MagnitudeUnrepresentable,
}

const fn gcd_u16(mut left: u16, mut right: u16) -> u16 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rate_reduces_and_rejects_zero_or_unbounded_values() {
        assert_eq!(PlaybackRate::new(-2, 4), PlaybackRate::new(-1, 2));
        assert_eq!(
            PlaybackRate::new(0, 1),
            Err(PlaybackRateError::ZeroNumerator)
        );
        assert_eq!(
            PlaybackRate::new(1, 0),
            Err(PlaybackRateError::ZeroDenominator)
        );
        assert!(matches!(
            PlaybackRate::new(33, 1),
            Err(PlaybackRateError::MagnitudeTooLarge { .. })
        ));
    }

    #[test]
    fn repeated_shuttle_steps_are_directional_and_bounded() {
        let mut rate = PlaybackRate::REVERSE_1X;
        for expected in [-2, -4, -8, -16, -32, -32] {
            rate = rate.next_shuttle_rate(PlaybackShuttleDirection::Reverse);
            assert_eq!(rate.numerator(), expected);
        }
        assert_eq!(
            rate.next_shuttle_rate(PlaybackShuttleDirection::Forward),
            PlaybackRate::FORWARD_1X
        );
    }
}
