//! Product Adapter for exact owner-time audio automation.
//!
//! Timeline owns curve addresses, time domains, lock admission, and mutation.
//! This Module only projects an explicit exact viewport into normalized Widget
//! coordinates and lowers one committed gesture back to a stable-ID edit.

use std::collections::BTreeMap;

use mondrian_core::{
    AuthoringTimeDomain, AutomationSegmentInterpolation, ExactAutomationKeyframe,
    ExactAutomationTangentMode, InterpolationType, ParameterInterpolation, PropertyValueType,
    TimeScale, TimelineTime, TimelineTimeRange,
};
use mondrian_editor_state::Action;
use mondrian_timeline::{
    inspect_audio_automation, AudioAutomationEdit, AudioAutomationEditRequest,
    AudioAutomationTarget, Sequence,
};
use mondrian_ui_widgets::{CurveEdit, CurvePoint, CurvePointPolicy};

use crate::app::ui_actions::audio_automation_edit_action;

const CURVE_QUANTIZATION_DENOMINATOR: u32 = 1_000_000;
const DISPLAY_SEGMENTS: i64 = 128;

/// Exact visible interval qualified by the curve owner's time domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioAutomationViewport {
    pub(crate) domain: AuthoringTimeDomain,
    pub(crate) range: TimelineTimeRange,
}

/// One editable key or read-only boundary anchor in a normalized viewport.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioAutomationCurveKeyModel {
    pub(crate) keyframe: Option<ExactAutomationKeyframe>,
    pub(crate) point: CurvePoint,
}

/// Complete normalized curve projection consumed by the shared CurveEditor.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioAutomationCurveModel {
    pub(crate) target: AudioAutomationTarget,
    pub(crate) viewport: AudioAutomationViewport,
    pub(crate) keys: Vec<AudioAutomationCurveKeyModel>,
    pub(crate) display_points: Vec<CurvePoint>,
    pub(crate) hard_min: f64,
    pub(crate) hard_max: f64,
    pub(crate) view_min: f64,
    pub(crate) view_max: f64,
    pub(crate) value_type: PropertyValueType,
    pub(crate) insert_interpolation: AutomationSegmentInterpolation,
    pub(crate) allowed_interpolations: Vec<ParameterInterpolation>,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
}

impl AudioAutomationCurveModel {
    pub(crate) fn is_automated(&self) -> bool {
        self.keys.iter().any(|key| key.keyframe.is_some())
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

/// Project one Timeline-owned curve into an explicit normalized viewport.
pub(crate) fn project_audio_automation(
    sequence: &Sequence,
    target: AudioAutomationTarget,
    viewport: AudioAutomationViewport,
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
    let mut view_min = contract.soft_range.min;
    let mut view_max = contract.soft_range.max;
    if let Some(curve) = inspection.curve().filter(|curve| !curve.keyframes.is_empty()) {
        for keyframe in &curve.keyframes {
            view_min = view_min.min(keyframe.value);
            view_max = view_max.max(keyframe.value);
            if let Some(handle) = keyframe.in_handle {
                view_min = view_min.min(keyframe.value + handle.value_offset);
                view_max = view_max.max(keyframe.value + handle.value_offset);
            }
            if let Some(handle) = keyframe.out_handle {
                view_min = view_min.min(keyframe.value + handle.value_offset);
                view_max = view_max.max(keyframe.value + handle.value_offset);
            }
        }
    } else {
        view_min = view_min.min(inspection.default_value());
        view_max = view_max.max(inspection.default_value());
    }
    let value_range =
        mondrian_core::automation::ParameterNumericRange::new(view_min, view_max).ok()?;
    let range_end = viewport.range.end().ok()?;
    let normalized_value =
        |value: f64| value_range.normalized_position(value).map(|position| position as f32);
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
            point: CurvePoint::new(0.0, normalized_value(evaluate(viewport.range.start)?)?),
        },
    );
    keys.insert(
        range_end,
        AudioAutomationCurveKeyModel {
            keyframe: None,
            point: CurvePoint::new(1.0, normalized_value(evaluate(range_end)?)?),
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
                    point: CurvePoint::new(
                        normalized_time(keyframe.time)?,
                        normalized_value(keyframe.value)?,
                    ),
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
            Some(CurvePoint::new(
                index as f32 / DISPLAY_SEGMENTS as f32,
                normalized_value(evaluate(time)?)?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;

    Some(AudioAutomationCurveModel {
        target,
        viewport,
        keys: keys.into_values().collect(),
        display_points,
        hard_min: contract.hard_range.min,
        hard_max: contract.hard_range.max,
        view_min,
        view_max,
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
    Some(audio_automation_edit_action(AudioAutomationEditRequest {
        target: model.target.clone(),
        edit: AudioAutomationEdit::SetInterpolation { keyframe_id, interpolation },
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

/// Lower one committed CurveEditor gesture into one stable-ID author action.
pub(crate) fn audio_automation_curve_edit_action(
    model: &AudioAutomationCurveModel,
    edit: CurveEdit,
) -> Option<Action> {
    if !model.is_editable {
        return None;
    }
    let edit = match edit {
        CurveEdit::Insert { point, .. } => {
            let time = denormalized_time(model.viewport.range, point.x)?;
            let value = denormalized_value(model, point.y)?;
            let mut keyframe = ExactAutomationKeyframe::linear(time, value);
            keyframe.interpolation_to_next = model.insert_interpolation;
            AudioAutomationEdit::UpsertKeyframe { keyframe }
        }
        CurveEdit::Move { index, point } => {
            let key = model.keys.get(index)?;
            let mut keyframe = key.keyframe.clone()?;
            if point.x != key.point.x {
                keyframe.time = denormalized_time(model.viewport.range, point.x)?;
            }
            if point.y != key.point.y {
                keyframe.value = denormalized_value(model, point.y)?;
            }
            AudioAutomationEdit::UpsertKeyframe { keyframe }
        }
        CurveEdit::Delete { index } => AudioAutomationEdit::RemoveKeyframe {
            keyframe_id: model.keys.get(index)?.keyframe.as_ref()?.id,
        },
    };
    Some(audio_automation_edit_action(AudioAutomationEditRequest {
        target: model.target.clone(),
        edit,
    }))
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

fn denormalized_value(model: &AudioAutomationCurveModel, ratio: f32) -> Option<f64> {
    let range =
        mondrian_core::automation::ParameterNumericRange::new(model.view_min, model.view_max)
            .ok()?;
    let mut value = range.value_at_normalized_position(f64::from(ratio.clamp(0.0, 1.0)))?;
    if model.value_type == PropertyValueType::Int {
        value = value.round();
    }
    value.is_finite().then_some(value.clamp(model.hard_min, model.hard_max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use mondrian_core::AutomationSegmentInterpolation;
    use mondrian_timeline::{AudioAutomationTarget, AudioChannelStripOwner};

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

        for (name, min, max, initial, ratio, expected) in [
            (
                "full range quarter",
                -f64::MAX,
                f64::MAX,
                PropertyValue::Double(0.0),
                0.25,
                -f64::MAX * 0.5,
            ),
            (
                "full range endpoint",
                -f64::MAX,
                f64::MAX,
                PropertyValue::Double(0.0),
                1.0,
                f64::MAX,
            ),
            (
                "fixed maximum",
                f64::MAX,
                f64::MAX,
                PropertyValue::Double(f64::MAX),
                0.5,
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
                0.25,
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
            let model = project_audio_automation(&sequence, target.clone(), viewport)
                .expect("actual processor projection");
            assert!(model.is_editable);
            assert!(model.points().iter().all(|point| point.x.is_finite() && point.y.is_finite()));
            let before = sequence.clone();
            let mut state = crate::app::AppState::new();
            state.test_set_sequence(Some(sequence));
            let action = audio_automation_curve_edit_action(
                &model,
                CurveEdit::Insert { index: 1, point: CurvePoint::new(0.25, ratio) },
            )
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
        let model = project_audio_automation(&sequence, target.clone(), viewport).unwrap();
        assert_eq!(model.keys.len(), 3);
        assert_eq!(model.keys[1].keyframe.as_ref().unwrap().id, keyframe_id);
        assert_eq!(model.keys[1].point, CurvePoint::new(0.5, 0.75));
        assert_eq!(model.display_points.len(), 129);

        let action = audio_automation_curve_edit_action(
            &model,
            CurveEdit::Move { index: 1, point: CurvePoint::new(0.75, 0.5) },
        )
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
        let mut model = project_audio_automation(&sequence, target, viewport).unwrap();
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
        let model = project_audio_automation(&sequence, target, viewport).unwrap();
        assert!(!model.is_editable);
        assert!(audio_automation_curve_edit_action(
            &model,
            CurveEdit::Insert { index: 1, point: CurvePoint::new(0.5, 0.5) }
        )
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
        let model = project_audio_automation(&sequence, target, viewport).unwrap();
        assert_eq!(model.keys[1].point.y, 0.0);

        let action = audio_automation_curve_edit_action(
            &model,
            CurveEdit::Move {
                index: 1,
                point: CurvePoint::new(0.75, model.keys[1].point.y),
            },
        )
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
            let model = project_audio_automation(&sequence, target.clone(), viewport).unwrap();
            let original = model.keys[1].point;
            let point = if horizontal {
                CurvePoint::new(0.75, original.y)
            } else {
                CurvePoint::new(original.x, 0.25)
            };
            let action =
                audio_automation_curve_edit_action(&model, CurveEdit::Move { index: 1, point })
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
}
