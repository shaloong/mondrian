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
    ProgramOutputMainSource, AUDIO_GAIN_DB_MAX, AUDIO_GAIN_DB_MIN,
};
use mondrian_timeline::{
    inspect_audio_channel_strip, inspect_audio_route, inspect_audio_route_candidates,
    AudioAutomationTarget, AudioBusRemovalPolicy, AudioChannelStripEdit,
    AudioChannelStripEditBlocker, AudioChannelStripEditRequest, AudioChannelStripRack,
    AudioProcessorRackAddress, AudioRouteCandidateInspection, AudioRoutingEdit,
    AudioRoutingEditBlocker, AudioRoutingEditRequest,
};

use crate::app::product_action::AudioTrackSoloPayload;
use crate::app::ui_actions::{
    audio_channel_strip_edit_action, audio_routing_edit_action, audio_track_solo_action,
    timeline_set_track_control_action, TimelineSetTrackControlPayload,
    TimelineTrackControlPayloadKind,
};
use crate::app::AppState;

use super::audio_automation::{
    project_audio_automation, sequence_automation_viewport, AudioAutomationCurveModel,
};
use super::audio_processor_rack::{project_audio_processor_rack, AudioProcessorRackModel};

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
    pub(crate) route_id: AudioRouteId,
    pub(crate) source: AudioRouteSource,
    pub(crate) destination: AudioRouteDestination,
    pub(crate) source_port_label: &'static str,
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
    pub(crate) fn from_app_state(state: &AppState) -> Self {
        let Some(sequence) = state.active_sequence() else {
            return Self {
                channels: Vec::new(),
                next_bus_name: None,
                new_bus_destination: None,
                empty_message: Some(
                    "没有活动序列\n打开或创建序列后，可在这里混合轨道、Bus 与节目输出。".to_owned(),
                ),
            };
        };
        let meter = state.latest_audio_meter_frame();
        let mut channels = Vec::with_capacity(
            sequence
                .audio_tracks
                .len()
                .saturating_add(sequence.audio_program.buses.len())
                .saturating_add(sequence.audio_program.outputs.len()),
        );
        let routing = AudioMixerRoutingIndex::build(sequence);
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
            )
        }));
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
                inspection.edit_blocker().map(channel_strip_blocker_label),
                strip.input_trim_db,
                fader,
            )
        }
        Err(error) => (
            false,
            Some(format!("作者状态无法解析此 Channel Strip：{error}")),
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
        )
    })
    .collect();
    let outbound_routes = routing.outbound.get(&owner).cloned().unwrap_or_default();
    let incoming_route_count = routing.incoming_counts.get(&owner).copied().unwrap_or(0);
    let route_create_options = route_source_for_owner(owner)
        .map_or_else(Vec::new, |owner_source| {
            route_create_options(sequence, routing, owner_source, is_editable)
        });
    let bus_removal = match owner {
        AudioChannelStripOwner::Bus { bus_id } => routing.bus_removals.get(&bus_id).cloned(),
        AudioChannelStripOwner::Track { .. } | AudioChannelStripOwner::ProgramOutput { .. } => None,
    };
    AudioMixerChannelModel {
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
    fn build(sequence: &mondrian_timeline::sequence::Sequence) -> Self {
        let mut index = Self {
            route_candidates: inspect_audio_route_candidates(sequence).ok(),
            ..Self::default()
        };
        index.bus_removals.extend(sequence.audio_program.buses.iter().map(|bus| {
            (
                bus.id,
                AudioMixerBusRemovalModel {
                    bus_id: bus.id,
                    connected_route_count: 0,
                    is_editable: true,
                    edit_disabled_reason: None,
                },
            )
        }));
        for route in &sequence.audio_program.routes {
            let projected = project_route(sequence, route);
            let connected_bus_disabled_reason = (!projected.is_editable)
                .then(|| projected.edit_disabled_reason.clone())
                .flatten();
            index
                .outbound
                .entry(owner_for_source(route.source))
                .or_default()
                .push(projected);
            *index
                .incoming_counts
                .entry(owner_for_destination(route.destination))
                .or_default() += 1;

            let mut connected_buses = Vec::with_capacity(2);
            if let AudioRouteSource::Bus { bus_id, .. } = route.source {
                connected_buses.push(bus_id);
            }
            if let AudioRouteDestination::Bus(bus_id) = route.destination {
                if !connected_buses.contains(&bus_id) {
                    connected_buses.push(bus_id);
                }
            }
            for bus_id in connected_buses {
                if let Some(removal) = index.bus_removals.get_mut(&bus_id) {
                    removal.connected_route_count += 1;
                    if let Some(reason) = connected_bus_disabled_reason.as_ref() {
                        removal.is_editable = false;
                        removal.edit_disabled_reason = Some(reason.clone());
                    }
                }
            }
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
) -> AudioMixerRouteModel {
    let (is_editable, edit_disabled_reason) = match inspect_audio_route(sequence, route.id) {
        Ok(inspection) => {
            let blocker = inspection.edit_blocker().cloned();
            (
                inspection.is_editable(),
                blocker.as_ref().map(routing_blocker_label),
            )
        }
        Err(error) => (false, Some(format!("作者状态无法解析此 Route：{error}"))),
    };
    AudioMixerRouteModel {
        route_id: route.id,
        source: route.source,
        destination: route.destination,
        source_port_label: source_port_label(route.source),
        destination_label: destination_label(sequence, route.destination),
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
            )
        }),
        is_editable,
        edit_disabled_reason,
    }
}

fn owner_for_source(source: AudioRouteSource) -> AudioChannelStripOwner {
    match source {
        AudioRouteSource::Track { track_id, .. } => AudioChannelStripOwner::Track { track_id },
        AudioRouteSource::Bus { bus_id, .. } => AudioChannelStripOwner::Bus { bus_id },
    }
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
                    format!("Bus · {}", bus.name),
                ))
        })
        .collect::<Vec<_>>();
    destinations.extend(sequence.audio_program.outputs.iter().filter_map(|output| {
        matches!(output.main_source, ProgramOutputMainSource::RoutedInputs).then_some((
            AudioRouteDestination::Output(output.id),
            format!("节目输出 · {}", output.name),
        ))
    }));
    let source_identity = match owner_source {
        AudioRouteSource::Track { track_id, .. } => RouteSourceIdentity::Track(track_id),
        AudioRouteSource::Bus { bus_id, .. } => RouteSourceIdentity::Bus(bus_id),
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
                        label: format!("{} → {destination_label}", source_port_label(source)),
                        source,
                        destination,
                    }
                }
            })
        })
        .filter(|option| routing.allows_candidate(option.source, option.destination))
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

fn source_port_label(source: AudioRouteSource) -> &'static str {
    match source {
        AudioRouteSource::Track { port: AudioChannelStripOutputPort::PreFader, .. }
        | AudioRouteSource::Bus { port: AudioChannelStripOutputPort::PreFader, .. } => "推子前",
        AudioRouteSource::Track {
            port: AudioChannelStripOutputPort::PostFaderPreMute,
            ..
        } => "推子后 / 静音前",
        AudioRouteSource::Track { port: AudioChannelStripOutputPort::PostMute, .. } => "静音后",
        AudioRouteSource::Bus {
            port:
                AudioChannelStripOutputPort::PostFaderPreMute | AudioChannelStripOutputPort::PostMute,
            ..
        } => "推子后",
    }
}

fn destination_label(
    sequence: &mondrian_timeline::sequence::Sequence,
    destination: AudioRouteDestination,
) -> String {
    match destination {
        AudioRouteDestination::Bus(bus_id) => {
            sequence.audio_program.buses.iter().find(|bus| bus.id == bus_id).map_or_else(
                || format!("缺失 Bus · {bus_id}"),
                |bus| format!("Bus · {}", bus.name),
            )
        }
        AudioRouteDestination::Output(output_id) => sequence
            .audio_program
            .outputs
            .iter()
            .find(|output| output.id == output_id)
            .map_or_else(
                || format!("缺失节目输出 · {output_id}"),
                |output| format!("节目输出 · {}", output.name),
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

fn channel_strip_blocker_label(blocker: &AudioChannelStripEditBlocker) -> String {
    match blocker {
        AudioChannelStripEditBlocker::LockedTrack(track_id) => {
            format!("轨道 {track_id} 已锁定；处理器、输入增益和推子为只读")
        }
    }
}

fn routing_blocker_label(blocker: &AudioRoutingEditBlocker) -> String {
    match blocker {
        AudioRoutingEditBlocker::LockedTrack(track_id) => {
            format!("轨道 {track_id} 已锁定；其 Route 为只读")
        }
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
            source: option.source,
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
                    source: option.source,
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

pub(crate) fn set_route_gain_action(route: &AudioMixerRouteModel, value_db: f32) -> Option<Action> {
    let value = normalized_gain(value_db)?;
    (route.is_editable && matches!(route.gain, AudioMixerGainModel::Static { .. })).then(|| {
        audio_routing_edit_action(AudioRoutingEditRequest {
            edit: AudioRoutingEdit::SetRouteGainDb { route_id: route.route_id, value },
        })
    })
}

pub(crate) fn remove_route_action(route: &AudioMixerRouteModel) -> Option<Action> {
    route.is_editable.then(|| {
        audio_routing_edit_action(AudioRoutingEditRequest {
            edit: AudioRoutingEdit::RemoveRoute { route_id: route.route_id },
        })
    })
}

pub(crate) fn set_input_trim_action(
    channel: &AudioMixerChannelModel,
    value_db: f32,
) -> Option<Action> {
    let value = normalized_gain(value_db)?;
    channel.is_editable.then(|| {
        audio_channel_strip_edit_action(AudioChannelStripEditRequest {
            owner: channel.owner,
            edit: AudioChannelStripEdit::SetInputTrimDb { value },
        })
    })
}

pub(crate) fn set_fader_action(channel: &AudioMixerChannelModel, value_db: f32) -> Option<Action> {
    let value = normalized_gain(value_db)?;
    (channel.is_editable && matches!(channel.fader, AudioMixerGainModel::Static { .. })).then(
        || {
            audio_channel_strip_edit_action(AudioChannelStripEditRequest {
                owner: channel.owner,
                edit: AudioChannelStripEdit::SetFaderDb { value },
            })
        },
    )
}

pub(crate) fn set_track_mute_action(track_id: TrackId, muted: bool) -> Action {
    timeline_set_track_control_action(TimelineSetTrackControlPayload {
        track_id,
        is_video_track: false,
        control: TimelineTrackControlPayloadKind::Mute,
        enabled: muted,
    })
}

pub(crate) fn set_track_solo_action(track_id: TrackId, soloed: bool) -> Action {
    audio_track_solo_action(AudioTrackSoloPayload { track_id, soloed })
}

fn normalized_gain(value: f32) -> Option<f64> {
    let value = f64::from(value);
    (value.is_finite() && (AUDIO_GAIN_DB_MIN..=AUDIO_GAIN_DB_MAX).contains(&value)).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use mondrian_core::{ExactAutomationCurve, ExactAutomationKeyframe, ParameterId, TimelineTime};
    use mondrian_timeline::audio::{
        AudioChannelStrip, AudioMixBus, AudioRoute, FADER_DB_PARAMETER_ID,
    };
    use mondrian_timeline::sequence::Sequence;

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

        let model = AudioMixerPanelModel::from_app_state(&state);
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
        let model = AudioMixerPanelModel::from_app_state(&state);
        let channel = &model.channels[0];

        assert!(matches!(
            channel.fader,
            AudioMixerGainModel::Automated { .. }
        ));
        assert!(set_fader_action(channel, -6.0).is_none());
        assert!(set_input_trim_action(channel, -6.0).is_none());
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
        let channel = &AudioMixerPanelModel::from_app_state(&state).channels[0];
        let action = set_fader_action(channel, -6.0).expect("static fader action");
        assert!(matches!(
            ProductAction::decode_external(&action).expect("decode"),
            Some(ProductAction::Audio(AudioProductAction::EditChannelStrip(
                AudioChannelStripEditRequest {
                    owner: AudioChannelStripOwner::Track { track_id: projected },
                    edit: AudioChannelStripEdit::SetFaderDb { value: -6.0 },
                }
            ))) if projected == track_id
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
        let model = AudioMixerPanelModel::from_app_state(&state);
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
        assert!(set_route_gain_action(route, -6.0).is_some());
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

        let model = AudioMixerPanelModel::from_app_state(&state);
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
