//! Shared Audio Processor Rack projection and typed Widget action factories.
//!
//! Inspector and future Mixer surfaces consume this Module instead of
//! traversing or interpreting the Sequence audio author model independently.

use fluent_bundle::FluentArgs;
use mondrian_audio::{ClapPluginDescriptor, Vst3PluginDescriptor};
use mondrian_core::automation::{ParameterSchema, PropertyValueType};
use mondrian_core::{AudioProcessorInstanceId, ParameterId};
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
    inspect_audio_processor_rack, AudioChannelStripOwner, AudioChannelStripRack,
    AudioProcessorRackAddress, AudioProcessorRackEdit, AudioProcessorRackEditError,
    AudioProcessorRackEditRequest, AudioProcessorRackInspection, AudioProcessorRackPlacement,
};

use super::audio_automation::{
    processing_scope_automation_viewport, project_audio_automation, sequence_automation_viewport,
    AudioAutomationCurveModel, AudioAutomationViewport,
};
use super::localization::Localizer;
use crate::app::product_action::{
    AudioProcessorBuiltInPreset, AudioProcessorInsertBuiltInPayload,
    AudioProcessorInsertClapPayload, AudioProcessorInsertVst3Payload,
    AudioProcessorRebindClapPayload, AudioProcessorRebindVst3Payload, AudioProductAction,
    ProductAction,
};
use crate::app::ui_actions::{
    audio_processor_insert_built_in_action, audio_processor_rack_edit_action,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioProcessorInsertOptionModel {
    pub(crate) label: String,
    pub(crate) choice: AudioProcessorInsertChoice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudioProcessorInsertChoice {
    BuiltIn(AudioProcessorBuiltInPreset),
    Clap(String),
    Vst3(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeAudioFormat {
    Clap,
    Vst3,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorParameterModel {
    pub(crate) parameter_id: ParameterId,
    pub(crate) label: String,
    pub(crate) schema: ParameterSchema,
    pub(crate) static_value: f64,
    pub(crate) keyframe_count: usize,
    pub(crate) automation: Option<AudioAutomationCurveModel>,
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
    pub(crate) native_format: Option<NativeAudioFormat>,
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
    pub(crate) scope_input_gain_automation: Option<AudioAutomationCurveModel>,
    pub(crate) insert_options: Vec<AudioProcessorInsertOptionModel>,
}

/// Project every unique Processing Scope bound by one selected Clip.
///
/// Shared Scope lock admission and binding counts span the complete Sequence,
/// matching the Timeline author transaction rather than only the selected Clip.
pub(crate) fn clip_processing_scope_racks(
    sequence: &Sequence,
    clip: &Clip,
    localizer: &Localizer,
) -> Vec<AudioProcessorRackModel> {
    let mut scope_ids = clip
        .audio_components
        .iter()
        .map(|edit| edit.processing.scope_id)
        .collect::<Vec<_>>();
    scope_ids.sort_unstable();
    scope_ids.dedup();
    scope_ids
        .into_iter()
        .map(|scope_id| {
            project_audio_processor_rack(
                sequence,
                AudioProcessorRackAddress::ProcessingScope { scope_id },
                localizer,
            )
        })
        .collect()
}

/// Project one Rack through Timeline's authoritative address and edit-admission
/// Interface. This is shared by the Clip Inspector and the Mixer surface.
pub(crate) fn project_audio_processor_rack(
    sequence: &Sequence,
    address: AudioProcessorRackAddress,
    localizer: &Localizer,
) -> AudioProcessorRackModel {
    let automation_viewport = rack_automation_viewport(sequence, address);
    let inspection = inspect_audio_processor_rack(sequence, &address);
    let ownership_label =
        rack_ownership_label(sequence, address, inspection.as_ref().ok(), localizer);
    let (is_editable, edit_disabled_reason, processors) = match inspection {
        Ok(inspection) => (
            inspection.is_editable(),
            inspection.edit_blocker().map(|blocker| edit_blocker_label(blocker, localizer)),
            inspection
                .rack()
                .processors
                .iter()
                .map(|processor| {
                    project_processor(sequence, address, automation_viewport, processor, localizer)
                })
                .collect(),
        ),
        Err(error) => (
            false,
            Some(localizer.format_text("audio-rack-invalid-state", "error", &error.to_string())),
            Vec::new(),
        ),
    };
    AudioProcessorRackModel {
        address,
        title: rack_title(address, localizer),
        ownership_label,
        is_editable,
        edit_disabled_reason,
        processors,
        scope_input_gain_automation: match address {
            AudioProcessorRackAddress::ProcessingScope { scope_id } => automation_viewport
                .and_then(|viewport| {
                    project_audio_automation(
                        sequence,
                        mondrian_timeline::AudioAutomationTarget::ProcessingScopeInputGain {
                            scope_id,
                        },
                        viewport,
                    )
                }),
            AudioProcessorRackAddress::ChannelStrip { .. } => None,
        },
        insert_options: vec![
            AudioProcessorInsertOptionModel {
                label: localizer.text("audio-processor-gain"),
                choice: AudioProcessorInsertChoice::BuiltIn(AudioProcessorBuiltInPreset::Gain),
            },
            AudioProcessorInsertOptionModel {
                label: localizer.text("audio-processor-lookahead-limiter"),
                choice: AudioProcessorInsertChoice::BuiltIn(
                    AudioProcessorBuiltInPreset::LookaheadLimiter,
                ),
            },
        ],
    }
}

pub(crate) fn append_clap_insert_options(
    racks: &mut [AudioProcessorRackModel],
    descriptors: &[ClapPluginDescriptor],
) {
    for rack in racks {
        rack.insert_options.extend(descriptors.iter().map(|descriptor| {
            let label =
                descriptor.vendor.as_deref().filter(|vendor| !vendor.is_empty()).map_or_else(
                    || descriptor.name.clone(),
                    |vendor| format!("{vendor} · {}", descriptor.name),
                );
            AudioProcessorInsertOptionModel {
                label,
                choice: AudioProcessorInsertChoice::Clap(descriptor.plugin_id.clone()),
            }
        }));
    }
}

pub(crate) fn append_vst3_insert_options(
    racks: &mut [AudioProcessorRackModel],
    descriptors: &[Vst3PluginDescriptor],
) {
    for rack in racks {
        rack.insert_options.extend(descriptors.iter().map(|descriptor| {
            let label =
                descriptor.vendor.as_deref().filter(|vendor| !vendor.is_empty()).map_or_else(
                    || descriptor.name.clone(),
                    |vendor| format!("{vendor} · {}", descriptor.name),
                );
            AudioProcessorInsertOptionModel {
                label,
                choice: AudioProcessorInsertChoice::Vst3(descriptor.class_id.clone()),
            }
        }));
    }
}

pub(crate) fn insert_option_action(
    rack: &AudioProcessorRackModel,
    choice: &AudioProcessorInsertChoice,
) -> Action {
    match choice {
        AudioProcessorInsertChoice::BuiltIn(preset) => insert_action(rack, *preset),
        AudioProcessorInsertChoice::Clap(plugin_id) => ProductAction::Audio(
            AudioProductAction::InsertClapProcessor(AudioProcessorInsertClapPayload {
                address: rack.address,
                plugin_id: plugin_id.clone(),
                placement: AudioProcessorRackPlacement::End,
            }),
        )
        .into_external_action(),
        AudioProcessorInsertChoice::Vst3(class_id) => ProductAction::Audio(
            AudioProductAction::InsertVst3Processor(AudioProcessorInsertVst3Payload {
                address: rack.address,
                class_id: class_id.clone(),
                placement: AudioProcessorRackPlacement::End,
            }),
        )
        .into_external_action(),
    }
}

fn rack_title(address: AudioProcessorRackAddress, localizer: &Localizer) -> String {
    localizer.text(match address {
        AudioProcessorRackAddress::ProcessingScope { .. } => "audio-rack-scope-title",
        AudioProcessorRackAddress::ChannelStrip {
            rack: AudioChannelStripRack::PreFader, ..
        } => "audio-rack-prefader-title",
        AudioProcessorRackAddress::ChannelStrip {
            rack: AudioChannelStripRack::PostFader, ..
        } => "audio-rack-postfader-title",
    })
}

fn rack_ownership_label(
    sequence: &Sequence,
    address: AudioProcessorRackAddress,
    inspection: Option<&AudioProcessorRackInspection<'_>>,
    localizer: &Localizer,
) -> String {
    match address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => {
            match inspection.and_then(AudioProcessorRackInspection::processing_scope_binding_count)
            {
                Some(0) => localizer.text("audio-rack-scope-unbound"),
                Some(1) => localizer.text("audio-rack-scope-single"),
                Some(count) => {
                    let mut args = FluentArgs::new();
                    args.set("count", count);
                    localizer.format("audio-rack-scope-shared", Some(&args))
                }
                None => localizer.format_text(
                    "audio-rack-scope-identity",
                    "identity",
                    &scope_id.to_string(),
                ),
            }
        }
        AudioProcessorRackAddress::ChannelStrip { owner, .. } => match owner {
            AudioChannelStripOwner::Track { track_id } => {
                sequence.audio_tracks.iter().find(|track| track.id == track_id).map_or_else(
                    || {
                        localizer.format_text(
                            "audio-rack-track-identity",
                            "identity",
                            &track_id.to_string(),
                        )
                    },
                    |track| track.name.clone(),
                )
            }
            AudioChannelStripOwner::Bus { bus_id } => {
                sequence.audio_program.buses.iter().find(|bus| bus.id == bus_id).map_or_else(
                    || format!("Bus · {bus_id}"),
                    |bus| format!("Bus · {}", bus.name),
                )
            }
            AudioChannelStripOwner::ProgramOutput { output_id } => sequence
                .audio_program
                .outputs
                .iter()
                .find(|output| output.id == output_id)
                .map_or_else(
                    || {
                        localizer.format_text(
                            "audio-mixer-missing-output",
                            "identity",
                            &output_id.to_string(),
                        )
                    },
                    |output| localizer.format_text("audio-mixer-output", "identity", &output.name),
                ),
        },
    }
}

fn edit_blocker_label(error: &AudioProcessorRackEditError, localizer: &Localizer) -> String {
    match error {
        AudioProcessorRackEditError::LockedTrack(track_id) => {
            localizer.format_text("audio-rack-locked-track", "identity", &track_id.to_string())
        }
        AudioProcessorRackEditError::LockedProcessingScopeBinding { track_id, .. } => localizer
            .format_text(
                "audio-rack-locked-scope-track",
                "identity",
                &track_id.to_string(),
            ),
        error => error.to_string(),
    }
}

fn rack_automation_viewport(
    sequence: &Sequence,
    address: AudioProcessorRackAddress,
) -> Option<AudioAutomationViewport> {
    match address {
        AudioProcessorRackAddress::ProcessingScope { scope_id } => {
            processing_scope_automation_viewport(sequence, scope_id)
        }
        AudioProcessorRackAddress::ChannelStrip { .. } => sequence_automation_viewport(sequence),
    }
}

fn project_processor(
    sequence: &Sequence,
    address: AudioProcessorRackAddress,
    viewport: Option<AudioAutomationViewport>,
    processor: &AudioProcessorInstance,
    localizer: &Localizer,
) -> AudioProcessorInstanceModel {
    let definition_id = match &processor.definition {
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. } => Some(definition_id.as_str()),
        AudioProcessorDefinitionRef::Vst3 { .. } | AudioProcessorDefinitionRef::Clap { .. } => None,
    };
    AudioProcessorInstanceModel {
        processor_id: processor.id,
        label: processor_label(&processor.definition, localizer),
        bypassed: processor.bypassed,
        native_format: match processor.definition {
            AudioProcessorDefinitionRef::Clap { .. } => Some(NativeAudioFormat::Clap),
            AudioProcessorDefinitionRef::Vst3 { .. } => Some(NativeAudioFormat::Vst3),
            AudioProcessorDefinitionRef::BuiltIn { .. } => None,
        },
        parameters: processor
            .parameters
            .values()
            .filter(|parameter| !parameter.ui.hidden)
            .map(|parameter| {
                let parameter_id = parameter.schema.parameter_id.clone();
                let automation = viewport.and_then(|viewport| {
                    project_audio_automation(
                        sequence,
                        mondrian_timeline::AudioAutomationTarget::ProcessorParameter {
                            rack: address,
                            processor_id: processor.id,
                            parameter_id: parameter_id.clone(),
                        },
                        viewport,
                    )
                });
                AudioProcessorParameterModel {
                    parameter_id: parameter_id.clone(),
                    label: parameter.ui.display_name.clone().unwrap_or_else(|| {
                        parameter_label(definition_id, &parameter_id, localizer)
                    }),
                    schema: parameter.schema.clone(),
                    static_value: parameter.automation.default_value,
                    keyframe_count: parameter.automation.keyframes.len(),
                    automation,
                }
            })
            .collect(),
    }
}

fn processor_label(definition: &AudioProcessorDefinitionRef, localizer: &Localizer) -> String {
    match definition {
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. }
            if definition_id == BUILTIN_GAIN_DEFINITION_ID =>
        {
            localizer.text("audio-processor-gain")
        }
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. }
            if definition_id == BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID =>
        {
            localizer.text("audio-processor-lookahead-limiter")
        }
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. }
            if definition_id == BUILTIN_SAMPLE_DELAY_DEFINITION_ID =>
        {
            localizer.text("audio-processor-sample-delay")
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

fn parameter_label(
    definition_id: Option<&str>,
    parameter_id: &ParameterId,
    localizer: &Localizer,
) -> String {
    match (definition_id, parameter_id.as_str()) {
        (Some(BUILTIN_GAIN_DEFINITION_ID), GAIN_DB_PARAMETER_ID) => {
            localizer.text("audio-parameter-gain")
        }
        (
            Some(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID),
            LOOKAHEAD_LIMITER_CEILING_DB_PARAMETER_ID,
        ) => localizer.text("audio-parameter-ceiling"),
        (
            Some(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID),
            LOOKAHEAD_LIMITER_LOOKAHEAD_MS_PARAMETER_ID,
        ) => localizer.text("audio-parameter-lookahead"),
        (
            Some(BUILTIN_LOOKAHEAD_LIMITER_DEFINITION_ID),
            LOOKAHEAD_LIMITER_RELEASE_MS_PARAMETER_ID,
        ) => localizer.text("audio-parameter-release"),
        (Some(BUILTIN_SAMPLE_DELAY_DEFINITION_ID), SAMPLE_DELAY_FRAMES_PARAMETER_ID) => {
            localizer.text("audio-parameter-delay-samples")
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

pub(crate) fn rebind_native_action(
    rack: &AudioProcessorRackModel,
    processor: &AudioProcessorInstanceModel,
) -> Option<Action> {
    if !rack.is_editable {
        return None;
    }
    let action = match processor.native_format? {
        NativeAudioFormat::Clap => {
            AudioProductAction::RebindClapProcessor(AudioProcessorRebindClapPayload {
                address: rack.address,
                processor_id: processor.processor_id,
            })
        }
        NativeAudioFormat::Vst3 => {
            AudioProductAction::RebindVst3Processor(AudioProcessorRebindVst3Payload {
                address: rack.address,
                processor_id: processor.processor_id,
            })
        }
    };
    Some(ProductAction::Audio(action).into_external_action())
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
        AudioProcessorRackEdit::SetParameterStaticValue {
            processor_id: processor.processor_id,
            parameter_id: parameter.parameter_id.clone(),
            value,
        },
    ))
}

fn rack_edit_action(address: AudioProcessorRackAddress, edit: AudioProcessorRackEdit) -> Action {
    audio_processor_rack_edit_action(AudioProcessorRackEditRequest { address, edit })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use crate::app_ui::localization::AppUiLocale;
    use mondrian_core::automation::PropertyValue;
    use mondrian_core::{AudioSourceComponentId, ExactAutomationKeyframe, TimelineTime};

    fn chinese() -> Localizer {
        Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog")
    }

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

        let racks = clip_processing_scope_racks(&sequence, clip, &chinese());

        assert_eq!(racks.len(), 1);
        assert_eq!(
            racks[0].ownership_label.replace(['\u{2068}', '\u{2069}'], ""),
            "共享 Processing Scope · 2 个 Component"
        );
        assert_eq!(racks[0].processors[0].label, "增益");
        assert_eq!(racks[0].processors[0].parameters[0].label, "增益");
        assert!(racks[0].is_editable);

        sequence.audio_tracks[0].is_locked = true;
        let clip = &sequence.audio_tracks[0].clips[0];
        let locked = clip_processing_scope_racks(&sequence, clip, &chinese());
        assert!(!locked[0].is_editable);
        assert!(locked[0].edit_disabled_reason.is_some());
    }

    #[test]
    fn english_projection_translates_builtin_copy_without_changing_rack_identity() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        let english_rack = &clip_processing_scope_racks(&sequence, clip, &english)[0];
        let chinese_rack = &clip_processing_scope_racks(&sequence, clip, &chinese())[0];
        assert_eq!(english_rack.title, "Audio Processor Rack");
        assert_eq!(english_rack.processors[0].label, "Gain");
        assert_eq!(english_rack.processors[0].parameters[0].label, "Gain");
        assert_eq!(english_rack.address, chinese_rack.address);
        assert_eq!(
            english_rack.processors[0].processor_id,
            chinese_rack.processors[0].processor_id
        );
    }

    #[test]
    fn one_projector_covers_track_bus_and_program_output_racks() {
        let (mut sequence, _) = sequence_with_gain_scope();
        let track_id = sequence.audio_tracks[0].id;
        let bus_id = mondrian_core::MixBusId::new();
        sequence.audio_program.buses.push(mondrian_timeline::audio::AudioMixBus {
            id: bus_id,
            name: "Dialog".to_owned(),
            strip: mondrian_timeline::audio::AudioChannelStrip::default(),
        });
        let output_id = sequence.audio_program.outputs[0].id;
        let addresses = [
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Track { track_id },
                rack: AudioChannelStripRack::PreFader,
            },
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Bus { bus_id },
                rack: AudioChannelStripRack::PostFader,
            },
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::ProgramOutput { output_id },
                rack: AudioChannelStripRack::PreFader,
            },
        ];

        let projected =
            addresses.map(|address| project_audio_processor_rack(&sequence, address, &chinese()));
        assert_eq!(projected[0].ownership_label, sequence.audio_tracks[0].name);
        assert_eq!(projected[1].ownership_label, "Bus · Dialog");
        assert!(projected[2].ownership_label.starts_with("节目输出 · "));
        assert!(projected.iter().all(|rack| rack.is_editable));

        sequence.audio_tracks[0].is_locked = true;
        let locked = project_audio_processor_rack(&sequence, addresses[0], &chinese());
        assert!(!locked.is_editable);
        assert!(locked
            .edit_disabled_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("已锁定")));
    }

    #[test]
    fn actions_preserve_typed_addresses_and_do_not_fake_automated_static_edits() {
        let (mut sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(&sequence, clip, &chinese());
        let rack = &racks[0];
        let processor = &rack.processors[0];
        let parameter = &processor.parameters[0];

        let action = set_static_parameter_action(rack, processor, parameter, -6.0)
            .expect("valid static edit");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::EditProcessorRack(
                AudioProcessorRackEditRequest {
                    address: AudioProcessorRackAddress::ProcessingScope { .. },
                    edit: AudioProcessorRackEdit::SetParameterStaticValue { .. },
                }
            )))
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
        let racks = clip_processing_scope_racks(&sequence, clip, &chinese());
        let processor = &racks[0].processors[0];
        let parameter = &processor.parameters[0];
        assert_eq!(parameter.keyframe_count, 1);
        assert!(set_static_parameter_action(&racks[0], processor, parameter, -9.0).is_none());
    }

    #[test]
    fn insertion_action_carries_a_preset_without_allocating_author_identity_in_projection() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(&sequence, clip, &chinese());
        let action = insert_action(&racks[0], AudioProcessorBuiltInPreset::LookaheadLimiter);

        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(
                AudioProductAction::InsertBuiltInProcessor(AudioProcessorInsertBuiltInPayload {
                    preset: AudioProcessorBuiltInPreset::LookaheadLimiter,
                    ..
                })
            ))
        ));
    }

    #[test]
    fn installed_clap_option_preserves_plugin_identity_and_rack_address() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let mut racks = clip_processing_scope_racks(&sequence, clip, &chinese());
        append_clap_insert_options(
            &mut racks,
            &[ClapPluginDescriptor {
                plugin_id: "org.example.gain".to_owned(),
                name: "Gain".to_owned(),
                vendor: Some("Example".to_owned()),
                version: None,
            }],
        );
        let rack = &racks[0];
        assert_eq!(rack.insert_options[2].label, "Example · Gain");
        let action = insert_option_action(rack, &rack.insert_options[2].choice);
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::InsertClapProcessor(
                AudioProcessorInsertClapPayload {
                    address,
                    plugin_id,
                    placement: AudioProcessorRackPlacement::End,
                }
            ))) if address == rack.address && plugin_id == "org.example.gain"
        ));
    }

    #[test]
    fn installed_vst3_option_preserves_class_identity_and_rack_address() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let mut racks = clip_processing_scope_racks(&sequence, clip, &chinese());
        let class_id = "0123456789ABCDEF0123456789ABCDEF".to_owned();
        append_vst3_insert_options(
            &mut racks,
            &[Vst3PluginDescriptor {
                class_id: class_id.clone(),
                name: "Gain".to_owned(),
                vendor: Some("Example".to_owned()),
                version: None,
            }],
        );
        let rack = &racks[0];
        assert_eq!(rack.insert_options[2].label, "Example · Gain");
        let action = insert_option_action(rack, &rack.insert_options[2].choice);
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::InsertVst3Processor(
                AudioProcessorInsertVst3Payload { address, class_id: actual, placement: AudioProcessorRackPlacement::End }
            ))) if address == rack.address && actual == class_id
        ));
    }

    #[test]
    fn clap_rack_projection_exposes_explicit_rebind_action() {
        let (mut sequence, _) = sequence_with_gain_scope();
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        let processor = &mut sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("scope")
            .processors
            .processors[0];
        processor.definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "org.example.gain".to_owned(),
            schema_version: 1,
            binary_sha256: None,
        };
        let rack = project_audio_processor_rack(
            &sequence,
            AudioProcessorRackAddress::ProcessingScope { scope_id },
            &chinese(),
        );
        let instance = &rack.processors[0];
        assert_eq!(instance.native_format, Some(NativeAudioFormat::Clap));
        let action = rebind_native_action(&rack, instance).expect("rebind action");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::RebindClapProcessor(payload)))
                if payload.address == rack.address && payload.processor_id == instance.processor_id
        ));
        sequence.audio_tracks[0].is_locked = true;
        let locked = project_audio_processor_rack(
            &sequence,
            AudioProcessorRackAddress::ProcessingScope { scope_id },
            &chinese(),
        );
        assert!(rebind_native_action(&locked, &locked.processors[0]).is_none());
    }

    #[test]
    fn clap_projection_uses_persisted_names_and_keeps_hidden_parameters_out_of_controls() {
        let (mut sequence, _) = sequence_with_gain_scope();
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        let processor = &mut sequence
            .audio_program
            .processing_scopes
            .iter_mut()
            .find(|scope| scope.id == scope_id)
            .expect("scope")
            .processors
            .processors[0];
        processor.definition = AudioProcessorDefinitionRef::Clap {
            plugin_id: "org.example.gain".to_owned(),
            schema_version: 1,
            binary_sha256: Some([7; 32]),
        };
        processor.parameters.values_mut().next().expect("gain").ui =
            mondrian_timeline::audio::AudioProcessorParameterUiMetadata {
                display_name: Some("Output Gain".to_owned()),
                hidden: false,
            };
        let hidden_id = ParameterId::new("clap.param.99").expect("hidden parameter ID");
        let hidden = mondrian_timeline::audio::AudioProcessorParameter::from_schema(
            ParameterSchema::v1(hidden_id.clone(), PropertyValue::Double(0.5)),
        )
        .expect("hidden parameter")
        .with_ui_metadata(
            mondrian_timeline::audio::AudioProcessorParameterUiMetadata {
                display_name: Some("Private meter".to_owned()),
                hidden: true,
            },
        )
        .expect("hidden UI facts");
        processor.parameters.insert(hidden_id, hidden);

        let rack = project_audio_processor_rack(
            &sequence,
            AudioProcessorRackAddress::ProcessingScope { scope_id },
            &chinese(),
        );
        assert_eq!(rack.processors[0].parameters.len(), 1);
        assert_eq!(rack.processors[0].parameters[0].label, "Output Gain");
        let authored = sequence
            .audio_program
            .processing_scopes
            .iter()
            .find(|scope| scope.id == scope_id)
            .expect("authored scope");
        assert_eq!(authored.processors.processors[0].parameters.len(), 2);
    }
}
