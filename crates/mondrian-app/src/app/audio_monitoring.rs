//! Open-Session audio audition intent and Runtime observation binding.
//!
//! Solo is a transient monitoring overlay: it changes the compiled playback
//! Signal Closure without mutating Sequence author state, dirty state, or
//! Undo/Redo. Meter observations are bound to the exact Authoring Session,
//! Sequence, and prepared audio Runtime that produced them.

use std::collections::{BTreeSet, HashMap};

use mondrian_audio::{AudioAuditionOverlay, AudioMeterFrame, AudioMeterObserver};
use mondrian_core::{SequenceId, TrackId};
use mondrian_editor_state::AuthoringSessionId;
use mondrian_timeline::Sequence;

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
    ) {
        self.ensure_session(session_id);
        self.meter = Some(BoundAudioMeter { session_id, sequence_id, observer });
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::product_action::AudioTrackSoloPayload;

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
