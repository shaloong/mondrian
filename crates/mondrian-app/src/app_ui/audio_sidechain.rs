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
    pub(crate) edit: AudioRoutingEdit,
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
    processor_id: AudioProcessorInstanceId,
    inputs: &mondrian_audio::AudioProcessorAuxiliaryInputContract,
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
                admission.allows_processor_sidechain_addition(*source, processor_id)
            })
        })
        .collect::<Vec<_>>();
    inputs
        .buses
        .iter()
        .map(|bus| SidechainInputModel {
            label: localizer.format_text("audio-sidechain-input", "identity", &bus.bus_key),
            options: sources
                .iter()
                .map(|source| SidechainCreateOption {
                    label: source_label(sequence, *source, localizer),
                    edit: AudioRoutingEdit::CreateProcessorSidechain {
                        source: *source,
                        processor_id,
                        bus_key: bus.bus_key.clone(),
                    },
                })
                .collect(),
        })
        .collect()
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
        let options = input_options(
            &sequence,
            id,
            &contract,
            Some(&admission),
            &Localizer::new(AppUiLocale::EnUs).expect("catalog"),
        );
        assert_eq!(options.len(), 1);
        assert!(!options[0].options.is_empty());
        for option in &options[0].options {
            let AudioRoutingEdit::CreateProcessorSidechain { source, processor_id, bus_key } =
                &option.edit
            else {
                panic!("creation")
            };
            assert_eq!(*processor_id, id);
            assert_eq!(bus_key, "detector.7");
            assert!(admission.allows_processor_sidechain_addition(*source, id));
            assert!(
                !matches!(source, AudioRouteSource::Track { track_id, .. } if *track_id == sequence.audio_tracks[0].id)
            );
        }
        assert!(input_options(
            &sequence,
            id,
            &contract,
            None,
            &Localizer::new(AppUiLocale::EnUs).expect("catalog")
        )[0]
        .options
        .is_empty());
    }
}
