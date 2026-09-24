//! Stable parameter identity and exact-time numeric automation curves.

use crate::{
    AuthoringList, InterpolationType, KeyframeId, TimeScale, TimelineTime, TimelineTimeError,
    TimelineTimeRange,
};
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

impl crate::AuthoringFootprint for ParameterId {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        let Self(value) = self;
        collector.collect(value)
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

/// Persistent constraint on both Bezier tangents of one numeric keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExactAutomationTangentMode {
    /// Preserve explicitly authored handles, including legacy curves.
    #[default]
    Manual,
    /// Recompute monotone tangents when neighboring keys move or change.
    Auto,
    /// Keep incoming and outgoing tangents collinear while allowing overshoot.
    Continuous,
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
    /// Sticky tangent constraint; absent in older project files means manual.
    #[serde(default)]
    pub tangent_mode: ExactAutomationTangentMode,
}

impl crate::AuthoringFootprint for ExactAutomationKeyframe {
    fn collect_authoring_footprint(
        &self,
        _collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        let Self {
            id: _,
            time: _,
            value: _,
            interpolation_to_next: _,
            in_handle: _,
            out_handle: _,
            tangent_mode: _,
        } = self;
        Ok(())
    }
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
            tangent_mode: ExactAutomationTangentMode::Manual,
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
    pub keyframes: AuthoringList<ExactAutomationKeyframe>,
}

impl crate::AuthoringFootprint for ExactAutomationCurve {
    fn collect_authoring_footprint(
        &self,
        collector: &mut crate::AuthoringFootprintCollector,
    ) -> Result<(), crate::AuthoringFootprintError> {
        let Self { parameter_id, default_value: _, keyframes } = self;
        collector.collect(parameter_id)?;
        collector.collect(keyframes)
    }
}

impl ExactAutomationCurve {
    /// Construct an empty validated curve.
    pub fn new(parameter_id: ParameterId, default_value: f64) -> Result<Self, AutomationError> {
        if !default_value.is_finite() {
            return Err(AutomationError::NonFiniteValue);
        }
        Ok(Self {
            parameter_id,
            default_value,
            keyframes: AuthoringList::new(),
        })
    }

    /// Insert or replace a keyframe at the same exact time.
    pub fn set_keyframe(
        &mut self,
        keyframe: ExactAutomationKeyframe,
    ) -> Result<(), AutomationError> {
        validate_keyframe(&keyframe)?;
        let mut candidate = self.clone();
        match candidate
            .keyframes
            .binary_search_by_key(&keyframe.time, |existing| existing.time)
        {
            Ok(index) => candidate.keyframes[index] = keyframe,
            Err(index) => candidate.keyframes.insert(index, keyframe),
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Change one key's outgoing interpolation and its optional two-sided tangent constraint.
    ///
    /// Auto and continuous modes also enable Bezier interpolation on the
    /// incoming segment. The edit is validated before publication.
    pub fn set_keyframe_interpolation(
        &mut self,
        keyframe_id: KeyframeId,
        interpolation: InterpolationType,
    ) -> Result<(), AutomationError> {
        let mut candidate = self.clone();
        let index = candidate
            .keyframes
            .iter()
            .position(|keyframe| keyframe.id == keyframe_id)
            .ok_or(AutomationError::UnknownKeyframe)?;
        let (segment, mode) = match interpolation {
            InterpolationType::Hold => (
                AutomationSegmentInterpolation::Hold,
                ExactAutomationTangentMode::Manual,
            ),
            InterpolationType::Linear => (
                AutomationSegmentInterpolation::Linear,
                ExactAutomationTangentMode::Manual,
            ),
            InterpolationType::AutoBezier => (
                AutomationSegmentInterpolation::Bezier,
                ExactAutomationTangentMode::Auto,
            ),
            InterpolationType::ContinuousBezier => (
                AutomationSegmentInterpolation::Bezier,
                ExactAutomationTangentMode::Continuous,
            ),
            InterpolationType::Bezier => (
                AutomationSegmentInterpolation::Bezier,
                ExactAutomationTangentMode::Manual,
            ),
            InterpolationType::EaseIn | InterpolationType::EaseOut => {
                return Err(AutomationError::UnsupportedInterpolationPreset);
            }
        };
        candidate.keyframes[index].interpolation_to_next = segment;
        candidate.keyframes[index].tangent_mode = mode;
        if mode != ExactAutomationTangentMode::Manual && index > 0 {
            candidate.keyframes[index - 1].interpolation_to_next =
                AutomationSegmentInterpolation::Bezier;
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Remove a key and refresh the constraints of its surviving neighbors.
    pub fn remove_keyframe(&mut self, keyframe_id: KeyframeId) -> Result<(), AutomationError> {
        let mut candidate = self.clone();
        let before = candidate.keyframes.len();
        candidate.keyframes.retain(|keyframe| keyframe.id != keyframe_id);
        if candidate.keyframes.len() == before {
            return Err(AutomationError::UnknownKeyframe);
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    fn normalize_constrained_tangents(&mut self) -> Result<(), AutomationError> {
        for index in 0..self.keyframes.len() {
            if self.keyframes[index].tangent_mode == ExactAutomationTangentMode::Manual {
                continue;
            }
            let (incoming, outgoing) = self.constrained_handles(index)?;
            self.keyframes[index].in_handle = incoming;
            self.keyframes[index].out_handle = outgoing;
        }
        Ok(())
    }

    fn constrained_handles(
        &self,
        index: usize,
    ) -> Result<(Option<ExactBezierHandle>, Option<ExactBezierHandle>), AutomationError> {
        let current = &self.keyframes[index];
        let previous = index.checked_sub(1).map(|position| &self.keyframes[position]);
        let next = self.keyframes.get(index + 1);
        let slope = constrained_slope(current.tangent_mode, previous, current, next)?;
        let incoming = previous
            .map(|keyframe| tangent_handle(keyframe.time, current.time, slope, true))
            .transpose()?;
        let outgoing = next
            .map(|keyframe| tangent_handle(current.time, keyframe.time, slope, false))
            .transpose()?;
        Ok((incoming, outgoing))
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
        for (index, keyframe) in self.keyframes.iter().enumerate() {
            if keyframe.tangent_mode == ExactAutomationTangentMode::Manual {
                continue;
            }
            if index + 1 < self.keyframes.len()
                && keyframe.interpolation_to_next != AutomationSegmentInterpolation::Bezier
            {
                return Err(AutomationError::InvalidConstrainedTangent);
            }
            let (incoming, outgoing) = self.constrained_handles(index)?;
            if keyframe.in_handle != incoming || keyframe.out_handle != outgoing {
                return Err(AutomationError::InvalidConstrainedTangent);
            }
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

    /// Shift every key at or after an exact owner-time boundary.
    ///
    /// This is the primitive used when an owning Sequence-time region follows
    /// an editorial insert. The edit is atomic: overflow or an invalid final
    /// curve leaves the original curve unchanged.
    pub fn shift_keyframes_at_or_after(
        &mut self,
        boundary: TimelineTime,
        delta: TimelineTime,
    ) -> Result<(), AutomationError> {
        if delta.is_negative() {
            return Err(AutomationError::Time(TimelineTimeError::NegativeDuration));
        }
        if delta.is_zero() {
            return Ok(());
        }

        let mut candidate = self.clone();
        for keyframe in &mut candidate.keyframes {
            if keyframe.time >= boundary {
                keyframe.time = keyframe.time.checked_add(delta)?;
            }
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Remove keys inside one half-open owner-time range and close the gap.
    ///
    /// Keys before `range.start` retain their exact time. Keys at or after the
    /// exclusive range end move earlier by the range duration while preserving
    /// stable identity, value, interpolation, and handles. The edit validates a
    /// complete candidate before publication.
    pub fn extract_time_range(&mut self, range: TimelineTimeRange) -> Result<(), AutomationError> {
        let end = range.end()?;
        if range.duration.is_zero() {
            return Ok(());
        }

        let mut candidate = self.clone();
        candidate
            .keyframes
            .retain(|keyframe| keyframe.time < range.start || keyframe.time >= end);
        for keyframe in &mut candidate.keyframes {
            if keyframe.time >= end {
                keyframe.time = keyframe.time.checked_sub(range.duration)?;
            }
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
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
    /// Addressed keyframe is absent from this curve.
    #[error("automation keyframe identity is absent")]
    UnknownKeyframe,
    /// This preset has no defined exact numeric automation interpretation.
    #[error("interpolation preset is unsupported by exact numeric automation")]
    UnsupportedInterpolationPreset,
    /// Stored handles disagree with their persistent auto/continuous constraint.
    #[error("automation constrained Bezier handles do not match neighboring keys")]
    InvalidConstrainedTangent,
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

fn constrained_slope(
    mode: ExactAutomationTangentMode,
    previous: Option<&ExactAutomationKeyframe>,
    current: &ExactAutomationKeyframe,
    next: Option<&ExactAutomationKeyframe>,
) -> Result<f64, AutomationError> {
    let incoming = previous
        .map(|keyframe| {
            let span = current.time.checked_sub(keyframe.time)?.to_f64();
            Ok::<_, AutomationError>(((current.value - keyframe.value) / span, span))
        })
        .transpose()?;
    let outgoing = next
        .map(|keyframe| {
            let span = keyframe.time.checked_sub(current.time)?.to_f64();
            Ok::<_, AutomationError>(((keyframe.value - current.value) / span, span))
        })
        .transpose()?;
    let slope = match (incoming, outgoing) {
        (Some((left, left_span)), Some((right, right_span))) => match mode {
            ExactAutomationTangentMode::Auto => {
                if left == 0.0 || right == 0.0 || left.signum() != right.signum() {
                    0.0
                } else {
                    let left_weight = 2.0 * right_span + left_span;
                    let right_weight = right_span + 2.0 * left_span;
                    (left_weight + right_weight) / (left_weight / left + right_weight / right)
                }
            }
            ExactAutomationTangentMode::Continuous => {
                (left * right_span + right * left_span) / (left_span + right_span)
            }
            ExactAutomationTangentMode::Manual => 0.0,
        },
        (Some((slope, _)), None) | (None, Some((slope, _))) => slope,
        (None, None) => 0.0,
    };
    slope.is_finite().then_some(slope).ok_or(AutomationError::NonFiniteValue)
}

fn tangent_handle(
    left: TimelineTime,
    right: TimelineTime,
    slope: f64,
    incoming: bool,
) -> Result<ExactBezierHandle, AutomationError> {
    let duration = right.checked_sub(left)?;
    let fraction = if incoming {
        TimeScale::new(-1, 3)?
    } else {
        TimeScale::new(1, 3)?
    };
    let time_offset = duration.checked_scale(fraction)?;
    let value_offset = slope * time_offset.to_f64();
    if !value_offset.is_finite() {
        return Err(AutomationError::NonFiniteValue);
    }
    Ok(ExactBezierHandle { time_offset, value_offset })
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
    fn auto_tangent_flattens_an_extremum_and_recomputes_after_neighbor_move() {
        let mut curve = ExactAutomationCurve::new(ParameterId::new_static("test.audio.auto"), 0.0)
            .expect("curve");
        let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 1.0);
        let last = ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("time"), 0.0);
        curve.set_keyframe(first).expect("first");
        curve.set_keyframe(middle.clone()).expect("middle");
        curve.set_keyframe(last.clone()).expect("last");
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::AutoBezier)
            .expect("auto");
        assert_eq!(
            curve.keyframes[1].tangent_mode,
            ExactAutomationTangentMode::Auto
        );
        assert_eq!(curve.keyframes[1].in_handle.expect("in").value_offset, 0.0);
        assert_eq!(
            curve.keyframes[1].out_handle.expect("out").value_offset,
            0.0
        );
        for index in 0..=20 {
            let time = TimelineTime::new(index, 10).expect("sample time");
            assert!(curve.evaluate(time).expect("evaluate") <= 1.0 + 1.0e-9);
        }

        let mut moved = last;
        moved.value = 2.0;
        curve.set_keyframe(moved).expect("move neighbor");
        let middle = &curve.keyframes[1];
        let incoming = middle.in_handle.expect("in after move");
        let outgoing = middle.out_handle.expect("out after move");
        assert!(incoming.value_offset < 0.0);
        assert!(outgoing.value_offset > 0.0);
        assert!((incoming.value_offset + outgoing.value_offset).abs() < 1.0e-9);
    }

    #[test]
    fn continuous_tangent_stays_collinear_and_legacy_keys_remain_manual() {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.audio.continuous"), 0.0)
                .expect("curve");
        let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        let last = ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("time"), 3.0);
        for keyframe in [first, middle.clone(), last] {
            curve.set_keyframe(keyframe).expect("keyframe");
        }
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::ContinuousBezier)
            .expect("continuous");
        let selected = &curve.keyframes[1];
        let incoming = selected.in_handle.expect("incoming");
        let outgoing = selected.out_handle.expect("outgoing");
        let incoming_slope = incoming.value_offset / incoming.time_offset.to_f64();
        let outgoing_slope = outgoing.value_offset / outgoing.time_offset.to_f64();
        assert!((incoming_slope - outgoing_slope).abs() < 1.0e-10);

        let mut legacy = serde_json::to_value(selected).expect("serialize keyframe");
        legacy.as_object_mut().expect("object").remove("tangent_mode");
        let restored: ExactAutomationKeyframe =
            serde_json::from_value(legacy).expect("deserialize legacy keyframe");
        assert_eq!(restored.tangent_mode, ExactAutomationTangentMode::Manual);
    }

    #[test]
    fn missing_key_interpolation_edit_is_atomic() {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.audio.missing"), 0.0)
                .expect("curve");
        let keyframe = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        curve.set_keyframe(keyframe).expect("keyframe");
        let before = curve.clone();
        assert_eq!(
            curve.set_keyframe_interpolation(KeyframeId::new(), InterpolationType::AutoBezier),
            Err(AutomationError::UnknownKeyframe)
        );
        assert_eq!(curve, before);
    }

    #[test]
    fn forged_constrained_handles_are_rejected_on_project_validation() {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.audio.forged"), 0.0)
                .expect("curve");
        let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 1.0);
        let last = ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("time"), 0.0);
        for keyframe in [first, middle.clone(), last] {
            curve.set_keyframe(keyframe).expect("keyframe");
        }
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::AutoBezier)
            .expect("auto");
        let encoded = serde_json::to_string(&curve).expect("serialize");
        let mut loaded: ExactAutomationCurve = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(loaded.validate(), Ok(()));
        loaded.keyframes[1].out_handle.as_mut().expect("out").value_offset = 0.5;
        assert_eq!(
            loaded.validate(),
            Err(AutomationError::InvalidConstrainedTangent)
        );
        loaded.keyframes[1].out_handle = curve.keyframes[1].out_handle;
        loaded.keyframes[1].interpolation_to_next = AutomationSegmentInterpolation::Linear;
        assert_eq!(
            loaded.validate(),
            Err(AutomationError::InvalidConstrainedTangent)
        );
    }

    #[test]
    fn rejected_keyframe_insertions_leave_the_curve_unchanged() {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("mondrian.test.atomic_curve"), 0.0)
                .expect("curve");
        let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        let second =
            ExactAutomationKeyframe::linear(TimelineTime::new(1, 1).expect("one second"), 1.0);
        curve.set_keyframe(first.clone()).expect("first key");
        curve.set_keyframe(second).expect("second key");
        let original = curve.clone();

        let mut invalid_handle = first.clone();
        invalid_handle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        invalid_handle.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(2, 1).expect("two seconds"),
            value_offset: 0.5,
        });
        assert_eq!(
            curve.set_keyframe(invalid_handle),
            Err(AutomationError::InvalidBezierTimeHandle)
        );
        assert_eq!(curve, original);

        let mut duplicate_identity =
            ExactAutomationKeyframe::linear(TimelineTime::new(2, 1).expect("two seconds"), 2.0);
        duplicate_identity.id = first.id;
        assert_eq!(
            curve.set_keyframe(duplicate_identity),
            Err(AutomationError::DuplicateKeyframeIdentity)
        );
        assert_eq!(curve, original);
        assert_eq!(
            curve.evaluate(TimelineTime::new(1, 2).expect("half second")),
            Ok(0.5)
        );
    }

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
    fn exact_curve_extract_uses_half_open_range_and_preserves_survivor_identities() {
        let id = ParameterId::new("mondrian.test.extract").expect("parameter ID");
        let mut curve = ExactAutomationCurve::new(id, 0.0).expect("curve");
        let keys = [0_i64, 10, 20, 30]
            .into_iter()
            .map(|time| {
                ExactAutomationKeyframe::linear(
                    TimelineTime::new(time, 1).expect("key time"),
                    time as f64,
                )
            })
            .collect::<Vec<_>>();
        let first_id = keys[0].id;
        let removed_id = keys[1].id;
        let end_id = keys[2].id;
        let after_id = keys[3].id;
        for key in keys {
            curve.set_keyframe(key).expect("key");
        }

        curve
            .extract_time_range(
                TimelineTimeRange::new(
                    TimelineTime::new(10, 1).expect("start"),
                    TimelineTime::new(10, 1).expect("duration"),
                )
                .expect("range"),
            )
            .expect("extract");

        assert_eq!(
            curve.keyframes.iter().map(|key| (key.id, key.time)).collect::<Vec<_>>(),
            [
                (first_id, TimelineTime::new(0, 1).expect("zero")),
                (end_id, TimelineTime::new(10, 1).expect("shifted end")),
                (after_id, TimelineTime::new(20, 1).expect("shifted after")),
            ]
        );
        assert!(!curve.keyframes.iter().any(|key| key.id == removed_id));
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
