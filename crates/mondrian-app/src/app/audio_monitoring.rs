//! Open-Session audio audition intent and Runtime observation binding.
//!
//! Solo is a transient monitoring overlay: it changes the compiled playback
//! Signal Closure without mutating Sequence author state, dirty state, or
//! Undo/Redo. Meter observations are bound to the exact Authoring Session,
//! Sequence, and prepared audio Runtime that produced them.

use std::collections::{BTreeSet, HashMap};

use mondrian_audio::{
    AudioAuditionOverlay, AudioDeliveryEvidence, AudioMeterFrame, AudioMeterObserver,
};
use mondrian_core::{SequenceId, TrackId};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_media::RealtimeAudioOutputDeviceEvidence;
use mondrian_timeline::Sequence;

/// Proven active path from one Sequence Program Output to one physical stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAudioMonitoringPathEvidence {
    /// Program-to-monitor semantic channel mapping selected before execution.
    pub delivery: AudioDeliveryEvidence,
    /// CPAL host/device/candidate selection for the current output contract.
    pub device: RealtimeAudioOutputDeviceEvidence,
    /// Concrete callback stream generation joining the two evidence sets.
    pub stream_generation: u64,
}

fn bind_active_audio_monitoring_path(
    delivery: AudioDeliveryEvidence,
    device: RealtimeAudioOutputDeviceEvidence,
    output: mondrian_media::RealtimeAudioOutputSnapshot,
) -> Option<ActiveAudioMonitoringPathEvidence> {
    if output.contract != device.contract
        || delivery.target_layout != output.contract.channel_layout
    {
        return None;
    }
    Some(ActiveAudioMonitoringPathEvidence {
        delivery,
        device,
        stream_generation: output.stream_generation,
    })
}

#[derive(Default)]
pub(super) struct AudioMonitoringState {
    session_id: Option<AuthoringSessionId>,
    soloed_tracks: HashMap<SequenceId, BTreeSet<TrackId>>,
    meter: Option<BoundAudioMeter>,
}

struct BoundAudioMeter {
    session_id: AuthoringSessionId,
    sequence_id: SequenceId,
    observer: AudioMeterObserver,
    delivery_evidence: AudioDeliveryEvidence,
}

impl AudioMonitoringState {
    pub(super) fn reset(&mut self) {
        self.session_id = None;
        self.soloed_tracks.clear();
        self.meter = None;
    }

    fn ensure_session(&mut self, session_id: AuthoringSessionId) {
        if self.session_id != Some(session_id) {
            self.session_id = Some(session_id);
            self.soloed_tracks.clear();
            self.meter = None;
        }
    }

    pub(super) fn audition_overlay(
        &self,
        session_id: AuthoringSessionId,
        sequence: &Sequence,
    ) -> AudioAuditionOverlay {
        if self.session_id != Some(session_id) {
            return AudioAuditionOverlay::default();
        }
        let valid_tracks =
            sequence.audio_tracks.iter().map(|track| track.id).collect::<BTreeSet<_>>();
        AudioAuditionOverlay {
            soloed_tracks: self
                .soloed_tracks
                .get(&sequence.id)
                .map(|tracks| tracks.intersection(&valid_tracks).copied().collect())
                .unwrap_or_default(),
        }
    }

    pub(super) fn set_track_solo(
        &mut self,
        session_id: AuthoringSessionId,
        sequence_id: SequenceId,
        track_id: TrackId,
        soloed: bool,
    ) -> bool {
        self.ensure_session(session_id);
        let tracks = self.soloed_tracks.entry(sequence_id).or_default();
        let changed = if soloed {
            tracks.insert(track_id)
        } else {
            tracks.remove(&track_id)
        };
        if tracks.is_empty() {
            self.soloed_tracks.remove(&sequence_id);
        }
        changed
    }

    pub(super) fn is_track_soloed(
        &self,
        session_id: AuthoringSessionId,
        sequence_id: SequenceId,
        track_id: TrackId,
    ) -> bool {
        self.session_id == Some(session_id)
            && self
                .soloed_tracks
                .get(&sequence_id)
                .is_some_and(|tracks| tracks.contains(&track_id))
    }

    pub(super) fn bind_meter(
        &mut self,
        session_id: AuthoringSessionId,
        sequence_id: SequenceId,
        observer: AudioMeterObserver,
        delivery_evidence: AudioDeliveryEvidence,
    ) {
        self.ensure_session(session_id);
        self.meter = Some(BoundAudioMeter {
            session_id,
            sequence_id,
            observer,
            delivery_evidence,
        });
    }

    pub(super) fn clear_meter(&mut self) {
        self.meter = None;
    }

    pub(super) fn latest_meter(
        &self,
        session_id: AuthoringSessionId,
        sequence_id: SequenceId,
    ) -> Option<AudioMeterFrame> {
        let meter = self
            .meter
            .as_ref()
            .filter(|meter| meter.session_id == session_id && meter.sequence_id == sequence_id)?;
        let frame = meter.observer.latest();
        (frame.block_serial != 0).then_some(frame)
    }

    pub(super) fn delivery_evidence(
        &self,
        session_id: AuthoringSessionId,
        sequence_id: SequenceId,
    ) -> Option<AudioDeliveryEvidence> {
        self.meter
            .as_ref()
            .filter(|meter| meter.session_id == session_id && meter.sequence_id == sequence_id)
            .map(|meter| meter.delivery_evidence)
    }
}

impl super::AppState {
    pub(super) fn set_audio_track_solo(
        &mut self,
        payload: super::product_action::AudioTrackSoloPayload,
    ) -> mondrian_core::Result<()> {
        let session_id = self.authoring_session_id().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "audio_set_track_solo".to_owned(),
                reason: "当前没有打开的项目会话".to_owned(),
            }
        })?;
        let sequence = self.active_sequence().ok_or_else(|| {
            mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "audio_set_track_solo".to_owned(),
                reason: "当前没有活动序列".to_owned(),
            }
        })?;
        if !sequence.audio_tracks.iter().any(|track| track.id == payload.track_id) {
            return Err(mondrian_core::MondrianError::WorkflowStepFailed {
                step_id: "audio_set_track_solo".to_owned(),
                reason: "目标不是活动序列中的音频轨道".to_owned(),
            });
        }
        let sequence_id = sequence.id;
        let was_soloed =
            self.audio_monitoring.is_track_soloed(session_id, sequence_id, payload.track_id);
        if was_soloed == payload.soloed {
            return Ok(());
        }
        self.audio_monitoring.set_track_solo(
            session_id,
            sequence_id,
            payload.track_id,
            payload.soloed,
        );
        if let Err(error) = self.refresh_audio_playback_after_program_change() {
            self.audio_monitoring.set_track_solo(
                session_id,
                sequence_id,
                payload.track_id,
                was_soloed,
            );
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn is_audio_track_soloed(&self, track_id: TrackId) -> bool {
        self.authoring_session_id().zip(self.active_sequence_id()).is_some_and(
            |(session_id, sequence_id)| {
                self.audio_monitoring.is_track_soloed(session_id, sequence_id, track_id)
            },
        )
    }

    pub(crate) fn latest_audio_meter_frame(&self) -> Option<AudioMeterFrame> {
        if !self.is_playing() {
            return None;
        }
        let (session_id, sequence_id) =
            self.authoring_session_id().zip(self.active_sequence_id())?;
        self.audio_monitoring.latest_meter(session_id, sequence_id)
    }

    /// Exact active Program Output to monitoring-device layout mapping.
    pub fn active_audio_delivery_evidence(&self) -> Option<AudioDeliveryEvidence> {
        let (session_id, sequence_id) =
            self.authoring_session_id().zip(self.active_sequence_id())?;
        self.audio_monitoring.delivery_evidence(session_id, sequence_id)
    }

    /// Complete active Program Output to physical monitoring-stream evidence.
    ///
    /// A stale device selection, absent stream, or layout mismatch returns no
    /// evidence instead of composing facts from different generations.
    pub fn active_audio_monitoring_path_evidence(
        &self,
    ) -> Option<ActiveAudioMonitoringPathEvidence> {
        let delivery = self.active_audio_delivery_evidence()?;
        let device = self.latest_audio_output_device_evidence()?;
        let output = self.audio_playback_snapshot().output?;
        bind_active_audio_monitoring_path(delivery, device, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::AudioTrackSoloPayload;
    use std::time::Instant;

    fn output_contract() -> mondrian_media::RealtimeAudioOutputContract {
        mondrian_media::RealtimeAudioOutputContract {
            sample_rate: 48_000,
            channel_layout: mondrian_core::AudioChannelLayout::Stereo,
            sample_format: mondrian_media::RealtimeAudioSampleFormat::F32,
            channel_semantics: mondrian_media::RealtimeAudioChannelSemantics::StereoConvention,
            supported_buffer_size: mondrian_media::RealtimeAudioSupportedBufferSize::Unknown,
            candidates: mondrian_media::RealtimeAudioCandidateCounts {
                enumerated: 3,
                matching_channels: 2,
                matching_sample_rate: 2,
                executable: 2,
            },
        }
    }

    #[test]
    fn monitoring_path_binds_only_matching_delivery_and_physical_contracts() {
        let contract = output_contract();
        let device = RealtimeAudioOutputDeviceEvidence {
            host_name: "test-host".to_owned(),
            device_id: mondrian_media::RealtimeAudioOutputDeviceId::new("test:test-device")
                .expect("test device identity"),
            selection: mondrian_media::RealtimeAudioOutputDeviceSelection::SystemDefault,
            was_system_default: true,
            device_name: Some("test-device".to_owned()),
            device_name_error: None,
            contract,
        };
        let output = mondrian_media::RealtimeAudioOutputSnapshot {
            captured_at: Instant::now(),
            stream_generation: 17,
            contract,
            callback_consumed_frames: 0,
            active_callback_consumed_frames: 0,
            active_duration: None,
            callback_count: 0,
            underrun_frames: 0,
            last_callback_frames: 0,
            last_callback_playback_delay: None,
            last_callback_age: None,
            buffered_frames: 0,
            stream_failed: false,
            active: false,
        };
        let delivery = AudioDeliveryEvidence {
            program_layout: mondrian_core::AudioChannelLayout::Mono,
            target_layout: mondrian_core::AudioChannelLayout::Stereo,
            mapping_kind: mondrian_audio::AudioDeliveryMappingKind::Standard,
            coefficient_count: 2,
        };
        let bound = bind_active_audio_monitoring_path(delivery, device.clone(), output)
            .expect("matching active path");
        assert_eq!(bound.stream_generation, 17);
        assert_eq!(bound.device.contract, contract);

        let mut stale_device = device;
        stale_device.contract.sample_rate = 44_100;
        assert!(bind_active_audio_monitoring_path(delivery, stale_device, output).is_none());
    }

    #[test]
    fn solo_is_session_local_idempotent_and_does_not_advance_author_state() {
        let mut state = crate::app::AppState::new();
        let sequence = Sequence::new("solo session");
        let sequence_id = sequence.id;
        let track_id = sequence.audio_tracks[0].id;
        let reopened = sequence.clone();
        state.test_set_sequence(Some(sequence));
        let session_id = state.authoring_session_id().expect("session");
        let generation = state.project_author_generation();

        state
            .set_audio_track_solo(AudioTrackSoloPayload { track_id, soloed: true })
            .expect("solo");
        assert!(state.is_audio_track_soloed(track_id));
        assert_eq!(state.project_author_generation(), generation);
        assert_eq!(
            state
                .audio_monitoring
                .audition_overlay(session_id, state.active_sequence().expect("sequence"))
                .soloed_tracks,
            BTreeSet::from([track_id])
        );

        state
            .set_audio_track_solo(AudioTrackSoloPayload { track_id, soloed: true })
            .expect("idempotent solo");
        assert_eq!(state.project_author_generation(), generation);

        state.test_set_sequence(None);
        state.test_set_sequence(Some(reopened));
        assert_eq!(state.active_sequence_id(), Some(sequence_id));
        assert!(!state.is_audio_track_soloed(track_id));
    }

    #[test]
    fn solo_rejects_non_audio_track_without_changing_monitoring_state() {
        let mut state = crate::app::AppState::new();
        let sequence = Sequence::new("solo validation");
        let audio_track = sequence.audio_tracks[0].id;
        let video_track = sequence.video_tracks[0].id;
        state.test_set_sequence(Some(sequence));

        assert!(state
            .set_audio_track_solo(AudioTrackSoloPayload { track_id: video_track, soloed: true })
            .is_err());
        assert!(!state.is_audio_track_soloed(audio_track));
        assert!(!state.is_audio_track_soloed(video_track));
    }
}
