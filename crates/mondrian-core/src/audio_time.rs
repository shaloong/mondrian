//! Exact conversion between timeline time and integer audio sample positions.
//!
//! Audio renderers must choose sample boundaries once and carry the resulting
//! integers through decode, mix, playback, and export. Converting through
//! floating-point seconds at every chunk boundary can duplicate or omit samples
//! for fractional video rates and long timelines.

use crate::TimelineTime;

/// Policy used when a timeline position falls between two audio samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSampleRounding {
    /// Greatest sample position not after the timeline position.
    Floor,
    /// Smallest sample position not before the timeline position.
    Ceil,
    /// Nearest sample, with exact half-sample ties rounded away from zero.
    Nearest,
}

/// Validated audio sample rate carried with every persisted or runtime position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioSampleRate(u32);

impl AudioSampleRate {
    /// Construct a positive sample rate.
    pub fn new(hz: u32) -> Result<Self, AudioTimeError> {
        if hz == 0 {
            return Err(AudioTimeError::ZeroSampleRate);
        }
        Ok(Self(hz))
    }

    /// Return the rate in samples per second.
    pub const fn hz(self) -> u32 {
        self.0
    }
}

/// One signed position on a specific integer audio-sample timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioSamplePosition {
    sample: i64,
    rate: AudioSampleRate,
}

impl AudioSamplePosition {
    /// Construct a position from an already resolved sample index.
    pub const fn new(sample: i64, rate: AudioSampleRate) -> Self {
        Self { sample, rate }
    }

    /// Resolve exact author time onto this sample grid.
    pub fn from_timeline_time(
        time: TimelineTime,
        rate: AudioSampleRate,
        rounding: AudioSampleRounding,
    ) -> Result<Self, AudioTimeError> {
        let numerator = i128::from(time.numerator())
            .checked_mul(i128::from(rate.hz()))
            .ok_or(AudioTimeError::PositionOverflow)?;
        let denominator = i128::from(time.denominator());
        let resolved = round_ratio(numerator, denominator, rounding)?;
        let sample = i64::try_from(resolved).map_err(|_| AudioTimeError::PositionOverflow)?;
        Ok(Self { sample, rate })
    }

    /// Signed zero-based sample index.
    pub const fn sample(self) -> i64 {
        self.sample
    }

    /// Sample rate defining this position.
    pub const fn rate(self) -> AudioSampleRate {
        self.rate
    }

    /// Exact signed distance in samples; different rates are rejected.
    pub fn samples_since(self, earlier: Self) -> Result<i64, AudioTimeError> {
        if self.rate != earlier.rate {
            return Err(AudioTimeError::RateMismatch {
                left: self.rate.hz(),
                right: earlier.rate.hz(),
            });
        }
        self.sample.checked_sub(earlier.sample).ok_or(AudioTimeError::PositionOverflow)
    }
}

/// Failures resolving or comparing integer audio sample positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AudioTimeError {
    /// A sample timeline cannot have a zero rate.
    #[error("audio sample rate must be positive")]
    ZeroSampleRate,
    /// The exact conversion did not fit the supported signed sample range.
    #[error("audio sample position is outside the supported i64 range")]
    PositionOverflow,
    /// Positions from different sample timelines cannot be subtracted implicitly.
    #[error("audio sample-rate mismatch: {left} Hz versus {right} Hz")]
    RateMismatch { left: u32, right: u32 },
}

fn round_ratio(
    numerator: i128,
    denominator: i128,
    rounding: AudioSampleRounding,
) -> Result<i128, AudioTimeError> {
    Ok(match rounding {
        AudioSampleRounding::Floor => numerator.div_euclid(denominator),
        AudioSampleRounding::Ceil => {
            let floor = numerator.div_euclid(denominator);
            if numerator.rem_euclid(denominator) == 0 {
                floor
            } else {
                floor + 1
            }
        }
        AudioSampleRounding::Nearest => {
            let magnitude = numerator.unsigned_abs();
            let divisor = denominator as u128;
            let quotient = magnitude / divisor;
            let remainder = magnitude % divisor;
            let rounded = quotient + u128::from(remainder.saturating_mul(2) >= divisor);
            let rounded = i128::try_from(rounded).map_err(|_| AudioTimeError::PositionOverflow)?;
            if numerator < 0 {
                rounded.checked_neg().ok_or(AudioTimeError::PositionOverflow)?
            } else {
                rounded
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_video_rate_resolves_to_one_stable_sample_boundary() {
        let rate = AudioSampleRate::new(48_000).expect("rate");
        let time = TimelineTime::new(1001, 30_000).expect("time");

        assert_eq!(
            AudioSamplePosition::from_timeline_time(time, rate, AudioSampleRounding::Floor)
                .expect("floor")
                .sample(),
            1_601
        );
        assert_eq!(
            AudioSamplePosition::from_timeline_time(time, rate, AudioSampleRounding::Nearest)
                .expect("nearest")
                .sample(),
            1_602
        );
        assert_eq!(
            AudioSamplePosition::from_timeline_time(time, rate, AudioSampleRounding::Ceil)
                .expect("ceil")
                .sample(),
            1_602
        );
    }

    #[test]
    fn long_fractional_timeline_conversion_does_not_accumulate_chunk_error() {
        let rate = AudioSampleRate::new(48_000).expect("rate");
        let start = AudioSamplePosition::from_timeline_time(
            TimelineTime::ZERO,
            rate,
            AudioSampleRounding::Nearest,
        )
        .expect("start");
        let middle = AudioSamplePosition::from_timeline_time(
            TimelineTime::new(100_100_000, 30_000).expect("middle time"),
            rate,
            AudioSampleRounding::Nearest,
        )
        .expect("middle");
        let end = AudioSamplePosition::from_timeline_time(
            TimelineTime::new(200_200_000, 30_000).expect("end time"),
            rate,
            AudioSampleRounding::Nearest,
        )
        .expect("end");

        assert_eq!(
            end.samples_since(start).expect("whole"),
            middle.samples_since(start).expect("first")
                + end.samples_since(middle).expect("second")
        );
    }

    #[test]
    fn negative_half_sample_ties_round_away_from_zero() {
        let rate = AudioSampleRate::new(1).expect("rate");
        let time = TimelineTime::new(-1, 2).expect("time");

        assert_eq!(
            AudioSamplePosition::from_timeline_time(time, rate, AudioSampleRounding::Nearest)
                .expect("nearest")
                .sample(),
            -1
        );
    }

    #[test]
    fn comparison_rejects_implicit_sample_rate_conversion() {
        let left = AudioSamplePosition::new(48_000, AudioSampleRate::new(48_000).expect("left"));
        let right = AudioSamplePosition::new(44_100, AudioSampleRate::new(44_100).expect("right"));

        assert_eq!(
            left.samples_since(right),
            Err(AudioTimeError::RateMismatch { left: 48_000, right: 44_100 })
        );
    }
}
