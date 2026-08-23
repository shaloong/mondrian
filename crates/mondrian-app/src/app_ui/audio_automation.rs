//! Product Adapter for exact owner-time audio automation.
//!
//! Timeline owns curve addresses, time domains, lock admission, and mutation.
//! This Module only projects an explicit exact viewport into normalized Widget
//! coordinates and lowers one committed gesture back to a stable-ID edit.

use std::collections::BTreeMap;

use mondrian_core::{
    AuthoringTimeDomain, AutomationSegmentInterpolation, ExactAutomationKeyframe,
    ParameterInterpolation, PropertyValueType, TimeScale, TimelineTime, TimelineTimeRange,
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
    pub(crate) step: Option<f64>,
    pub(crate) value_type: PropertyValueType,
    pub(crate) insert_interpolation: AutomationSegmentInterpolation,
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
    let insert_interpolation = if allowed.contains(&ParameterInterpolation::Linear) {
        AutomationSegmentInterpolation::Linear
    } else if allowed.contains(&ParameterInterpolation::Hold) {
        AutomationSegmentInterpolation::Hold
    } else if allowed.contains(&ParameterInterpolation::Bezier) {
        AutomationSegmentInterpolation::Bezier
    } else {
        return None;
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
    let value_span = view_max - view_min;
    if !value_span.is_finite() || value_span <= 0.0 {
        return None;
    }
    let range_end = viewport.range.end().ok()?;
    let normalized_value = |value: f64| {
        value
            .is_finite()
            .then_some(((value - view_min) / value_span).clamp(0.0, 1.0) as f32)
    };
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
        step: contract.step,
        value_type: contract.value_type,
        insert_interpolation,
        is_editable: inspection.is_editable(),
        edit_disabled_reason: inspection.edit_blocker().map(ToString::to_string),
    })
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
            let mut keyframe = model.keys.get(index)?.keyframe.clone()?;
            keyframe.time = denormalized_time(model.viewport.range, point.x)?;
            keyframe.value = denormalized_value(model, point.y)?;
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
    let span = model.view_max - model.view_min;
    let mut value = model.view_min + span * f64::from(ratio.clamp(0.0, 1.0));
    if let Some(step) = model.step.filter(|step| step.is_finite() && *step > 0.0) {
        value = (value / step).round() * step;
    }
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
}
