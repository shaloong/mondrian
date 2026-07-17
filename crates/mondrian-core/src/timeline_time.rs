//! Exact authoring time, domains, and checked mappings.

use crate::{
    AssetId, AudioComponentEditId, AudioProcessingScopeId, AudioTransitionId, FramePosition,
    Rational, SequenceId,
};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

/// A canonical exact rational offset in an owner-declared authoring time domain.
///
/// The represented duration is `numerator / denominator` seconds. Values are
/// always reduced, zero is always `0/1`, and the denominator is always positive.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TimelineTime {
    numerator: i64,
    denominator: i64,
}

impl TimelineTime {
    /// Exact zero.
    pub const ZERO: Self = Self { numerator: 0, denominator: 1 };
    /// Exact one.
    pub const ONE: Self = Self { numerator: 1, denominator: 1 };
    /// Exact negative one.
    pub const NEGATIVE_ONE: Self = Self { numerator: -1, denominator: 1 };
    /// Exact one third.
    pub const ONE_THIRD: Self = Self { numerator: 1, denominator: 3 };
    /// Exact negative one third.
    pub const NEGATIVE_ONE_THIRD: Self = Self { numerator: -1, denominator: 3 };
    /// Exact five percent.
    pub const FIVE_PERCENT: Self = Self { numerator: 1, denominator: 20 };
    /// Exact ninety-five percent.
    pub const NINETY_FIVE_PERCENT: Self = Self { numerator: 19, denominator: 20 };

    /// Construct and canonicalize an exact time value.
    pub fn new(numerator: i64, denominator: i64) -> Result<Self, TimelineTimeError> {
        normalize_i128(i128::from(numerator), i128::from(denominator))
    }

    /// Convert an integer frame-grid position without floating-point seconds.
    pub fn from_frame_position(value: FramePosition) -> Result<Self, TimelineTimeError> {
        if value.time_base.num <= 0 || value.time_base.den <= 0 {
            return Err(TimelineTimeError::InvalidLegacyTimeBase {
                numerator: value.time_base.num,
                denominator: value.time_base.den,
            });
        }
        let numerator = i128::from(value.frame)
            .checked_mul(i128::from(value.time_base.num))
            .ok_or(TimelineTimeError::Overflow)?;
        normalize_i128(numerator, i128::from(value.time_base.den))
    }

    /// Resolve author time once onto an integer frame evaluation grid.
    pub fn to_frame_position(
        self,
        frame_rate: Rational,
        rounding: FrameRounding,
    ) -> Result<FramePosition, TimelineTimeError> {
        if frame_rate.num <= 0 || frame_rate.den <= 0 {
            return Err(TimelineTimeError::InvalidFrameRate {
                numerator: frame_rate.num,
                denominator: frame_rate.den,
            });
        }
        let numerator = i128::from(self.numerator)
            .checked_mul(i128::from(frame_rate.num))
            .ok_or(TimelineTimeError::Overflow)?;
        let denominator = i128::from(self.denominator)
            .checked_mul(i128::from(frame_rate.den))
            .ok_or(TimelineTimeError::Overflow)?;
        let frame = round_frame_ratio(numerator, denominator, rounding)?;
        Ok(FramePosition::new(
            i64::try_from(frame).map_err(|_| TimelineTimeError::Overflow)?,
            Rational::new(frame_rate.den, frame_rate.num),
        ))
    }

    /// Quantize a finite UI/runtime floating value onto an explicit exact grid.
    ///
    /// This is an input-boundary operation, not a persisted arithmetic path.
    pub fn from_f64_quantized(value: f64, timescale: u32) -> Result<Self, TimelineTimeError> {
        if !value.is_finite() {
            return Err(TimelineTimeError::NonFiniteInput);
        }
        if timescale == 0 {
            return Err(TimelineTimeError::ZeroDenominator);
        }
        let scaled = value * f64::from(timescale);
        if scaled < i64::MIN as f64 || scaled > i64::MAX as f64 {
            return Err(TimelineTimeError::Overflow);
        }
        Self::new(scaled.round() as i64, i64::from(timescale))
    }

    /// Signed canonical numerator.
    pub const fn numerator(self) -> i64 {
        self.numerator
    }

    /// Positive canonical denominator.
    pub const fn denominator(self) -> i64 {
        self.denominator
    }

    /// Whether this value is zero.
    pub const fn is_zero(self) -> bool {
        self.numerator == 0
    }

    /// Whether this value is negative.
    pub const fn is_negative(self) -> bool {
        self.numerator < 0
    }

    /// Checked exact addition.
    pub fn checked_add(self, other: Self) -> Result<Self, TimelineTimeError> {
        let left = i128::from(self.numerator)
            .checked_mul(i128::from(other.denominator))
            .ok_or(TimelineTimeError::Overflow)?;
        let right = i128::from(other.numerator)
            .checked_mul(i128::from(self.denominator))
            .ok_or(TimelineTimeError::Overflow)?;
        let denominator = i128::from(self.denominator)
            .checked_mul(i128::from(other.denominator))
            .ok_or(TimelineTimeError::Overflow)?;
        normalize_i128(
            left.checked_add(right).ok_or(TimelineTimeError::Overflow)?,
            denominator,
        )
    }

    /// Checked exact subtraction.
    pub fn checked_sub(self, other: Self) -> Result<Self, TimelineTimeError> {
        let negated = other.numerator.checked_neg().ok_or(TimelineTimeError::Overflow)?;
        self.checked_add(Self { numerator: negated, denominator: other.denominator })
    }

    /// Scale this time by an exact dimensionless ratio.
    pub fn checked_scale(self, scale: TimeScale) -> Result<Self, TimelineTimeError> {
        let numerator = i128::from(self.numerator)
            .checked_mul(i128::from(scale.numerator()))
            .ok_or(TimelineTimeError::Overflow)?;
        let denominator = i128::from(self.denominator)
            .checked_mul(i128::from(scale.denominator()))
            .ok_or(TimelineTimeError::Overflow)?;
        normalize_i128(numerator, denominator)
    }

    /// Floating-point projection for interpolation math and diagnostics only.
    pub fn to_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }
}

/// Policy for resolving exact author time onto an integer frame grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameRounding {
    /// Greatest frame position not after the author time.
    Floor,
    /// Smallest frame position not before the author time.
    Ceil,
    /// Nearest frame, with exact half-frame ties rounded away from zero.
    Nearest,
}

impl<'de> Deserialize<'de> for TimelineTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Repr {
            numerator: i64,
            denominator: i64,
        }

        let value = Repr::deserialize(deserializer)?;
        TimelineTime::new(value.numerator, value.denominator).map_err(D::Error::custom)
    }
}

impl PartialEq for TimelineTime {
    fn eq(&self, other: &Self) -> bool {
        self.numerator == other.numerator && self.denominator == other.denominator
    }
}

impl Eq for TimelineTime {}

impl PartialOrd for TimelineTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimelineTime {
    fn cmp(&self, other: &Self) -> Ordering {
        let left = i128::from(self.numerator) * i128::from(other.denominator);
        let right = i128::from(other.numerator) * i128::from(self.denominator);
        left.cmp(&right)
    }
}

impl Hash for TimelineTime {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.numerator.hash(state);
        self.denominator.hash(state);
    }
}

impl fmt::Display for TimelineTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}s", self.numerator, self.denominator)
    }
}

/// Exact dimensionless scale used by a time-domain transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "TimelineTime", into = "TimelineTime")]
pub struct TimeScale(TimelineTime);

impl TimeScale {
    /// Identity scale.
    pub const ONE: Self = Self(TimelineTime { numerator: 1, denominator: 1 });
    /// Sign-reversing identity scale.
    pub const NEGATIVE_ONE: Self = Self(TimelineTime { numerator: -1, denominator: 1 });

    /// Construct a canonical scale. Zero is allowed for a non-invertible hold.
    pub fn new(numerator: i64, denominator: i64) -> Result<Self, TimelineTimeError> {
        TimelineTime::new(numerator, denominator).map(Self)
    }

    /// Signed numerator.
    pub const fn numerator(self) -> i64 {
        self.0.numerator
    }

    /// Positive denominator.
    pub const fn denominator(self) -> i64 {
        self.0.denominator
    }

    /// Exact reciprocal, rejected for a zero scale.
    pub fn reciprocal(self) -> Result<Self, TimelineTimeError> {
        if self.0.is_zero() {
            return Err(TimelineTimeError::NonInvertibleTransform);
        }
        Self::new(self.0.denominator, self.0.numerator)
    }
}

impl TryFrom<TimelineTime> for TimeScale {
    type Error = TimelineTimeError;

    fn try_from(value: TimelineTime) -> Result<Self, Self::Error> {
        Ok(Self(value))
    }
}

impl From<TimeScale> for TimelineTime {
    fn from(value: TimeScale) -> Self {
        value.0
    }
}

/// Stable identity of an authoring coordinate domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AuthoringTimeDomain {
    /// Sequence-local author time.
    Sequence(SequenceId),
    /// Placement-local audio component author time.
    AudioComponentEdit(AudioComponentEditId),
    /// Non-placement processing-scope-local author time.
    AudioProcessingScope(AudioProcessingScopeId),
    /// Audio-transition-local author time.
    AudioTransition(AudioTransitionId),
    /// Source-media-local author time.
    Source(AssetId),
}

/// An exact time paired with the domain that gives it meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DomainTime {
    /// Coordinate domain.
    pub domain: AuthoringTimeDomain,
    /// Exact offset from that domain's origin.
    pub time: TimelineTime,
}

impl DomainTime {
    /// Construct a domain-qualified time.
    pub const fn new(domain: AuthoringTimeDomain, time: TimelineTime) -> Self {
        Self { domain, time }
    }

    /// Exact distance, rejected when the domains differ.
    pub fn checked_duration_since(self, earlier: Self) -> Result<TimelineTime, TimelineTimeError> {
        if self.domain != earlier.domain {
            return Err(TimelineTimeError::DomainMismatch);
        }
        self.time.checked_sub(earlier.time)
    }
}

/// One exact affine mapping between two authoring time domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimeTransform {
    /// Input coordinate domain.
    pub source_domain: AuthoringTimeDomain,
    /// Output coordinate domain.
    pub target_domain: AuthoringTimeDomain,
    /// Input anchor.
    pub source_anchor: TimelineTime,
    /// Output anchor corresponding to `source_anchor`.
    pub target_anchor: TimelineTime,
    /// Target-time delta per source-time delta.
    pub scale: TimeScale,
}

impl TimeTransform {
    /// Map a domain-qualified time through this transform.
    pub fn map(self, value: DomainTime) -> Result<DomainTime, TimelineTimeError> {
        if value.domain != self.source_domain {
            return Err(TimelineTimeError::DomainMismatch);
        }
        let delta = value.time.checked_sub(self.source_anchor)?;
        let mapped = self.target_anchor.checked_add(delta.checked_scale(self.scale)?)?;
        Ok(DomainTime::new(self.target_domain, mapped))
    }

    /// Construct the exact inverse, rejected for a zero scale.
    pub fn inverse(self) -> Result<Self, TimelineTimeError> {
        Ok(Self {
            source_domain: self.target_domain,
            target_domain: self.source_domain,
            source_anchor: self.target_anchor,
            target_anchor: self.source_anchor,
            scale: self.scale.reciprocal()?,
        })
    }
}

/// Exact half-open interval `[start, start + duration)` in one owner domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TimelineTimeRange {
    /// Inclusive start.
    pub start: TimelineTime,
    /// Non-negative duration.
    pub duration: TimelineTime,
}

impl TimelineTimeRange {
    /// Construct a validated half-open range.
    pub fn new(start: TimelineTime, duration: TimelineTime) -> Result<Self, TimelineTimeError> {
        if duration.is_negative() {
            return Err(TimelineTimeError::NegativeDuration);
        }
        Ok(Self { start, duration })
    }

    /// Exclusive end.
    pub fn end(self) -> Result<TimelineTime, TimelineTimeError> {
        self.start.checked_add(self.duration)
    }

    /// Whether this interval contains no time.
    pub fn is_empty(self) -> bool {
        self.duration == TimelineTime::ZERO
    }

    /// Whether the value lies inside this half-open range.
    pub fn contains(self, value: TimelineTime) -> Result<bool, TimelineTimeError> {
        Ok(value >= self.start && value < self.end()?)
    }
}

/// Exact-time validation or arithmetic failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimelineTimeError {
    /// Denominators must be non-zero.
    #[error("timeline time denominator must not be zero")]
    ZeroDenominator,
    /// The canonical result does not fit the supported persisted range.
    #[error("timeline time arithmetic overflow")]
    Overflow,
    /// A legacy frame time base must be positive.
    #[error("invalid legacy time base {numerator}/{denominator}")]
    InvalidLegacyTimeBase { numerator: i64, denominator: i64 },
    /// A frame evaluation grid must have a positive rate.
    #[error("invalid frame rate {numerator}/{denominator}")]
    InvalidFrameRate { numerator: i64, denominator: i64 },
    /// Cross-domain arithmetic requires an explicit transform.
    #[error("authoring time domain mismatch")]
    DomainMismatch,
    /// A zero-scale transform has no inverse.
    #[error("time transform is not invertible")]
    NonInvertibleTransform,
    /// Ranges cannot have negative duration.
    #[error("timeline range duration must be non-negative")]
    NegativeDuration,
    /// Floating input boundaries reject NaN and infinities.
    #[error("timeline time input must be finite")]
    NonFiniteInput,
}

fn round_frame_ratio(
    numerator: i128,
    denominator: i128,
    rounding: FrameRounding,
) -> Result<i128, TimelineTimeError> {
    Ok(match rounding {
        FrameRounding::Floor => numerator.div_euclid(denominator),
        FrameRounding::Ceil => {
            let floor = numerator.div_euclid(denominator);
            floor + i128::from(numerator.rem_euclid(denominator) != 0)
        }
        FrameRounding::Nearest => {
            let magnitude = numerator.unsigned_abs();
            let divisor = denominator as u128;
            let quotient = magnitude / divisor;
            let remainder = magnitude % divisor;
            let rounded = quotient + u128::from(remainder.saturating_mul(2) >= divisor);
            let rounded = i128::try_from(rounded).map_err(|_| TimelineTimeError::Overflow)?;
            if numerator < 0 {
                rounded.checked_neg().ok_or(TimelineTimeError::Overflow)?
            } else {
                rounded
            }
        }
    })
}

fn normalize_i128(numerator: i128, denominator: i128) -> Result<TimelineTime, TimelineTimeError> {
    if denominator == 0 {
        return Err(TimelineTimeError::ZeroDenominator);
    }
    if numerator == 0 {
        return Ok(TimelineTime::ZERO);
    }
    let (numerator, denominator) = if denominator < 0 {
        (
            numerator.checked_neg().ok_or(TimelineTimeError::Overflow)?,
            denominator.checked_neg().ok_or(TimelineTimeError::Overflow)?,
        )
    } else {
        (numerator, denominator)
    };
    let divisor = gcd_u128(numerator.unsigned_abs(), denominator as u128);
    let reduced_numerator = numerator / divisor as i128;
    let reduced_denominator = denominator / divisor as i128;
    Ok(TimelineTime {
        numerator: i64::try_from(reduced_numerator).map_err(|_| TimelineTimeError::Overflow)?,
        denominator: i64::try_from(reduced_denominator).map_err(|_| TimelineTimeError::Overflow)?,
    })
}

fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
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
    use crate::Rational;

    #[test]
    fn canonical_values_compare_and_hash_by_exact_time() {
        let half = TimelineTime::new(1, 2).expect("half");
        let reduced = TimelineTime::new(50, 100).expect("reduced");
        let negative = TimelineTime::new(1, -2).expect("negative");

        assert_eq!(half, reduced);
        assert_eq!(
            negative,
            TimelineTime::new(-1, 2).expect("canonical negative")
        );
        assert!(negative < TimelineTime::ZERO);
    }

    #[test]
    fn legacy_fractional_frames_convert_without_float() {
        let time = TimelineTime::from_frame_position(FramePosition::new(
            100_000,
            Rational::new(1001, 30_000),
        ))
        .expect("convert");
        assert_eq!(time, TimelineTime::new(10_010, 3).expect("expected"));
    }

    #[test]
    fn cross_domain_arithmetic_is_rejected_and_transform_is_invertible() {
        let sequence = AuthoringTimeDomain::Sequence(SequenceId::new());
        let contribution = AuthoringTimeDomain::AudioComponentEdit(AudioComponentEditId::new());
        let transform = TimeTransform {
            source_domain: contribution,
            target_domain: sequence,
            source_anchor: TimelineTime::ZERO,
            target_anchor: TimelineTime::new(10, 1).expect("anchor"),
            scale: TimeScale::new(1, 2).expect("scale"),
        };
        let local = DomainTime::new(contribution, TimelineTime::new(4, 1).expect("local"));
        let mapped = transform.map(local).expect("map");

        assert_eq!(mapped.time, TimelineTime::new(12, 1).expect("mapped time"));
        assert_eq!(
            transform.inverse().expect("inverse").map(mapped).expect("roundtrip"),
            local
        );
        assert_eq!(
            mapped.checked_duration_since(local),
            Err(TimelineTimeError::DomainMismatch)
        );
    }

    #[test]
    fn deserialization_rejects_invalid_and_canonicalizes_valid_values() {
        let canonical: TimelineTime =
            serde_json::from_str(r#"{"numerator":6,"denominator":-8}"#).expect("deserialize");
        assert_eq!(canonical, TimelineTime::new(-3, 4).expect("expected"));
        assert!(
            serde_json::from_str::<TimelineTime>(r#"{"numerator":1,"denominator":0}"#).is_err()
        );
    }
}
