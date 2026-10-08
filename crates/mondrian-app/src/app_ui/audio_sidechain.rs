//! Shared processor sidechain projection over canonical routing admission.

use super::localization::Localizer;
use mondrian_core::{AudioProcessorInstanceId, AudioRouteId};
use mondrian_editor_state::Action;
use mondrian_timeline::audio::{AudioChannelStripOutputPort, AudioRouteSource};
use mondrian_timeline::{
    AudioRouteCandidateInspection, AudioRoutingEdit, AudioRoutingEditRequest, Sequence,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SidechainRouteModel {
    pub(crate) route_id: AudioRouteId,
    pub(crate) source: AudioRouteSource,
    pub(crate) source_label: String,
    pub(crate) rewire_options: Vec<SidechainRewireOption>,
    pub(crate) bus_key: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) static_gain: Option<f64>,
    pub(crate) automation_key_count: usize,
    pub(crate) is_editable: bool,
    pub(crate) disabled_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SidechainCreateOption {
    pub(crate) label: String,
    pub(crate) payload: crate::app::product_action::AudioProcessorInputConnectionPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SidechainRewireOption {
    pub(crate) label: String,
    pub(crate) payload: crate::app::product_action::AudioProcessorInputRewirePayload,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SidechainInputModel {
    pub(crate) label: String,
    pub(crate) options: Vec<SidechainCreateOption>,
}

pub(crate) fn retained_routes(
    sequence: &Sequence,
    processor_id: AudioProcessorInstanceId,
    localizer: &Localizer,
) -> Vec<SidechainRouteModel> {
    sequence
        .audio_program
        .sidechain_routes
        .iter()
        .filter(|route| route.processor_id == processor_id)
        .map(|route| {
            let inspection =
                mondrian_timeline::inspect_audio_processor_sidechain(sequence, route.id);
            let is_editable = inspection.as_ref().is_ok_and(|value| value.is_editable());
            let disabled_reason = match inspection {
                Ok(value) => value.edit_blocker().map(|blocker| match blocker {
                    mondrian_timeline::AudioRoutingEditBlocker::LockedTrack(track_id) => localizer
                        .format_text("audio-rack-locked-track", "identity", &track_id.to_string()),
                }),
                Err(error) => Some(error.to_string()),
            };
            SidechainRouteModel {
                route_id: route.id,
                source: route.source,
                source_label: source_label(sequence, route.source, localizer),
                rewire_options: Vec::new(),
                bus_key: route.bus_key.clone(),
                label: format!(
                    "{} → {}",
                    source_label(sequence, route.source, localizer),
                    route.bus_key
                ),
                enabled: route.enabled,
                static_gain: route.gain_automation.is_none().then_some(route.gain_db),
                automation_key_count: route
                    .gain_automation
                    .as_ref()
                    .map_or(0, |curve| curve.keyframes.len()),
                is_editable,
                disabled_reason,
            }
        })
        .collect()
}

pub(crate) fn input_options(
    sequence: &Sequence,
    binding: crate::app::product_action::AudioProcessorInputBinding,
    inputs: &mondrian_audio::AudioProcessorAuxiliaryInputContract,
    input_names: &[mondrian_audio::AudioProcessorInputName],
    admission: Option<&AudioRouteCandidateInspection>,
    localizer: &Localizer,
) -> Vec<SidechainInputModel> {
    let sources = sequence
        .audio_tracks
        .iter()
        .flat_map(|track| {
            [
                AudioRouteSource::Track {
                    track_id: track.id,
                    port: AudioChannelStripOutputPort::PreFader,
                },
                AudioRouteSource::Track {
                    track_id: track.id,
                    port: AudioChannelStripOutputPort::PostFaderPreMute,
                },
                AudioRouteSource::Track {
                    track_id: track.id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
            ]
        })
        .chain(sequence.audio_program.buses.iter().flat_map(|bus| {
            [
                AudioRouteSource::Bus {
                    bus_id: bus.id,
                    port: AudioChannelStripOutputPort::PreFader,
                },
                AudioRouteSource::Bus {
                    bus_id: bus.id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
            ]
        }))
        .filter(|source| {
            admission.is_some_and(|admission| {
                admission.allows_processor_sidechain_addition(*source, binding.processor_id)
            })
        })
        .collect::<Vec<_>>();
    inputs
        .buses
        .iter()
        .map(|bus| SidechainInputModel {
            label: localizer.format_text(
                "audio-sidechain-input",
                "identity",
                &input_label(&bus.bus_key, inputs, input_names),
            ),
            options: sources
                .iter()
                .map(|source| SidechainCreateOption {
                    label: source_label(sequence, *source, localizer),
                    payload: crate::app::product_action::AudioProcessorInputConnectionPayload {
                        binding,
                        source: *source,
                        bus_key: bus.bus_key.clone(),
                    },
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn bind_rewire_options(
    routes: &mut [SidechainRouteModel],
    inputs: &[SidechainInputModel],
) {
    for route in routes {
        route.rewire_options = if route.is_editable {
            inputs
                .iter()
                .flat_map(|input| &input.options)
                .filter(|option| {
                    option.payload.bus_key == route.bus_key && option.payload.source != route.source
                })
                .map(|option| SidechainRewireOption {
                    label: option.label.clone(),
                    payload: crate::app::product_action::AudioProcessorInputRewirePayload {
                        connection: option.payload.clone(),
                        route_id: route.route_id,
                        expected_source: route.source,
                    },
                })
                .collect()
        } else {
            Vec::new()
        };
    }
}

pub(crate) fn rewire_action(option: &SidechainRewireOption) -> Action {
    crate::app::product_action::ProductAction::Audio(
        crate::app::product_action::AudioProductAction::RewireProcessorInput(
            option.payload.clone(),
        ),
    )
    .into_external_action()
}

pub(crate) fn bind_retained_input_names(
    routes: &mut [SidechainRouteModel],
    sequence: &Sequence,
    inputs: &mondrian_audio::AudioProcessorAuxiliaryInputContract,
    names: &[mondrian_audio::AudioProcessorInputName],
    localizer: &Localizer,
) {
    for route in routes {
        route.label = format!(
            "{} → {}",
            source_label(sequence, route.source, localizer),
            input_label(&route.bus_key, inputs, names)
        );
    }
}

fn input_label(
    key: &str,
    inputs: &mondrian_audio::AudioProcessorAuxiliaryInputContract,
    names: &[mondrian_audio::AudioProcessorInputName],
) -> String {
    let label_for = |key: &str| {
        names
            .iter()
            .find(|name| name.bus_key == key)
            .map_or_else(|| key.to_owned(), |name| name.name.clone())
    };
    // Missing retained ports remain addressable using their persisted stable key.
    if !inputs.buses.iter().any(|bus| bus.bus_key == key) {
        return key.to_owned();
    }
    let label = label_for(key);
    if inputs
        .buses
        .iter()
        .any(|bus| bus.bus_key != key && label_for(&bus.bus_key) == label)
    {
        format!("{label} ({key})")
    } else {
        label
    }
}

fn source_label(sequence: &Sequence, source: AudioRouteSource, localizer: &Localizer) -> String {
    let (name, port, is_bus) = match source {
        AudioRouteSource::Track { track_id, port } => (
            sequence.audio_tracks.iter().find(|track| track.id == track_id).map_or_else(
                || {
                    localizer.format_text(
                        "audio-sidechain-missing-track",
                        "identity",
                        &track_id.to_string(),
                    )
                },
                |track| track.name.clone(),
            ),
            port,
            false,
        ),
        AudioRouteSource::Bus { bus_id, port } => (
            sequence.audio_program.buses.iter().find(|bus| bus.id == bus_id).map_or_else(
                || {
                    localizer.format_text(
                        "audio-mixer-missing-bus",
                        "identity",
                        &bus_id.to_string(),
                    )
                },
                |bus| localizer.format_text("audio-sidechain-bus", "identity", &bus.name),
            ),
            port,
            true,
        ),
    };
    let port = localizer.text(match (port, is_bus) {
        (AudioChannelStripOutputPort::PreFader, _) => "audio-mixer-port-prefader",
        (AudioChannelStripOutputPort::PostFaderPreMute, false) => {
            "audio-mixer-port-postfader-premute"
        }
        (AudioChannelStripOutputPort::PostMute, false) => "audio-mixer-port-postmute",
        (_, true) => "audio-mixer-port-postfader",
    });
    format!("{name} · {port}")
}

pub(crate) fn connection_action(option: &SidechainCreateOption) -> Action {
    crate::app::product_action::ProductAction::Audio(
        crate::app::product_action::AudioProductAction::ConnectProcessorInput(
            option.payload.clone(),
        ),
    )
    .into_external_action()
}

pub(crate) fn edit_action(edit: AudioRoutingEdit) -> Action {
    crate::app::ui_actions::audio_routing_edit_action(AudioRoutingEditRequest { edit })
}

pub(crate) fn enabled_action(route: &SidechainRouteModel, enabled: bool) -> Option<Action> {
    route.is_editable.then(|| {
        edit_action(AudioRoutingEdit::SetProcessorSidechainEnabled {
            route_id: route.route_id,
            enabled,
        })
    })
}

pub(crate) fn gain_action(route: &SidechainRouteModel, value: f64) -> Option<Action> {
    (route.is_editable
        && route.static_gain.is_some()
        && value.is_finite()
        && (mondrian_timeline::audio::AUDIO_GAIN_DB_MIN
            ..=mondrian_timeline::audio::AUDIO_GAIN_DB_MAX)
            .contains(&value))
    .then(|| {
        edit_action(AudioRoutingEdit::SetProcessorSidechainGainDb {
            route_id: route.route_id,
            value,
        })
    })
}

pub(crate) fn remove_action(route: &SidechainRouteModel) -> Option<Action> {
    route.is_editable.then(|| {
        edit_action(AudioRoutingEdit::RemoveProcessorSidechain { route_id: route.route_id })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::app_ui::localization::{AppUiLocale, Localizer};
    use mondrian_audio::{
        AudioProcessorAuxiliaryInputBusContract, AudioProcessorAuxiliaryInputContract,
    };
    use mondrian_core::AudioChannelLayout;
    use mondrian_timeline::audio::{
        AudioProcessorInstance, AudioProcessorSidechainRoute, BUILTIN_GAIN_DEFINITION_ID,
    };

    fn fixture() -> (Sequence, AudioProcessorInstanceId, AudioRouteSource) {
        let mut sequence = Sequence::new("Sidechain controls");
        let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
        let id = processor.id;
        sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
        let source = AudioRouteSource::Track {
            track_id: sequence.audio_tracks[0].id,
            port: AudioChannelStripOutputPort::PostMute,
        };
        (sequence, id, source)
    }

    #[test]
    fn sidechain_controls_preserve_multiple_disabled_and_unavailable_bus_routes() {
        let (mut sequence, id, source) = fixture();
        let first = AudioProcessorSidechainRoute::new(source, id, "native.absent");
        let mut second = AudioProcessorSidechainRoute::new(source, id, "native.absent");
        second.enabled = false;
        sequence.audio_program.sidechain_routes.extend([first, second]);
        for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            let models = retained_routes(&sequence, id, &localizer);
            assert_eq!(models.len(), 2);
            assert_ne!(models[0].route_id, models[1].route_id);
            assert!(models[0].enabled);
            assert!(!models[1].enabled);
            assert!(models.iter().all(|model| remove_action(model).is_some()));
            assert!(models.iter().all(|model| model.label.contains("native.absent")));
            assert!(!localizer
                .text("audio-sidechain-add-source")
                .contains("audio-sidechain-add-source"));
        }
        sequence.audio_tracks[0].is_locked = true;
        let models = retained_routes(
            &sequence,
            id,
            &Localizer::new(AppUiLocale::EnUs).expect("catalog"),
        );
        for model in &models {
            assert!(!model.is_editable);
            assert!(model.disabled_reason.is_some());
            assert!(remove_action(model).is_none());
            assert!(enabled_action(model, true).is_none());
            assert!(gain_action(model, 1.0).is_none());
        }
    }

    #[test]
    fn sidechain_controls_use_real_transactions_and_restore_exact_route_identity() {
        let (sequence, id, source) = fixture();
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence));
        for _ in 0..2 {
            state
                .dispatch_action(edit_action(AudioRoutingEdit::CreateProcessorSidechain {
                    source,
                    processor_id: id,
                    bus_key: "native.absent".into(),
                }))
                .expect("connect");
        }
        let localizer = Localizer::new(AppUiLocale::ZhCn).expect("catalog");
        let routes = retained_routes(state.active_sequence().expect("Sequence"), id, &localizer);
        let second = routes[1].route_id;
        let precise = -6.123456789123456_f64;
        state
            .dispatch_action(gain_action(&routes[0], precise).expect("gain action"))
            .expect("gain");
        assert_eq!(
            state.active_sequence().expect("Sequence").audio_program.sidechain_routes[0].gain_db,
            precise
        );
        assert!(state.undo_timeline().expect("undo gain"));
        assert_eq!(
            state.active_sequence().expect("Sequence").audio_program.sidechain_routes[0].gain_db,
            0.0
        );
        assert!(state.redo_timeline().expect("redo gain"));
        state
            .dispatch_action(enabled_action(&routes[0], false).expect("disable action"))
            .expect("disable");
        assert!(
            !state.active_sequence().expect("Sequence").audio_program.sidechain_routes[0].enabled
        );
        state
            .dispatch_action(remove_action(&routes[0]).expect("disconnect action"))
            .expect("disconnect");
        let sequence = state.active_sequence().expect("Sequence");
        assert_eq!(sequence.audio_program.sidechain_routes.len(), 1);
        assert_eq!(sequence.audio_program.sidechain_routes[0].id, second);
        assert!(state.undo_timeline().expect("undo disconnect"));
        let sequence = state.active_sequence().expect("Sequence");
        assert_eq!(
            sequence.audio_program.sidechain_routes[0].id,
            routes[0].route_id
        );
        assert_eq!(sequence.audio_program.sidechain_routes[0].gain_db, precise);
        assert!(!sequence.audio_program.sidechain_routes[0].enabled);
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            mondrian_timeline::audio::AUDIO_GAIN_DB_MAX + 1.0,
        ] {
            assert!(gain_action(&routes[0], invalid).is_none());
        }
    }

    #[test]
    fn sidechain_source_choices_share_canonical_lock_and_cycle_admission() {
        let (mut sequence, id, _) = fixture();
        sequence.audio_tracks[0].is_locked = true;
        let contract = AudioProcessorAuxiliaryInputContract {
            buses: vec![AudioProcessorAuxiliaryInputBusContract {
                bus_key: "detector.7".into(),
                channel_layout: AudioChannelLayout::Stereo,
            }],
        };
        let admission =
            mondrian_timeline::inspect_audio_route_candidates(&sequence).expect("routing");
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence.clone()));
        let binding = crate::app::product_action::AudioProcessorInputBinding {
            session_id: state.authoring_session_id().expect("Session"),
            sequence_id: sequence.id,
            target: mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip {
                owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                    output_id: sequence.audio_program.outputs[0].id,
                },
                rack: mondrian_timeline::AudioChannelStripRack::PreFader,
            },
            processor_id: id,
            observation_id: mondrian_core::AudioProcessorInputObservationId::new(),
        };
        let options = input_options(
            &sequence,
            binding,
            &contract,
            &[],
            Some(&admission),
            &Localizer::new(AppUiLocale::EnUs).expect("catalog"),
        );
        assert_eq!(options.len(), 1);
        assert!(!options[0].options.is_empty());
        for option in &options[0].options {
            let source = option.payload.source;
            assert_eq!(option.payload.binding, binding);
            assert_eq!(option.payload.bus_key, "detector.7");
            assert!(admission.allows_processor_sidechain_addition(source, id));
            assert!(
                !matches!(source, AudioRouteSource::Track { track_id, .. } if track_id == sequence.audio_tracks[0].id)
            );
        }
        assert!(input_options(
            &sequence,
            binding,
            &contract,
            &[],
            None,
            &Localizer::new(AppUiLocale::EnUs).expect("catalog")
        )[0]
        .options
        .is_empty());
    }
    #[test]
    fn retained_source_options_require_editable_current_port_and_preserve_route_identity() {
        let (mut sequence, id, source) = fixture();
        let route = AudioProcessorSidechainRoute::new(source, id, "detector.7");
        let route_id = route.id;
        sequence.audio_program.sidechain_routes.push(route);
        sequence
            .audio_program
            .sidechain_routes
            .push(AudioProcessorSidechainRoute::new(source, id, "missing"));
        let mut state = AppState::new();
        state.test_set_sequence(Some(sequence.clone()));
        let binding = crate::app::product_action::AudioProcessorInputBinding {
            session_id: state.authoring_session_id().expect("Session"),
            sequence_id: sequence.id,
            target: mondrian_audio::AudioProcessorInputInspectionTarget::ChannelStrip {
                owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                    output_id: sequence.audio_program.outputs[0].id,
                },
                rack: mondrian_timeline::AudioChannelStripRack::PreFader,
            },
            processor_id: id,
            observation_id: mondrian_core::AudioProcessorInputObservationId::new(),
        };
        let contract = AudioProcessorAuxiliaryInputContract {
            buses: vec![AudioProcessorAuxiliaryInputBusContract {
                bus_key: "detector.7".into(),
                channel_layout: AudioChannelLayout::Stereo,
            }],
        };
        let admission =
            mondrian_timeline::inspect_audio_route_candidates(&sequence).expect("routing");
        let before = serde_json::to_value(&sequence).expect("snapshot");
        for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("locale");
            let inputs = input_options(
                &sequence,
                binding,
                &contract,
                &[],
                Some(&admission),
                &localizer,
            );
            let mut routes = retained_routes(&sequence, id, &localizer);
            bind_rewire_options(&mut routes, &inputs);
            assert!(!routes[0].rewire_options.is_empty());
            assert!(
                routes[1].rewire_options.is_empty(),
                "unavailable ports cannot silently rebind"
            );
            for option in &routes[0].rewire_options {
                assert_eq!(option.payload.route_id, route_id);
                assert_eq!(option.payload.expected_source, source);
                assert_eq!(option.payload.connection.binding, binding);
                assert_ne!(option.payload.connection.source, source);
                assert!(crate::app::product_action::ProductAction::decode_external(
                    &rewire_action(option)
                )
                .expect("codec")
                .is_some());
            }
            routes[0].is_editable = false;
            bind_rewire_options(&mut routes, &inputs);
            assert!(routes[0].rewire_options.is_empty());
            bind_rewire_options(&mut routes, &[]);
            assert!(routes.iter().all(|route| route.rewire_options.is_empty()));
            assert_eq!(serde_json::to_value(&sequence).expect("read only"), before);
        }
    }

    #[test]
    fn sidechain_names_disambiguate_native_labels_and_preserve_unavailable_routes() {
        let (mut sequence, id, source) = fixture();
        let keys = ["detector.11", "detector.7", "nameless"];
        let contract = AudioProcessorAuxiliaryInputContract {
            buses: keys
                .iter()
                .map(|key| AudioProcessorAuxiliaryInputBusContract {
                    bus_key: (*key).into(),
                    channel_layout: AudioChannelLayout::Stereo,
                })
                .collect(),
        };
        let mut names = keys[..2]
            .iter()
            .map(|key| mondrian_audio::AudioProcessorInputName {
                bus_key: (*key).into(),
                name: "压缩探测 🙂".into(),
            })
            .collect::<Vec<_>>();
        sequence.audio_program.sidechain_routes.extend(
            keys.iter()
                .chain([&"missing"])
                .map(|key| AudioProcessorSidechainRoute::new(source, id, *key)),
        );
        let before = serde_json::to_value(&sequence).expect("author snapshot");
        for locale in [AppUiLocale::EnUs, AppUiLocale::ZhCn, AppUiLocale::Pseudo] {
            let localizer = Localizer::new(locale).expect("catalog");
            let mut models = retained_routes(&sequence, id, &localizer);
            bind_retained_input_names(&mut models, &sequence, &contract, &names, &localizer);
            assert!(models[0].label.ends_with("压缩探测 🙂 (detector.11)"));
            assert!(models[1].label.ends_with("压缩探测 🙂 (detector.7)"));
            assert!(models[2].label.ends_with("nameless"));
            assert!(models[3].label.ends_with("missing"));
            assert!(models.iter().all(|route| remove_action(route).is_some()));
            assert_eq!(
                serde_json::to_value(&sequence).expect("unchanged author"),
                before
            );
        }
        names[0].name = "nameless".into();
        assert_eq!(
            input_label("detector.11", &contract, &names),
            "nameless (detector.11)"
        );
        assert_eq!(
            input_label("nameless", &contract, &names),
            "nameless (nameless)"
        );
    }
}
