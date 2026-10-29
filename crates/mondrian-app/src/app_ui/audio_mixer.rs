//! Audio Mixer projection and typed action factories.
//!
//! This App UI Adapter presents Sequence-owned Track, Bus, Program Output,
//! Channel Strip, and Route state. Timeline remains authority for addresses,
//! locks, automation, graph validity, and mutation; no Widget owns a shadow
//! mixer or a second Send graph.

use std::collections::HashMap;

use mondrian_audio::{AudioChannelMeterReading, AudioMeterFrame, AudioMeterTarget};
use mondrian_core::{AudioRouteId, MixBusId, TrackId};
use mondrian_editor_state::Action;
use mondrian_timeline::audio::{
    AudioChannelStripOutputPort, AudioChannelStripOwner, AudioRouteDestination, AudioRouteSource,
    ProgramOutputMainSource,
};
use mondrian_timeline::{
    inspect_audio_channel_strip, inspect_audio_mix_buses, inspect_audio_route,
    inspect_audio_route_candidates, AudioAutomationTarget, AudioBusRemovalPolicy,
    AudioChannelStripEditBlocker, AudioChannelStripRack, AudioProcessorRackAddress,
    AudioRouteCandidateInspection, AudioRoutingEdit, AudioRoutingEditBlocker,
    AudioRoutingEditRequest,
};

#[cfg(test)]
use mondrian_timeline::audio::AUDIO_GAIN_DB_MIN;

use crate::app::product_action::AudioTrackSoloPayload;
use crate::app::ui_actions::{
    audio_routing_edit_action, audio_track_solo_action, track_set_author_control_action,
    TrackAuthorControl, TrackSetAuthorControlPayload,
};
use crate::app::AppState;

use super::audio_automation::{
    project_audio_automation, sequence_automation_viewport, AudioAutomationCurveModel,
};
use super::audio_processor_rack::{
    append_clap_insert_options, append_vst3_insert_options, project_audio_processor_rack,
    AudioProcessorRackModel,
};
use super::localization::Localizer;

/// Complete immutable Mixer panel projection.
#[derive(Debug, Clone)]
pub(crate) struct AudioMixerPanelModel {
    pub(crate) channels: Vec<AudioMixerChannelModel>,
    pub(crate) next_bus_name: Option<String>,
    pub(crate) new_bus_destination: Option<AudioRouteDestination>,
    pub(crate) empty_message: Option<String>,
}

/// Product-facing kind of one Channel Strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioMixerChannelKind {
    Track,
    Bus,
    ProgramOutput,
}

/// Honest decibel control projection: static and automated authority are disjoint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AudioMixerGainModel {
    Static { value_db: f64 },
    Automated { keyframe_count: usize },
}

/// One available typed Route creation choice for a source Channel Strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioMixerRouteCreateOption {
    pub(crate) label: String,
    pub(crate) source: AudioRouteSource,
    pub(crate) destination: AudioRouteDestination,
}

/// One existing principal Route or parallel send shown by the Mixer.
#[derive(Debug, Clone)]
pub(crate) struct AudioMixerRouteModel {
    pub(crate) source_sequence_id: mondrian_core::SequenceId,
    pub(crate) scalar_author_lease: Option<crate::app::parameter_gesture::AudioScalarAuthorLease>,
    pub(crate) route_id: AudioRouteId,
    pub(crate) source: AudioRouteSource,
    pub(crate) destination: AudioRouteDestination,
    pub(crate) source_port_label: String,
    pub(crate) destination_label: String,
    pub(crate) enabled: bool,
    pub(crate) gain: AudioMixerGainModel,
    pub(crate) gain_automation: Option<AudioAutomationCurveModel>,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
}

/// Bus deletion projection with explicit strong-Route consequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioMixerBusRemovalModel {
    pub(crate) bus_id: MixBusId,
    pub(crate) connected_route_count: usize,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
}

/// One Track, Bus, or Program Output Channel Strip shown by the Mixer.
#[derive(Debug, Clone)]
pub(crate) struct AudioMixerChannelModel {
    pub(crate) source_sequence_id: mondrian_core::SequenceId,
    pub(crate) scalar_author_lease: Option<crate::app::parameter_gesture::AudioScalarAuthorLease>,
    pub(crate) owner: AudioChannelStripOwner,
    pub(crate) kind: AudioMixerChannelKind,
    pub(crate) name: String,
    pub(crate) is_editable: bool,
    pub(crate) edit_disabled_reason: Option<String>,
    pub(crate) input_trim_db: f64,
    pub(crate) fader: AudioMixerGainModel,
    pub(crate) fader_automation: Option<AudioAutomationCurveModel>,
    pub(crate) track_muted: Option<bool>,
    pub(crate) track_soloed: Option<bool>,
    /// Latest complete post-mute block for this exact prepared Channel Strip.
    pub(crate) meter: Option<AudioMixerMeterModel>,
    pub(crate) incoming_route_count: usize,
    pub(crate) outbound_routes: Vec<AudioMixerRouteModel>,
    pub(crate) route_create_options: Vec<AudioMixerRouteCreateOption>,
    pub(crate) bus_removal: Option<AudioMixerBusRemovalModel>,
    pub(crate) processor_racks: Vec<AudioProcessorRackModel>,
}

/// One complete block observation projected for a Mixer Channel Strip.
#[derive(Debug, Clone)]
pub(crate) struct AudioMixerMeterModel {
    pub(crate) channels: Vec<AudioChannelMeterReading>,
}

impl AudioMixerPanelModel {
    pub(crate) fn from_app_state(state: &AppState, localizer: &Localizer) -> Self {
        let Some(sequence) = state.active_sequence() else {
            return Self {
                channels: Vec::new(),
                next_bus_name: None,
                new_bus_destination: None,
                empty_message: Some(localizer.text("audio-mixer-empty")),
            };
        };
        let time_context =
            crate::app_ui::audio_automation::AudioAutomationTimeContext::at_sequence_time(
                sequence,
                state.current_timeline_time().ok().flatten().unwrap_or(sequence.playhead),
            );
        let meter = state.latest_audio_meter_frame();
        let mut channels = Vec::with_capacity(
            sequence
                .audio_tracks
                .len()
                .saturating_add(sequence.audio_program.buses.len())
                .saturating_add(sequence.audio_program.outputs.len()),
        );
        let routing = AudioMixerRoutingIndex::build(sequence, localizer, time_context);
        channels.extend(sequence.audio_tracks.iter().map(|track| {
            project_channel(
                sequence,
                &routing,
                AudioChannelStripOwner::Track { track_id: track.id },
                AudioMixerChannelKind::Track,
                track.name.clone(),
                Some(track.is_muted),
                Some(state.is_audio_track_soloed(track.id)),
                meter.as_ref(),
                localizer,
                time_context,
            )
        }));
        channels.extend(sequence.audio_program.buses.iter().map(|bus| {
            project_channel(
                sequence,
                &routing,
                AudioChannelStripOwner::Bus { bus_id: bus.id },
                AudioMixerChannelKind::Bus,
                bus.name.clone(),
                None,
                None,
                meter.as_ref(),
                localizer,
                time_context,
            )
        }));
        channels.extend(sequence.audio_program.outputs.iter().map(|output| {
            project_channel(
                sequence,
                &routing,
                AudioChannelStripOwner::ProgramOutput { output_id: output.id },
                AudioMixerChannelKind::ProgramOutput,
                output.name.clone(),
                None,
                None,
                meter.as_ref(),
                localizer,
                time_context,
            )
        }));
        if let Ok(descriptors) = state.installed_clap_processors() {
            for channel in &mut channels {
                append_clap_insert_options(&mut channel.processor_racks, &descriptors);
            }
        }
        if let Ok(descriptors) = state.installed_vst3_processors() {
            for channel in &mut channels {
                append_vst3_insert_options(&mut channel.processor_racks, &descriptors);
            }
        }
        for channel in &mut channels {
            channel.scalar_author_lease =
                crate::app::parameter_gesture::AudioScalarAuthorLease::from_state(
                    state,
                    channel.source_sequence_id,
                );
            for route in &mut channel.outbound_routes {
                route.scalar_author_lease = channel
                    .scalar_author_lease
                    .filter(|lease| lease.sequence_id == route.source_sequence_id);
            }
            super::audio_processor_rack::bind_audio_scalar_author_lease(
                &mut channel.processor_racks,
                state,
            );
            super::audio_processor_rack::bind_processor_input_queries(
                &mut channel.processor_racks,
                state,
                localizer,
                routing.route_candidates.as_ref(),
            );
        }
        Self {
            channels,
            next_bus_name: Some(next_bus_name(sequence)),
            new_bus_destination: sequence
                .audio_program
                .outputs
                .iter()
                .find(|output| matches!(output.main_source, ProgramOutputMainSource::RoutedInputs))
                .map(|output| AudioRouteDestination::Output(output.id)),
            empty_message: None,
        }
    }
}

fn project_channel(
    sequence: &mondrian_timeline::sequence::Sequence,
    routing: &AudioMixerRoutingIndex,
    owner: AudioChannelStripOwner,
    kind: AudioMixerChannelKind,
    name: String,
    track_muted: Option<bool>,
    track_soloed: Option<bool>,
    meter: Option<&AudioMeterFrame>,
    localizer: &Localizer,
    time_context: crate::app_ui::audio_automation::AudioAutomationTimeContext,
) -> AudioMixerChannelModel {
    let automation_viewport = sequence_automation_viewport(sequence);
    let inspection = inspect_audio_channel_strip(sequence, owner);
    let (is_editable, edit_disabled_reason, input_trim_db, fader) = match inspection {
        Ok(inspection) => {
            let strip = inspection.strip();
            let fader = strip.fader_automation.as_ref().map_or(
                AudioMixerGainModel::Static { value_db: strip.fader_db },
                |curve| AudioMixerGainModel::Automated { keyframe_count: curve.keyframes.len() },
            );
            (
                inspection.is_editable(),
                inspection
                    .edit_blocker()
                    .map(|blocker| channel_strip_blocker_label(blocker, localizer)),
                strip.input_trim_db,
                fader,
            )
        }
        Err(error) => (
            false,
            Some(localizer.format_text("audio-mixer-invalid-strip", "error", &error.to_string())),
            0.0,
            AudioMixerGainModel::Static { value_db: 0.0 },
        ),
    };
    let processor_racks = [
        AudioChannelStripRack::PreFader,
        AudioChannelStripRack::PostFader,
    ]
    .into_iter()
    .map(|rack| {
        project_audio_processor_rack(
            sequence,
            AudioProcessorRackAddress::ChannelStrip { owner, rack },
            localizer,
            time_context,
        )
    })
    .collect();
    let outbound_routes = routing.outbound.get(&owner).cloned().unwrap_or_default();
    let incoming_route_count = routing.incoming_counts.get(&owner).copied().unwrap_or(0);
    let route_create_options =
        route_source_for_owner(owner).map_or_else(Vec::new, |owner_source| {
            route_create_options(sequence, routing, owner_source, is_editable, localizer)
        });
    let bus_removal = match owner {
        AudioChannelStripOwner::Bus { bus_id } => routing.bus_removals.get(&bus_id).cloned(),
        AudioChannelStripOwner::Track { .. } | AudioChannelStripOwner::ProgramOutput { .. } => None,
    };
    AudioMixerChannelModel {
        source_sequence_id: sequence.id,
        scalar_author_lease: None,
        owner,
        kind,
        name,
        is_editable,
        edit_disabled_reason,
        input_trim_db,
        fader,
        fader_automation: automation_viewport.and_then(|viewport| {
            project_audio_automation(
                sequence,
                AudioAutomationTarget::ChannelFader { owner },
                viewport,
                time_context,
            )
        }),
        track_muted,
        track_soloed,
        meter: meter.and_then(|frame| {
            let target = match owner {
                AudioChannelStripOwner::Track { track_id } => AudioMeterTarget::Track(track_id),
                AudioChannelStripOwner::Bus { bus_id } => AudioMeterTarget::Bus(bus_id),
                AudioChannelStripOwner::ProgramOutput { output_id } => {
                    AudioMeterTarget::ProgramOutput(output_id)
                }
            };
            frame
                .target(target)
                .map(|target| AudioMixerMeterModel { channels: target.channels.clone() })
        }),
        incoming_route_count,
        outbound_routes,
        route_create_options,
        bus_removal,
        processor_racks,
    }
}

#[derive(Debug, Default)]
struct AudioMixerRoutingIndex {
    outbound: HashMap<AudioChannelStripOwner, Vec<AudioMixerRouteModel>>,
    incoming_counts: HashMap<AudioChannelStripOwner, usize>,
    bus_removals: HashMap<MixBusId, AudioMixerBusRemovalModel>,
    route_candidates: Option<AudioRouteCandidateInspection>,
}

impl AudioMixerRoutingIndex {
    fn build(
        sequence: &mondrian_timeline::sequence::Sequence,
        localizer: &Localizer,
        time_context: crate::app_ui::audio_automation::AudioAutomationTimeContext,
    ) -> Self {
        let mut index = Self {
            route_candidates: inspect_audio_route_candidates(sequence).ok(),
            ..Self::default()
        };
        match inspect_audio_mix_buses(sequence) {
            Ok(inspections) => index.bus_removals.extend(inspections.iter().map(|inspection| {
                let bus_id = inspection.bus().id;
                (
                    bus_id,
                    AudioMixerBusRemovalModel {
                        bus_id,
                        connected_route_count: inspection.connected_route_count(),
                        is_editable: inspection.can_disconnect_and_remove(),
                        edit_disabled_reason: inspection
                            .disconnect_blocker()
                            .map(|blocker| routing_blocker_label(blocker, localizer)),
                    },
                )
            })),
            Err(error) => {
                let reason =
                    localizer.format_text("audio-mixer-invalid-route", "error", &error.to_string());
                index.bus_removals.extend(sequence.audio_program.buses.iter().map(|bus| {
                    (
                        bus.id,
                        AudioMixerBusRemovalModel {
                            bus_id: bus.id,
                            connected_route_count: 0,
                            is_editable: false,
                            edit_disabled_reason: Some(reason.clone()),
                        },
                    )
                }));
            }
        }
        for route in &sequence.audio_program.routes {
            let projected = project_route(sequence, route, localizer, time_context);
            if let Some(owner) = owner_for_source(sequence, &route.source) {
                index.outbound.entry(owner).or_default().push(projected);
            }
            *index
                .incoming_counts
                .entry(owner_for_destination(route.destination))
                .or_default() += 1;
        }
        index
    }

    fn allows_candidate(
        &self,
        source: AudioRouteSource,
        destination: AudioRouteDestination,
    ) -> bool {
        self.route_candidates
            .as_ref()
            .is_some_and(|inspection| inspection.allows_addition(source, destination))
    }
}

fn project_route(
    sequence: &mondrian_timeline::sequence::Sequence,
    route: &mondrian_timeline::audio::AudioRoute,
    localizer: &Localizer,
    time_context: crate::app_ui::audio_automation::AudioAutomationTimeContext,
) -> AudioMixerRouteModel {
    let (is_editable, edit_disabled_reason) = match inspect_audio_route(sequence, route.id) {
        Ok(inspection) => {
            let blocker = inspection.edit_blocker().cloned();
            (
                inspection.is_editable(),
                blocker.as_ref().map(|blocker| routing_blocker_label(blocker, localizer)),
            )
        }
        Err(error) => (
            false,
            Some(localizer.format_text("audio-mixer-invalid-route", "error", &error.to_string())),
        ),
    };
    AudioMixerRouteModel {
        source_sequence_id: sequence.id,
        scalar_author_lease: None,
        route_id: route.id,
        source: route.source.clone(),
        destination: route.destination,
        source_port_label: source_port_label(&route.source, localizer),
        destination_label: destination_label(sequence, route.destination, localizer),
        enabled: route.enabled,
        gain: route.gain_automation.as_ref().map_or(
            AudioMixerGainModel::Static { value_db: route.gain_db },
            |curve| AudioMixerGainModel::Automated { keyframe_count: curve.keyframes.len() },
        ),
        gain_automation: sequence_automation_viewport(sequence).and_then(|viewport| {
            project_audio_automation(
                sequence,
                AudioAutomationTarget::RouteGain { route_id: route.id },
                viewport,
                time_context,
            )
        }),
        is_editable,
        edit_disabled_reason,
    }
}

fn owner_for_source(
    sequence: &mondrian_timeline::Sequence,
    source: &AudioRouteSource,
) -> Option<AudioChannelStripOwner> {
    sequence.audio_program.route_source_channel(source).ok().map(|(owner, _)| owner)
}

fn owner_for_destination(destination: AudioRouteDestination) -> AudioChannelStripOwner {
    match destination {
        AudioRouteDestination::Bus(bus_id) => AudioChannelStripOwner::Bus { bus_id },
        AudioRouteDestination::Output(output_id) => {
            AudioChannelStripOwner::ProgramOutput { output_id }
        }
    }
}

fn route_create_options(
    sequence: &mondrian_timeline::sequence::Sequence,
    routing: &AudioMixerRoutingIndex,
    owner_source: AudioRouteSource,
    is_editable: bool,
    localizer: &Localizer,
) -> Vec<AudioMixerRouteCreateOption> {
    if !is_editable {
        return Vec::new();
    }
    let mut destinations = sequence
        .audio_program
        .buses
        .iter()
        .filter_map(|bus| {
            (!matches!(owner_source, AudioRouteSource::Bus { bus_id, .. } if bus_id == bus.id))
                .then_some((
                    AudioRouteDestination::Bus(bus.id),
                    localizer.format_text("audio-sidechain-bus", "identity", &bus.name),
                ))
        })
        .collect::<Vec<_>>();
    destinations.extend(sequence.audio_program.outputs.iter().filter_map(|output| {
        matches!(output.main_source, ProgramOutputMainSource::RoutedInputs).then_some((
            AudioRouteDestination::Output(output.id),
            localizer.format_text("audio-mixer-output", "identity", &output.name),
        ))
    }));
    let source_identity = match owner_source {
        AudioRouteSource::Track { track_id, .. } => RouteSourceIdentity::Track(track_id),
        AudioRouteSource::Bus { bus_id, .. } => RouteSourceIdentity::Bus(bus_id),
        AudioRouteSource::ProcessorOutput { .. } => return Vec::new(),
    };
    let ports: &[AudioChannelStripOutputPort] = match source_identity {
        RouteSourceIdentity::Track(_) => &[
            AudioChannelStripOutputPort::PreFader,
            AudioChannelStripOutputPort::PostFaderPreMute,
            AudioChannelStripOutputPort::PostMute,
        ],
        RouteSourceIdentity::Bus(_) => &[
            AudioChannelStripOutputPort::PreFader,
            AudioChannelStripOutputPort::PostMute,
        ],
    };
    destinations
        .into_iter()
        .flat_map(|(destination, destination_label)| {
            ports.iter().copied().map({
                let destination_label = destination_label.clone();
                move |port| {
                    let source = source_identity.with_port(port);
                    AudioMixerRouteCreateOption {
                        label: format!(
                            "{} → {destination_label}",
                            source_port_label(&source, localizer)
                        ),
                        source,
                        destination,
                    }
                }
            })
        })
        .filter(|option| routing.allows_candidate(option.source.clone(), option.destination))
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum RouteSourceIdentity {
    Track(TrackId),
    Bus(MixBusId),
}

impl RouteSourceIdentity {
    fn with_port(self, port: AudioChannelStripOutputPort) -> AudioRouteSource {
        match self {
            Self::Track(track_id) => AudioRouteSource::Track { track_id, port },
            Self::Bus(bus_id) => AudioRouteSource::Bus { bus_id, port },
        }
    }
}

fn route_source_for_owner(owner: AudioChannelStripOwner) -> Option<AudioRouteSource> {
    match owner {
        AudioChannelStripOwner::Track { track_id } => Some(AudioRouteSource::Track {
            track_id,
            port: AudioChannelStripOutputPort::PostMute,
        }),
        AudioChannelStripOwner::Bus { bus_id } => Some(AudioRouteSource::Bus {
            bus_id,
            port: AudioChannelStripOutputPort::PostMute,
        }),
        AudioChannelStripOwner::ProgramOutput { .. } => None,
    }
}

fn source_port_label(source: &AudioRouteSource, localizer: &Localizer) -> String {
    if let AudioRouteSource::ProcessorOutput { processor_id, port } = source {
        return match port {
            mondrian_timeline::AudioProcessorOutputPort::Main => format!("{processor_id} · Main"),
            mondrian_timeline::AudioProcessorOutputPort::Auxiliary { bus_key } => {
                format!("{processor_id} · {bus_key}")
            }
        };
    }
    localizer.text(match source {
        AudioRouteSource::Track { port: AudioChannelStripOutputPort::PreFader, .. }
        | AudioRouteSource::Bus { port: AudioChannelStripOutputPort::PreFader, .. } => {
            "audio-mixer-port-prefader"
        }
        AudioRouteSource::Track {
            port: AudioChannelStripOutputPort::PostFaderPreMute,
            ..
        } => "audio-mixer-port-postfader-premute",
        AudioRouteSource::Track { port: AudioChannelStripOutputPort::PostMute, .. } => {
            "audio-mixer-port-postmute"
        }
        AudioRouteSource::Bus {
            port:
                AudioChannelStripOutputPort::PostFaderPreMute | AudioChannelStripOutputPort::PostMute,
            ..
        } => "audio-mixer-port-postfader",
        AudioRouteSource::ProcessorOutput { .. } => unreachable!("raw output handled above"),
    })
}

fn destination_label(
    sequence: &mondrian_timeline::sequence::Sequence,
    destination: AudioRouteDestination,
    localizer: &Localizer,
) -> String {
    match destination {
        AudioRouteDestination::Bus(bus_id) => {
            sequence.audio_program.buses.iter().find(|bus| bus.id == bus_id).map_or_else(
                || {
                    localizer.format_text(
                        "audio-mixer-missing-bus",
                        "identity",
                        &bus_id.to_string(),
                    )
                },
                |bus| localizer.format_text("audio-sidechain-bus", "identity", &bus.name),
            )
        }
        AudioRouteDestination::Output(output_id) => sequence
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
    }
}

fn next_bus_name(sequence: &mondrian_timeline::sequence::Sequence) -> String {
    let names = sequence
        .audio_program
        .buses
        .iter()
        .map(|bus| bus.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    (1_u32..)
        .map(|index| format!("Bus {index}"))
        .find(|candidate| !names.contains(candidate.as_str()))
        .unwrap_or_else(|| "Bus".to_owned())
}

fn channel_strip_blocker_label(
    blocker: &AudioChannelStripEditBlocker,
    localizer: &Localizer,
) -> String {
    match blocker {
        AudioChannelStripEditBlocker::LockedTrack(track_id) => localizer.format_text(
            "audio-mixer-locked-strip",
            "identity",
            &track_id.to_string(),
        ),
    }
}

fn routing_blocker_label(blocker: &AudioRoutingEditBlocker, localizer: &Localizer) -> String {
    match blocker {
        AudioRoutingEditBlocker::LockedTrack(track_id) => localizer.format_text(
            "audio-mixer-locked-route",
            "identity",
            &track_id.to_string(),
        ),
    }
}

pub(crate) fn create_bus_action(model: &AudioMixerPanelModel) -> Option<Action> {
    let name = model.next_bus_name.clone()?;
    Some(audio_routing_edit_action(AudioRoutingEditRequest {
        edit: AudioRoutingEdit::CreateBus { name, route_to: model.new_bus_destination },
    }))
}

pub(crate) fn remove_bus_action(model: &AudioMixerBusRemovalModel) -> Option<Action> {
    model.is_editable.then(|| {
        audio_routing_edit_action(AudioRoutingEditRequest {
            edit: AudioRoutingEdit::RemoveBus {
                bus_id: model.bus_id,
                policy: AudioBusRemovalPolicy::Disconnect,
            },
        })
    })
}

pub(crate) fn rename_bus_action(channel: &AudioMixerChannelModel, name: &str) -> Option<Action> {
    let AudioChannelStripOwner::Bus { bus_id } = channel.owner else {
        return None;
    };
    channel.is_editable.then(|| {
        audio_routing_edit_action(AudioRoutingEditRequest {
            edit: AudioRoutingEdit::RenameBus { bus_id, name: name.to_owned() },
        })
    })
}

pub(crate) fn create_route_action(option: &AudioMixerRouteCreateOption) -> Action {
    audio_routing_edit_action(AudioRoutingEditRequest {
        edit: AudioRoutingEdit::CreateRoute {
            source: option.source.clone(),
            destination: option.destination,
        },
    })
}

pub(crate) fn rewire_route_action(
    route: &AudioMixerRouteModel,
    option: &AudioMixerRouteCreateOption,
) -> Option<Action> {
    (route.is_editable
        && (route.source != option.source || route.destination != option.destination))
        .then(|| {
            audio_routing_edit_action(AudioRoutingEditRequest {
                edit: AudioRoutingEdit::SetRouteEndpoints {
                    route_id: route.route_id,
                    source: option.source.clone(),
                    destination: option.destination,
                },
            })
        })
}

pub(crate) fn set_route_enabled_action(
    route: &AudioMixerRouteModel,
    enabled: bool,
) -> Option<Action> {
    route.is_editable.then(|| {
        audio_routing_edit_action(AudioRoutingEditRequest {
            edit: AudioRoutingEdit::SetRouteEnabled { route_id: route.route_id, enabled },
        })
    })
}

/// One static Route owner with exact endpoints and open author lifetime.
pub(crate) fn route_gain_target(
    route: &AudioMixerRouteModel,
) -> Option<crate::app::parameter_gesture::AudioScalarGestureTarget> {
    (route.is_editable && matches!(route.gain, AudioMixerGainModel::Static { .. })).then_some(
        crate::app::parameter_gesture::AudioScalarGestureTarget {
            lease: route
                .scalar_author_lease
                .filter(|lease| lease.sequence_id == route.source_sequence_id)?,
            owner: crate::app::parameter_gesture::AudioScalarOwner::RouteLevel {
                route_id: route.route_id,
                source: route.source.clone(),
                destination: route.destination,
            },
        },
    )
}

pub(crate) fn remove_route_action(route: &AudioMixerRouteModel) -> Option<Action> {
    route.is_editable.then(|| {
        audio_routing_edit_action(AudioRoutingEditRequest {
            edit: AudioRoutingEdit::RemoveRoute { route_id: route.route_id },
        })
    })
}

/// Exact input-trim owner from an author-bound immutable channel projection.
pub(crate) fn input_trim_target(
    channel: &AudioMixerChannelModel,
) -> Option<crate::app::parameter_gesture::AudioScalarGestureTarget> {
    channel
        .is_editable
        .then_some(crate::app::parameter_gesture::AudioScalarGestureTarget {
            lease: channel
                .scalar_author_lease
                .filter(|lease| lease.sequence_id == channel.source_sequence_id)?,
            owner: crate::app::parameter_gesture::AudioScalarOwner::Trim { owner: channel.owner },
        })
}

/// Static fader owner; keyed faders retain their curve authority.
pub(crate) fn fader_target(
    channel: &AudioMixerChannelModel,
) -> Option<crate::app::parameter_gesture::AudioScalarGestureTarget> {
    (channel.is_editable && matches!(channel.fader, AudioMixerGainModel::Static { .. })).then_some(
        crate::app::parameter_gesture::AudioScalarGestureTarget {
            lease: channel
                .scalar_author_lease
                .filter(|lease| lease.sequence_id == channel.source_sequence_id)?,
            owner: crate::app::parameter_gesture::AudioScalarOwner::Fader { owner: channel.owner },
        },
    )
}

pub(crate) fn set_track_mute_action(track_id: TrackId, muted: bool) -> Action {
    track_set_author_control_action(TrackSetAuthorControlPayload {
        track_id,
        control: TrackAuthorControl::Mute,
        enabled: muted,
    })
}

pub(crate) fn set_track_solo_action(track_id: TrackId, soloed: bool) -> Action {
    audio_track_solo_action(AudioTrackSoloPayload { track_id, soloed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use crate::app_ui::localization::AppUiLocale;
    use mondrian_core::{ExactAutomationCurve, ExactAutomationKeyframe, ParameterId, TimelineTime};
    use mondrian_timeline::audio::{
        AudioChannelStrip, AudioMixBus, AudioRoute, FADER_DB_PARAMETER_ID,
    };
    use mondrian_timeline::sequence::Sequence;

    fn chinese() -> Localizer {
        Localizer::new(AppUiLocale::ZhCn).expect("Chinese catalog")
    }

    #[test]
    fn sidechain_bus_removal_projection_retains_connections_and_locked_detector() {
        use mondrian_timeline::audio::{
            AudioProcessorInstance, AudioProcessorSidechainRoute, BUILTIN_GAIN_DEFINITION_ID,
        };
        for enabled in [true, false] {
            for locked in [false, true] {
                let mut sequence = Sequence::new("Sidechain deletion");
                let detector_id = sequence.audio_tracks[0].id;
                sequence.audio_tracks[0].is_locked = locked;
                let target = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
                let target_id = target.id;
                let output = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
                let output_id = output.id;
                sequence.audio_program.outputs[0].strip.pre_fader.processors.push(output);
                let bus_id = MixBusId::new();
                let mut strip = AudioChannelStrip::default();
                strip.pre_fader.processors.push(target);
                sequence.audio_program.buses.push(AudioMixBus {
                    id: bus_id,
                    name: "Detector submix".into(),
                    strip,
                });
                let mut incoming = AudioProcessorSidechainRoute::new(
                    AudioRouteSource::Track {
                        track_id: detector_id,
                        port: AudioChannelStripOutputPort::PostFaderPreMute,
                    },
                    target_id,
                    "test.detector",
                );
                incoming.enabled = enabled;
                let mut outgoing = AudioProcessorSidechainRoute::new(
                    AudioRouteSource::Bus {
                        bus_id,
                        port: AudioChannelStripOutputPort::PostMute,
                    },
                    output_id,
                    "test.detector",
                );
                outgoing.enabled = enabled;
                sequence.audio_program.sidechain_routes.extend([incoming, outgoing]);
                let domain = mondrian_timeline::inspect_audio_mix_bus(&sequence, bus_id)
                    .expect("valid bus dependencies");
                assert_eq!(domain.connected_route_count(), 2);
                assert_eq!(domain.can_disconnect_and_remove(), !locked);
                let mut state = AppState::new();
                state.test_set_sequence(Some(sequence));
                let localizer = chinese();
                let projection = AudioMixerPanelModel::from_app_state(&state, &localizer);
                let removal = projection
                    .channels
                    .iter()
                    .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id })
                    .and_then(|channel| channel.bus_removal.as_ref())
                    .expect("bus removal controls");
                assert_eq!(
                    removal.connected_route_count, 2,
                    "retained auxiliary routes are strong references, including disabled routes"
                );
                assert_eq!(removal.is_editable, !locked);
                assert_eq!(remove_bus_action(removal).is_some(), !locked);
                assert_eq!(
                    removal.edit_disabled_reason,
                    locked.then(|| routing_blocker_label(
                        &AudioRoutingEditBlocker::LockedTrack(detector_id),
                        &localizer,
                    ))
                );
                assert!(!state.can_undo_action(), "inspection is not authoring");
            }
        }
    }

    #[test]
    fn mixer_gain_actions_preserve_precision_through_author_undo() {
        let mut state = AppState::new();
        state.test_set_sequence(Some(Sequence::new("Precise Mixer")));
        let precise = -6.123456789123456_f64;
        let snapshot = |state: &AppState, field: usize| {
            let model = AudioMixerPanelModel::from_app_state(state, &chinese());
            let channel = &model.channels[0];
            if field == 0 {
                channel.input_trim_db
            } else {
                let gain = if field == 1 {
                    channel.fader
                } else {
                    channel.outbound_routes[0].gain
                };
                match gain {
                    AudioMixerGainModel::Static { value_db } => value_db,
                    _ => panic!("static gain"),
                }
            }
        };
        for field in 0..3 {
            let model = AudioMixerPanelModel::from_app_state(&state, &chinese());
            let channel = &model.channels[0];
            let action = match field {
                0 => input_trim_target(channel).and_then(|target| target.set_action(precise)),
                1 => fader_target(channel).and_then(|target| target.set_action(precise)),
                _ => route_gain_target(&channel.outbound_routes[0])
                    .and_then(|target| target.set_action(precise)),
            }
            .expect("gain action");
            let before = snapshot(&state, field);
            state.dispatch_action(action).expect("author gain");
            assert_eq!(snapshot(&state, field), precise);
            assert!(state.undo_timeline().expect("undo"));
            assert_eq!(snapshot(&state, field), before);
            assert!(state.redo_timeline().expect("redo"));
            assert_eq!(snapshot(&state, field), precise);
            assert!(input_trim_target(channel)
                .and_then(|target| target.set_action(f64::NAN))
                .is_none());
            assert!(fader_target(channel)
                .and_then(|target| target.set_action(f64::INFINITY))
                .is_none());
            assert!(route_gain_target(&channel.outbound_routes[0])
                .and_then(|target| target.set_action(AUDIO_GAIN_DB_MIN - 1.0))
                .is_none());
        }
    }

    #[test]
    fn projection_orders_tracks_buses_outputs_and_reuses_racks_and_routes() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Mixer");
        let bus_id = mondrian_core::MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus_id,
            name: "Dialog".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        let track_count = sequence.audio_tracks.len();
        let bus_count = sequence.audio_program.buses.len();
        let output_count = sequence.audio_program.outputs.len();
        state.test_set_sequence(Some(sequence));

        let model = AudioMixerPanelModel::from_app_state(&state, &chinese());
        assert_eq!(model.channels.len(), track_count + bus_count + output_count);
        assert_eq!(model.channels[0].kind, AudioMixerChannelKind::Track);
        assert_eq!(model.channels[track_count].kind, AudioMixerChannelKind::Bus);
        assert_eq!(
            model.channels[track_count + bus_count].kind,
            AudioMixerChannelKind::ProgramOutput
        );
        assert!(model.channels.iter().all(|channel| channel.processor_racks.len() == 2));
        assert!(matches!(
            model.channels[track_count].processor_racks[0].address,
            AudioProcessorRackAddress::ChannelStrip {
                owner: AudioChannelStripOwner::Bus { bus_id: projected },
                rack: AudioChannelStripRack::PreFader,
            } if projected == bus_id
        ));
        assert!(!model.channels[0].outbound_routes.is_empty());
        assert!(!model.channels[0].route_create_options.is_empty());
        assert_eq!(model.channels[0].track_soloed, Some(false));
        assert!(model.channels.iter().all(|channel| channel.meter.is_none()));
        assert_eq!(
            model.channels[track_count + bus_count].incoming_route_count,
            track_count
        );
    }

    #[test]
    fn english_mixer_projects_route_and_rack_copy_with_stable_addresses() {
        let mut state = AppState::new();
        let empty = AudioMixerPanelModel::from_app_state(
            &state,
            &Localizer::new(AppUiLocale::EnUs).expect("English catalog"),
        );
        assert!(empty
            .empty_message
            .as_deref()
            .is_some_and(|text| text.starts_with("No active sequence")));
        state.test_set_sequence(Some(Sequence::new("Mixer locale")));
        let chinese = AudioMixerPanelModel::from_app_state(&state, &chinese());
        let english = AudioMixerPanelModel::from_app_state(
            &state,
            &Localizer::new(AppUiLocale::EnUs).expect("English catalog"),
        );
        let chinese_track = &chinese.channels[0];
        let english_track = &english.channels[0];
        assert_eq!(
            english_track.processor_racks[0].title,
            "Pre-Fader Processor Rack"
        );
        assert_eq!(
            english_track.outbound_routes[0].source_port_label,
            "Post-Mute"
        );
        assert_eq!(english_track.owner, chinese_track.owner);
        assert_eq!(
            english_track.outbound_routes[0].route_id,
            chinese_track.outbound_routes[0].route_id
        );
    }

    #[test]
    fn strip_actions_are_typed_and_automated_fader_never_uses_static_control() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Mixer actions");
        let track_id = sequence.audio_tracks[0].id;
        let mut automation =
            ExactAutomationCurve::new(ParameterId::new_static(FADER_DB_PARAMETER_ID), -3.0)
                .expect("curve");
        automation
            .set_keyframe(ExactAutomationKeyframe::linear(TimelineTime::ZERO, -3.0))
            .expect("keyframe");
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("channel")
            .strip
            .fader_automation = Some(automation);
        let bus_id = MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus_id,
            name: "Locked Bus".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        sequence.audio_program.routes.push(AudioRoute::new(
            AudioRouteSource::Track {
                track_id,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(bus_id),
        ));
        sequence.audio_tracks[0].is_locked = true;
        state.test_set_sequence(Some(sequence));
        let model = AudioMixerPanelModel::from_app_state(&state, &chinese());
        let channel = &model.channels[0];

        assert!(matches!(
            channel.fader,
            AudioMixerGainModel::Automated { .. }
        ));
        assert!(fader_target(channel).and_then(|target| target.set_action(-6.0)).is_none());
        assert!(input_trim_target(channel).and_then(|target| target.set_action(-6.0)).is_none());
        assert!(channel.route_create_options.is_empty());
        assert!(channel.outbound_routes.iter().all(|route| !route.is_editable));
        let bus = model
            .channels
            .iter()
            .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id })
            .expect("Bus channel");
        assert!(!bus.bus_removal.as_ref().expect("Bus removal").is_editable);
        let _mute = set_track_mute_action(track_id, true);
        let solo = set_track_solo_action(track_id, true);
        assert!(matches!(
            ProductAction::decode_external(&solo).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::SetTrackSolo(
                crate::app::product_action::AudioTrackSoloPayload {
                    track_id: projected,
                    soloed: true,
                }
            ))) if projected == track_id
        ));

        let mut sequence = state.active_sequence().expect("Sequence").clone();
        sequence.audio_tracks[0].is_locked = false;
        sequence
            .audio_program
            .track_channels
            .get_mut(&track_id)
            .expect("channel")
            .strip
            .fader_automation = None;
        state.test_set_sequence(Some(sequence));
        let channel = &AudioMixerPanelModel::from_app_state(&state, &chinese()).channels[0];
        let action = fader_target(channel)
            .and_then(|target| target.set_action(-6.0))
            .expect("static fader action");
        assert!(matches!(
            crate::app::parameter_gesture::ParameterGestureAction::decode(&action).expect("typed scalar codec"),
            Some(crate::app::parameter_gesture::ParameterGestureAction::SetAudioScalar { edit })
                if edit.target.owner == (crate::app::parameter_gesture::AudioScalarOwner::Fader { owner: AudioChannelStripOwner::Track { track_id } })
                    && edit.value.to_bits() == (-6.0_f64).to_bits()
        ));
    }

    #[test]
    fn bus_and_route_factories_emit_only_typed_routing_intents() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Mixer routing");
        let bus_id = MixBusId::new();
        sequence.audio_program.buses.push(AudioMixBus {
            id: bus_id,
            name: "Bus 1".to_owned(),
            strip: AudioChannelStrip::default(),
        });
        state.test_set_sequence(Some(sequence));
        let model = AudioMixerPanelModel::from_app_state(&state, &chinese());
        let action = create_bus_action(&model).expect("create Bus action");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::EditRouting(
                AudioRoutingEditRequest {
                    edit: AudioRoutingEdit::CreateBus { route_to: Some(_), .. }
                }
            )))
        ));

        let track = &model.channels[0];
        let option = &track.route_create_options[0];
        let action = create_route_action(option);
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::EditRouting(
                AudioRoutingEditRequest { edit: AudioRoutingEdit::CreateRoute { .. } }
            )))
        ));
        let route = &track.outbound_routes[0];
        assert!(set_route_enabled_action(route, false).is_some());
        assert!(route_gain_target(route).and_then(|target| target.set_action(-6.0)).is_some());
        assert!(remove_route_action(route).is_some());
        let current = track
            .route_create_options
            .iter()
            .find(|option| option.source == route.source && option.destination == route.destination)
            .expect("current Route endpoints remain a visible candidate");
        assert!(rewire_route_action(route, current).is_none());
        let replacement = track
            .route_create_options
            .iter()
            .find(|option| option.source != route.source || option.destination != route.destination)
            .expect("replacement Route endpoints");
        let action = rewire_route_action(route, replacement).expect("rewire Route action");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::EditRouting(
                AudioRoutingEditRequest {
                    edit: AudioRoutingEdit::SetRouteEndpoints {
                        route_id,
                        source,
                        destination,
                    }
                }
            ))) if route_id == route.route_id
                && source == replacement.source
                && destination == replacement.destination
        ));

        let bus = model
            .channels
            .iter()
            .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id })
            .expect("Bus channel");
        let action = rename_bus_action(bus, "Dialogue").expect("rename Bus action");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::EditRouting(
                AudioRoutingEditRequest {
                    edit: AudioRoutingEdit::RenameBus { bus_id: projected, name }
                }
            ))) if projected == bus_id && name == "Dialogue"
        ));
        assert!(rename_bus_action(track, "invalid owner").is_none());
    }

    #[test]
    fn route_menu_hides_feedback_through_enabled_and_retained_sidechains() {
        use mondrian_timeline::audio::{
            AudioProcessorInstance, AudioProcessorSidechainRoute, BUILTIN_GAIN_DEFINITION_ID,
        };
        for enabled in [false, true] {
            let mut state = AppState::new();
            let mut sequence = Sequence::new("Sidechain-aware Mixer");
            let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
            let processor_id = processor.id;
            let target = MixBusId::new();
            let detector = MixBusId::new();
            let mut strip = AudioChannelStrip::default();
            strip.post_fader.processors.push(processor);
            sequence.audio_program.buses.extend([
                AudioMixBus { id: target, name: "Target".to_owned(), strip },
                AudioMixBus {
                    id: detector,
                    name: "Detector".to_owned(),
                    strip: AudioChannelStrip::default(),
                },
            ]);
            let mut sidechain = AudioProcessorSidechainRoute::new(
                AudioRouteSource::Bus {
                    bus_id: detector,
                    port: AudioChannelStripOutputPort::PreFader,
                },
                processor_id,
                "detector",
            );
            sidechain.enabled = enabled;
            sequence.audio_program.sidechain_routes.push(sidechain);
            state.test_set_sequence(Some(sequence));
            let model = AudioMixerPanelModel::from_app_state(&state, &chinese());
            let target_channel = model
                .channels
                .iter()
                .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id: target })
                .expect("Target Bus");
            assert!(
                target_channel.route_create_options.iter().any(|option| option.destination
                    == AudioRouteDestination::Bus(detector)
                    && matches!(
                        option.source,
                        AudioRouteSource::Bus { port: AudioChannelStripOutputPort::PreFader, .. }
                    )),
                "Causal pre-fader forward send remains accessible"
            );
            assert!(
                target_channel
                    .route_create_options
                    .iter()
                    .filter(|option| option.destination == AudioRouteDestination::Bus(detector))
                    .all(|option| matches!(
                        option.source,
                        AudioRouteSource::Bus { port: AudioChannelStripOutputPort::PreFader, .. }
                    )),
                "Final-tap feedback must be hidden even when sidechain enabled={enabled}"
            );
            assert!(
                target_channel
                    .route_create_options
                    .iter()
                    .any(|option| matches!(option.destination, AudioRouteDestination::Output(_))),
                "Valid output destination remains accessible"
            );
            let detector_channel = model
                .channels
                .iter()
                .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id: detector })
                .expect("Detector Bus");
            assert!(
                detector_channel
                    .route_create_options
                    .iter()
                    .any(|option| option.destination == AudioRouteDestination::Bus(target)),
                "Parallel forward connection is valid"
            );
        }
    }

    #[test]
    fn route_candidates_hide_cycles_even_when_the_existing_edge_is_disabled() {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("Cycle-aware Mixer");
        let first = MixBusId::new();
        let second = MixBusId::new();
        let third = MixBusId::new();
        sequence.audio_program.buses.extend([
            AudioMixBus {
                id: first,
                name: "First".to_owned(),
                strip: AudioChannelStrip::default(),
            },
            AudioMixBus {
                id: second,
                name: "Second".to_owned(),
                strip: AudioChannelStrip::default(),
            },
            AudioMixBus {
                id: third,
                name: "Third".to_owned(),
                strip: AudioChannelStrip::default(),
            },
        ]);
        let mut edge = AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id: first,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(second),
        );
        edge.enabled = false;
        sequence.audio_program.routes.push(edge);
        sequence.audio_program.routes.push(AudioRoute::new(
            AudioRouteSource::Bus {
                bus_id: second,
                port: AudioChannelStripOutputPort::PostMute,
            },
            AudioRouteDestination::Bus(third),
        ));
        state.test_set_sequence(Some(sequence));

        let model = AudioMixerPanelModel::from_app_state(&state, &chinese());
        let first_channel = model
            .channels
            .iter()
            .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id: first })
            .expect("first Bus");
        let second_channel = model
            .channels
            .iter()
            .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id: second })
            .expect("second Bus");
        let third_channel = model
            .channels
            .iter()
            .find(|channel| channel.owner == AudioChannelStripOwner::Bus { bus_id: third })
            .expect("third Bus");

        assert!(first_channel
            .route_create_options
            .iter()
            .any(|option| option.destination == AudioRouteDestination::Bus(second)));
        assert!(second_channel
            .route_create_options
            .iter()
            .all(|option| option.destination != AudioRouteDestination::Bus(first)));
        assert!(third_channel.route_create_options.iter().all(|option| {
            option.destination != AudioRouteDestination::Bus(first)
                && option.destination != AudioRouteDestination::Bus(second)
        }));
        assert!(third_channel
            .route_create_options
            .iter()
            .any(|option| matches!(option.destination, AudioRouteDestination::Output(_))));
    }
}

#[cfg(test)]
mod scalar_contract_tests {
    use super::*;
    use crate::app::parameter_gesture::{
        AudioScalarGestureTarget, AudioScalarOwner, AudioScalarValueEdit, ParameterGestureAction,
        ParameterGestureEdit,
    };
    use mondrian_core::{AuthoringGestureId, SequenceId, TimelineTime};
    use mondrian_timeline::Sequence;

    #[derive(Clone, Copy, Debug)]
    enum ScalarContractField {
        Parameter,
        Trim,
        Fader,
        Route,
        Sidechain,
    }

    struct ScalarContractFixture {
        state: AppState,
        track_id: mondrian_core::TrackId,
        processor_id: mondrian_core::AudioProcessorInstanceId,
        route_id: mondrian_core::AudioRouteId,
        sidechain_id: mondrian_core::AudioRouteId,
    }

    impl ScalarContractFixture {
        fn new() -> Self {
            use mondrian_timeline::audio::{
                AudioChannelStripOutputPort, AudioProcessorInstance, AudioProcessorSidechainRoute,
                AudioRouteSource, BUILTIN_GAIN_DEFINITION_ID,
            };
            let mut sequence = Sequence::new("Closed scalar contracts");
            let track_id = sequence.audio_tracks[0].id;
            let processor = AudioProcessorInstance::built_in(BUILTIN_GAIN_DEFINITION_ID, 1);
            let processor_id = processor.id;
            sequence.audio_program.outputs[0].strip.pre_fader.processors.push(processor);
            let sidechain = AudioProcessorSidechainRoute::new(
                AudioRouteSource::Track {
                    track_id,
                    port: AudioChannelStripOutputPort::PostMute,
                },
                processor_id,
                "retained.absent.detector",
            );
            let sidechain_id = sidechain.id;
            sequence.audio_program.sidechain_routes.push(sidechain);
            let route_id = sequence.audio_program.routes.iter().find(|route| matches!(route.source, AudioRouteSource::Track { track_id: id, .. } if id == track_id)).expect("principal route").id;
            let mut state = AppState::new();
            state.test_set_sequence(Some(sequence));
            Self {
                state,
                track_id,
                processor_id,
                route_id,
                sidechain_id,
            }
        }

        fn target(&self, field: ScalarContractField) -> Option<AudioScalarGestureTarget> {
            use crate::app_ui::{
                audio_mixer::{
                    fader_target, input_trim_target, route_gain_target, AudioMixerPanelModel,
                },
                audio_processor_rack::static_parameter_target,
                localization::{AppUiLocale, Localizer},
            };
            let model = AudioMixerPanelModel::from_app_state(
                &self.state,
                &Localizer::new(AppUiLocale::EnUs).expect("catalog"),
            );
            let track = model
                .channels
                .iter()
                .find(|channel| {
                    channel.owner
                        == mondrian_timeline::AudioChannelStripOwner::Track {
                            track_id: self.track_id,
                        }
                })
                .expect("current Track channel");
            match field {
                ScalarContractField::Trim => input_trim_target(track),
                ScalarContractField::Fader => fader_target(track),
                ScalarContractField::Route => route_gain_target(
                    track.outbound_routes.iter().find(|route| route.route_id == self.route_id)?,
                ),
                ScalarContractField::Parameter | ScalarContractField::Sidechain => {
                    let (rack, processor) = model
                        .channels
                        .iter()
                        .flat_map(|channel| &channel.processor_racks)
                        .find_map(|rack| {
                            rack.processors
                                .iter()
                                .find(|processor| processor.processor_id == self.processor_id)
                                .map(|processor| (rack, processor))
                        })?;
                    if matches!(field, ScalarContractField::Parameter) {
                        static_parameter_target(
                            rack,
                            processor,
                            processor.parameters.iter().find(|parameter| {
                                parameter.parameter_id.as_str()
                                    == mondrian_timeline::audio::GAIN_DB_PARAMETER_ID
                            })?,
                        )
                    } else {
                        processor
                            .sidechain_routes
                            .iter()
                            .find(|route| route.route_id == self.sidechain_id)?
                            .scalar_target
                            .clone()
                    }
                }
            }
        }

        fn value(&self, field: ScalarContractField) -> f64 {
            let sequence = self.state.active_sequence().expect("Sequence");
            match field {
                ScalarContractField::Parameter => {
                    sequence.audio_program.outputs[0]
                        .strip
                        .pre_fader
                        .processors
                        .iter()
                        .find(|processor| processor.id == self.processor_id)
                        .expect("same processor")
                        .parameters[&mondrian_core::ParameterId::new_static(
                        mondrian_timeline::audio::GAIN_DB_PARAMETER_ID,
                    )]
                        .automation
                        .as_ref()
                        .expect("bound host curve")
                        .default_value
                }
                ScalarContractField::Trim => {
                    sequence.audio_program.track_channels[&self.track_id].strip.input_trim_db
                }
                ScalarContractField::Fader => {
                    sequence.audio_program.track_channels[&self.track_id].strip.fader_db
                }
                ScalarContractField::Route => {
                    sequence
                        .audio_program
                        .routes
                        .iter()
                        .find(|route| route.id == self.route_id)
                        .expect("same route")
                        .gain_db
                }
                ScalarContractField::Sidechain => {
                    sequence
                        .audio_program
                        .sidechain_routes
                        .iter()
                        .find(|route| route.id == self.sidechain_id)
                        .expect("same sidechain")
                        .gain_db
                }
            }
        }

        fn edit(target: &AudioScalarGestureTarget, value: f64) -> ParameterGestureEdit {
            ParameterGestureEdit::AudioScalar(AudioScalarValueEdit {
                target: Box::new(target.clone()),
                value,
            })
        }

        fn assert_refused(&mut self, target: &AudioScalarGestureTarget) {
            let before = self.state.active_sequence().expect("refusal baseline").clone();
            let history = self.state.authoring_history().expect("history").diagnostics();
            assert!(self
                .state
                .dispatch_action(target.set_action(-7.0).expect("closed finite queued intent"))
                .is_err());
            assert!(self
                .state
                .active_sequence()
                .expect("refusal")
                .author_state_eq_ignoring_revision(&before));
            assert_eq!(
                self.state.authoring_history().expect("history").diagnostics(),
                history
            );
        }
    }

    const SCALAR_CONTRACT_FIELDS: [ScalarContractField; 5] = [
        ScalarContractField::Parameter,
        ScalarContractField::Trim,
        ScalarContractField::Fader,
        ScalarContractField::Route,
        ScalarContractField::Sidechain,
    ];

    #[test]
    fn typed_audio_scalar_discrete_edits_preserve_exact_values_and_independent_undo() {
        for field in SCALAR_CONTRACT_FIELDS {
            let mut fixture = ScalarContractFixture::new();
            let original = fixture.state.active_sequence().expect("origin").clone();
            let history = fixture.state.authoring_history().expect("history").diagnostics();
            let target = fixture.target(field).expect("genuine current author lease");
            for value in [-6.123456789123456_f64, -3.234567891234567_f64] {
                fixture
                    .state
                    .dispatch_action(target.set_action(value).expect("precise intent"))
                    .expect("discrete canonical author action");
                assert_eq!(
                    fixture.value(field).to_bits(),
                    value.to_bits(),
                    "{field:?} retains the authored f64"
                );
                assert!(!fixture.state.parameter_gesture_is_current());
            }
            let committed = fixture.state.active_sequence().expect("commit").clone();
            assert_eq!(
                fixture.state.authoring_history().expect("history").diagnostics().undo_entries,
                history.undo_entries + 2
            );
            for _ in 0..2 {
                assert!(fixture.state.undo_timeline().expect("independent Undo"));
            }
            assert!(fixture
                .state
                .active_sequence()
                .expect("origin restored")
                .author_state_eq_ignoring_revision(&original));
            for _ in 0..2 {
                assert!(fixture.state.redo_timeline().expect("independent Redo"));
            }
            assert!(fixture
                .state
                .active_sequence()
                .expect("commit restored")
                .author_state_eq_ignoring_revision(&committed));
            assert!(
                target.set_action(f64::NAN).is_none() && target.set_action(f64::INFINITY).is_none()
            );
            if !matches!(field, ScalarContractField::Parameter) {
                assert!(target
                    .set_action(mondrian_timeline::audio::AUDIO_GAIN_DB_MIN - 1.0)
                    .is_none());
                assert!(target
                    .set_action(mondrian_timeline::audio::AUDIO_GAIN_DB_MAX + 1.0)
                    .is_none());
            }
        }
    }

    #[test]
    fn typed_audio_scalar_reopened_session_and_sequence_leases_refuse_queued_values() {
        for field in SCALAR_CONTRACT_FIELDS {
            let mut fixture = ScalarContractFixture::new();
            let target = fixture.target(field).expect("old concrete lease");
            let same_ids = fixture.state.active_sequence().expect("Sequence").clone();
            fixture.state.test_set_sequence(None);
            fixture.state.test_set_sequence(Some(same_ids));
            let fresh = fixture.target(field).expect("reopened concrete lease");
            assert_eq!(
                target.owner, fresh.owner,
                "{field:?}: identical IDs are not a lifetime lease"
            );
            assert_ne!(target.lease.session_id, fresh.lease.session_id);
            fixture.assert_refused(&target);
            let mut same_entities_in_other_sequence =
                fixture.state.active_sequence().expect("reopened Sequence").clone();
            same_entities_in_other_sequence.id = SequenceId::new();
            let other_sequence_id = same_entities_in_other_sequence.id;
            fixture.state.test_add_sequence(same_entities_in_other_sequence);
            fixture.state.test_set_active_sequence(other_sequence_id);
            let other = fixture.target(field).expect("same typed entities in the other Sequence");
            assert_eq!(fresh.owner, other.owner);
            assert_eq!(fresh.lease.session_id, other.lease.session_id);
            assert_ne!(fresh.lease.sequence_id, other.lease.sequence_id);
            fixture.assert_refused(&fresh);
        }
    }

    #[test]
    fn typed_audio_scalar_stale_begin_never_seals_another_valid_live_owner() {
        for field in SCALAR_CONTRACT_FIELDS {
            let mut fixture = ScalarContractFixture::new();
            let target = fixture.target(field).expect("current scalar");
            let original = fixture.state.active_sequence().expect("origin").clone();
            let gesture_id = AuthoringGestureId::new();
            fixture
                .state
                .dispatch_action(
                    ParameterGestureAction::Begin {
                        gesture_id,
                        edit: ScalarContractFixture::edit(&target, -6.0),
                    }
                    .action(),
                )
                .expect("current Begin");
            let history = fixture.state.authoring_history().expect("live history").diagnostics();
            let mut stale = target.clone();
            stale.lease.sequence_id = SequenceId::new();
            fixture
                .state
                .dispatch_action(
                    ParameterGestureAction::Begin {
                        gesture_id: AuthoringGestureId::new(),
                        edit: ScalarContractFixture::edit(&stale, -30.0),
                    }
                    .action(),
                )
                .expect("safe stale Begin refusal");
            assert!(
                fixture.state.parameter_gesture_is_current()
                    && fixture.state.parameter_gesture_owns(gesture_id),
                "{field:?}: original exact gesture still owns its author transaction"
            );
            assert_eq!(
                fixture.state.authoring_history().expect("history").diagnostics(),
                history
            );
            fixture
                .state
                .dispatch_action(
                    ParameterGestureAction::Update {
                        gesture_id,
                        edit: ScalarContractFixture::edit(&target, -3.0),
                    }
                    .action(),
                )
                .expect("original Update");
            fixture
                .state
                .dispatch_action(ParameterGestureAction::Cancel { gesture_id }.action())
                .expect("original Cancel");
            assert!(fixture
                .state
                .active_sequence()
                .expect("cancel")
                .author_state_eq_ignoring_revision(&original));
            assert!(
                !fixture.state.can_undo_action(),
                "{field:?}: canceled live updates publish no author Undo"
            );
        }
    }

    #[test]
    fn typed_audio_scalar_intervening_edit_keeps_independent_history_and_blocks_late_cancel() {
        for field in SCALAR_CONTRACT_FIELDS {
            let mut fixture = ScalarContractFixture::new();
            let target = fixture.target(field).expect("current scalar");
            let original = fixture.state.active_sequence().expect("origin").clone();
            let gesture_id = AuthoringGestureId::new();
            for action in [
                ParameterGestureAction::Begin {
                    gesture_id,
                    edit: ScalarContractFixture::edit(&target, -6.0),
                },
                ParameterGestureAction::Update {
                    gesture_id,
                    edit: ScalarContractFixture::edit(&target, -3.0),
                },
            ] {
                fixture.state.dispatch_action(action.action()).expect("live scalar authoring");
            }
            let dragged = fixture.state.active_sequence().expect("dragged").clone();
            fixture
                .state
                .dispatch_action(crate::app::ui_actions::audio_routing_edit_action(
                    mondrian_timeline::AudioRoutingEditRequest {
                        edit: mondrian_timeline::AudioRoutingEdit::SetRouteEnabled {
                            route_id: fixture.route_id,
                            enabled: false,
                        },
                    },
                ))
                .expect("independent route switch");
            let intervening = fixture.state.active_sequence().expect("independent edit").clone();
            let history = fixture.state.authoring_history().expect("history").diagnostics();
            assert!(!fixture.state.parameter_gesture_is_current());
            for action in [
                ParameterGestureAction::Update {
                    gesture_id,
                    edit: ScalarContractFixture::edit(&target, -15.0),
                },
                ParameterGestureAction::Cancel { gesture_id },
                ParameterGestureAction::Finish { gesture_id },
            ] {
                fixture
                    .state
                    .dispatch_action(action.action())
                    .expect("late intent safely refused");
            }
            assert!(fixture
                .state
                .active_sequence()
                .expect("late refusal")
                .author_state_eq_ignoring_revision(&intervening));
            assert_eq!(
                fixture.state.authoring_history().expect("history").diagnostics(),
                history
            );
            assert!(fixture.state.undo_timeline().expect("Undo independent switch"));
            assert!(fixture
                .state
                .active_sequence()
                .expect("drag remains")
                .author_state_eq_ignoring_revision(&dragged));
            assert!(fixture.state.undo_timeline().expect("Undo drag"));
            assert!(
                fixture
                    .state
                    .active_sequence()
                    .expect("origin")
                    .author_state_eq_ignoring_revision(&original),
                "{field:?}: no whole-Rack fallback restoration"
            );
        }
    }

    #[test]
    fn typed_audio_scalar_keyed_fields_and_source_locks_refuse_previously_queued_static_values() {
        use mondrian_timeline::{
            AudioAutomationEdit, AudioAutomationEditRequest, AudioAutomationTarget,
        };
        for field in [
            ScalarContractField::Parameter,
            ScalarContractField::Fader,
            ScalarContractField::Route,
            ScalarContractField::Sidechain,
        ] {
            let mut fixture = ScalarContractFixture::new();
            let target = fixture.target(field).expect("previous static owner");
            let automation = match &target.owner {
                AudioScalarOwner::Parameter { processor_id, parameter_schema, .. } => {
                    AudioAutomationTarget::ProcessorParameter {
                        rack: target.owner.processor_address().expect("exact rack"),
                        processor_id: *processor_id,
                        parameter_id: parameter_schema.parameter_id.clone(),
                    }
                }
                AudioScalarOwner::Fader { owner } => {
                    AudioAutomationTarget::ChannelFader { owner: *owner }
                }
                AudioScalarOwner::RouteLevel { route_id, .. }
                | AudioScalarOwner::ProcessorSidechainLevel { route_id, .. } => {
                    AudioAutomationTarget::RouteGain { route_id: *route_id }
                }
                AudioScalarOwner::Trim { .. } => unreachable!(),
            };
            fixture
                .state
                .dispatch_action(crate::app::ui_actions::audio_automation_edit_action(
                    AudioAutomationEditRequest {
                        target: automation,
                        edit: AudioAutomationEdit::UpsertKeyframe {
                            keyframe: mondrian_core::ExactAutomationKeyframe::linear(
                                TimelineTime::ZERO,
                                -3.0,
                            ),
                        },
                    },
                ))
                .expect("canonical curve becomes author authority");
            assert!(
                fixture.target(field).is_none(),
                "{field:?}: keyed control cannot acquire a static author lease"
            );
            fixture.assert_refused(&target);
        }
        for field in [
            ScalarContractField::Trim,
            ScalarContractField::Fader,
            ScalarContractField::Route,
            ScalarContractField::Sidechain,
        ] {
            let mut fixture = ScalarContractFixture::new();
            let target = fixture.target(field).expect("previous editable owner");
            fixture
                .state
                .dispatch_action(crate::app::ui_actions::track_set_author_control_action(
                    crate::app::ui_actions::TrackSetAuthorControlPayload {
                        track_id: fixture.track_id,
                        control: crate::app::ui_actions::TrackAuthorControl::Lock,
                        enabled: true,
                    },
                ))
                .expect("canonical persistent Track lock");
            assert!(
                fixture.target(field).is_none(),
                "{field:?}: fresh projection disables a locked scalar"
            );
            fixture.assert_refused(&target);
        }
    }

    #[test]
    fn typed_audio_scalar_routes_require_exact_retained_endpoints_and_live_identity() {
        use mondrian_timeline::audio::{AudioChannelStripOutputPort, AudioRouteSource};
        use mondrian_timeline::{AudioRoutingEdit, AudioRoutingEditRequest};
        for field in [ScalarContractField::Route, ScalarContractField::Sidechain] {
            let mut fixture = ScalarContractFixture::new();
            let old = fixture.target(field).expect("original endpoint owner");
            let source = AudioRouteSource::Track {
                track_id: fixture.track_id,
                port: AudioChannelStripOutputPort::PreFader,
            };
            let edit = match &old.owner {
                AudioScalarOwner::RouteLevel { route_id, destination, .. } => {
                    AudioRoutingEdit::SetRouteEndpoints {
                        route_id: *route_id,
                        source,
                        destination: *destination,
                    }
                }
                AudioScalarOwner::ProcessorSidechainLevel { route_id, processor_id, .. } => {
                    AudioRoutingEdit::SetProcessorSidechainEndpoints {
                        route_id: *route_id,
                        source,
                        processor_id: *processor_id,
                        bus_key: "retained.rebound.detector".into(),
                    }
                }
                _ => unreachable!(),
            };
            fixture
                .state
                .dispatch_action(crate::app::ui_actions::audio_routing_edit_action(
                    AudioRoutingEditRequest { edit },
                ))
                .expect("canonical same-ID rewire");
            let fresh = fixture.target(field).expect("new exact endpoint owner");
            assert_eq!(old.lease, fresh.lease);
            assert_ne!(
                old.owner, fresh.owner,
                "changing endpoints is a new control binding"
            );
            fixture.assert_refused(&old);
            let edit = match &fresh.owner {
                AudioScalarOwner::RouteLevel { route_id, .. } => {
                    AudioRoutingEdit::RemoveRoute { route_id: *route_id }
                }
                AudioScalarOwner::ProcessorSidechainLevel { route_id, .. } => {
                    AudioRoutingEdit::RemoveProcessorSidechain { route_id: *route_id }
                }
                _ => unreachable!(),
            };
            fixture
                .state
                .dispatch_action(crate::app::ui_actions::audio_routing_edit_action(
                    AudioRoutingEditRequest { edit },
                ))
                .expect("canonical route deletion");
            assert!(fixture.target(field).is_none());
            fixture.assert_refused(&fresh);
        }
    }

    #[test]
    fn typed_audio_scalar_codec_is_closed_and_does_not_infer_opaque_author_intents() {
        let fixture = ScalarContractFixture::new();
        for field in SCALAR_CONTRACT_FIELDS {
            let target = fixture.target(field).expect("author-bound target");
            let mut action = target.set_action(-6.0).expect("closed scalar action");
            assert!(matches!(
                ParameterGestureEdit::from_action(&action),
                Some(ParameterGestureEdit::AudioScalar(_))
            ));
            let Action::Custom { payload, .. } = &mut action else {
                panic!("explicit typed namespace")
            };
            payload["edit"].as_object_mut().expect("closed fields").insert(
                "author_operation".into(),
                serde_json::json!({"operation":"remove_route","route_id":fixture.route_id}),
            );
            assert!(
                ParameterGestureAction::decode(&action).is_err(),
                "{field:?}: nested author commands are not admitted"
            );
        }
        for raw in [
            crate::app::ui_actions::audio_channel_strip_edit_action(
                mondrian_timeline::AudioChannelStripEditRequest {
                    owner: mondrian_timeline::AudioChannelStripOwner::Track {
                        track_id: fixture.track_id,
                    },
                    edit: mondrian_timeline::AudioChannelStripEdit::SetInputTrimDb { value: -6.0 },
                },
            ),
            crate::app::ui_actions::audio_routing_edit_action(
                mondrian_timeline::AudioRoutingEditRequest {
                    edit: mondrian_timeline::AudioRoutingEdit::SetRouteGainDb {
                        route_id: fixture.route_id,
                        value: -6.0,
                    },
                },
            ),
        ] {
            assert!(
                ParameterGestureEdit::from_action(&raw).is_none(),
                "arbitrary AudioProductAction envelopes remain discrete author commands"
            );
        }
    }
}
