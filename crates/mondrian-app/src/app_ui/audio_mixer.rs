//! Audio Mixer projection and typed action factories.
//!
//! This App UI Adapter presents Sequence-owned Track, Bus, and Program Output
//! Channel Strips. Timeline remains authority for strip/Rack addresses, locks,
//! exact automation, and mutations; no mixer widget owns shadow author state.

use mondrian_core::TrackId;
use mondrian_editor_state::Action;
use mondrian_timeline::audio::{AudioChannelStripOwner, AUDIO_GAIN_DB_MAX, AUDIO_GAIN_DB_MIN};
use mondrian_timeline::{
    inspect_audio_channel_strip, AudioChannelStripEdit, AudioChannelStripEditBlocker,
    AudioChannelStripEditRequest, AudioChannelStripRack, AudioProcessorRackAddress,
};

use crate::app::ui_actions::{
    audio_channel_strip_edit_action, timeline_set_track_control_action,
    TimelineSetTrackControlPayload, TimelineTrackControlPayloadKind,
};
use crate::app::AppState;

use super::audio_processor_rack::{project_audio_processor_rack, AudioProcessorRackModel};

/// Complete immutable Mixer panel projection.
#[derive(Debug, Clone)]
pub(crate) struct AudioMixerPanelModel {
    pub(crate) channels: Vec<AudioMixerChannelModel>,
    pub(crate) empty_message: Option<String>,
}

/// Product-facing kind of one Channel Strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioMixerChannelKind {
    Track,
    Bus,
    ProgramOutput,
}

/// Honest fader projection: static and automated values are disjoint.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AudioMixerFaderModel {
    Static { value_db: f64 },
    Automated { keyframe_count: usize },
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
    pub(crate) fader: AudioMixerFaderModel,
    pub(crate) track_muted: Option<bool>,
    pub(crate) processor_racks: Vec<AudioProcessorRackModel>,
}

impl AudioMixerPanelModel {
    pub(crate) fn from_app_state(state: &AppState) -> Self {
        let Some(sequence) = state.active_sequence() else {
            return Self {
                channels: Vec::new(),
                empty_message: Some(
                    "没有活动序列\n打开或创建序列后，可在这里混合轨道、Bus 与节目输出。".to_owned(),
                ),
            };
        };
        let mut channels = Vec::with_capacity(
            sequence
                .audio_tracks
                .len()
                .saturating_add(sequence.audio_program.buses.len())
                .saturating_add(sequence.audio_program.outputs.len()),
        );
        channels.extend(sequence.audio_tracks.iter().map(|track| {
            project_channel(
                sequence,
                AudioChannelStripOwner::Track { track_id: track.id },
                AudioMixerChannelKind::Track,
                track.name.clone(),
                Some(track.is_muted),
            )
        }));
        channels.extend(sequence.audio_program.buses.iter().map(|bus| {
            project_channel(
                sequence,
                AudioChannelStripOwner::Bus { bus_id: bus.id },
                AudioMixerChannelKind::Bus,
                bus.name.clone(),
                None,
            )
        }));
        channels.extend(sequence.audio_program.outputs.iter().map(|output| {
            project_channel(
                sequence,
                AudioChannelStripOwner::ProgramOutput { output_id: output.id },
                AudioMixerChannelKind::ProgramOutput,
                output.name.clone(),
                None,
            )
        }));
        Self { channels, empty_message: None }
    }
}

fn project_channel(
    sequence: &mondrian_timeline::sequence::Sequence,
    owner: AudioChannelStripOwner,
    kind: AudioMixerChannelKind,
    name: String,
    track_muted: Option<bool>,
) -> AudioMixerChannelModel {
    let inspection = inspect_audio_channel_strip(sequence, owner);
    let (is_editable, edit_disabled_reason, input_trim_db, fader) = match inspection {
        Ok(inspection) => {
            let strip = inspection.strip();
            let fader = strip.fader_automation.as_ref().map_or(
                AudioMixerFaderModel::Static { value_db: strip.fader_db },
                |curve| AudioMixerFaderModel::Automated { keyframe_count: curve.keyframes.len() },
            );
            (
                inspection.is_editable(),
                inspection.edit_blocker().map(edit_blocker_label),
                strip.input_trim_db,
                fader,
            )
        }
        Err(error) => (
            false,
            Some(format!("作者状态无法解析此 Channel Strip：{error}")),
            0.0,
            AudioMixerFaderModel::Static { value_db: 0.0 },
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
    AudioMixerChannelModel {
        owner,
        kind,
        name,
        is_editable,
        edit_disabled_reason,
        input_trim_db,
        fader,
        track_muted,
        processor_racks,
    }
}

fn edit_blocker_label(blocker: &AudioChannelStripEditBlocker) -> String {
    match blocker {
        AudioChannelStripEditBlocker::LockedTrack(track_id) => {
            format!("轨道 {track_id} 已锁定；处理器、输入增益和推子为只读")
        }
    }
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
    (channel.is_editable && matches!(channel.fader, AudioMixerFaderModel::Static { .. })).then(
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

fn normalized_gain(value: f32) -> Option<f64> {
    let value = f64::from(value);
    (value.is_finite() && (AUDIO_GAIN_DB_MIN..=AUDIO_GAIN_DB_MAX).contains(&value)).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::{AudioProductAction, ProductAction};
    use mondrian_core::{ExactAutomationCurve, ExactAutomationKeyframe, ParameterId, TimelineTime};
    use mondrian_timeline::audio::{AudioChannelStrip, AudioMixBus, FADER_DB_PARAMETER_ID};
    use mondrian_timeline::sequence::Sequence;

    #[test]
    fn projection_orders_tracks_buses_outputs_and_reuses_both_rack_addresses() {
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
        sequence.audio_tracks[0].is_locked = true;
        state.test_set_sequence(Some(sequence));
        let channel = &AudioMixerPanelModel::from_app_state(&state).channels[0];

        assert!(matches!(
            channel.fader,
            AudioMixerFaderModel::Automated { .. }
        ));
        assert!(set_fader_action(channel, -6.0).is_none());
        assert!(set_input_trim_action(channel, -6.0).is_none());
        assert!(matches!(
            channel.owner,
            AudioChannelStripOwner::Track { .. }
        ));
        let _mute = set_track_mute_action(track_id, true);

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
}
