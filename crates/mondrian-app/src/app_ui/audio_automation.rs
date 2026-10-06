//! Product Adapter for exact owner-time audio automation.
//!
//! Timeline owns curve addresses, time domains, lock admission, and mutation.
//! This Module projects an explicit exact time viewport and absolute author value
//! coordinates and lowers one committed gesture back to a stable-ID edit.

use std::collections::BTreeMap;

use mondrian_core::{
    AuthoringTimeDomain, AutomationSegmentInterpolation, ExactAutomationKeyframe,
    ExactAutomationTangentMode, InterpolationType, ParameterInterpolation, PropertyValue,
    PropertyValueType, TimeScale, TimelineTime, TimelineTimeRange,
};
use mondrian_editor_state::Action;
use mondrian_timeline::{
    inspect_audio_automation, AudioAutomationEdit, AudioAutomationEditRequest,
    AudioAutomationTarget, Sequence,
};
use mondrian_ui_widgets::{CurveEdit, CurvePoint, CurvePointPolicy, CurvePointTarget};

use crate::app::ui_actions::audio_automation_edit_action;

const CURVE_QUANTIZATION_DENOMINATOR: u32 = 1_000_000;
const DISPLAY_SEGMENTS: i64 = 128;

/// Exact visible interval qualified by the curve owner's time domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioAutomationViewport {
    pub(crate) domain: AuthoringTimeDomain,
    pub(crate) range: TimelineTimeRange,
}

/// One authored key or virtual boundary that becomes a key when its value is edited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioAutomationCurveKeyModel {
    pub(crate) keyframe: Option<ExactAutomationKeyframe>,
    pub(crate) point: CurvePoint,
}

/// Current Sequence coordinate plus the selected shared-Scope occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioAutomationTimeContext {
    pub(crate) sequence_time: mondrian_core::DomainTime,
    pub(crate) scope_occurrence: Option<mondrian_timeline::AudioComponentAddress>,
}

impl AudioAutomationTimeContext {
    /// Explicit Sequence coordinate; shared Scopes must additionally name an occurrence.
    pub(crate) fn at_sequence_time(sequence: &Sequence, time: TimelineTime) -> Self {
        Self {
            sequence_time: mondrian_core::DomainTime::new(
                AuthoringTimeDomain::Sequence(sequence.id),
                time,
            ),
            scope_occurrence: None,
        }
    }
}

/// Absolute author values and normalized owner time consumed by CurveEditor.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioAutomationCurveModel {
    pub(crate) time_context: AudioAutomationTimeContext,
    pub(crate) current_author_time:
        Result<mondrian_core::DomainTime, mondrian_timeline::AudioAutomationTimeError>,
    pub(crate) frame_time_step: TimelineTime,
    pub(crate) source_curve: mondrian_core::ExactAutomationCurve,
    pub(crate) curve_contract: mondrian_timeline::AudioAutomationCurveContract,
    pub(crate) target: AudioAutomationTarget,
    pub(crate) viewport: AudioAutomationViewport,
    pub(crate) keys: Vec<AudioAutomationCurveKeyModel>,
    pub(crate) display_points: Vec<CurvePoint>,
    pub(crate) value_axis: mondrian_ui_widgets::CurveValueAxis,
    pub(crate) value_type: PropertyValueType,
    pub(crate) insert_interpolation: AutomationSegmentInterpolation,
    pub(crate) allowed_interpolations: Vec<ParameterInterpolation>,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
}

impl AudioAutomationCurveModel {
    pub(crate) fn key_points(&self) -> Vec<(mondrian_core::KeyframeId, CurvePoint)> {
        let Ok(end) = self.viewport.range.end() else {
            return Vec::new();
        };
        self.source_curve
            .keyframes
            .iter()
            .filter(|key| key.time >= self.viewport.range.start && key.time <= end)
            .filter_map(|key| {
                Some((
                    key.id,
                    crate::app_ui::curve_tangents::project_key(
                        key.time,
                        key.value,
                        self.viewport.range,
                    )?,
                ))
            })
            .collect()
    }

    pub(crate) fn is_automated(&self) -> bool {
        self.keys.iter().any(|key| key.keyframe.is_some())
    }

    pub(crate) fn tangents(&self) -> Vec<mondrian_ui_widgets::CurveTangent> {
        let end = match self.viewport.range.end() {
            Ok(end) => end,
            Err(_) => return Vec::new(),
        };
        self.source_curve
            .keyframe_controls()
            .filter(|control| {
                control.key_time >= self.viewport.range.start && control.key_time <= end
            })
            .filter_map(|control| {
                crate::app_ui::curve_tangents::project(
                    control.keyframe_id,
                    control.side,
                    (control.key_time, control.key_value),
                    control.neighbor_time,
                    control.handle,
                    self.viewport.range,
                )
            })
            .collect()
    }

    pub(crate) fn points(&self) -> Vec<CurvePoint> {
        self.keys.iter().map(|key| key.point).collect()
    }

    pub(crate) fn point_policies(&self) -> Vec<CurvePointPolicy> {
        self.keys
            .iter()
            .map(|key| {
                if key.keyframe.is_some() {
                    CurvePointPolicy::editable()
                } else {
                    CurvePointPolicy::anchor()
                }
            })
            .collect()
    }
}

/// Sequence-local viewport covering authored content, with a useful empty span.
pub(crate) fn sequence_automation_viewport(sequence: &Sequence) -> Option<AudioAutomationViewport> {
    let duration = sequence.total_duration().ok()?.max(TimelineTime::ONE);
    Some(AudioAutomationViewport {
        domain: AuthoringTimeDomain::Sequence(sequence.id),
        range: TimelineTimeRange::new(TimelineTime::ZERO, duration).ok()?,
    })
}

/// Scope-local viewport covering the union of every current binding.
///
/// A shared Processing Scope has no placement of its own. Showing the binding
/// union makes all currently audible authored time reachable without pretending
/// that one selected Clip owns the shared definition.
pub(crate) fn processing_scope_automation_viewport(
    sequence: &Sequence,
    scope_id: mondrian_core::AudioProcessingScopeId,
) -> Option<AudioAutomationViewport> {
    let mut start = None::<TimelineTime>;
    let mut end = None::<TimelineTime>;
    for clip in sequence.audio_tracks.iter().flat_map(|track| &track.clips) {
        for edit in &clip.audio_components {
            if edit.processing.scope_id != scope_id {
                continue;
            }
            let binding_start = edit.processing.scope_in;
            let binding_end = binding_start.checked_add(clip.duration).ok()?;
            start = Some(start.map_or(binding_start, |current| current.min(binding_start)));
            end = Some(end.map_or(binding_end, |current| current.max(binding_end)));
        }
    }
    let start = start.unwrap_or(TimelineTime::ZERO);
    let end = end.unwrap_or(TimelineTime::ONE);
    let duration = end.checked_sub(start).ok()?.max(TimelineTime::ONE);
    Some(AudioAutomationViewport {
        domain: AuthoringTimeDomain::AudioProcessingScope(scope_id),
        range: TimelineTimeRange::new(start, duration).ok()?,
    })
}

/// Component-local viewport covering the exact visible Clip binding.
pub(crate) fn component_automation_viewport(
    edit_id: mondrian_core::AudioComponentEditId,
    local_time_in: TimelineTime,
    clip_duration: TimelineTime,
) -> Option<AudioAutomationViewport> {
    let duration = clip_duration.max(TimelineTime::ONE);
    Some(AudioAutomationViewport {
        domain: AuthoringTimeDomain::AudioComponentEdit(edit_id),
        range: TimelineTimeRange::new(local_time_in, duration).ok()?,
    })
}

/// Project one Timeline-owned curve without clamping author values to the display range.
pub(crate) fn project_audio_automation(
    sequence: &Sequence,
    target: AudioAutomationTarget,
    viewport: AudioAutomationViewport,
    time_context: AudioAutomationTimeContext,
) -> Option<AudioAutomationCurveModel> {
    let inspection = inspect_audio_automation(sequence, &target).ok()?;
    if inspection.time_domain() != viewport.domain || viewport.range.duration.is_zero() {
        return None;
    }
    let contract = inspection.value_contract();
    let allowed = inspection.allowed_interpolations();
    let insert_interpolation = match allowed.first()? {
        ParameterInterpolation::Linear => AutomationSegmentInterpolation::Linear,
        ParameterInterpolation::Hold => AutomationSegmentInterpolation::Hold,
        ParameterInterpolation::Bezier => AutomationSegmentInterpolation::Bezier,
    };
    let value_axis = mondrian_ui_widgets::CurveValueAxis::new(
        contract.soft_range,
        contract.hard_range,
        if contract.value_type == PropertyValueType::Int {
            Some(contract.step.unwrap_or(1.0).round().max(1.0))
        } else {
            contract.step
        },
    )
    .ok()?;
    let range_end = viewport.range.end().ok()?;
    let author_value = |value: f64| value.is_finite().then_some(value);
    let normalized_time = |time: TimelineTime| {
        let elapsed = time.checked_sub(viewport.range.start).ok()?.to_f64();
        Some((elapsed / viewport.range.duration.to_f64()).clamp(0.0, 1.0) as f32)
    };
    let evaluate = |time| match inspection.curve() {
        Some(curve) => curve.evaluate(time).ok(),
        None => Some(inspection.default_value()),
    };

    let mut keys = BTreeMap::<TimelineTime, AudioAutomationCurveKeyModel>::new();
    keys.insert(
        viewport.range.start,
        AudioAutomationCurveKeyModel {
            keyframe: None,
            point: CurvePoint {
                x: 0.0,
                y: author_value(evaluate(viewport.range.start)?)?,
            },
        },
    );
    keys.insert(
        range_end,
        AudioAutomationCurveKeyModel {
            keyframe: None,
            point: CurvePoint { x: 1.0, y: author_value(evaluate(range_end)?)? },
        },
    );
    if let Some(curve) = inspection.curve() {
        for keyframe in &curve.keyframes {
            if keyframe.time < viewport.range.start || keyframe.time > range_end {
                continue;
            }
            keys.insert(
                keyframe.time,
                AudioAutomationCurveKeyModel {
                    keyframe: Some(keyframe.clone()),
                    point: CurvePoint {
                        x: normalized_time(keyframe.time)?,
                        y: author_value(keyframe.value)?,
                    },
                },
            );
        }
    }

    let display_points = (0..=DISPLAY_SEGMENTS)
        .map(|index| {
            let scale = TimeScale::new(index, DISPLAY_SEGMENTS).ok()?;
            let time = viewport
                .range
                .start
                .checked_add(viewport.range.duration.checked_scale(scale).ok()?)
                .ok()?;
            Some(CurvePoint {
                x: index as f32 / DISPLAY_SEGMENTS as f32,
                y: author_value(evaluate(time)?)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;

    Some(AudioAutomationCurveModel {
        time_context,
        current_author_time: mondrian_timeline::audio_automation_author_time(
            sequence,
            &target,
            time_context.sequence_time,
            time_context.scope_occurrence,
        ),
        frame_time_step: TimelineTime::from_frame_position(mondrian_core::FramePosition::new(
            1,
            sequence.time_base(),
        ))
        .ok()?,
        source_curve: match inspection.curve() {
            Some(curve) => curve.clone(),
            None => mondrian_core::ExactAutomationCurve::new(
                inspection.parameter_id().clone(),
                inspection.default_value(),
            )
            .ok()?,
        },
        curve_contract: inspection.curve_contract().clone(),
        target,
        viewport,
        keys: keys.into_values().collect(),
        display_points,
        value_axis,
        value_type: contract.value_type,
        insert_interpolation,
        allowed_interpolations: allowed.to_vec(),
        is_editable: inspection.is_editable(),
        edit_disabled_reason: inspection.edit_blocker().map(ToString::to_string),
    })
}

/// Lower a keyframe context-menu choice through the same stable target address.
pub(crate) fn audio_automation_interpolation_action(
    model: &AudioAutomationCurveModel,
    index: usize,
    interpolation: InterpolationType,
) -> Option<Action> {
    if !model.is_editable {
        return None;
    }
    let family = match interpolation {
        InterpolationType::Hold => ParameterInterpolation::Hold,
        InterpolationType::Linear => ParameterInterpolation::Linear,
        _ => ParameterInterpolation::Bezier,
    };
    if !model.allowed_interpolations.contains(&family) {
        return None;
    }
    let keyframe_id = model.keys.get(index)?.keyframe.as_ref()?.id;
    let edit = AudioAutomationEdit::SetInterpolation { keyframe_id, interpolation };
    model.curve_contract.preview_edit(&model.source_curve, &edit).ok()?;
    Some(audio_automation_edit_action(AudioAutomationEditRequest {
        target: model.target.clone(),
        edit,
    }))
}

pub(crate) fn audio_keyframe_interpolation(
    keyframe: &ExactAutomationKeyframe,
) -> InterpolationType {
    match keyframe.tangent_mode {
        ExactAutomationTangentMode::Auto => InterpolationType::AutoBezier,
        ExactAutomationTangentMode::Continuous => InterpolationType::ContinuousBezier,
        ExactAutomationTangentMode::Manual => match keyframe.interpolation_to_next {
            AutomationSegmentInterpolation::Hold => InterpolationType::Hold,
            AutomationSegmentInterpolation::Linear => InterpolationType::Linear,
            AutomationSegmentInterpolation::Bezier => InterpolationType::Bezier,
        },
    }
}

/// One definition-admitted author intent and its exact projected source.
pub(crate) struct PreparedAudioCurveEdit {
    /// Closed author action for this admitted candidate.
    pub(crate) action: Action,
    /// Exact candidate shared by UI geometry and author execution.
    pub(crate) curve: mondrian_core::ExactAutomationCurve,
}

/// Prepare one edit once before either author intent or preview geometry is exposed.
pub(crate) fn prepare_audio_curve_edit(
    model: &AudioAutomationCurveModel,
    edit: CurveEdit,
) -> Option<PreparedAudioCurveEdit> {
    let intent = lower_audio_curve_edit(model, edit)?;
    let curve = model.curve_contract.preview_edit(&model.source_curve, &intent).ok()?;
    if let AudioAutomationEdit::ShiftKeyframes { keyframe_ids, .. } = &intent {
        let end = model.viewport.range.end().ok()?;
        let identities = keyframe_ids.iter().copied().collect::<std::collections::HashSet<_>>();
        if curve
            .keyframes
            .iter()
            .filter(|key| identities.contains(&key.id))
            .any(|key| key.time < model.viewport.range.start || key.time > end)
        {
            return None;
        }
    }
    Some(PreparedAudioCurveEdit {
        action: audio_automation_edit_action(AudioAutomationEditRequest {
            target: model.target.clone(),
            edit: intent,
        }),
        curve,
    })
}

fn lower_audio_curve_edit(
    model: &AudioAutomationCurveModel,
    edit: CurveEdit,
) -> Option<AudioAutomationEdit> {
    if !model.is_editable {
        return None;
    }
    let edit = match edit {
        CurveEdit::NudgeValue { keyframe_id, delta, steps } => {
            let mut key =
                model.source_curve.keyframes.iter().find(|key| key.id == keyframe_id)?.clone();
            let range = model.value_axis.hard_range();
            let contract = mondrian_core::automation::ParameterNumericContract::closed(
                range.min,
                range.max,
                None,
                mondrian_core::automation::ParameterInvalidValuePolicy::Reject,
            )
            .ok()?;
            key.value = contract
                .offset_scalar(PropertyValue::Double(key.value), delta, steps)?
                .as_f64()?;
            AudioAutomationEdit::UpsertKeyframe { keyframe: key }
        }
        CurveEdit::NudgeValueMany { keyframe_ids, delta, steps } => {
            AudioAutomationEdit::OffsetKeyframeValues { keyframe_ids, delta, steps }
        }
        CurveEdit::NudgeTimeMany { keyframe_ids, delta } => {
            AudioAutomationEdit::ShiftKeyframes { keyframe_ids, delta }
        }
        CurveEdit::NudgeTime { keyframe_id, side, delta } => {
            if let Some(side) = side {
                let control = model
                    .source_curve
                    .keyframe_controls()
                    .find(|control| control.keyframe_id == keyframe_id && control.side == side)?;
                AudioAutomationEdit::EditHandle {
                    keyframe_id,
                    side,
                    edit: crate::app_ui::curve_tangents::nudge_handle_time(control, delta)?,
                }
            } else {
                let mut key =
                    model.source_curve.keyframes.iter().find(|key| key.id == keyframe_id)?.clone();
                key.time = crate::app_ui::curve_tangents::nudge_key_time(
                    key.time,
                    delta,
                    model.viewport.range,
                    model.source_curve.keyframes.iter().map(|key| key.time),
                )?;
                AudioAutomationEdit::UpsertKeyframe { keyframe: key }
            }
        }
        CurveEdit::Handle { keyframe_id: id, side, point } => {
            let key = model.source_curve.keyframes.iter().find(|key| key.id == id)?;
            let handle = model.source_curve.keyframe_handle(id, side)?;
            let tangent = model
                .tangents()
                .into_iter()
                .find(|tangent| tangent.keyframe_id == id && tangent.side == side)?;
            let edit = crate::app_ui::curve_tangents::lower(
                tangent,
                point,
                (key.time, key.value),
                handle,
                model.viewport.range,
            )?;
            AudioAutomationEdit::EditHandle { keyframe_id: id, side, edit }
        }
        CurveEdit::Insert { keyframe_id, point, .. } => {
            let time = denormalized_time(model.viewport.range, point.x)?;
            let value = deauthor_value(model, point.y)?;
            let mut keyframe = ExactAutomationKeyframe::linear(time, value);
            keyframe.id = keyframe_id;
            keyframe.interpolation_to_next = model.insert_interpolation;
            AudioAutomationEdit::UpsertKeyframe { keyframe }
        }
        CurveEdit::Move { target, point } => {
            let (identity, reference) = match target {
                CurvePointTarget::Keyframe(id) => {
                    let key = model.source_curve.keyframes.iter().find(|key| key.id == id)?;
                    let reference = crate::app_ui::curve_tangents::project_key(
                        key.time,
                        key.value,
                        model.viewport.range,
                    )?;
                    (Some(id), reference)
                }
                CurvePointTarget::VirtualBoundary(boundary) => {
                    let (x, time) = match boundary {
                        mondrian_ui_widgets::CurveBoundary::Start => {
                            (0.0, model.viewport.range.start)
                        }
                        mondrian_ui_widgets::CurveBoundary::End => {
                            (1.0, model.viewport.range.end().ok()?)
                        }
                    };
                    (
                        None,
                        CurvePoint { x, y: model.source_curve.evaluate(time).ok()? },
                    )
                }
                CurvePointTarget::Snapshot(_) => return None,
            };
            let keyframe = if let Some(id) = identity {
                let mut keyframe =
                    model.source_curve.keyframes.iter().find(|key| key.id == id)?.clone();
                if point.x != reference.x {
                    keyframe.time = denormalized_time(model.viewport.range, point.x)?;
                }
                if point.y != reference.y {
                    keyframe.value = deauthor_value(model, point.y)?;
                }
                keyframe
            } else {
                if point.y == reference.y {
                    return None;
                }
                let time = denormalized_time(model.viewport.range, reference.x)?;
                let value = deauthor_value(model, point.y)?;
                let mut keyframe = ExactAutomationKeyframe::linear(time, value);
                keyframe.interpolation_to_next = model.insert_interpolation;
                keyframe
            };
            AudioAutomationEdit::UpsertKeyframe { keyframe }
        }
        CurveEdit::DeleteMany { keyframe_ids } => AudioAutomationEdit::RemoveKeyframes {
            keyframe_ids,
            retain_time: model.current_author_time.as_ref().ok()?.time,
        },
        CurveEdit::Delete { target } => {
            let CurvePointTarget::Keyframe(keyframe_id) = target else {
                return None;
            };
            AudioAutomationEdit::RemoveKeyframe { keyframe_id }
        }
    };
    Some(edit)
}

/// Author action and geometry derived from one admitted exact candidate.
pub(crate) struct AudioCurveEditProjection {
    /// Closed action corresponding to the projected curve.
    pub(crate) action: Action,
    /// Evaluated drawing points; never reconstructed author values.
    pub(crate) display_points: Vec<CurvePoint>,
}

pub(crate) fn preview_audio_curve_edit(
    model: &mut AudioAutomationCurveModel,
    edit: CurveEdit,
) -> Option<AudioCurveEditProjection> {
    let PreparedAudioCurveEdit { action, curve } = prepare_audio_curve_edit(model, edit)?;
    let segments = curve.prepared_segments().ok()?;
    let display = (0..=DISPLAY_SEGMENTS)
        .map(|index| {
            let time = model
                .viewport
                .range
                .start
                .checked_add(
                    model
                        .viewport
                        .range
                        .duration
                        .checked_scale(TimeScale::new(index, DISPLAY_SEGMENTS).ok()?)
                        .ok()?,
                )
                .ok()?;
            let value = if segments.is_empty() {
                curve.keyframes.first().map_or(curve.default_value, |key| key.value)
            } else {
                let segment = segments
                    .partition_point(|segment| segment.end_time() < time)
                    .min(segments.len() - 1);
                segments[segment].evaluate(time).ok()?
            };
            Some(CurvePoint {
                x: index as f32 / DISPLAY_SEGMENTS as f32,
                y: value,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    model.source_curve = curve;
    Some(AudioCurveEditProjection { action, display_points: display })
}

fn denormalized_time(range: TimelineTimeRange, ratio: f32) -> Option<TimelineTime> {
    let ratio = TimelineTime::from_f64_quantized(
        f64::from(ratio.clamp(0.0, 1.0)),
        CURVE_QUANTIZATION_DENOMINATOR,
    )
    .ok()?;
    range
        .start
        .checked_add(range.duration.checked_scale(TimeScale::try_from(ratio).ok()?).ok()?)
        .ok()
}

fn deauthor_value(model: &AudioAutomationCurveModel, author_value: f64) -> Option<f64> {
    let value = if model.value_type == PropertyValueType::Int {
        author_value.round()
    } else {
        author_value
    };
    value.is_finite().then_some(value.clamp(
        model.value_axis.hard_range().min,
        model.value_axis.hard_range().max,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use mondrian_core::AutomationSegmentInterpolation;
    use mondrian_timeline::{AudioAutomationTarget, AudioChannelStripOwner};

    fn audio_target(model: &AudioAutomationCurveModel, index: usize) -> CurvePointTarget {
        model.keys[index].keyframe.as_ref().map(|key| key.id).map_or(
            CurvePointTarget::VirtualBoundary(if model.keys[index].point.x == 0.0 {
                mondrian_ui_widgets::CurveBoundary::Start
            } else {
                mondrian_ui_widgets::CurveBoundary::End
            }),
            CurvePointTarget::Keyframe,
        )
    }

    #[test]
    fn processor_curve_edits_preserve_continuous_values_across_finite_ranges() {
        use mondrian_core::automation::{
            ParameterInvalidValuePolicy, ParameterNumericContract, ParameterSchema, ParameterUnit,
        };
        use mondrian_core::{AudioProcessorInstanceId, AuthoringMap, ParameterId, PropertyValue};
        use mondrian_timeline::audio::{
            AudioProcessorDefinitionRef, AudioProcessorInstance, AudioProcessorParameter,
        };
        use mondrian_timeline::{
            apply_audio_processor_rack_edit, AudioChannelStripRack, AudioProcessorRackAddress,
            AudioProcessorRackEdit, AudioProcessorRackEditRequest, AudioProcessorRackPlacement,
        };

        for (name, min, max, initial, author_value, expected) in [
            (
                "full range quarter",
                -f64::MAX,
                f64::MAX,
                PropertyValue::Double(0.0),
                -f64::MAX * 0.5,
                -f64::MAX * 0.5,
            ),
            (
                "full range endpoint",
                -f64::MAX,
                f64::MAX,
                PropertyValue::Double(0.0),
                f64::MAX,
                f64::MAX,
            ),
            (
                "fixed maximum",
                f64::MAX,
                f64::MAX,
                PropertyValue::Double(f64::MAX),
                f64::MAX,
                f64::MAX,
            ),
            (
                "continuous value",
                0.0,
                1.0,
                PropertyValue::Double(0.0),
                0.125,
                0.125,
            ),
            (
                "integer rounding",
                0.0,
                10.0,
                PropertyValue::Int(0),
                2.5,
                3.0,
            ),
        ] {
            let parameter_id = ParameterId::new_static("test.audio.continuous.value");
            let step = if matches!(initial, PropertyValue::Int(_)) {
                2.0
            } else {
                0.1
            };
            let schema = ParameterSchema::v1(parameter_id.clone(), initial).with_numeric_contract(
                ParameterUnit::Unitless,
                ParameterNumericContract::closed(
                    min,
                    max,
                    Some(step),
                    ParameterInvalidValuePolicy::Reject,
                )
                .expect("legal finite numeric contract"),
            );
            let mut parameters = AuthoringMap::new();
            parameters.insert(
                parameter_id.clone(),
                AudioProcessorParameter::from_schema(schema)
                    .expect("validated processor parameter"),
            );
            let processor = AudioProcessorInstance {
                id: AudioProcessorInstanceId::new(),
                definition: AudioProcessorDefinitionRef::Clap {
                    plugin_id: "org.mondrian.test.continuous".into(),
                    schema_version: 1,
                    binary_sha256: None,
                },
                bypassed: false,
                parameters,
                opaque_state: None,
            };
            let processor_id = processor.id;
            let mut sequence = Sequence::new(name);
            let rack = AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
                rack: AudioChannelStripRack::PreFader,
            };
            apply_audio_processor_rack_edit(
                &mut sequence,
                &AudioProcessorRackEditRequest {
                    address: rack,
                    edit: AudioProcessorRackEdit::Insert {
                        processor,
                        placement: AudioProcessorRackPlacement::End,
                    },
                },
            )
            .expect("atomic validated processor insertion");
            let target =
                AudioAutomationTarget::ProcessorParameter { rack, processor_id, parameter_id };
            let viewport = sequence_automation_viewport(&sequence).expect("Sequence viewport");
            let model = project_audio_automation(
                &sequence,
                target.clone(),
                viewport,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
            )
            .expect("actual processor projection");
            assert!(model.is_editable);
            assert!(model.points().iter().all(|point| point.x.is_finite() && point.y.is_finite()));
            let before = sequence.clone();
            let mut state = crate::app::AppState::new();
            state.test_set_sequence(Some(sequence));
            let action = prepare_audio_curve_edit(
                &model,
                CurveEdit::Insert {
                    keyframe_id: mondrian_core::KeyframeId::new(),
                    index: 1,
                    point: CurvePoint { x: 0.25, y: author_value },
                },
            )
            .map(|prepared| prepared.action)
            .expect(
                "legal continuous curve edit must dispatch even when value / editor step overflows",
            );
            state.dispatch_action(action).expect("canonical author transaction");
            let after = state.active_sequence().expect("committed Sequence").clone();
            let inspection =
                inspect_audio_automation(&after, &target).expect("stable target inspection");
            let keys = &inspection.curve().expect("authored curve").keyframes;
            assert_eq!(keys.len(), 1);
            assert_eq!(keys[0].time, TimelineTime::new(1, 4).expect("exact time"));
            assert_eq!(
                keys[0].value, expected,
                "{name}: an editor increment must not quantize continuous automation"
            );
            assert!(state.undo_timeline().expect("Undo"));
            assert!(state
                .active_sequence()
                .expect("Undo state")
                .author_state_eq_ignoring_revision(&before));
            assert!(state.redo_timeline().expect("Redo"));
            assert!(state
                .active_sequence()
                .expect("Redo state")
                .author_state_eq_ignoring_revision(&after));
        }
    }

    #[test]
    fn live_audio_curve_display_samples_match_canonical_author_edits() {
        for interpolation in [
            AutomationSegmentInterpolation::Hold,
            AutomationSegmentInterpolation::Linear,
            AutomationSegmentInterpolation::Bezier,
        ] {
            let mut sequence = Sequence::new("Canonical preview samples");
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            };
            for (time, value) in [(TimelineTime::ZERO, -18.0), (TimelineTime::ONE, -6.0)] {
                let mut keyframe = ExactAutomationKeyframe::linear(time, value);
                keyframe.interpolation_to_next = interpolation;
                if interpolation == AutomationSegmentInterpolation::Bezier {
                    keyframe.out_handle = Some(mondrian_core::ExactBezierHandle {
                        time_offset: TimelineTime::new(1, 4).unwrap(),
                        value_offset: 3.0,
                    });
                    keyframe.in_handle = Some(mondrian_core::ExactBezierHandle {
                        time_offset: TimelineTime::new(-1, 4).unwrap(),
                        value_offset: -1.0,
                    });
                }
                mondrian_timeline::apply_audio_automation_edit(
                    &mut sequence,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    },
                )
                .unwrap();
            }
            let viewport = AudioAutomationViewport {
                domain: AuthoringTimeDomain::Sequence(sequence.id),
                range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            };
            let model = project_audio_automation(
                &sequence,
                target.clone(),
                viewport,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
            )
            .unwrap();
            for edit in [
                CurveEdit::Move {
                    target: audio_target(&model, 0),
                    point: CurvePoint::new(0.1, 0.2),
                },
                CurveEdit::Insert {
                    keyframe_id: mondrian_core::KeyframeId::new(),
                    index: 1,
                    point: CurvePoint::new(0.5, 0.6),
                },
                CurveEdit::Delete { target: audio_target(&model, 0) },
            ] {
                let display = preview_audio_curve_edit(&mut model.clone(), edit.clone())
                    .unwrap()
                    .display_points;
                let mut authored = sequence.clone();
                mondrian_timeline::apply_audio_automation_edit(
                    &mut authored,
                    &AudioAutomationEditRequest {
                        target: target.clone(),
                        edit: lower_audio_curve_edit(&model, edit.clone()).unwrap(),
                    },
                )
                .unwrap();
                let projected = project_audio_automation(
                    &authored,
                    target.clone(),
                    viewport,
                    crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                        &authored,
                        authored.playhead,
                    ),
                )
                .unwrap();
                assert_eq!(
                    display, projected.display_points,
                    "preview must equal canonical authored output for {interpolation:?} / {edit:?}"
                );
            }
        }
    }

    #[test]
    fn dragging_virtual_audio_boundaries_authors_real_endpoint_keys() {
        for index in [0, 1] {
            let sequence = Sequence::new("Boundary automation");
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            };
            let viewport = AudioAutomationViewport {
                domain: AuthoringTimeDomain::Sequence(sequence.id),
                range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            };
            let model = project_audio_automation(
                &sequence,
                target.clone(),
                viewport,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
            )
            .unwrap();
            assert_eq!(model.keys.len(), 2);
            assert!(model.keys[index].keyframe.is_none());
            assert!(
                prepare_audio_curve_edit(
                    &model,
                    CurveEdit::Move {
                        target: audio_target(&model, index),
                        point: model.keys[index].point
                    },
                )
                .map(|prepared| prepared.action)
                .is_none(),
                "an unchanged virtual boundary must not create automation"
            );
            let point = CurvePoint::new(model.keys[index].point.x, 0.25);
            let action = prepare_audio_curve_edit(
                &model,
                CurveEdit::Move { target: audio_target(&model, index), point },
            )
            .map(|prepared| prepared.action)
            .expect("boundary drag must author a key, not leave an uncommitted widget point");
            let Some(ProductAction::Audio(AudioProductAction::EditAutomation(request))) =
                ProductAction::decode_external(&action).unwrap()
            else {
                panic!("expected canonical audio author action");
            };
            let AudioAutomationEdit::UpsertKeyframe { keyframe } = &request.edit else {
                panic!("expected boundary key insertion");
            };
            let id = keyframe.id;
            assert_eq!(
                keyframe.time,
                if index == 0 {
                    TimelineTime::ZERO
                } else {
                    TimelineTime::ONE
                }
            );
            let before = sequence.clone();
            let mut state = crate::app::AppState::new();
            state.test_set_sequence(Some(sequence));
            state.dispatch_action(action).expect("canonical endpoint transaction");
            let committed = state.active_sequence().unwrap().clone();
            assert!(state.undo_timeline().unwrap());
            assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&before));
            assert!(state.redo_timeline().unwrap());
            assert!(state.active_sequence().unwrap().author_state_eq_ignoring_revision(&committed));
            let directory = std::env::temp_dir()
                .join(format!("mondrian-audio-boundary-{}", uuid::Uuid::new_v4()));
            let library = mondrian_assets::AssetLibrary::open(directory.join("library")).unwrap();
            let archive = directory.join("音频 端点.mdp");
            let document = mondrian_project::ProjectDocument::new(
                "Boundary automation",
                mondrian_timeline::SequenceCollection::new(committed),
                mondrian_core::ProjectColorEnvironment::default(),
                mondrian_timeline::SequenceSettings::default(),
                mondrian_core::ProjectSettings::default(),
            );
            mondrian_project::save_project_archive(&document, &library.database_path(), &archive)
                .unwrap();
            let reopened =
                mondrian_project::load_project_archive(&archive, &directory.join("reopened.db"))
                    .unwrap();
            assert_eq!(reopened.document, document);
            let after = project_audio_automation(
                &reopened.document.sequences.sequences[0],
                target,
                viewport,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &reopened.document.sequences.sequences[0],
                    reopened.document.sequences.sequences[0].playhead,
                ),
            )
            .unwrap();
            let authored = after
                .keys
                .iter()
                .find(|key| key.keyframe.as_ref().is_some_and(|key| key.id == id))
                .unwrap();
            assert_eq!(authored.point, point);
            drop(library);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn audio_frame_nudge_preserves_author_metadata_and_sparse_handle_values() {
        use mondrian_core::{BezierHandleSide, Rational};
        use mondrian_timeline::apply_audio_automation_edit;
        let mut sequence = Sequence::new("Exact audio nudge");
        sequence.settings.frame_rate = Rational::new(30000, 1001);
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
        };
        let middle =
            ExactAutomationKeyframe::linear(TimelineTime::new(1, 2).expect("middle"), -6.123456789);
        for keyframe in [
            ExactAutomationKeyframe::linear(TimelineTime::ZERO, -12.0),
            middle.clone(),
            ExactAutomationKeyframe::linear(TimelineTime::ONE, -3.0),
        ] {
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("seed");
        }
        let viewport = AudioAutomationViewport {
            domain: AuthoringTimeDomain::Sequence(sequence.id),
            range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).expect("range"),
        };
        let model = project_audio_automation(
            &sequence,
            target.clone(),
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .expect("model");
        let frame = TimelineTime::new(1001, 30000).expect("frame");
        assert_eq!(model.frame_time_step, frame);
        let edit = lower_audio_curve_edit(
            &model,
            CurveEdit::NudgeTime { keyframe_id: middle.id, side: None, delta: frame },
        )
        .expect("exact intent");
        let mut expected = middle.clone();
        expected.time = expected.time.checked_add(frame).expect("next frame");
        assert_eq!(
            edit,
            AudioAutomationEdit::UpsertKeyframe { keyframe: expected.clone() }
        );
        assert!(lower_audio_curve_edit(
            &model,
            CurveEdit::NudgeTime {
                keyframe_id: middle.id,
                side: None,
                delta: TimelineTime::new(1, 2).expect("collision")
            }
        )
        .is_none());
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest { target: target.clone(), edit },
        )
        .expect("author move");
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolation {
                    keyframe_id: middle.id,
                    interpolation: InterpolationType::ContinuousBezier,
                },
            },
        )
        .expect("constrained mode");
        let model = project_audio_automation(
            &sequence,
            target,
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .expect("current model");
        assert_eq!(
            model.key_points().iter().find(|(id, _)| *id == middle.id).expect("marker").1.x,
            expected.time.to_f64() as f32
        );
        let control = model
            .source_curve
            .keyframe_controls()
            .find(|control| {
                control.keyframe_id == middle.id && control.side == BezierHandleSide::Outgoing
            })
            .expect("control");
        assert_eq!(
            lower_audio_curve_edit(
                &model,
                CurveEdit::NudgeTime {
                    keyframe_id: middle.id,
                    side: Some(BezierHandleSide::Outgoing),
                    delta: frame
                }
            ),
            Some(AudioAutomationEdit::EditHandle {
                keyframe_id: middle.id,
                side: BezierHandleSide::Outgoing,
                edit: mondrian_core::ExactBezierHandleEdit {
                    time_offset: Some(
                        control.handle.time_offset.checked_add(frame).expect("control step")
                    ),
                    value_offset: None
                }
            })
        );
    }

    #[test]
    fn projection_preserves_stable_keys_and_exact_owner_domain() {
        let mut sequence = Sequence::new("Audio curve projection");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let keyframe = ExactAutomationKeyframe::linear(TimelineTime::new(1, 2).unwrap(), -6.0);
        let keyframe_id = keyframe.id;
        mondrian_timeline::apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
            },
        )
        .unwrap();
        let viewport = AudioAutomationViewport {
            domain: AuthoringTimeDomain::Sequence(sequence.id),
            range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        };
        let model = project_audio_automation(
            &sequence,
            target.clone(),
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .unwrap();
        assert_eq!(model.keys.len(), 3);
        assert_eq!(model.keys[1].keyframe.as_ref().unwrap().id, keyframe_id);
        assert_eq!(model.keys[1].point, CurvePoint { x: 0.5, y: -6.0 });
        assert_eq!(model.display_points.len(), 129);

        let action = prepare_audio_curve_edit(
            &model,
            CurveEdit::Move {
                target: audio_target(&model, 1),
                point: CurvePoint::new(0.75, 0.5),
            },
        )
        .map(|prepared| prepared.action)
        .unwrap();
        assert!(matches!(
            ProductAction::decode_external(&action).unwrap(),
            Some(ProductAction::Audio(AudioProductAction::EditAutomation(
                AudioAutomationEditRequest {
                    target: AudioAutomationTarget::ChannelFader { .. },
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                }
            ))) if keyframe.id == keyframe_id
                && keyframe.time == TimelineTime::new(3, 4).unwrap()
                && keyframe.interpolation_to_next == AutomationSegmentInterpolation::Linear
        ));
    }

    #[test]
    fn interpolation_choice_uses_stable_key_and_schema_gate() {
        let mut sequence = Sequence::new("Audio interpolation menu");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let keyframe = ExactAutomationKeyframe::linear(TimelineTime::new(1, 2).unwrap(), -6.0);
        let keyframe_id = keyframe.id;
        mondrian_timeline::apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
            },
        )
        .unwrap();
        let viewport = AudioAutomationViewport {
            domain: AuthoringTimeDomain::Sequence(sequence.id),
            range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        };
        let mut model = project_audio_automation(
            &sequence,
            target,
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .unwrap();
        let index = model.keys.iter().position(|key| key.keyframe.is_some()).unwrap();
        let action = audio_automation_interpolation_action(
            &model,
            index,
            InterpolationType::ContinuousBezier,
        )
        .unwrap();
        assert!(matches!(
            ProductAction::decode_external(&action).unwrap(),
            Some(ProductAction::Audio(AudioProductAction::EditAutomation(
                AudioAutomationEditRequest {
                    edit: AudioAutomationEdit::SetInterpolation {
                        keyframe_id: id,
                        interpolation: InterpolationType::ContinuousBezier,
                    },
                    ..
                }
            ))) if id == keyframe_id
        ));
        model
            .allowed_interpolations
            .retain(|mode| *mode != ParameterInterpolation::Bezier);
        assert!(audio_automation_interpolation_action(
            &model,
            index,
            InterpolationType::AutoBezier,
        )
        .is_none());
        assert!(
            audio_automation_interpolation_action(&model, 0, InterpolationType::Linear,).is_none()
        );
    }

    #[test]
    fn locked_projection_is_visible_but_cannot_dispatch() {
        let mut sequence = Sequence::new("Locked audio curve");
        let track_id = sequence.audio_tracks[0].id;
        sequence.audio_tracks[0].is_locked = true;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let viewport = sequence_automation_viewport(&sequence).unwrap();
        let model = project_audio_automation(
            &sequence,
            target,
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .unwrap();
        assert!(!model.is_editable);
        assert!(prepare_audio_curve_edit(
            &model,
            CurveEdit::Insert {
                keyframe_id: mondrian_core::KeyframeId::new(),
                index: 1,
                point: CurvePoint::new(0.5, 0.5)
            }
        )
        .map(|prepared| prepared.action)
        .is_none());
    }

    #[test]
    fn time_only_drag_preserves_existing_values_outside_the_soft_range() {
        let mut sequence = Sequence::new("Extended audio value range");
        let track_id = sequence.audio_tracks[0].id;
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id },
        };
        let keyframe = ExactAutomationKeyframe::linear(TimelineTime::new(1, 2).unwrap(), -100.0);
        mondrian_timeline::apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
            },
        )
        .unwrap();
        let viewport = AudioAutomationViewport {
            domain: AuthoringTimeDomain::Sequence(sequence.id),
            range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
        };
        let model = project_audio_automation(
            &sequence,
            target,
            viewport,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        )
        .unwrap();
        assert_eq!(model.keys[1].point.y, -100.0);

        let action = prepare_audio_curve_edit(
            &model,
            CurveEdit::Move {
                target: audio_target(&model, 1),
                point: CurvePoint { x: 0.75, y: model.keys[1].point.y },
            },
        )
        .map(|prepared| prepared.action)
        .unwrap();
        assert!(matches!(
            ProductAction::decode_external(&action).unwrap(),
            Some(ProductAction::Audio(AudioProductAction::EditAutomation(
                AudioAutomationEditRequest {
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                    ..
                }
            ))) if keyframe.value == -100.0
        ));
    }

    #[test]
    fn single_axis_audio_key_drag_preserves_the_untouched_exact_coordinate() {
        for horizontal in [true, false] {
            let mut sequence = Sequence::new("Exact audio key drag");
            let target = AudioAutomationTarget::ChannelFader {
                owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
            };
            let time = TimelineTime::new(1, 3).unwrap();
            let value = -6.123456789;
            let keyframe = ExactAutomationKeyframe::linear(time, value);
            let keyframe_id = keyframe.id;
            mondrian_timeline::apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .unwrap();
            let viewport = AudioAutomationViewport {
                domain: AuthoringTimeDomain::Sequence(sequence.id),
                range: TimelineTimeRange::new(TimelineTime::ZERO, TimelineTime::ONE).unwrap(),
            };
            let model = project_audio_automation(
                &sequence,
                target.clone(),
                viewport,
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
            )
            .unwrap();
            let original = model.keys[1].point;
            let point = if horizontal {
                CurvePoint { x: 0.75, y: original.y }
            } else {
                CurvePoint::new(original.x, 0.25)
            };
            let action = prepare_audio_curve_edit(
                &model,
                CurveEdit::Move { target: audio_target(&model, 1), point },
            )
            .map(|prepared| prepared.action)
            .unwrap();
            let Some(ProductAction::Audio(AudioProductAction::EditAutomation(request))) =
                ProductAction::decode_external(&action).unwrap()
            else {
                panic!("canonical audio automation edit");
            };
            mondrian_timeline::apply_audio_automation_edit(&mut sequence, &request).unwrap();
            let inspection = inspect_audio_automation(&sequence, &target).unwrap();
            let moved = inspection
                .curve()
                .unwrap()
                .keyframes
                .iter()
                .find(|key| key.id == keyframe_id)
                .unwrap();
            if horizontal {
                assert_eq!(
                    moved.value, value,
                    "horizontal drag must preserve exact author value"
                );
                assert_eq!(moved.time, TimelineTime::new(3, 4).unwrap());
            } else {
                assert_eq!(
                    moved.time, time,
                    "vertical drag must preserve exact author time"
                );
                assert_ne!(moved.value, value);
            }
        }
    }

    #[test]
    fn audio_group_crossing_rejects_invalid_new_continuous_control_before_preview() {
        use mondrian_timeline::{apply_audio_automation_edit, AudioAutomationEditError};
        let mut sequence = Sequence::new("Crossed continuous endpoint");
        let target = AudioAutomationTarget::ChannelFader {
            owner: AudioChannelStripOwner::Track { track_id: sequence.audio_tracks[0].id },
        };
        let mut ids = Vec::new();
        for (num, den, value) in [(1, 5, -119.0), (6, 25, -1.0), (19, 50, -1.0)] {
            let keyframe =
                ExactAutomationKeyframe::linear(TimelineTime::new(num, den).expect("time"), value);
            ids.push(keyframe.id);
            apply_audio_automation_edit(
                &mut sequence,
                &AudioAutomationEditRequest {
                    target: target.clone(),
                    edit: AudioAutomationEdit::UpsertKeyframe { keyframe },
                },
            )
            .expect("seed keys");
        }
        apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest {
                target: target.clone(),
                edit: AudioAutomationEdit::SetInterpolation {
                    keyframe_id: ids[0],
                    interpolation: InterpolationType::ContinuousBezier,
                },
            },
        )
        .expect("legal original continuous endpoint");
        let edit = AudioAutomationEdit::ShiftKeyframes {
            keyframe_ids: ids[..2].to_vec(),
            delta: TimelineTime::new(1, 5).expect("five frames"),
        };
        let original = sequence.clone();
        let error = apply_audio_automation_edit(
            &mut sequence,
            &AudioAutomationEditRequest { target: target.clone(), edit },
        )
        .expect_err("new reverse control falls below the gain floor");
        assert!(matches!(
            error,
            AudioAutomationEditError::AuthorState(
                mondrian_timeline::audio::AudioAuthoringError::InvalidGain
            )
        ));
        assert_eq!(sequence, original);
        let mut model = project_audio_automation(
            &sequence,
            target,
            sequence_automation_viewport(&sequence).expect("viewport"),
            AudioAutomationTimeContext::at_sequence_time(&sequence, TimelineTime::ZERO),
        )
        .expect("actual audio model");
        let original_model = model.clone();
        let ui_edit = CurveEdit::NudgeTimeMany {
            keyframe_ids: ids[..2].to_vec(),
            delta: TimelineTime::new(1, 5).expect("five frames"),
        };
        assert!(
            preview_audio_curve_edit(&mut model, ui_edit.clone()).is_none(),
            "a canonically refused curve cannot become projected geometry"
        );
        assert_eq!(model, original_model);
        assert!(
            prepare_audio_curve_edit(&model, ui_edit)
                .map(|prepared| prepared.action)
                .is_none(),
            "a canonically refused curve cannot publish an author intent"
        );
    }
}
