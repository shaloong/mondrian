//! Stable parameter identity and exact-time numeric automation curves.

use crate::{KeyframeId, TimeScale, TimelineTime, TimelineTimeError};
use serde::{de::Error as _, Deserialize, Deserializer, Serialize};
use std::{collections::HashSet, fmt};

/// Stable schema identity for one editable parameter.
///
/// IDs are machine-facing namespaced ASCII identifiers. Display names, UI
/// ordering, plugin scan indexes, and property-path suffixes are not identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ParameterId(String);

impl ParameterId {
    /// Construct a validated namespaced parameter identity.
    pub fn new(value: impl Into<String>) -> Result<Self, ParameterIdError> {
        let value = value.into();
        if value.is_empty() || value.len() > 255 {
            return Err(ParameterIdError::InvalidLength);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        {
            return Err(ParameterIdError::InvalidCharacter);
        }
        if !value.contains('.') && !value.contains(':') {
            return Err(ParameterIdError::MissingNamespace);
        }
        Ok(Self(value))
    }

    /// Construct an ID embedded in a built-in schema definition.
    ///
    /// Invalid static definitions are programmer errors and fail immediately;
    /// runtime/plugin input must use [`Self::new`] and handle validation.
    #[track_caller]
    pub fn new_static(value: &'static str) -> Self {
        Self::new(value)
            .unwrap_or_else(|error| panic!("invalid static parameter ID `{value}`: {error}"))
    }

    /// Canonical serialized identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParameterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ParameterId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// Invalid stable parameter identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParameterIdError {
    /// IDs are bounded and cannot be empty.
    #[error("parameter ID length must be between 1 and 255 bytes")]
    InvalidLength,
    /// Only portable machine-facing ASCII characters are accepted.
    #[error("parameter ID contains an unsupported character")]
    InvalidCharacter,
    /// IDs must include a namespace separator.
    #[error("parameter ID must be namespaced with `.` or `:`")]
    MissingNamespace,
}

/// Exact temporal and floating value offset for one Bezier control handle.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExactBezierHandle {
    /// Exact offset from the owning keyframe time.
    pub time_offset: TimelineTime,
    /// Parameter-value offset from the owning keyframe value.
    pub value_offset: f64,
}

/// Interpolation used from one keyframe to the following keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AutomationSegmentInterpolation {
    /// Preserve the left value until the next key.
    Hold,
    /// Linear interpolation in exact author time.
    #[default]
    Linear,
    /// Cubic Bezier interpolation using the left out and right in handles.
    Bezier,
}

/// One numeric automation keyframe in its curve owner's time domain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExactAutomationKeyframe {
    /// Stable keyframe identity for editing and selection.
    pub id: KeyframeId,
    /// Exact owner-local author time.
    pub time: TimelineTime,
    /// Numeric parameter value.
    pub value: f64,
    /// Interpolation from this key to the next key.
    pub interpolation_to_next: AutomationSegmentInterpolation,
    /// Incoming Bezier handle used by the previous segment.
    pub in_handle: Option<ExactBezierHandle>,
    /// Outgoing Bezier handle used by this segment.
    pub out_handle: Option<ExactBezierHandle>,
}

impl ExactAutomationKeyframe {
    /// Construct a linear keyframe.
    pub fn linear(time: TimelineTime, value: f64) -> Self {
        Self {
            id: KeyframeId::new(),
            time,
            value,
            interpolation_to_next: AutomationSegmentInterpolation::Linear,
            in_handle: None,
            out_handle: None,
        }
    }
}

/// One stable numeric parameter curve using exact author-time coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExactAutomationCurve {
    /// Parameter schema identity.
    pub parameter_id: ParameterId,
    /// Value used when no keyframes exist.
    pub default_value: f64,
    /// Strictly time-ordered unique keyframes.
    pub keyframes: Vec<ExactAutomationKeyframe>,
}

impl ExactAutomationCurve {
    /// Construct an empty validated curve.
    pub fn new(parameter_id: ParameterId, default_value: f64) -> Result<Self, AutomationError> {
        if !default_value.is_finite() {
            return Err(AutomationError::NonFiniteValue);
        }
        Ok(Self { parameter_id, default_value, keyframes: Vec::new() })
    }

    /// Insert or replace a keyframe at the same exact time.
    pub fn set_keyframe(
        &mut self,
        keyframe: ExactAutomationKeyframe,
    ) -> Result<(), AutomationError> {
        validate_keyframe(&keyframe)?;
        match self.keyframes.binary_search_by_key(&keyframe.time, |candidate| candidate.time) {
            Ok(index) => self.keyframes[index] = keyframe,
            Err(index) => self.keyframes.insert(index, keyframe),
        }
        self.validate()
    }

    /// Validate ordering, values, and monotonic Bezier time handles.
    pub fn validate(&self) -> Result<(), AutomationError> {
        if !self.default_value.is_finite() {
            return Err(AutomationError::NonFiniteValue);
        }
        let mut keyframe_ids = HashSet::with_capacity(self.keyframes.len());
        for (index, keyframe) in self.keyframes.iter().enumerate() {
            validate_keyframe(keyframe)?;
            if !keyframe_ids.insert(keyframe.id) {
                return Err(AutomationError::DuplicateKeyframeIdentity);
            }
            if index > 0 && self.keyframes[index - 1].time >= keyframe.time {
                return Err(AutomationError::NonIncreasingTime);
            }
        }
        for pair in self.keyframes.windows(2) {
            validate_segment(&pair[0], &pair[1])?;
        }
        Ok(())
    }

    /// Evaluate at one exact owner-local time.
    ///
    /// Values before/after the keyed range extend the first/last key. Evaluation
    /// depends only on absolute author time, never on render block partitioning.
    pub fn evaluate(&self, time: TimelineTime) -> Result<f64, AutomationError> {
        self.validate()?;
        let Some(first) = self.keyframes.first() else {
            return Ok(self.default_value);
        };
        if time <= first.time {
            return Ok(first.value);
        }
        let Some(last) = self.keyframes.last() else {
            return Ok(self.default_value);
        };
        if time >= last.time {
            return Ok(last.value);
        }
        let right_index = self.keyframes.partition_point(|candidate| candidate.time <= time);
        let left = &self.keyframes[right_index - 1];
        let right = &self.keyframes[right_index];
        match left.interpolation_to_next {
            AutomationSegmentInterpolation::Hold => Ok(left.value),
            AutomationSegmentInterpolation::Linear => linear_value(left, right, time),
            AutomationSegmentInterpolation::Bezier => bezier_value(left, right, time),
        }
    }

    /// Validate once and materialize the ordered interpolation segments.
    ///
    /// Realtime consumers use these immutable segments to select interpolation
    /// at preparation time instead of validating and searching the author
    /// curve for every evaluation sample. Values outside a segment are clamped
    /// to that segment's endpoint, matching [`Self::evaluate`].
    pub fn prepared_segments(&self) -> Result<Vec<ExactAutomationSegment>, AutomationError> {
        self.validate()?;
        Ok(self
            .keyframes
            .windows(2)
            .map(|pair| ExactAutomationSegment { left: pair[0].clone(), right: pair[1].clone() })
            .collect())
    }
}

/// One validated, immutable interpolation span from an exact automation curve.
///
/// Construction is owned by [`ExactAutomationCurve::prepared_segments`], so
/// repeated evaluation does not revisit whole-curve validation or key lookup.
#[derive(Debug, Clone, PartialEq)]
pub struct ExactAutomationSegment {
    left: ExactAutomationKeyframe,
    right: ExactAutomationKeyframe,
}

impl ExactAutomationSegment {
    /// Exact owner-local time at which this segment begins.
    pub fn start_time(&self) -> TimelineTime {
        self.left.time
    }

    /// Exact owner-local time at which this segment ends.
    pub fn end_time(&self) -> TimelineTime {
        self.right.time
    }

    /// Evaluate this already-validated segment with endpoint extension.
    pub fn evaluate(&self, time: TimelineTime) -> Result<f64, AutomationError> {
        if time <= self.left.time {
            return Ok(self.left.value);
        }
        if time >= self.right.time {
            return Ok(self.right.value);
        }
        match self.left.interpolation_to_next {
            AutomationSegmentInterpolation::Hold => Ok(self.left.value),
            AutomationSegmentInterpolation::Linear => linear_value(&self.left, &self.right, time),
            AutomationSegmentInterpolation::Bezier => bezier_value(&self.left, &self.right, time),
        }
    }
}

/// Invalid automation author state or evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AutomationError {
    /// Numeric author values and handle values must be finite.
    #[error("automation values must be finite")]
    NonFiniteValue,
    /// Keyframe times must be strictly increasing.
    #[error("automation keyframe times must be strictly increasing")]
    NonIncreasingTime,
    /// One curve cannot address two keys through the same stable identity.
    #[error("automation keyframe identities must be unique within one curve")]
    DuplicateKeyframeIdentity,
    /// Bezier time handles must stay within their segment and remain monotonic.
    #[error("automation Bezier time handles are outside their segment")]
    InvalidBezierTimeHandle,
    /// Exact-time arithmetic failed.
    #[error(transparent)]
    Time(#[from] TimelineTimeError),
}

fn validate_keyframe(keyframe: &ExactAutomationKeyframe) -> Result<(), AutomationError> {
    if !keyframe.value.is_finite()
        || keyframe.in_handle.is_some_and(|handle| !handle.value_offset.is_finite())
        || keyframe.out_handle.is_some_and(|handle| !handle.value_offset.is_finite())
    {
        return Err(AutomationError::NonFiniteValue);
    }
    Ok(())
}

fn validate_segment(
    left: &ExactAutomationKeyframe,
    right: &ExactAutomationKeyframe,
) -> Result<(), AutomationError> {
    if left.interpolation_to_next != AutomationSegmentInterpolation::Bezier {
        return Ok(());
    }
    let duration = right.time.checked_sub(left.time)?;
    if left
        .out_handle
        .is_some_and(|handle| handle.time_offset.is_negative() || handle.time_offset > duration)
        || right.in_handle.is_some_and(|handle| {
            handle.time_offset > TimelineTime::ZERO
                || handle
                    .time_offset
                    .checked_scale(TimeScale::NEGATIVE_ONE)
                    .map_or(true, |magnitude| magnitude > duration)
        })
    {
        return Err(AutomationError::InvalidBezierTimeHandle);
    }
    Ok(())
}

fn linear_value(
    left: &ExactAutomationKeyframe,
    right: &ExactAutomationKeyframe,
    time: TimelineTime,
) -> Result<f64, AutomationError> {
    let elapsed = time.checked_sub(left.time)?.to_f64();
    let duration = right.time.checked_sub(left.time)?.to_f64();
    Ok(left.value + (right.value - left.value) * (elapsed / duration))
}

fn bezier_value(
    left: &ExactAutomationKeyframe,
    right: &ExactAutomationKeyframe,
    time: TimelineTime,
) -> Result<f64, AutomationError> {
    let duration = right.time.checked_sub(left.time)?;
    let default_out = duration.checked_scale(TimeScale::new(1, 3)?)?;
    let default_in = duration.checked_scale(TimeScale::new(-1, 3)?)?;
    let out_handle = left.out_handle.unwrap_or(ExactBezierHandle {
        time_offset: default_out,
        value_offset: (right.value - left.value) / 3.0,
    });
    let in_handle = right.in_handle.unwrap_or(ExactBezierHandle {
        time_offset: default_in,
        value_offset: -(right.value - left.value) / 3.0,
    });
    let x0 = left.time.to_f64();
    let x1 = left.time.checked_add(out_handle.time_offset)?.to_f64();
    let x2 = right.time.checked_add(in_handle.time_offset)?.to_f64();
    let x3 = right.time.to_f64();
    let target = time.to_f64();
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..48 {
        let candidate = (low + high) * 0.5;
        if cubic(x0, x1, x2, x3, candidate) < target {
            low = candidate;
        } else {
            high = candidate;
        }
    }
    let t = (low + high) * 0.5;
    Ok(cubic(
        left.value,
        left.value + out_handle.value_offset,
        right.value + in_handle.value_offset,
        right.value,
        t,
    ))
}

fn cubic(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let one_minus = 1.0 - t;
    one_minus * one_minus * one_minus * p0
        + 3.0 * one_minus * one_minus * t * p1
        + 3.0 * one_minus * t * t * p2
        + t * t * t * p3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_ids_are_stable_namespaced_values() {
        let id = ParameterId::new("mondrian.audio.gain_db").expect("parameter ID");
        assert_eq!(id.as_str(), "mondrian.audio.gain_db");
        assert!(ParameterId::new("gain").is_err());
        assert!(serde_json::from_str::<ParameterId>(r#""bad id""#).is_err());
    }

    #[test]
    fn exact_curve_evaluation_is_independent_of_block_partitioning() {
        let id = ParameterId::new("mondrian.test.value").expect("parameter ID");
        let mut curve = ExactAutomationCurve::new(id, 0.0).expect("curve");
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0))
            .expect("first");
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(1, 1).expect("time"),
                1.0,
            ))
            .expect("second");

        let direct = curve.evaluate(TimelineTime::new(1, 3).expect("direct time")).expect("direct");
        let same_absolute_time = TimelineTime::new(16_000, 48_000).expect("sample time");
        let partitioned = curve.evaluate(same_absolute_time).expect("partitioned");
        assert!((direct - partitioned).abs() < 1.0e-12);
    }

    #[test]
    fn bezier_time_handles_are_exact_and_monotonic() {
        let id = ParameterId::new("mondrian.test.bezier").expect("parameter ID");
        let mut left = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        left.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        left.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(1, 4).expect("out time"),
            value_offset: 0.1,
        });
        let mut right =
            ExactAutomationKeyframe::linear(TimelineTime::new(1, 1).expect("right time"), 1.0);
        right.in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 4).expect("in time"),
            value_offset: -0.1,
        });
        let mut curve = ExactAutomationCurve::new(id, 0.0).expect("curve");
        curve.set_keyframe(left).expect("left");
        curve.set_keyframe(right).expect("right");

        let middle = curve.evaluate(TimelineTime::new(1, 2).expect("middle time")).expect("middle");
        assert!((middle - 0.5).abs() < 1.0e-9);
    }
}
