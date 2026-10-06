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

/// Temporal side of a keyframe's Bezier control handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BezierHandleSide {
    /// Handle controlling the preceding segment.
    Incoming,
    /// Handle controlling the following segment.
    Outgoing,
}

/// Read-only canonical control resolved from one ordered author curve.
/// This projection carries author identity and coordinates without creating state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TemporalBezierControl {
    /// Stable identity of the owning key.
    pub keyframe_id: KeyframeId,
    /// Exact time in the curve owner's domain.
    pub key_time: TimelineTime,
    /// Canonical numeric value of the owning channel.
    pub key_value: f64,
    /// Exact time of the neighbor defining this control's admitted span.
    pub neighbor_time: TimelineTime,
    /// Temporal side of the owning key.
    pub side: BezierHandleSide,
    /// The stored or canonical default control used by execution.
    pub handle: ExactBezierHandle,
}

/// Sparse author edit retaining every omitted exact handle coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExactBezierHandleEdit {
    /// Replacement owner-local time offset, when the temporal axis changed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_coordinate"
    )]
    pub time_offset: Option<TimelineTime>,
    /// Replacement parameter offset, when the value axis changed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_finite_coordinate"
    )]
    pub value_offset: Option<f64>,
}

/// Decode a supplied sparse coordinate without treating invalid input as omission.
///
/// Use with Serde's `default` and `skip_serializing_if = "Option::is_none"`:
/// an absent field retains its author coordinate, while a present field must
/// contain a finite number. Explicit null is invalid, including JSON null
/// produced by serializing a nonfinite floating value.
pub fn deserialize_present_finite_coordinate<'de, D>(
    deserializer: D,
) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    if !value.is_finite() {
        return Err(D::Error::custom("a supplied coordinate must be finite"));
    }
    Ok(Some(value))
}

/// Decode a supplied typed coordinate without interpreting it as omission.
///
/// Use with Serde's `default` for absent fields. The coordinate type retains its
/// normal admission rules; nonnullable author time and value types reject null.
pub fn deserialize_present_coordinate<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
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
    /// Preserve independently authored handles.
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
    /// Explicit persistent constraint on the authored handles.
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
        self.set_keyframes_interpolation(&[keyframe_id], interpolation)
    }

    /// Set one preset on a nonempty unique selection with one constraint normalization.
    /// Stable identities, exact owner times and raw scalar values are retained.
    /// Invalid or unsupported selections are rejected before publication.
    pub fn set_keyframes_interpolation(
        &mut self,
        ids: &[KeyframeId],
        interpolation: InterpolationType,
    ) -> Result<(), AutomationError> {
        self.validate()?;
        if ids.is_empty() {
            return Err(AutomationError::EmptyKeySelection);
        }
        let identities = ids.iter().copied().collect::<HashSet<_>>();
        if identities.len() != ids.len() {
            return Err(AutomationError::DuplicateKeyframeIdentity);
        }
        let indices = self
            .keyframes
            .iter()
            .enumerate()
            .filter(|(_, key)| identities.contains(&key.id))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if indices.len() != ids.len() {
            return Err(AutomationError::UnknownKeyframe);
        }
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
        let mut candidate = self.clone();
        for index in indices {
            let previous_mode = candidate.keyframes[index].tangent_mode;
            candidate.keyframes[index].interpolation_to_next = segment;
            candidate.keyframes[index].tangent_mode = mode;
            if mode != ExactAutomationTangentMode::Manual && index > 0 {
                candidate.keyframes[index - 1].interpolation_to_next =
                    AutomationSegmentInterpolation::Bezier;
            }
            if mode == ExactAutomationTangentMode::Continuous
                && previous_mode != ExactAutomationTangentMode::Continuous
            {
                candidate.initialize_continuous_constraint(index);
            }
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Atomically remove unique stable keys and normalize surviving constraints once.
    /// When no keys survive, preserve the original evaluated value at `retain_time`.
    pub fn remove_keyframes(
        &mut self,
        ids: &[KeyframeId],
        retain_time: TimelineTime,
    ) -> Result<(), AutomationError> {
        if ids.is_empty() {
            return Err(AutomationError::EmptyKeySelection);
        }
        let identities = ids.iter().copied().collect::<std::collections::HashSet<_>>();
        if identities.len() != ids.len() {
            return Err(AutomationError::DuplicateKeyframeIdentity);
        }
        let present = self
            .keyframes
            .iter()
            .map(|key| key.id)
            .collect::<std::collections::HashSet<_>>();
        if !identities.is_subset(&present) {
            return Err(AutomationError::UnknownKeyframe);
        }
        let mut candidate = self.clone();
        let retained = candidate.evaluate(retain_time)?;
        candidate.keyframes.retain(|key| !identities.contains(&key.id));
        if candidate.keyframes.is_empty() {
            candidate.default_value = retained;
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Translate unique stable keys by one exact delta, admitting the final set atomically.
    /// Values and manual handle offsets are retained; constraints normalize once.
    /// Empty, duplicate or stale selections, overflow and final collisions leave this curve unchanged.
    pub fn shift_keyframes(
        &mut self,
        ids: &[KeyframeId],
        delta: TimelineTime,
    ) -> Result<(), AutomationError> {
        self.validate()?;
        if ids.is_empty() {
            return Err(AutomationError::EmptyKeySelection);
        }
        let identities = ids.iter().copied().collect::<HashSet<_>>();
        if identities.len() != ids.len() {
            return Err(AutomationError::DuplicateKeyframeIdentity);
        }
        if self.keyframes.iter().filter(|key| identities.contains(&key.id)).count() != ids.len() {
            return Err(AutomationError::UnknownKeyframe);
        }
        if delta.is_zero() {
            return Ok(());
        }
        let mut candidate = self.clone();
        for key in &mut candidate.keyframes {
            if identities.contains(&key.id) {
                key.time = key.time.checked_add(delta)?;
            }
        }
        candidate.keyframes.sort_by_key(|key| key.time);
        if candidate.keyframes.windows(2).any(|pair| pair[0].time == pair[1].time) {
            return Err(AutomationError::NonIncreasingTime);
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Offset stable keys with one common definition-bounded displacement.
    /// Times, identities and manual offsets remain intact; constraints normalize once.
    /// Definition-specific control admission belongs to the owning adapter.
    pub fn offset_keyframe_values(
        &mut self,
        ids: &[KeyframeId],
        contract: crate::ParameterNumericContract,
        delta: f64,
        steps: i8,
    ) -> Result<(), AutomationError> {
        self.validate()?;
        if ids.is_empty() {
            return Err(AutomationError::EmptyKeySelection);
        }
        let identities = ids.iter().copied().collect::<HashSet<_>>();
        if identities.len() != ids.len() {
            return Err(AutomationError::DuplicateKeyframeIdentity);
        }
        let (indices, values): (Vec<_>, Vec<_>) = self
            .keyframes
            .iter()
            .enumerate()
            .filter(|(_, key)| identities.contains(&key.id))
            .map(|(index, key)| (index, crate::PropertyValue::Double(key.value)))
            .unzip();
        if indices.len() != ids.len() {
            return Err(AutomationError::UnknownKeyframe);
        }
        let shifted = contract
            .offset_scalars(&values, delta, steps)
            .ok_or(AutomationError::InvalidValueIncrement)?;
        if shifted == values {
            return Ok(());
        }
        let mut candidate = self.clone();
        for (index, value) in indices.into_iter().zip(shifted) {
            let crate::PropertyValue::Double(value) = value else {
                return Err(AutomationError::InvalidValueIncrement);
            };
            candidate.keyframes[index].value = value;
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

    /// Edit one handle atomically, retaining omitted author coordinates.
    ///
    /// Changing an Auto handle converts it to Continuous. Continuous edits
    /// align the opposite handle while retaining its independent temporal
    /// length, or its value length for a vertical direction. Manual handles
    /// remain independent. The edited segment becomes Bezier; absent segments,
    /// empty edits and invalid coordinates are rejected without publication.
    pub fn edit_keyframe_handle(
        &mut self,
        keyframe_id: KeyframeId,
        side: BezierHandleSide,
        edit: ExactBezierHandleEdit,
    ) -> Result<(), AutomationError> {
        if edit.time_offset.is_none() && edit.value_offset.is_none() {
            return Err(AutomationError::EmptyHandleEdit);
        }
        let mut candidate = self.clone();
        let index = candidate
            .keyframes
            .iter()
            .position(|key| key.id == keyframe_id)
            .ok_or(AutomationError::UnknownKeyframe)?;
        let neighbor_index = match side {
            BezierHandleSide::Incoming => index.checked_sub(1),
            BezierHandleSide::Outgoing => {
                (index + 1 < candidate.keyframes.len()).then_some(index + 1)
            }
        }
        .ok_or(AutomationError::MissingBezierSegment)?;
        let incoming = side == BezierHandleSide::Incoming;
        let current = &candidate.keyframes[index];
        let neighbor = &candidate.keyframes[neighbor_index];
        let duration = if incoming {
            current.time.checked_sub(neighbor.time)?
        } else {
            neighbor.time.checked_sub(current.time)?
        };
        let original = if incoming {
            current.in_handle
        } else {
            current.out_handle
        };
        let baseline = match original {
            Some(handle) => handle,
            None => initial_segment_handle(current, neighbor, edit.time_offset)?,
        };
        let edited = ExactBezierHandle {
            time_offset: edit.time_offset.unwrap_or(baseline.time_offset),
            value_offset: edit.value_offset.unwrap_or(baseline.value_offset),
        };
        validate_handle_time(edited, duration, incoming)?;
        if original == Some(edited) {
            return Ok(());
        }
        let key = &mut candidate.keyframes[index];
        if key.tangent_mode == ExactAutomationTangentMode::Auto {
            key.tangent_mode = ExactAutomationTangentMode::Continuous;
        }
        if incoming {
            key.in_handle = Some(edited);
        } else {
            key.out_handle = Some(edited);
        }
        validate_keyframe(key)?;
        if incoming {
            candidate.keyframes[neighbor_index].interpolation_to_next =
                AutomationSegmentInterpolation::Bezier;
        } else {
            candidate.keyframes[index].interpolation_to_next =
                AutomationSegmentInterpolation::Bezier;
        }
        if candidate.keyframes[index].tangent_mode == ExactAutomationTangentMode::Continuous {
            let partner_index = if incoming {
                (index + 1 < candidate.keyframes.len()).then_some(index + 1)
            } else {
                index.checked_sub(1)
            };
            if let Some(partner_index) = partner_index {
                let key = &candidate.keyframes[index];
                let partner = if incoming {
                    key.out_handle
                } else {
                    key.in_handle
                };
                let partner = match partner {
                    Some(handle) => handle,
                    None => initial_segment_handle(
                        key,
                        &candidate.keyframes[partner_index],
                        edited.time_offset.is_zero().then_some(TimelineTime::ZERO),
                    )?,
                };
                let aligned = align_continuous_partner(edited, partner);
                if incoming {
                    candidate.keyframes[index].out_handle = Some(aligned);
                    candidate.keyframes[index].interpolation_to_next =
                        AutomationSegmentInterpolation::Bezier;
                } else {
                    candidate.keyframes[index].in_handle = Some(aligned);
                    candidate.keyframes[partner_index].interpolation_to_next =
                        AutomationSegmentInterpolation::Bezier;
                }
            }
        }
        candidate.normalize_constrained_tangents()?;
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Normalize numeric constraints for domain-owned channel admission.
    pub(crate) fn normalize_constrained_tangents(&mut self) -> Result<(), AutomationError> {
        for index in 0..self.keyframes.len() {
            if self.keyframes[index].tangent_mode == ExactAutomationTangentMode::Manual {
                continue;
            }
            if self.keyframes[index].tangent_mode == ExactAutomationTangentMode::Continuous {
                self.normalize_continuous_handles(index)?;
                continue;
            }
            let (incoming, outgoing) = self.constrained_handles(index)?;
            self.keyframes[index].in_handle = incoming;
            self.keyframes[index].out_handle = outgoing;
        }
        Ok(())
    }

    fn normalize_continuous_handles(&mut self, index: usize) -> Result<(), AutomationError> {
        let current = &self.keyframes[index];
        let previous = index.checked_sub(1).map(|position| &self.keyframes[position]);
        let next = self.keyframes.get(index + 1);
        let needs_incoming = previous.is_some() && current.in_handle.is_none();
        let needs_outgoing = next.is_some() && current.out_handle.is_none();
        let (mut incoming, mut outgoing) = (current.in_handle, current.out_handle);
        if (needs_incoming || needs_outgoing)
            && let Some(handle) = incoming
                .or(outgoing)
                .filter(|handle| handle.time_offset.is_zero() && handle.value_offset != 0.0)
        {
            let partner = ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: -handle.value_offset,
            };
            if needs_incoming {
                incoming = Some(partner);
            }
            if needs_outgoing {
                outgoing = Some(partner);
            }
        } else if needs_incoming || needs_outgoing {
            let authored = incoming.or(outgoing).filter(|handle| !handle.time_offset.is_zero());
            let slope = if let Some(handle) = authored {
                let span = handle.time_offset.to_f64();
                let slope = handle.value_offset / span;
                if slope.is_finite() {
                    ConstrainedSlope { value: slope, value_scale: 1.0 }
                } else {
                    ConstrainedSlope {
                        value: handle.value_offset.signum() / span,
                        value_scale: handle.value_offset.abs(),
                    }
                }
            } else {
                constrained_slope(
                    ExactAutomationTangentMode::Continuous,
                    previous,
                    current,
                    next,
                )?
            };
            if needs_incoming {
                incoming = previous
                    .map(|key| tangent_handle(key.time, current.time, slope, true))
                    .transpose()?;
            }
            if needs_outgoing {
                outgoing = next
                    .map(|key| tangent_handle(current.time, key.time, slope, false))
                    .transpose()?;
            }
        }
        incoming = match previous {
            Some(key) => incoming
                .map(|handle| {
                    bound_continuous_handle(handle, current.time.checked_sub(key.time)?, true)
                })
                .transpose()?,
            None => None,
        };
        outgoing = match next {
            Some(key) => outgoing
                .map(|handle| {
                    bound_continuous_handle(handle, key.time.checked_sub(current.time)?, false)
                })
                .transpose()?,
            None => None,
        };
        self.keyframes[index].in_handle = incoming;
        self.keyframes[index].out_handle = outgoing;
        Ok(())
    }

    /// Project initial paired controls onto their continuous constraint.
    pub(crate) fn initialize_continuous_constraint(&mut self, index: usize) {
        let key = &mut self.keyframes[index];
        if continuous_handles_are_collinear(key.in_handle, key.out_handle) {
            return;
        }
        let handles = [key.in_handle, key.out_handle];
        let scale = handles
            .iter()
            .flatten()
            .map(|handle| handle.value_offset.abs())
            .fold(0.0_f64, f64::max);
        let (sum, count) = handles
            .iter()
            .flatten()
            .filter(|handle| !handle.time_offset.is_zero())
            .fold((0.0, 0_u32), |(sum, count), handle| {
                (
                    sum + (handle.value_offset / scale) / handle.time_offset.to_f64(),
                    count + 1,
                )
            });
        let slope = if count == 0 {
            0.0
        } else {
            sum / f64::from(count)
        };
        for handle in [&mut key.in_handle, &mut key.out_handle].into_iter().flatten() {
            handle.value_offset = (slope * handle.time_offset.to_f64()) * scale;
        }
    }

    fn constrained_handles(
        &self,
        index: usize,
    ) -> Result<(Option<ExactBezierHandle>, Option<ExactBezierHandle>), AutomationError> {
        let current = &self.keyframes[index];
        let previous = index.checked_sub(1).map(|position| &self.keyframes[position]);
        let next = self.keyframes.get(index + 1);
        let slope = if current.tangent_mode == ExactAutomationTangentMode::Auto && index == 0 {
            endpoint_constrained_slope(current, next, self.keyframes.get(2), false)?
        } else if current.tangent_mode == ExactAutomationTangentMode::Auto
            && index + 1 == self.keyframes.len()
        {
            endpoint_constrained_slope(
                current,
                previous,
                index.checked_sub(2).map(|position| &self.keyframes[position]),
                true,
            )?
        } else {
            constrained_slope(current.tangent_mode, previous, current, next)?
        };
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
            if keyframe.tangent_mode == ExactAutomationTangentMode::Continuous {
                if (index > 0 && keyframe.in_handle.is_none())
                    || (index + 1 < self.keyframes.len() && keyframe.out_handle.is_none())
                    || !continuous_handles_are_collinear(keyframe.in_handle, keyframe.out_handle)
                {
                    return Err(AutomationError::InvalidConstrainedTangent);
                }
                if let Some(handle) = keyframe.in_handle
                    && let Some(previous) =
                        index.checked_sub(1).map(|position| &self.keyframes[position])
                {
                    validate_handle_time(handle, keyframe.time.checked_sub(previous.time)?, true)?;
                }
                if let Some(handle) = keyframe.out_handle
                    && let Some(next) = self.keyframes.get(index + 1)
                {
                    validate_handle_time(handle, next.time.checked_sub(keyframe.time)?, false)?;
                }
            } else {
                let (incoming, outgoing) = self.constrained_handles(index)?;
                if keyframe.in_handle != incoming || keyframe.out_handle != outgoing {
                    return Err(AutomationError::InvalidConstrainedTangent);
                }
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

    /// Resolve the same exact Bezier control used by the canonical segment evaluator.
    /// A non-Bezier segment or missing neighbor has no visible handle.
    pub fn keyframe_handle(
        &self,
        keyframe_id: KeyframeId,
        side: BezierHandleSide,
    ) -> Option<ExactBezierHandle> {
        let index = self.keyframes.iter().position(|key| key.id == keyframe_id)?;
        self.keyframe_control(index, side).map(|control| control.handle)
    }

    /// Visit canonical controls in key-time order, incoming before outgoing.
    /// Resolves each neighbor once per side without repeated identity searches.
    pub fn keyframe_controls(&self) -> impl Iterator<Item = TemporalBezierControl> + '_ {
        (0..self.keyframes.len()).flat_map(move |index| {
            [BezierHandleSide::Incoming, BezierHandleSide::Outgoing]
                .into_iter()
                .filter_map(move |side| self.keyframe_control(index, side))
        })
    }

    fn keyframe_control(
        &self,
        index: usize,
        side: BezierHandleSide,
    ) -> Option<TemporalBezierControl> {
        let key = self.keyframes.get(index)?;
        let (neighbor, stored, interpolation) = match side {
            BezierHandleSide::Incoming => {
                let neighbor = self.keyframes.get(index.checked_sub(1)?)?;
                (neighbor, key.in_handle, neighbor.interpolation_to_next)
            }
            BezierHandleSide::Outgoing => (
                self.keyframes.get(index + 1)?,
                key.out_handle,
                key.interpolation_to_next,
            ),
        };
        if interpolation != AutomationSegmentInterpolation::Bezier {
            return None;
        }
        let handle = stored.or_else(|| initial_segment_handle(key, neighbor, None).ok())?;
        Some(TemporalBezierControl {
            keyframe_id: key.id,
            key_time: key.time,
            key_value: key.value,
            neighbor_time: neighbor.time,
            side,
            handle,
        })
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
    /// A batch key edit must address at least one key.
    #[error("automation key selection is empty")]
    EmptyKeySelection,
    /// A common increment or selected values violate the supplied numeric contract.
    #[error("automation value increment is invalid for its numeric contract")]
    InvalidValueIncrement,
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
    /// The addressed temporal side has no neighboring keyframe.
    #[error("automation handle has no corresponding segment")]
    MissingBezierSegment,
    /// A handle edit must specify at least one changed coordinate.
    #[error("automation handle edit has no coordinates")]
    EmptyHandleEdit,
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

fn initial_segment_handle(
    current: &ExactAutomationKeyframe,
    neighbor: &ExactAutomationKeyframe,
    time_offset: Option<TimelineTime>,
) -> Result<ExactBezierHandle, AutomationError> {
    Ok(ExactBezierHandle {
        time_offset: match time_offset {
            Some(offset) => offset,
            None => {
                neighbor.time.checked_sub(current.time)?.checked_scale(TimeScale::new(1, 3)?)?
            }
        },
        value_offset: crate::automation::interpolate_numeric_value(
            current.value,
            neighbor.value,
            1.0 / 3.0,
        ) - current.value,
    })
}

fn validate_handle_time(
    handle: ExactBezierHandle,
    duration: TimelineTime,
    incoming: bool,
) -> Result<(), AutomationError> {
    let magnitude = if incoming {
        handle.time_offset.checked_scale(TimeScale::NEGATIVE_ONE)?
    } else {
        handle.time_offset
    };
    if magnitude.is_negative() || magnitude > duration {
        return Err(AutomationError::InvalidBezierTimeHandle);
    }
    Ok(())
}

/// Align the opposite owner-space handle while retaining its independent length.
pub(crate) fn align_continuous_partner(
    edited: ExactBezierHandle,
    partner: ExactBezierHandle,
) -> ExactBezierHandle {
    if !edited.time_offset.is_zero() {
        ExactBezierHandle {
            time_offset: partner.time_offset,
            value_offset: edited.value_offset
                * (partner.time_offset.to_f64() / edited.time_offset.to_f64()),
        }
    } else if edited.value_offset != 0.0 {
        let length = if partner.value_offset == 0.0 {
            edited.value_offset.abs()
        } else {
            partner.value_offset.abs()
        };
        ExactBezierHandle {
            time_offset: TimelineTime::ZERO,
            value_offset: -edited.value_offset.signum() * length,
        }
    } else {
        partner
    }
}

/// Shorten a retained numeric control to its new owner span without rotating it.
pub(crate) fn bound_continuous_handle(
    handle: ExactBezierHandle,
    duration: TimelineTime,
    incoming: bool,
) -> Result<ExactBezierHandle, AutomationError> {
    let magnitude = if incoming {
        handle.time_offset.checked_scale(TimeScale::NEGATIVE_ONE)?
    } else {
        handle.time_offset
    };
    if magnitude <= duration {
        return Ok(handle);
    }
    let time_offset = if incoming {
        duration.checked_scale(TimeScale::NEGATIVE_ONE)?
    } else {
        duration
    };
    Ok(ExactBezierHandle {
        time_offset,
        value_offset: handle.value_offset * (duration.to_f64() / magnitude.to_f64()),
    })
}

fn continuous_handles_are_collinear(
    incoming: Option<ExactBezierHandle>,
    outgoing: Option<ExactBezierHandle>,
) -> bool {
    let (Some(incoming), Some(outgoing)) = (incoming, outgoing) else {
        return true;
    };
    let incoming_time = incoming.time_offset.to_f64();
    let outgoing_time = outgoing.time_offset.to_f64();
    let time_scale = incoming_time.abs().max(outgoing_time.abs());
    let value_scale = incoming.value_offset.abs().max(outgoing.value_offset.abs());
    if value_scale == 0.0 {
        return true;
    }
    let incoming_value = incoming.value_offset / value_scale;
    let outgoing_value = outgoing.value_offset / value_scale;
    if time_scale == 0.0 {
        return incoming_value * outgoing_value <= 0.0;
    }
    let incoming_time = incoming_time / time_scale;
    let outgoing_time = outgoing_time / time_scale;
    let left = incoming_time * outgoing_value;
    let right = outgoing_time * incoming_value;
    let tolerance = 64.0 * f64::EPSILON * left.abs().max(right.abs());
    (left - right).abs() <= tolerance
        && incoming_time * outgoing_time + incoming_value * outgoing_value <= 0.0
}

fn validate_keyframe(keyframe: &ExactAutomationKeyframe) -> Result<(), AutomationError> {
    if !keyframe.value.is_finite()
        || keyframe.in_handle.is_some_and(|handle| {
            !handle.value_offset.is_finite() || !(keyframe.value + handle.value_offset).is_finite()
        })
        || keyframe.out_handle.is_some_and(|handle| {
            !handle.value_offset.is_finite() || !(keyframe.value + handle.value_offset).is_finite()
        })
    {
        return Err(AutomationError::NonFiniteValue);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct ConstrainedSlope {
    value: f64,
    value_scale: f64,
}

fn endpoint_constrained_slope(
    current: &ExactAutomationKeyframe,
    adjacent: Option<&ExactAutomationKeyframe>,
    second: Option<&ExactAutomationKeyframe>,
    incoming: bool,
) -> Result<ConstrainedSlope, AutomationError> {
    let Some(adjacent) = adjacent else {
        return Ok(ConstrainedSlope { value: 0.0, value_scale: 1.0 });
    };
    let primary_span = (if incoming {
        current.time.checked_sub(adjacent.time)?
    } else {
        adjacent.time.checked_sub(current.time)?
    })
    .to_f64();
    let secondary_span = second
        .map(|key| {
            if incoming {
                adjacent.time.checked_sub(key.time)
            } else {
                key.time.checked_sub(adjacent.time)
            }
        })
        .transpose()?
        .map(|span| span.to_f64());
    let slopes = |scale| {
        let difference =
            |later, earlier| crate::automation::scaled_numeric_difference(later, earlier, scale);
        let primary = (if incoming {
            difference(current.value, adjacent.value)
        } else {
            difference(adjacent.value, current.value)
        }) / primary_span;
        let secondary = second.zip(secondary_span).map(|(key, span)| {
            (if incoming {
                difference(adjacent.value, key.value)
            } else {
                difference(key.value, adjacent.value)
            }) / span
        });
        (primary, secondary)
    };
    let (primary, secondary) = slopes(1.0);
    let value =
        crate::automation::endpoint_auto_slope(primary, secondary, primary_span, secondary_span);
    if primary.is_finite() && secondary.is_none_or(f64::is_finite) && value.is_finite() {
        return Ok(ConstrainedSlope { value, value_scale: 1.0 });
    }
    let value_scale = std::iter::once(current)
        .chain(std::iter::once(adjacent))
        .chain(second)
        .map(|key| key.value.abs())
        .fold(0.0_f64, f64::max);
    let value_scale = if value_scale == 0.0 { 1.0 } else { value_scale };
    let (primary, secondary) = slopes(value_scale);
    let value =
        crate::automation::endpoint_auto_slope(primary, secondary, primary_span, secondary_span);
    if !value.is_finite() {
        return Err(AutomationError::NonFiniteValue);
    }
    Ok(ConstrainedSlope { value, value_scale })
}

fn constrained_slope(
    mode: ExactAutomationTangentMode,
    previous: Option<&ExactAutomationKeyframe>,
    current: &ExactAutomationKeyframe,
    next: Option<&ExactAutomationKeyframe>,
) -> Result<ConstrainedSlope, AutomationError> {
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
    let slope = resolve_constrained_slope(mode, incoming, outgoing);
    if slope.is_finite() {
        return Ok(ConstrainedSlope { value: slope, value_scale: 1.0 });
    }
    let value_scale = previous
        .into_iter()
        .chain(std::iter::once(current))
        .chain(next)
        .map(|key| key.value.abs())
        .fold(0.0_f64, f64::max);
    let value_scale = if value_scale == 0.0 { 1.0 } else { value_scale };
    let incoming = previous.zip(incoming).map(|(key, (_, span))| {
        (
            crate::automation::scaled_numeric_difference(current.value, key.value, value_scale)
                / span,
            span,
        )
    });
    let outgoing = next.zip(outgoing).map(|(key, (_, span))| {
        (
            crate::automation::scaled_numeric_difference(key.value, current.value, value_scale)
                / span,
            span,
        )
    });
    let value = resolve_constrained_slope(mode, incoming, outgoing);
    if !value.is_finite() {
        return Err(AutomationError::NonFiniteValue);
    }
    Ok(ConstrainedSlope { value, value_scale })
}

fn resolve_constrained_slope(
    mode: ExactAutomationTangentMode,
    incoming: Option<(f64, f64)>,
    outgoing: Option<(f64, f64)>,
) -> f64 {
    match (incoming, outgoing) {
        (Some((left, left_span)), Some((right, right_span))) => match mode {
            ExactAutomationTangentMode::Auto => {
                crate::automation::monotone_harmonic_slope(left, right, left_span, right_span)
            }
            ExactAutomationTangentMode::Continuous => {
                (left * right_span + right * left_span) / (left_span + right_span)
            }
            ExactAutomationTangentMode::Manual => 0.0,
        },
        (Some((slope, _)), None) | (None, Some((slope, _))) => slope,
        (None, None) => 0.0,
    }
}

fn tangent_handle(
    left: TimelineTime,
    right: TimelineTime,
    slope: ConstrainedSlope,
    incoming: bool,
) -> Result<ExactBezierHandle, AutomationError> {
    let duration = right.checked_sub(left)?;
    let fraction = if incoming {
        TimeScale::new(-1, 3)?
    } else {
        TimeScale::new(1, 3)?
    };
    let time_offset = duration.checked_scale(fraction)?;
    let value_offset = (slope.value * time_offset.to_f64()) * slope.value_scale;
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
    Ok(crate::automation::interpolate_numeric_value(
        left.value,
        right.value,
        elapsed / duration,
    ))
}

fn bezier_value(
    left: &ExactAutomationKeyframe,
    right: &ExactAutomationKeyframe,
    time: TimelineTime,
) -> Result<f64, AutomationError> {
    if left.out_handle.is_none() && right.in_handle.is_none() {
        return linear_value(left, right, time);
    }
    let duration = right.time.checked_sub(left.time)?;
    let y1 = left.out_handle.map_or_else(
        || crate::automation::interpolate_numeric_value(left.value, right.value, 1.0 / 3.0),
        |handle| left.value + handle.value_offset,
    );
    let y2 = right.in_handle.map_or_else(
        || crate::automation::interpolate_numeric_value(left.value, right.value, 2.0 / 3.0),
        |handle| right.value + handle.value_offset,
    );
    if !y1.is_finite() || !y2.is_finite() {
        return Err(AutomationError::NonFiniteValue);
    }
    let span = duration.to_f64();
    let x1 = left.out_handle.map_or(1.0 / 3.0, |handle| handle.time_offset.to_f64() / span);
    let x2 = right.in_handle.map_or(1.0 - 1.0 / 3.0, |handle| {
        1.0 + handle.time_offset.to_f64() / span
    });
    let target = time.checked_sub(left.time)?.to_f64() / span;
    let outgoing_third = TimeScale::new(3, 1)?;
    let incoming_third = TimeScale::new(-3, 1)?;
    let uniform_time = left.out_handle.is_none_or(|handle| {
        handle
            .time_offset
            .checked_scale(outgoing_third)
            .is_ok_and(|offset| offset == duration)
    }) && right.in_handle.is_none_or(|handle| {
        handle
            .time_offset
            .checked_scale(incoming_third)
            .is_ok_and(|offset| offset == duration)
    });
    Ok(crate::numeric_bezier::evaluate_numeric_bezier(
        target,
        [x1, x2],
        [left.value, y1, y2, right.value],
        uniform_time,
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn batch_key_removal_rejects_whole_selection_and_retains_original_final_value() {
        for mode in [
            ExactAutomationTangentMode::Auto,
            ExactAutomationTangentMode::Continuous,
        ] {
            let mut curve =
                ExactAutomationCurve::new(ParameterId::new_static("test.batch.remove"), 0.0)
                    .expect("curve");
            for index in 0..5 {
                let mut key = ExactAutomationKeyframe::linear(
                    TimelineTime::new(index, 1).expect("time"),
                    (index % 3) as f64,
                );
                key.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
                key.tangent_mode = mode;
                curve.set_keyframe(key).expect("seed");
            }
            let original = curve.clone();
            let ids = curve.keyframes.iter().map(|key| key.id).collect::<Vec<_>>();
            for invalid in [
                vec![],
                vec![ids[1], ids[1]],
                vec![ids[1], KeyframeId::new()],
            ] {
                assert!(curve.remove_keyframes(&invalid, TimelineTime::ONE).is_err());
                assert_eq!(
                    curve, original,
                    "invalid batch cannot delete even its valid first key"
                );
            }
            let retain_time = TimelineTime::new(3, 2).expect("sample");
            let retained = original.evaluate(retain_time).expect("original evaluation");
            curve.remove_keyframes(&[ids[3], ids[1]], retain_time).expect("batch");
            assert_eq!(
                curve.keyframes.iter().map(|key| key.id).collect::<Vec<_>>(),
                [ids[0], ids[2], ids[4]]
            );
            curve.validate().expect("surviving constrained geometry");
            let mut reverse = original.clone();
            reverse
                .remove_keyframes(&[ids[1], ids[3]], retain_time)
                .expect("same set reversed");
            assert_eq!(
                curve, reverse,
                "selection order cannot change constrained geometry"
            );
            curve = original;
            curve.remove_keyframes(&ids, retain_time).expect("remove all");
            assert!(curve.keyframes.is_empty());
            assert_eq!(curve.default_value, retained);
            assert_eq!(
                curve.evaluate(TimelineTime::new(100, 1).expect("later")).expect("unkeyed"),
                retained
            );
        }
    }

    #[test]
    fn ordered_audio_controls_preserve_defaults_stored_handles_and_hold_segments() {
        use super::*;
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.ordered.controls"), 0.0)
                .expect("curve");
        assert_eq!(curve.keyframe_controls().count(), 0);
        let stored = ExactBezierHandle {
            time_offset: TimelineTime::new(1, 6).expect("time"),
            value_offset: 0.2,
        };
        let mut first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        first.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        first.out_handle = Some(stored);
        let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 1.0);
        middle.interpolation_to_next = AutomationSegmentInterpolation::Hold;
        let last = ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last"), 2.0);
        curve.keyframes = vec![first, middle, last].into();
        curve.validate().expect("legal mixed segments");
        let before = curve.clone();
        let controls = curve.keyframe_controls().collect::<Vec<_>>();
        assert_eq!(controls.len(), 2);
        assert_eq!(controls[0].handle, stored);
        assert_eq!(controls[0].keyframe_id, curve.keyframes[0].id);
        assert_eq!(controls[0].neighbor_time, TimelineTime::ONE);
        assert_eq!(controls[1].side, BezierHandleSide::Incoming);
        assert_eq!(controls[1].key_time, TimelineTime::ONE);
        assert_eq!(controls[1].key_value, 1.0);
        assert_eq!(controls[1].neighbor_time, TimelineTime::ZERO);
        assert_eq!(
            controls[1].handle.time_offset,
            TimelineTime::NEGATIVE_ONE_THIRD
        );
        // The canonical default first interpolates its world control, then subtracts the owner.
        assert_eq!(controls[1].handle.value_offset, (1.0 - 1.0 / 3.0) - 1.0);
        assert_eq!(
            curve, before,
            "read projections never normalize or mutate author state"
        );
    }

    use super::*;

    #[test]
    fn sparse_handle_coordinate_codec_distinguishes_omission_from_invalid_input() {
        for edit in [
            ExactBezierHandleEdit { time_offset: None, value_offset: None },
            ExactBezierHandleEdit {
                time_offset: Some(TimelineTime::ZERO),
                value_offset: None,
            },
            ExactBezierHandleEdit { time_offset: None, value_offset: Some(0.25) },
            ExactBezierHandleEdit {
                time_offset: Some(TimelineTime::ONE),
                value_offset: Some(-0.25),
            },
        ] {
            let encoded = serde_json::to_value(edit).expect("encode sparse edit");
            assert_eq!(
                encoded.get("time_offset").is_some(),
                edit.time_offset.is_some()
            );
            assert_eq!(
                encoded.get("value_offset").is_some(),
                edit.value_offset.is_some()
            );
            assert_eq!(
                serde_json::from_value::<ExactBezierHandleEdit>(encoded).expect("decode"),
                edit
            );
        }
        for encoded in [
            serde_json::json!({ "time_offset": null, "value_offset": 0.25 }),
            serde_json::json!({ "value_offset": null }),
            serde_json::json!({ "value_offset": "0.25" }),
        ] {
            assert!(serde_json::from_value::<ExactBezierHandleEdit>(encoded).is_err());
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let edit = ExactBezierHandleEdit {
                time_offset: Some(TimelineTime::ONE),
                value_offset: Some(invalid),
            };
            let encoded = serde_json::to_value(edit).expect("encode illegal request");
            assert!(serde_json::from_value::<ExactBezierHandleEdit>(encoded).is_err());
        }
    }

    #[test]
    fn full_finite_linear_automation_retains_convex_values() {
        for reverse in [false, true] {
            let (left, right) = if reverse {
                (f64::MAX, -f64::MAX)
            } else {
                (-f64::MAX, f64::MAX)
            };
            let mut curve =
                ExactAutomationCurve::new(ParameterId::new_static("test.fullrange.linear"), 0.0)
                    .expect("curve");
            curve
                .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, left))
                .expect("left key");
            curve
                .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ONE, right))
                .expect("right key");
            let segments = curve.prepared_segments().expect("prepared curve");
            for (numerator, expected) in [
                (0, left),
                (1, left * 0.5),
                (2, 0.0),
                (3, right * 0.5),
                (4, right),
            ] {
                let time = TimelineTime::new(numerator, 4).expect("exact sample time");
                assert_eq!(
                    curve.evaluate(time).expect("curve evaluation"),
                    expected,
                    "finite convex author values must not overflow during evaluation"
                );
                assert_eq!(
                    segments[0].evaluate(time).expect("prepared evaluation"),
                    expected
                );
            }
        }
    }

    fn assert_full_finite_bezier_automation(mode: InterpolationType) {
        for reverse in [false, true] {
            let (left, right) = if reverse {
                (f64::MAX, -f64::MAX)
            } else {
                (-f64::MAX, f64::MAX)
            };
            let mut curve =
                ExactAutomationCurve::new(ParameterId::new_static("test.fullrange.bezier"), 0.0)
                    .expect("curve");
            let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, left);
            let first_id = first.id;
            let last = ExactAutomationKeyframe::linear(TimelineTime::ONE, right);
            let last_id = last.id;
            curve.set_keyframe(first).expect("left key");
            curve.set_keyframe(last).expect("right key");
            for id in [first_id, last_id] {
                curve.set_keyframe_interpolation(id, mode).expect("finite control points must admit the constrained mode even when the absolute slope overflows");
            }
            let segments = curve.prepared_segments().expect("prepared curve");
            for numerator in 0..=16 {
                let time = TimelineTime::new(numerator, 16).expect("exact sample time");
                let ratio = numerator as f64 / 16.0;
                let expected_normalized = if reverse {
                    1.0 - 2.0 * ratio
                } else {
                    -1.0 + 2.0 * ratio
                };
                let evaluated = curve.evaluate(time).expect("curve evaluation");
                assert!(
                    evaluated.is_finite(),
                    "{mode:?}: a finite convex curve must produce finite values"
                );
                assert!(
                    (evaluated / f64::MAX - expected_normalized).abs() <= 64.0 * f64::EPSILON,
                    "{mode:?}: expected normalized value {expected_normalized}, got {}",
                    evaluated / f64::MAX
                );
                assert_eq!(
                    segments[0].evaluate(time).expect("prepared evaluation"),
                    evaluated
                );
            }
        }
    }

    #[test]
    fn full_finite_bezier_automation_retains_convex_values() {
        assert_full_finite_bezier_automation(InterpolationType::Bezier);
    }

    #[test]
    fn full_finite_auto_bezier_automation_retains_convex_values() {
        assert_full_finite_bezier_automation(InterpolationType::AutoBezier);
    }

    #[test]
    fn full_finite_continuous_bezier_automation_retains_convex_values() {
        assert_full_finite_bezier_automation(InterpolationType::ContinuousBezier);
    }

    #[test]
    fn unrepresentable_bezier_control_point_is_rejected_atomically() {
        for sign in [-1.0, 1.0] {
            let mut curve =
                ExactAutomationCurve::new(ParameterId::new_static("test.invalid.control"), 0.0)
                    .expect("curve");
            let before = curve.clone();
            let mut key = ExactAutomationKeyframe::linear(TimelineTime::ZERO, sign * f64::MAX);
            key.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
            key.out_handle = Some(ExactBezierHandle {
                time_offset: TimelineTime::ONE_THIRD,
                value_offset: sign * f64::MAX,
            });
            assert_eq!(
                curve.set_keyframe(key),
                Err(AutomationError::NonFiniteValue),
                "a finite offset must not admit an infinite control point"
            );
            assert_eq!(
                curve, before,
                "failed admission must preserve exact author state"
            );
        }
    }

    #[test]
    fn subnormal_auto_automation_preserves_scale_invariance() {
        let scale = f64::MIN_POSITIVE;
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.subnormal.auto"), 0.0)
                .expect("curve");
        let mut ids = Vec::new();
        for (time, value) in [
            (TimelineTime::ZERO, 0.0),
            (TimelineTime::ONE, scale * 0.5),
            (TimelineTime::new(2, 1).expect("last key time"), scale),
        ] {
            let key = ExactAutomationKeyframe::linear(time, value);
            ids.push(key.id);
            curve.set_keyframe(key).expect("canonical key mutation");
        }
        for id in ids {
            curve
                .set_keyframe_interpolation(id, InterpolationType::AutoBezier)
                .expect("automatic interpolation");
        }
        for numerator in 0..=16 {
            let time = TimelineTime::new(numerator, 8).expect("exact sample time");
            let evaluated = curve.evaluate(time).expect("canonical curve evaluation");
            let expected = numerator as f64 / 16.0;
            assert!((evaluated / scale - expected).abs() <= 64.0 * f64::EPSILON, "an overflowing reciprocal must not flatten a representable subnormal tangent: expected {expected}, got {}", evaluated / scale);
        }
    }

    #[test]
    fn auto_endpoints_retain_nonuniform_neighbor_shape_at_full_value_scale() {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new_static("test.numeric.auto_endpoint_shape"),
            0.0,
        )
        .expect("curve");
        let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, -f64::MAX);
        for key in [
            first.clone(),
            ExactAutomationKeyframe::linear(
                TimelineTime::new(2, 1).expect("second"),
                -0.1 * f64::MAX,
            ),
            ExactAutomationKeyframe::linear(
                TimelineTime::new(3, 1).expect("third"),
                0.8 * f64::MAX,
            ),
        ] {
            curve.set_keyframe(key).expect("finite key");
        }
        curve
            .set_keyframe_interpolation(first.id, InterpolationType::AutoBezier)
            .expect("auto");
        let handle = curve.keyframes[0].out_handle.expect("outgoing control");
        assert_eq!(
            handle.time_offset,
            TimelineTime::new(2, 3).expect("one third span")
        );
        // Secants are .45*MAX and .9*MAX over spans 2 and 1. The one-sided
        // shape-preserving derivative is (5*.45 - 2*.9)/3 = .15*MAX.
        // Multiplying by the 2/3-second handle length yields .1*MAX.
        assert!((handle.value_offset / f64::MAX - 0.1).abs() <= 8.0 * f64::EPSILON);
        curve.validate().expect("admitted constraints");
    }

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
    fn continuous_tangent_extraction_bounds_length_without_changing_direction() {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new_static("test.audio.continuous.extract"),
            0.0,
        )
        .expect("curve");
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        for key in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
            middle.clone(),
            ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
        ] {
            curve.set_keyframe(key).expect("key");
        }
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::ContinuousBezier)
            .expect("mode");
        let mut edited = curve.keyframes[1].clone();
        edited.in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 5).expect("in length"),
            value_offset: -0.8,
        });
        edited.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(2, 5).expect("out length"),
            value_offset: 1.6,
        });
        curve.set_keyframe(edited.clone()).expect("authored handles");
        curve
            .extract_time_range(
                TimelineTimeRange::new(
                    TimelineTime::new(11, 10).expect("cut start"),
                    TimelineTime::new(19, 10).expect("cut duration"),
                )
                .expect("cut range"),
            )
            .expect("atomic extraction");
        let actual = &curve.keyframes[1];
        assert_eq!(actual.id, middle.id);
        assert_eq!(actual.in_handle, edited.in_handle);
        assert_eq!(
            actual.out_handle,
            Some(ExactBezierHandle {
                time_offset: TimelineTime::new(1, 10).expect("bounded length"),
                value_offset: 0.4,
            })
        );
        assert_eq!(curve.validate(), Ok(()));
        let encoded = serde_json::to_string(&curve).expect("encode");
        let restored: ExactAutomationCurve =
            serde_json::from_str(&encoded).expect("current schema");
        assert_eq!(restored, curve);
        assert_eq!(restored.validate(), Ok(()));
        for numerator in 0..=22 {
            let time = TimelineTime::new(numerator, 20).expect("sample");
            assert_eq!(restored.evaluate(time), curve.evaluate(time));
        }
    }

    #[test]
    fn handle_evaluation_uses_derived_default_coordinates_in_tiny_exact_spans() {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.audio.handle.evaluation"), 0.0)
                .expect("curve");
        let mut first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        first.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        first.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::ZERO,
            value_offset: 0.25,
        });
        curve.set_keyframe(first).expect("first");
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(2, i64::MAX).expect("span"),
                1.0,
            ))
            .expect("last");
        let value = curve.evaluate(TimelineTime::new(1, i64::MAX).expect("half span")).expect(
            "Evaluation defaults are derived coordinates, not new persisted rational author values",
        );
        assert!(value > 0.5 && value < 1.0);
    }

    #[test]
    fn explicit_handle_edit_needs_no_unrepresentable_default_time() {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.audio.handle.tiny"), 0.0)
                .expect("curve");
        let first = ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0);
        let id = first.id;
        curve.set_keyframe(first).expect("first");
        curve
            .set_keyframe(ExactAutomationKeyframe::linear(
                TimelineTime::new(1, i64::MAX).expect("exact tiny span"),
                1.0,
            ))
            .expect("last");
        assert_eq!(
            curve.edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::ZERO),
                    value_offset: Some(0.25),
                }
            ),
            Ok(()),
            "Explicit admitted coordinates must not compute an unused default third of the span"
        );
        assert_eq!(
            curve.keyframes[0].out_handle,
            Some(ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: 0.25
            })
        );
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn continuous_handle_direction_is_validated_even_with_linear_predecessor() {
        let (mut curve, id) = handle_edit_curve();
        curve
            .set_keyframe_interpolation(id, InterpolationType::ContinuousBezier)
            .expect("continuous");
        curve.keyframes[0].interpolation_to_next = AutomationSegmentInterpolation::Linear;
        curve.keyframes[1].in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(1, 3).expect("wrong side"),
            value_offset: 0.0,
        });
        curve.keyframes[1].out_handle.as_mut().expect("outgoing").value_offset = 0.0;
        assert_eq!(curve.validate(), Err(AutomationError::InvalidBezierTimeHandle), "Stored constrained handles must retain their temporal side independently of the predecessor's active interpolation");
    }

    #[test]
    fn handle_edit_preserves_axes_and_converts_auto_to_continuous() {
        let (mut curve, id) = handle_edit_curve();
        let original = curve.keyframes[1].clone();
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::new(2, 5).expect("length")),
                    value_offset: Some(1.6),
                },
            )
            .expect("handle edit");
        let actual = &curve.keyframes[1];
        assert_eq!(actual.tangent_mode, ExactAutomationTangentMode::Continuous);
        assert_eq!(actual.id, original.id);
        assert_eq!(actual.time, original.time);
        assert_eq!(actual.value, original.value);
        assert_eq!(
            actual.in_handle.expect("partner").time_offset,
            original.in_handle.expect("original partner").time_offset
        );
        assert!(
            (actual.in_handle.expect("partner").value_offset + 4.0 / 3.0).abs()
                <= 8.0 * f64::EPSILON
        );
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit { time_offset: None, value_offset: Some(0.8) },
            )
            .expect("vertical-only drag");
        assert_eq!(
            curve.keyframes[1].out_handle.expect("edited").time_offset,
            TimelineTime::new(2, 5).expect("exact length")
        );
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::new(1, 5).expect("length")),
                    value_offset: None,
                },
            )
            .expect("horizontal-only drag");
        assert_eq!(
            curve.keyframes[1].out_handle.expect("edited").value_offset,
            0.8
        );
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn handle_edit_noop_keeps_auto_and_manual_edit_keeps_partner() {
        let (mut curve, id) = handle_edit_curve();
        let before = curve.clone();
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Incoming,
                ExactBezierHandleEdit {
                    time_offset: None,
                    value_offset: Some(
                        curve.keyframes[1].in_handle.expect("incoming").value_offset,
                    ),
                },
            )
            .expect("noop");
        assert_eq!(curve, before);
        curve.set_keyframe_interpolation(id, InterpolationType::Bezier).expect("manual");
        let partner = curve.keyframes[1].in_handle;
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit { time_offset: None, value_offset: Some(1.0) },
            )
            .expect("manual edit");
        assert_eq!(curve.keyframes[1].in_handle, partner);
        assert_eq!(
            curve.keyframes[1].tangent_mode,
            ExactAutomationTangentMode::Manual
        );
    }

    #[test]
    fn handle_edit_vertical_and_zero_vectors_roundtrip_without_nonfinite_values() {
        let (mut curve, id) = handle_edit_curve();
        let partner_length = curve.keyframes[1].in_handle.expect("partner").value_offset.abs();
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::ZERO),
                    value_offset: Some(1.0),
                },
            )
            .expect("vertical handle");
        assert_eq!(
            curve.keyframes[1].in_handle,
            Some(ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: -partner_length,
            })
        );
        let restored: ExactAutomationCurve =
            serde_json::from_str(&serde_json::to_string(&curve).expect("encode")).expect("decode");
        assert_eq!(restored, curve);
        for numerator in 0..=30 {
            assert!(restored
                .evaluate(TimelineTime::new(numerator, 10).expect("sample"))
                .expect("evaluation")
                .is_finite());
        }
        let partner = curve.keyframes[1].in_handle;
        curve
            .edit_keyframe_handle(
                id,
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit { time_offset: None, value_offset: Some(0.0) },
            )
            .expect("collapsed handle");
        assert_eq!(curve.keyframes[1].in_handle, partner);
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn handle_edit_invalid_requests_are_atomic() {
        let (mut curve, id) = handle_edit_curve();
        let before = curve.clone();
        for (side, edit, expected) in [
            (
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit { time_offset: None, value_offset: None },
                AutomationError::EmptyHandleEdit,
            ),
            (
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: None,
                    value_offset: Some(f64::INFINITY),
                },
                AutomationError::NonFiniteValue,
            ),
            (
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::new(-1, 5).expect("wrong side")),
                    value_offset: None,
                },
                AutomationError::InvalidBezierTimeHandle,
            ),
            (
                BezierHandleSide::Incoming,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::new(-2, 1).expect("outside")),
                    value_offset: None,
                },
                AutomationError::InvalidBezierTimeHandle,
            ),
            (
                BezierHandleSide::Outgoing,
                ExactBezierHandleEdit {
                    time_offset: Some(TimelineTime::new(1, i64::MAX).expect("tiny")),
                    value_offset: Some(f64::MAX),
                },
                AutomationError::NonFiniteValue,
            ),
        ] {
            assert_eq!(curve.edit_keyframe_handle(id, side, edit), Err(expected));
            assert_eq!(curve, before);
        }
        let edit = ExactBezierHandleEdit { time_offset: None, value_offset: Some(1.0) };
        assert_eq!(
            curve.edit_keyframe_handle(curve.keyframes[0].id, BezierHandleSide::Incoming, edit),
            Err(AutomationError::MissingBezierSegment)
        );
        assert_eq!(
            curve.edit_keyframe_handle(curve.keyframes[2].id, BezierHandleSide::Outgoing, edit),
            Err(AutomationError::MissingBezierSegment)
        );
        assert_eq!(
            curve.edit_keyframe_handle(KeyframeId::new(), BezierHandleSide::Outgoing, edit),
            Err(AutomationError::UnknownKeyframe)
        );
        assert_eq!(curve, before);
    }

    fn handle_edit_curve() -> (ExactAutomationCurve, KeyframeId) {
        let mut curve =
            ExactAutomationCurve::new(ParameterId::new_static("test.audio.handles"), 0.0)
                .expect("curve");
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        for key in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
            middle.clone(),
            ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
        ] {
            curve.set_keyframe(key).expect("key");
        }
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::AutoBezier)
            .expect("auto");
        (curve, middle.id)
    }

    #[test]
    fn continuous_vertical_tangent_missing_partner_retains_authored_direction() {
        for incoming in [false, true] {
            let mut curve = ExactAutomationCurve::new(
                ParameterId::new_static("test.audio.continuous.vertical"),
                0.0,
            )
            .expect("curve");
            let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
            middle.tangent_mode = ExactAutomationTangentMode::Continuous;
            middle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
            let handle = ExactBezierHandle {
                time_offset: TimelineTime::ZERO,
                value_offset: if incoming { -1.0 } else { 1.0 },
            };
            if incoming {
                middle.in_handle = Some(handle);
            } else {
                middle.out_handle = Some(handle);
            }
            for key in [
                ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
                ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
            ] {
                curve.set_keyframe(key).expect("neighbor");
            }
            assert_eq!(
                curve.set_keyframe(middle.clone()),
                Ok(()),
                "A legal authored vertical direction must not be replaced by neighboring secants"
            );
            let actual = &curve.keyframes[1];
            assert_eq!(
                actual.in_handle,
                Some(ExactBezierHandle {
                    time_offset: TimelineTime::ZERO,
                    value_offset: -1.0,
                })
            );
            assert_eq!(
                actual.out_handle,
                Some(ExactBezierHandle { time_offset: TimelineTime::ZERO, value_offset: 1.0 })
            );
            assert_eq!(curve.validate(), Ok(()));
            let encoded = serde_json::to_string(&curve).expect("encode");
            let restored: ExactAutomationCurve = serde_json::from_str(&encoded).expect("decode");
            assert_eq!(restored, curve);
            for numerator in 0..=30 {
                let time = TimelineTime::new(numerator, 10).expect("sample");
                let value = restored.evaluate(time).expect("evaluate vertical curve");
                assert!(value.is_finite());
                assert_eq!(value, curve.evaluate(time).expect("original evaluation"));
            }
        }
    }

    #[test]
    fn continuous_tangent_missing_partner_retains_authored_direction() {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new_static("test.audio.continuous.partner"),
            0.0,
        )
        .expect("curve");
        let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        middle.tangent_mode = ExactAutomationTangentMode::Continuous;
        middle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        middle.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(2, 5).expect("out length"),
            value_offset: 1.6,
        });
        for key in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
            ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
            middle.clone(),
        ] {
            curve.set_keyframe(key).expect("key");
        }
        let actual = &curve.keyframes[1];
        assert_eq!(actual.out_handle, middle.out_handle);
        let incoming = actual.in_handle.expect("filled partner");
        assert!(
            (incoming.value_offset / incoming.time_offset.to_f64() - 4.0).abs()
                <= 32.0 * f64::EPSILON
        );
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn continuous_mode_converts_broken_manual_tangents_without_losing_lengths() {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new_static("test.audio.continuous.convert"),
            0.0,
        )
        .expect("curve");
        let mut middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        middle.interpolation_to_next = AutomationSegmentInterpolation::Bezier;
        middle.in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 5).expect("in length"),
            value_offset: -0.4,
        });
        middle.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(2, 5).expect("out length"),
            value_offset: 1.6,
        });
        for key in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
            middle.clone(),
            ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
        ] {
            curve.set_keyframe(key).expect("key");
        }
        assert_eq!(
            curve.set_keyframe_interpolation(middle.id, InterpolationType::ContinuousBezier),
            Ok(()),
            "Mode conversion must establish the constraint on valid manual handles"
        );
        let actual = &curve.keyframes[1];
        assert_eq!(
            actual.in_handle.expect("incoming").time_offset,
            middle.in_handle.expect("authored incoming").time_offset
        );
        assert_eq!(
            actual.out_handle.expect("outgoing").time_offset,
            middle.out_handle.expect("authored outgoing").time_offset
        );
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn continuous_tangent_rejects_noncollinear_replacement_atomically() {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new_static("test.audio.continuous.invalid"),
            0.0,
        )
        .expect("curve");
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        for key in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
            middle.clone(),
            ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
        ] {
            curve.set_keyframe(key).expect("key");
        }
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::ContinuousBezier)
            .expect("mode");
        let before = curve.clone();
        let mut invalid = curve.keyframes[1].clone();
        invalid.out_handle.as_mut().expect("out").value_offset *= 2.0;
        assert_eq!(
            curve.set_keyframe(invalid),
            Err(AutomationError::InvalidConstrainedTangent)
        );
        assert_eq!(curve, before);
    }

    #[test]
    fn continuous_keyframe_requires_explicit_current_tangent_mode() {
        let key = ExactAutomationKeyframe::linear(TimelineTime::ONE, 1.0);
        let mut encoded = serde_json::to_value(key).expect("encode");
        encoded.as_object_mut().expect("object").remove("tangent_mode");
        assert!(serde_json::from_value::<ExactAutomationKeyframe>(encoded).is_err(),
            "Alpha project keys must express their current constraint rather than admit an old schema");
    }

    #[test]
    fn continuous_tangent_preserves_manually_authored_collinear_handles() {
        let mut curve = ExactAutomationCurve::new(
            ParameterId::new_static("test.audio.continuous.authored"),
            0.0,
        )
        .expect("curve");
        let middle = ExactAutomationKeyframe::linear(TimelineTime::ONE, 2.0);
        for key in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, 0.0),
            middle.clone(),
            ExactAutomationKeyframe::linear(TimelineTime::new(3, 1).expect("last time"), 3.0),
        ] {
            curve.set_keyframe(key).expect("key");
        }
        curve
            .set_keyframe_interpolation(middle.id, InterpolationType::ContinuousBezier)
            .expect("continuous mode");
        let mut edited = curve.keyframes[1].clone();
        edited.in_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(-1, 5).expect("incoming length"),
            value_offset: -0.8,
        });
        edited.out_handle = Some(ExactBezierHandle {
            time_offset: TimelineTime::new(2, 5).expect("outgoing length"),
            value_offset: 1.6,
        });
        curve.set_keyframe(edited.clone()).expect("valid collinear author edit");
        assert_eq!(curve.keyframes[1], edited,
            "Continuous mode must retain the author's slope and independent handle lengths rather than recalculate Auto tangents");
        assert_eq!(curve.validate(), Ok(()));
    }

    #[test]
    fn continuous_tangent_stays_collinear() {
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
