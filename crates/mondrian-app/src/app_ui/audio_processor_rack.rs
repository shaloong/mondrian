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

/// Maximum magnitude in the contiguous exact-integer domain of canonical audio f64 values.
pub(crate) const MAX_EXACT_AUDIO_INTEGER: f64 = (1_u64 << f64::MANTISSA_DIGITS) as f64;

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

    fn normalized_static_value(&self, value: f64) -> Option<f64> {
        if !value.is_finite() {
            return None;
        }
        if self.schema.value_type == PropertyValueType::Int
            && (value.fract() != 0.0 || value.abs() > MAX_EXACT_AUDIO_INTEGER)
        {
            return None;
        }
        let contract = self.schema.numeric.as_ref()?.first()?;
        (value >= contract.hard_range.min && value <= contract.hard_range.max).then_some(value)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorInstanceModel {
    pub(crate) definition: AudioProcessorDefinitionRef,
    pub(crate) processor_id: AudioProcessorInstanceId,
    pub(crate) label: String,
    pub(crate) bypassed: bool,
    pub(crate) native_format: Option<NativeAudioFormat>,
    pub(crate) parameters: Vec<AudioProcessorParameterModel>,
    pub(crate) input_query: Option<AudioProcessorInputQueryModel>,
    pub(crate) sidechain_routes: Vec<super::audio_sidechain::SidechainRouteModel>,
    pub(crate) sidechain_inputs: Vec<super::audio_sidechain::SidechainInputModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorRackModel {
    pub(crate) address: AudioProcessorRackAddress,
    pub(crate) input_inspection_target: Option<mondrian_audio::AudioProcessorInputInspectionTarget>,
    pub(crate) title: String,
    pub(crate) ownership_label: String,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
    pub(crate) processors: Vec<AudioProcessorInstanceModel>,
    pub(crate) scope_input_gain_automation: Option<AudioAutomationCurveModel>,
    pub(crate) scope_occurrence_selector: Option<AudioScopeOccurrenceSelectorModel>,
    pub(crate) insert_options: Vec<AudioProcessorInsertOptionModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioScopeOccurrenceSelectorModel {
    pub(crate) clip_id: mondrian_core::ClipId,
    pub(crate) selected_label: String,
    pub(crate) options: Vec<AudioScopeOccurrenceOptionModel>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioScopeOccurrenceOptionModel {
    pub(crate) label: String,
    pub(crate) intent: crate::app::product_action::AudioScopeOccurrencePayload,
}

/// Project every unique Processing Scope bound by one selected Clip.
///
/// Shared Scope lock admission and binding counts span the complete Sequence,
/// matching the Timeline author transaction rather than only the selected Clip.
pub(crate) fn clip_processing_scope_racks(
    sequence: &Sequence,
    clip: &Clip,
    localizer: &Localizer,
    time_context: crate::app_ui::audio_automation::AudioAutomationTimeContext,
    selected_occurrence: impl Fn(
        mondrian_core::AudioProcessingScopeId,
    ) -> Option<mondrian_timeline::AudioComponentAddress>,
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
            let track_id = sequence
                .clip_track_location(clip.id)
                .filter(|location| !location.is_video_track)
                .map(|location| location.track_id);
            let mut bindings =
                clip.audio_components.iter().filter(|edit| edit.processing.scope_id == scope_id);
            let first = bindings.next();
            if bindings.next().is_none() {
                let occurrence = first.and_then(|edit| {
                    Some(mondrian_timeline::AudioComponentAddress {
                        track_id: track_id?,
                        clip_id: clip.id,
                        edit_id: edit.id,
                    })
                });
                return project_audio_processor_rack(
                    sequence,
                    AudioProcessorRackAddress::ProcessingScope { scope_id },
                    localizer,
                    crate::app_ui::audio_automation::AudioAutomationTimeContext {
                        scope_occurrence: occurrence,
                        ..time_context
                    },
                );
            }
            let options = clip
                .audio_components
                .iter()
                .enumerate()
                .filter(|(_, edit)| edit.processing.scope_id == scope_id)
                .filter_map(|(index, edit)| {
                    let address = mondrian_timeline::AudioComponentAddress {
                        track_id: track_id?,
                        clip_id: clip.id,
                        edit_id: edit.id,
                    };
                    let mut args = FluentArgs::new();
                    args.set("index", (index + 1).to_string());
                    let time = edit.processing.scope_in;
                    let label_time = if time.denominator() == 1 {
                        time.numerator().to_string()
                    } else {
                        format!("{}/{}", time.numerator(), time.denominator())
                    };
                    args.set("time", label_time);
                    Some(AudioScopeOccurrenceOptionModel {
                        label: localizer.format("audio-rack-occurrence-option", Some(&args)),
                        intent: crate::app::product_action::AudioScopeOccurrencePayload {
                            sequence_id: sequence.id,
                            scope_id,
                            address,
                        },
                    })
                })
                .collect::<Vec<_>>();
            let explicit = selected_occurrence(scope_id);
            let occurrence = explicit.and_then(|selected| {
                options
                    .iter()
                    .find(|option| {
                        option.intent.address.clip_id == selected.clip_id
                            && option.intent.address.edit_id == selected.edit_id
                    })
                    .map(|option| option.intent.address)
            });
            let time_context = crate::app_ui::audio_automation::AudioAutomationTimeContext {
                scope_occurrence: occurrence,
                ..time_context
            };
            let mut rack = project_audio_processor_rack(
                sequence,
                AudioProcessorRackAddress::ProcessingScope { scope_id },
                localizer,
                time_context,
            );
            if options.len() > 1 {
                rack.scope_occurrence_selector = Some(AudioScopeOccurrenceSelectorModel {
                    clip_id: clip.id,
                    selected_label: options
                        .iter()
                        .find(|option| Some(option.intent.address) == occurrence)
                        .map(|option| option.label.clone())
                        .unwrap_or_else(|| localizer.text("audio-rack-occurrence-choose")),
                    options,
                });
            }
            rack
        })
        .collect()
}

/// Project one Rack through Timeline's authoritative address and edit-admission
/// Interface. This is shared by the Clip Inspector and the Mixer surface.
pub(crate) fn project_audio_processor_rack(
    sequence: &Sequence,
    address: AudioProcessorRackAddress,
    localizer: &Localizer,
    time_context: crate::app_ui::audio_automation::AudioAutomationTimeContext,
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
                    project_processor(
                        sequence,
                        address,
                        automation_viewport,
                        processor,
                        localizer,
                        time_context,
                    )
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
        input_inspection_target: match address {
            AudioProcessorRackAddress::ChannelStrip { owner, rack } => Some(
                mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack },
            ),
            AudioProcessorRackAddress::ProcessingScope { scope_id } => {
                time_context.scope_occurrence.map(|occurrence| {
                    mondrian_audio::AudioProcessorInputInspectionTarget::ProcessingScope {
                        scope_id,
                        occurrence,
                    }
                })
            }
        },
        title: rack_title(address, localizer),
        ownership_label,
        is_editable,
        edit_disabled_reason,
        processors,
        scope_occurrence_selector: None,
        scope_input_gain_automation: match address {
            AudioProcessorRackAddress::ProcessingScope { scope_id } => automation_viewport
                .and_then(|viewport| {
                    project_audio_automation(
                        sequence,
                        mondrian_timeline::AudioAutomationTarget::ProcessingScopeInputGain {
                            scope_id,
                        },
                        viewport,
                        time_context,
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
    time_context: crate::app_ui::audio_automation::AudioAutomationTimeContext,
) -> AudioProcessorInstanceModel {
    let definition_id = match &processor.definition {
        AudioProcessorDefinitionRef::BuiltIn { definition_id, .. } => Some(definition_id.as_str()),
        AudioProcessorDefinitionRef::Vst3 { .. } | AudioProcessorDefinitionRef::Clap { .. } => None,
    };
    AudioProcessorInstanceModel {
        definition: processor.definition.clone(),
        input_query: None,
        sidechain_routes: super::audio_sidechain::retained_routes(
            sequence,
            processor.id,
            localizer,
            time_context,
        ),
        sidechain_inputs: Vec::new(),
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
                        time_context,
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
    value: f64,
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
    fn scope_sidechain_gain_uses_sequence_viewport_instead_of_scope_binding_time() {
        use crate::app_ui::audio_automation::AudioAutomationTimeContext;
        use mondrian_core::AuthoringTimeDomain;
        use mondrian_timeline::audio::{
            AudioChannelStripOutputPort, AudioProcessorSidechainRoute, AudioRouteSource,
        };
        let (mut sequence, _) = sequence_with_gain_scope();
        sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_in =
            TimelineTime::new(5, 1).expect("scope trim");
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        let processor_id = sequence
            .audio_program
            .processing_scopes
            .iter()
            .find(|scope| scope.id == scope_id)
            .expect("scope")
            .processors
            .processors[0]
            .id;
        let detector_id = sequence.add_audio_track();
        let source = AudioRouteSource::Track {
            track_id: detector_id,
            port: AudioChannelStripOutputPort::PreFader,
        };
        sequence.audio_program.sidechain_routes.push(AudioProcessorSidechainRoute::new(
            source,
            processor_id,
            "offline.detector",
        ));
        mondrian_timeline::inspect_audio_route_candidates(&sequence)
            .expect("independent detector keeps the Scope routing graph causal");
        let rack = project_audio_processor_rack(
            &sequence,
            AudioProcessorRackAddress::ProcessingScope { scope_id },
            &chinese(),
            AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                TimelineTime::new(1, 2).expect("playhead"),
            ),
        );
        let gain = rack.processors[0].sidechain_routes[0]
            .gain_automation
            .as_ref()
            .expect("sidechain curve");
        assert_eq!(
            gain.viewport.domain,
            AuthoringTimeDomain::Sequence(sequence.id)
        );
        assert_eq!(gain.viewport.range.start, TimelineTime::ZERO);
        assert_eq!(gain.viewport.range.duration, TimelineTime::ONE);
        assert_eq!(
            gain.current_author_time.as_ref().expect("no Scope occurrence required").time,
            TimelineTime::new(1, 2).expect("Sequence time")
        );
        let parameter =
            rack.processors[0].parameters[0].automation.as_ref().expect("processor curve");
        assert_eq!(
            parameter.viewport.domain,
            AuthoringTimeDomain::AudioProcessingScope(scope_id)
        );
        assert_eq!(
            parameter.viewport.range.start,
            TimelineTime::new(5, 1).expect("Scope time")
        );
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

        let racks = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );

        assert_eq!(racks.len(), 1);
        assert_eq!(
            racks[0].ownership_label.replace(['\u{2068}', '\u{2069}'], ""),
            "共享处理范围 · 2 个音频组件"
        );
        assert_eq!(racks[0].processors[0].label, "增益");
        assert_eq!(racks[0].processors[0].parameters[0].label, "增益");
        assert!(racks[0].is_editable);

        sequence.audio_tracks[0].is_locked = true;
        let clip = &sequence.audio_tracks[0].clips[0];
        let locked = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
        assert!(!locked[0].is_editable);
        assert!(locked[0].edit_disabled_reason.is_some());
    }

    #[test]
    fn english_projection_translates_builtin_copy_without_changing_rack_identity() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let english = Localizer::new(AppUiLocale::EnUs).expect("English catalog");
        let english_rack = &clip_processing_scope_racks(
            &sequence,
            clip,
            &english,
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        )[0];
        let chinese_rack = &clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        )[0];
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

        let projected = addresses.map(|address| {
            project_audio_processor_rack(
                &sequence,
                address,
                &chinese(),
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    &sequence,
                    sequence.playhead,
                ),
            )
        });
        assert_eq!(projected[0].ownership_label, sequence.audio_tracks[0].name);
        assert_eq!(projected[1].ownership_label, "Bus · Dialog");
        assert!(projected[2].ownership_label.starts_with("节目输出 · "));
        assert!(projected.iter().all(|rack| rack.is_editable));

        sequence.audio_tracks[0].is_locked = true;
        let locked = project_audio_processor_rack(
            &sequence,
            addresses[0],
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
        );
        assert!(!locked.is_editable);
        assert!(locked
            .edit_disabled_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("已锁定")));
    }

    #[test]
    fn static_parameter_action_preserves_canonical_double_precision() {
        let (sequence, _) = sequence_with_gain_scope();
        let racks = clip_processing_scope_racks(
            &sequence,
            &sequence.audio_tracks[0].clips[0],
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
        let rack = &racks[0];
        let processor = &rack.processors[0];
        let parameter = &processor.parameters[0];
        let action = set_static_parameter_action(rack, processor, parameter, -6.123456789123456)
            .expect("value");
        let Some(ProductAction::Audio(AudioProductAction::EditProcessorRack(request))) =
            ProductAction::decode_external(&action).expect("decode")
        else {
            panic!("rack edit");
        };
        let AudioProcessorRackEdit::SetParameterStaticValue { value, .. } = request.edit else {
            panic!("static value");
        };
        assert_eq!(value, -6.123456789123456_f64);
        let mut state = crate::app::AppState::new();
        state.test_set_sequence(Some(sequence));
        let snapshot = |state: &crate::app::AppState| {
            let sequence = state.active_sequence().expect("sequence");
            clip_processing_scope_racks(
                sequence,
                &sequence.audio_tracks[0].clips[0],
                &chinese(),
                crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                    sequence,
                    sequence.playhead,
                ),
                |_| None,
            )[0]
            .processors[0]
                .parameters[0]
                .static_value
        };
        let before = snapshot(&state);
        state.dispatch_action(action).expect("author value");
        assert_eq!(snapshot(&state), value);
        assert!(state.undo_timeline().expect("undo"));
        assert_eq!(snapshot(&state), before);
        assert!(state.redo_timeline().expect("redo"));
        assert_eq!(snapshot(&state), value);
    }

    #[test]
    fn actions_preserve_typed_addresses_and_do_not_fake_automated_static_edits() {
        let (mut sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
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
        let racks = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
        let processor = &racks[0].processors[0];
        let parameter = &processor.parameters[0];
        assert_eq!(parameter.keyframe_count, 1);
        assert!(set_static_parameter_action(&racks[0], processor, parameter, -9.0).is_none());
    }

    #[test]
    fn insertion_action_carries_a_preset_without_allocating_author_identity_in_projection() {
        let (sequence, _) = sequence_with_gain_scope();
        let clip = &sequence.audio_tracks[0].clips[0];
        let racks = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
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
        let mut racks = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
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
        let mut racks = clip_processing_scope_racks(
            &sequence,
            clip,
            &chinese(),
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
            |_| None,
        );
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
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
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
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
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
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                &sequence,
                sequence.playhead,
            ),
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

    #[test]
    fn scope_occurrence_navigation_survives_track_move_without_adopting_other_clip() {
        let (mut sequence, clip_id) = sequence_with_gain_scope();
        let old_track_id = sequence.audio_tracks[0].id;
        let scope_id = sequence.audio_tracks[0].clips[0].audio_components[0].processing.scope_id;
        let mut second = mondrian_timeline::audio::AudioComponentEdit::media(
            AudioSourceComponentId::new(),
            scope_id,
        );
        second.processing.scope_in = TimelineTime::new(9, 1).expect("selected Scope origin");
        let old_address = mondrian_timeline::AudioComponentAddress {
            track_id: old_track_id,
            clip_id,
            edit_id: second.id,
        };
        sequence.audio_tracks[0].clips[0].audio_components.push(second);
        let clip = sequence.audio_tracks[0].clips.remove(0);
        let mut destination = mondrian_timeline::Track::new_audio("Destination");
        let new_track_id = destination.id;
        destination.add_clip(clip).expect("moved fixture");
        sequence.audio_tracks.push(destination);
        let clip = sequence.find_clip(clip_id).expect("moved Clip");
        let context = crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
            &sequence,
            TimelineTime::ZERO,
        );
        let racks = clip_processing_scope_racks(&sequence, clip, &chinese(), context, |_| {
            Some(old_address)
        });
        let curve = racks[0].scope_input_gain_automation.as_ref().expect("Scope curve");
        assert_eq!(
            curve
                .current_author_time
                .as_ref()
                .expect("stable occurrence survives Track move")
                .time,
            TimelineTime::new(9, 1).expect("second origin")
        );
        assert_eq!(
            curve.time_context.scope_occurrence,
            Some(mondrian_timeline::AudioComponentAddress {
                track_id: new_track_id,
                ..old_address
            })
        );
        for stale in [
            mondrian_timeline::AudioComponentAddress {
                clip_id: mondrian_core::ClipId::new(),
                ..old_address
            },
            mondrian_timeline::AudioComponentAddress {
                edit_id: mondrian_core::AudioComponentEditId::new(),
                ..old_address
            },
        ] {
            let racks =
                clip_processing_scope_racks(&sequence, clip, &chinese(), context, |_| Some(stale));
            assert!(racks[0]
                .scope_input_gain_automation
                .as_ref()
                .expect("Scope curve")
                .current_author_time
                .is_err());
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AudioProcessorInputQueryModel {
    pub(crate) payload: crate::app::product_action::AudioProcessorInputQueryPayload,
    pub(crate) button_label: String,
    pub(crate) status_label: String,
}

/// Attach cached control observations without loading a native plugin on UI.
pub(crate) fn bind_processor_input_queries(
    racks: &mut [AudioProcessorRackModel],
    state: &crate::app::AppState,
    localizer: &Localizer,
    routing: Option<&mondrian_timeline::AudioRouteCandidateInspection>,
) {
    use crate::app::audio_input_observation::AudioInputObservation;
    use crate::app::product_action::{
        AudioProcessorInputQueryOperation, AudioProcessorInputQueryPayload,
    };
    let (Some(session_id), Some(sequence_id)) =
        (state.authoring_session_id(), state.active_sequence_id())
    else {
        return;
    };
    if state.project_close_blocks_actions() {
        return;
    }
    let Some(sequence) = state.active_sequence() else {
        return;
    };
    let local_routing = std::cell::OnceCell::new();
    for rack in racks {
        let Some(target) = rack.input_inspection_target else {
            continue;
        };
        for processor in &mut rack.processors {
            processor.sidechain_inputs.clear();
            for route in &mut processor.sidechain_routes {
                route.rewire_options.clear();
            }
            if processor.native_format.is_none() {
                continue;
            }
            let mut payload = AudioProcessorInputQueryPayload {
                session_id,
                sequence_id,
                target,
                processor_id: processor.processor_id,
                operation: AudioProcessorInputQueryOperation::Query,
            };
            let observation = state.audio_processor_input_observation(payload);
            let (button_label, status_label) = match observation {
                AudioInputObservation::Unknown => (
                    localizer.text("audio-input-query"),
                    localizer.text("audio-input-not-inspected"),
                ),
                AudioInputObservation::Pending => {
                    payload.operation = AudioProcessorInputQueryOperation::Cancel;
                    (
                        localizer.text("audio-input-cancel"),
                        localizer.text("audio-input-pending"),
                    )
                }
                AudioInputObservation::Ready(observed) => {
                    let inputs = &observed.inputs;
                    super::audio_sidechain::bind_retained_input_names(
                        &mut processor.sidechain_routes,
                        sequence,
                        &inputs.auxiliary_inputs,
                        &inputs.input_names,
                        localizer,
                    );
                    processor.sidechain_inputs = super::audio_sidechain::input_options(
                        sequence,
                        observed.binding,
                        &inputs.auxiliary_inputs,
                        &inputs.input_names,
                        routing.or_else(|| {
                            local_routing
                                .get_or_init(|| {
                                    mondrian_timeline::inspect_audio_route_candidates(sequence).ok()
                                })
                                .as_ref()
                        }),
                        localizer,
                    );
                    super::audio_sidechain::bind_rewire_options(
                        &mut processor.sidechain_routes,
                        &processor.sidechain_inputs,
                    );
                    let status = if inputs.auxiliary_inputs.buses.is_empty() {
                        localizer.text("audio-input-none")
                    } else {
                        let mut args = FluentArgs::new();
                        args.set("count", inputs.auxiliary_inputs.buses.len() as f64);
                        localizer.format("audio-input-ready", Some(&args))
                    };
                    (localizer.text("audio-input-requery"), status)
                }
                AudioInputObservation::Failed(error) => (
                    localizer.text("audio-input-requery"),
                    localizer.format_text("audio-input-failed", "error", &error.to_string()),
                ),
            };
            processor.input_query =
                Some(AudioProcessorInputQueryModel { payload, button_label, status_label });
        }
    }
}

#[cfg(test)]
mod input_query_tests {
    use crate::app::audio_input_observation::tests::{finish, fixture};
    use crate::app::audio_input_observation::AudioInputObservation;
    use crate::app::product_action::{
        AudioProcessorInputQueryOperation, AudioProductAction, ProductAction,
    };
    use crate::app::AppState;
    use mondrian_audio::AudioProcessorInputInspectionTarget;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn input_observation_rack_controls_localize_query_cancel_refresh_and_failure_without_probing() {
        use super::{bind_processor_input_queries, project_audio_processor_rack};
        use crate::app_ui::audio_automation::AudioAutomationTimeContext;
        use crate::app_ui::localization::{AppUiLocale, Localizer};
        use mondrian_timeline::{audio::AudioProcessorDefinitionRef, AudioProcessorRackAddress};
        for vst3 in [false, true] {
            let (mut state, payloads, calls, started, release) = fixture(1);
            let payload = payloads[0];
            state
                .active_sequence_mut_uncommitted()
                .expect("Fixture Sequence")
                .audio_program
                .outputs[0]
                .strip
                .pre_fader
                .processors[0]
                .definition = if vst3 {
                AudioProcessorDefinitionRef::Vst3 {
                    class_id: "00000000000000000000000000000001".to_owned(),
                    vendor: None,
                    schema_version: 1,
                    binary_sha256: Some([7; 32]),
                }
            } else {
                AudioProcessorDefinitionRef::Clap {
                    plugin_id: "fixture.inputs".to_owned(),
                    schema_version: 1,
                    binary_sha256: Some([7; 32]),
                }
            };
            let project = |state: &AppState, locale| {
                let localizer = Localizer::new(locale).expect("Input control locale");
                let sequence = state.active_sequence().expect("Fixture Sequence");
                let AudioProcessorInputInspectionTarget::ChannelStrip { owner, rack } =
                    payload.target
                else {
                    panic!("Fixture channel target");
                };
                let mut racks = vec![project_audio_processor_rack(
                    sequence,
                    AudioProcessorRackAddress::ChannelStrip { owner, rack },
                    &localizer,
                    AudioAutomationTimeContext::at_sequence_time(sequence, sequence.playhead),
                )];
                bind_processor_input_queries(&mut racks, state, &localizer, None);
                if !racks[0].processors[0].sidechain_inputs.is_empty() {
                    let inputs = &racks[0].processors[0].sidechain_inputs;
                    assert_eq!(inputs.len(), 2);
                    assert_eq!(
                        inputs[0].label,
                        localizer.format_text("audio-sidechain-input", "identity", "压缩探测 🙂")
                    );
                    assert_eq!(
                        inputs[1].label,
                        localizer.format_text("audio-sidechain-input", "identity", "备用探测")
                    );
                    assert!(inputs[0]
                        .options
                        .iter()
                        .all(|option| option.payload.bus_key == "detector.11"));
                    assert!(inputs[1]
                        .options
                        .iter()
                        .all(|option| option.payload.bus_key == "detector.7"));
                }
                let model =
                    racks[0].processors[0].input_query.clone().expect("Native query control");
                (model, localizer)
            };
            for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
                let (model, localizer) = project(&state, locale);
                assert_eq!(model.button_label, localizer.text("audio-input-query"));
                assert_eq!(
                    model.status_label,
                    localizer.text("audio-input-not-inspected")
                );
                assert_eq!(model.payload, payload);
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "Projection never loads native code"
            );
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::QueryProcessorInputs(payload))
                        .into_external_action(),
                )
                .expect("Actual control dispatch");
            started
                .recv_timeout(Duration::from_secs(2))
                .expect("Controlled native adapter query");
            let (pending, localizer) = project(&state, AppUiLocale::EnUs);
            assert_eq!(pending.button_label, localizer.text("audio-input-cancel"));
            assert_eq!(
                pending.payload.operation,
                AudioProcessorInputQueryOperation::Cancel
            );
            release.send(()).expect("Resolve fixture ports");
            finish(&mut state);
            for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
                let (ready, localizer) = project(&state, locale);
                assert_eq!(ready.button_label, localizer.text("audio-input-requery"));
                let mut args = fluent_bundle::FluentArgs::new();
                args.set("count", 2);
                assert_eq!(
                    ready.status_label,
                    localizer.format("audio-input-ready", Some(&args))
                );
                assert_eq!(
                    ready.payload.operation,
                    AudioProcessorInputQueryOperation::Query
                );
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "Locale/model updates only read facts"
            );
            for _ in 0..100 {
                let _ = project(&state, AppUiLocale::EnUs);
            }
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "Repeated UI reads never prepare native adapters"
            );
            // Actual failed adapter preparation remains a quiet localized, retryable control state.
            state.active_sequence_mut_uncommitted().expect("Sequence").audio_program.outputs[0]
                .strip
                .pre_fader
                .processors[0]
                .definition = AudioProcessorDefinitionRef::Clap {
                plugin_id: "fixture.missing".to_owned(),
                schema_version: 1,
                binary_sha256: Some([7; 32]),
            };
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::QueryProcessorInputs(payload))
                        .into_external_action(),
                )
                .expect("Retry changed definition");
            started.recv_timeout(Duration::from_secs(2)).expect("Missing adapter query");
            release.send(()).expect("Finish failed query");
            finish(&mut state);
            let (failed, localizer) = project(&state, AppUiLocale::ZhCn);
            assert_eq!(failed.button_label, localizer.text("audio-input-requery"));
            let AudioInputObservation::Failed(error) =
                state.audio_processor_input_observation(payload)
            else {
                panic!("Expected actual adapter failure");
            };
            assert_eq!(
                failed.status_label,
                localizer.format_text("audio-input-failed", "error", &error.to_string())
            );
            assert!(!state.can_undo_action());
        }
    }

    #[test]
    fn input_observation_idle_native_wake_stops_only_after_owned_work_terminates() {
        use crate::app_ui::host::AppUiHost;
        use crate::app_ui::preferences_store::AppUiPreferences;
        let (state, payloads, _calls, started, release) = fixture(1);
        let path = std::env::temp_dir().join(format!(
            "mondrian-input-wake-{}.json",
            payloads[0].sequence_id
        ));
        let host = AppUiHost::new_with_preferences_path(state, AppUiPreferences::default(), path);
        assert_eq!(host.native_audio_control_next_wake_delay(), None);
        host.with_validation_app_state_mut(|state| {
            state.dispatch_action(
                ProductAction::Audio(AudioProductAction::QueryProcessorInputs(payloads[0]))
                    .into_external_action(),
            )
        })
        .expect("Start query through native host state");
        started.recv_timeout(Duration::from_secs(2)).expect("Worker started");
        assert_eq!(
            host.native_audio_control_next_wake_delay(),
            Some(Duration::from_millis(25))
        );
        let mut cancel = payloads[0];
        cancel.operation = AudioProcessorInputQueryOperation::Cancel;
        host.with_validation_app_state_mut(|state| {
            state.dispatch_action(
                ProductAction::Audio(AudioProductAction::QueryProcessorInputs(cancel))
                    .into_external_action(),
            )
        })
        .expect("Cancel query without detaching worker");
        assert!(host.native_audio_control_next_wake_delay().is_some());
        release.send(()).expect("Terminate canceled worker");
        host.with_validation_app_state_mut(finish);
        assert_eq!(host.native_audio_control_next_wake_delay(), None);
    }
}
