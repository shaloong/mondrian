//! Shared Audio Processor Rack projection and typed Widget action factories.
//!
//! Inspector and future Mixer surfaces consume this Module instead of
//! traversing or interpreting the Sequence audio author model independently.

use std::collections::BTreeSet;

use mondrian_core::automation::{ParameterSchema, PropertyValueType};
use mondrian_core::{AudioProcessingScopeId, AudioProcessorInstanceId, ParameterId};
use mondrian_editor_state::Action;
use mondrian_timeline::audio::{
    AudioProcessorDefinitionRef, AudioProcessorInstance, BUILTIN_GAIN_DEFINITION_ID,
    BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID, BUILTIN_SAMPLE_DELAY_DEFINITION_ID,
    GAIN_DB_PARAMETER_ID, LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID,
    LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID, LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID,
    SAMPLE_DELAY_FRAMES_PARAMETER_ID,
};
use mondrian_timeline::clip::Clip;
use mondrian_timeline::sequence::Sequence;
use mondrian_timeline::{
    audio_processor_rack, AudioProcessorParameterEdit, AudioProcessorRackAddress,
    AudioProcessorRackEdit, AudioProcessorRackEditRequest, AudioProcessorRackPlacement,
};

use crate::app::product_action::{AudioProcessorBuiltInPreset, AudioProcessorInsertBuiltInPayload};
use crate::app::ui_actions::{
    audio_processor_insert_built_in_action, audio_processor_rack_edit_action,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AudioProcessorInsertOptionModel {
    pub(crate) label: &'static str,
    pub(crate) preset: AudioProcessorBuiltInPreset,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorParameterModel {
    pub(crate) parameter_id: ParameterId,
    pub(crate) label: String,
    pub(crate) schema: ParameterSchema,
    pub(crate) static_value: f64,
    pub(crate) keyframe_count: usize,
}

impl AudioProcessorParameterModel {
    pub(crate) fn is_static_editable(&self) -> bool {
        self.keyframe_count == 0 && self.schema.numeric.is_some()
    }

    fn normalized_static_value(&self, value: f32) -> Option<f64> {
        let mut value = f64::from(value);
        if !value.is_finite() {
            return None;
        }
        if self.schema.value_type == PropertyValueType::Int {
            value = value.round();
        }
        let contract = self.schema.numeric?;
        (value >= contract.hard_range.min && value <= contract.hard_range.max).then_some(value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorInstanceModel {
    pub(crate) processor_id: AudioProcessorInstanceId,
    pub(crate) label: String,
    pub(crate) bypassed: bool,
    pub(crate) parameters: Vec<AudioProcessorParameterModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorRackModel {
    pub(crate) address: AudioProcessorRackAddress,
    pub(crate) title: String,
    pub(crate) ownership_label: String,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
    pub(crate) processors: Vec<AudioProcessorInstanceModel>,
    pub(crate) insert_options: Vec<AudioProcessorInsertOptionModel>,
}

/// Project every unique Processing Scope bound by one selected Clip.
///
/// Shared Scope lock admission and binding counts span the complete Sequence,
/// matching the Timeline author transaction rather than only the selected Clip.
pub(crate) fn clip_processing_scope_racks(
    sequence: &Sequence,
    clip: &Clip,
) -> Vec<AudioProcessorRackModel> {
    let mut seen = BTreeSet::new();
    clip.audio_components
        .iter()
        .filter_map(|edit| {
            let scope_id = edit.processing.scope_id;
            if seen.insert(scope_id) {
                processing_scope_rack(sequence, scope_id)
            } else {
                None
            }
        })
        .collect()
}

fn processing_scope_rack(
    sequence: &Sequence,
    scope_id: AudioProcessingScopeId,
) -> Option<AudioProcessorRackModel> {
    let address = AudioProcessorRackAddress::ProcessingScope { scope_id };
    let rack = audio_processor_rack(sequence, &address).ok()?;
    let mut binding_count = 0usize;
    let mut locked_track = None;
    for track in &sequence.audio_tracks {
        for clip in &track.clips {
            for edit in &clip.audio_components {
                if edit.processing.scope_id == scope_id {
                    binding_count = binding_count.saturating_add(1);
                    if track.is_locked {
                        locked_track.get_or_insert(track.id);
                    }
                }
            }
        }
    }
    let ownership_label = match binding_count {
        0 => "未绑定的 Processing Scope".to_owned(),
        1 => "仅由当前 Component 使用".to_owned(),
        count => format!("共享 Processing Scope · {count} 个 Component"),
    };
    Some(AudioProcessorRackModel {
        address,
        title: "音频处理器 Rack".to_owned(),
        ownership_label,
        is_editable: locked_track.is_none(),
        edit_disabled_reason: locked_track
            .map(|track_id| format!("共享 Scope 同时绑定到已锁定轨道 {track_id}，必须先解锁")),
        processors: rack.processors.iter().map(project_processor).collect(),
        insert_options: vec![
            AudioProcessorInsertOptionModel {
                label: "增益",
                preset: AudioProcessorBuiltInPreset::Gain,
            },
            AudioProcessorInsertOptionModel {
                label: "前瞻限制器（Sample Peak）",
                preset: AudioProcessorBuiltInPreset::LookaheadLimiter,
            },
        ],
    })
}

fn project_processor(processor: &AudioProcessorInstance) -> AudioProcessorInstanceModel {
    let definition_id = match &processor.definition {
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. } => Some(definition_id.as_str()),
        AudioProcessorDefinitionRef::Vst3 { .. } | AudioProcessorDefinitionRef::Clap { .. } => None,
    };
    AudioProcessorInstanceModel {
        processor_id: processor.id,
        label: processor_label(&processor.definition),
        bypassed: processor.bypassed,
        parameters: processor
            .parameters
            .values()
            .map(|parameter| AudioProcessorParameterModel {
                parameter_id: parameter.schema.parameter_id.clone(),
                label: parameter_label(definition_id, &parameter.schema.parameter_id),
                schema: parameter.schema.clone(),
                static_value: parameter.automation.default_value,
                keyframe_count: parameter.automation.keyframes.len(),
            })
            .collect(),
    }
}

fn processor_label(definition: &AudioProcessorDefinitionRef) -> String {
    match definition {
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. }
            if definition_id == BUILTIN_GAIN_DEFINITION_ID =>
        {
            "增益".to_owned()
        }
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. }
            if definition_id == BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID =>
        {
            "前瞻限制器（Sample Peak）".to_owned()
        }
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. }
            if definition_id == BUILTIN_SAMPLE_DELAY_DEFINITION_ID =>
        {
            "Sample Delay".to_owned()
        }
        AudioProcessorDefinitionRef::BuiltIn { definition_id, schema_version } => {
            format!("{definition_id} · schema {schema_version}")
        }
        AudioProcessorDefinitionRef::Vst3 { class_id, vendor, .. } => vendor.as_ref().map_or_else(
            || format!("VST3 · {class_id}"),
            |vendor| format!("{vendor} · {class_id}"),
        ),
        AudioProcessorDefinitionRef::Clap { plugin_id, .. } => format!("CLAP · {plugin_id}"),
    }
}

fn parameter_label(definition_id: Option<&str>, parameter_id: &ParameterId) -> String {
    match (definition_id, parameter_id.as_str()) {
        (Some(BUILTIN_GAIN_DEFINITION_ID), GAIN_DB_PARAMETER_ID) => "增益".to_owned(),
        (
            Some(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID),
            LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID,
        ) => "上限".to_owned(),
        (
            Some(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID),
            LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID,
        ) => "前瞻".to_owned(),
        (
            Some(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID),
            LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID,
        ) => "释放".to_owned(),
        (Some(BUILTIN_SAMPLE_DELAY_DEFINITION_ID), SAMPLE_DELAY_FRAMES_PARAMETER_ID) => {
            "延迟采样".to_owned()
        }
        _ => parameter_id.as_str().to_owned(),
    }
}

pub(crate) fn insert_action(
    rack: &AudioProcessorRackModel,
    preset: AudioProcessorBuiltInPreset,
) -> Action {
    audio_processor_insert_built_in_action(AudioProcessorInsertBuiltInPayload {
        address: rack.address,
        preset,
        placement: AudioProcessorRackPlacement::End,
    })
}

pub(crate) fn bypass_action(
    rack: &AudioProcessorRackModel,
    processor: &AudioProcessorInstanceModel,
    bypassed: bool,
) -> Action {
    rack_edit_action(
        rack.address,
        AudioProcessorRackEdit::SetBypassed { processor_id: processor.processor_id, bypassed },
    )
}

pub(crate) fn remove_action(
    rack: &AudioProcessorRackModel,
    processor: &AudioProcessorInstanceModel,
) -> Action {
    rack_edit_action(
        rack.address,
        AudioProcessorRackEdit::Remove { processor_id: processor.processor_id },
    )
}

pub(crate) fn move_before_action(
    rack: &AudioProcessorRackModel,
    processor: &AudioProcessorInstanceModel,
    anchor: AudioProcessorInstanceId,
) -> Action {
    rack_edit_action(
        rack.address,
        AudioProcessorRackEdit::Move {
            processor_id: processor.processor_id,
            placement: AudioProcessorRackPlacement::Before { processor_id: anchor },
        },
    )
}

pub(crate) fn move_to_end_action(
    rack: &AudioProcessorRackModel,
    processor: &AudioProcessorInstanceModel,
) -> Action {
    rack_edit_action(
        rack.address,
        AudioProcessorRackEdit::Move {
            processor_id: processor.processor_id,
            placement: AudioProcessorRackPlacement::End,
        },
    )
}

pub(crate) fn set_static_parameter_action(
    rack: &AudioProcessorRackModel,
    processor: &AudioProcessorInstanceModel,
    parameter: &AudioProcessorParameterModel,
    value: f32,
) -> Option<Action> {
    if !rack.is_editable || !parameter.is_static_editable() {
        return None;
    }
    let value = parameter.normalized_static_value(value)?;
    Some(rack_edit_action(
        rack.address,
        AudioProcessorRackEdit::EditParameter {
            processor_id: processor.processor_id,
            parameter_id: parameter.parameter_id.clone(),
            edit: AudioProcessorParameterEdit::SetStaticValue { value },
        },
    ))
}

fn rack_edit_action(address: AudioProcessorRackAddress, edit: AudioProcessorRackEdit) -> Action {
    audio_processor_rack_edit_action(AudioProcessorRackEditRequest { address, edit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProcessorProductAction, ProductAction};
    use mondrian_core::{AudioSourceComponentId, ExactAutomationKeyframe, TimelineTime};

    fn sequence_with_gain_scope() -> (Sequence, mondrian_core::ClipId) {
        let mut sequence = Sequence::new("Rack projection");
        let track_id = sequence.audio_tracks[0].id;
        let clip = Clip::new(
            mondrian_core::AssetId::new(),
            TimelineTime::ZERO,
            TimelineTime::ONE,
        )
        .expect("Clip");
        let clip_id = clip.id;
        sequence
            .add_media_audio_clip(track_id, clip, AudioSourceComponentId::primary())
            .expect("audio Clip");
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        let scope = sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("Processing Scope");
        scope.processors.processors.push(AudioProcessorInstance::built_in(
            BUILTIN_GAIN_DEFINITION_ID,
            1,
        ));
        (sequence, clip_id)
    }

    #[test]
    fn projection_deduplicates_shared_scope_and_matches_lock_admission() {
        let (mut sequence, clip_id) = sequence_with_gain_scope();
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        sequence.audio_tracks[0].clips[0].audio_components.push(
            mondrian_timeline::audio::AudioComponentEdit::media(
                AudioSourceComponentId::new(),
                scope_id,
            ),
        );
        let clip = sequence.audio_tracks[0]
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)
            .expect("Clip");

        let racks = clip_processing_scope_racks(&sequence, clip);

        assert_eq!(racks.len(), 1);
        assert_eq!(
            racks[0].ownership_label,
            "共享 Processing Scope · 2 个 Component"
        );
        assert_eq!(racks[0].processors[0].label, "增益");
        assert_eq!(racks[0].processors[0].parameters[0].label, "增益");
        assert!(racks[0].is_editable);

        sequence.audio_tracks[0].is_locked = true;
        let clip = &sequence.audio_tracks[0].clips[0];
        let locked = clip_processing_scope_racks(&sequence, clip);
        assert!(!locked[0].is_editable);
        assert!(locked[0].edit_disabled_reason.is_some());
    }

    #[test]
    fn actions_preserve_typed_addresses_and_do_not_fake_automated_static_edits() {
        let (mut sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(&sequence, clip);
        let rack = &racks[0];
        let processor = &rack.processors[0];
        let parameter = &processor.parameters[0];

        let action = set_static_parameter_action(rack, processor, parameter, -6.0)
            .expect("valid static edit");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::AudioProcessor(
                AudioProcessorProductAction::EditRack(AudioProcessorRackEditRequest {
                    address: AudioProcessorRackAddress::ProcessingScope { .. },
                    edit: AudioProcessorRackEdit::EditParameter { .. },
                })
            ))
        ));
        assert!(set_static_parameter_action(rack, processor, parameter, -121.0).is_none());

        let scope_id = clip.audio_components[0].processing.scope_id;
        let parameter = sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("Scope")
            .processors
            .processors[0]
            .parameters
            .values_mut()
            .next()
            .expect("parameter");
        parameter
            .automation
            .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, -3.0))
            .expect("keyframe");
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(&sequence, clip);
        let processor = &racks[0].processors[0];
        let parameter = &processor.parameters[0];
        assert_eq!(parameter.keyframe_count, 1);
        assert!(set_static_parameter_action(&racks[0], processor, parameter, -9.0).is_none());
    }

    #[test]
    fn insertion_action_carries_a_preset_without_allocating_author_identity_in_projection() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(&sequence, clip);
        let action = insert_action(&racks[0], AudioProcessorBuiltInPreset::LookaheadLimiter);

        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::AudioProcessor(
                AudioProcessorProductAction::InsertBuiltIn(AudioProcessorInsertBuiltInPayload {
                    preset: AudioProcessorBuiltInPreset::LookaheadLimiter,
                    ..
                })
            ))
        ));
    }
}
