//! Exact picture and independently qualified physical Audio handoff evidence.
//!
//! Startup/recovery may follow the already-running clock after its initial
//! picture is proved. This does not change ordinary interval transition gates
//! or reinterpret the Playback Engine's sample-position/phase calculations.

use anyhow::{ensure, Context};
use mondrian_core::AudioSamplePosition;
use mondrian_media::{AudioPlaybackSnapshot, AudioPlaybackState};
use mondrian_playback::{
    AudioClockHandoffStatus, AudioClockObservationGrade, AudioDeviceClockState, ClockMaster,
    PlaybackRate, PlaybackSnapshot, TransportState,
};
use serde::Serialize;
use std::time::Instant;

use super::headless_realtime_playback::{HeadlessGpuCandidateIntent, HeadlessPreviewSample};

/// Complete evidence from one accepted Audio Device/current-picture join.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HeadlessAvPictureCompletion {
    pub(crate) initial_epoch: u64,
    pub(crate) initial_frame: i64,
    pub(crate) initial_quality_revision: u64,
    pub(crate) resolved_epoch: u64,
    pub(crate) resolved_frame: i64,
    pub(crate) resolved_quality_revision: u64,
    pub(crate) original_picture_proven: bool,
    pub(crate) sample: HeadlessPreviewSample,
    pub(crate) transport_state: TransportState,
    pub(crate) playback_rate: PlaybackRate,
    pub(crate) audio_observation: HeadlessAudioObservationEvidence,
    pub(crate) audio_handoff: HeadlessAudioHandoffEvidence,
    pub(crate) physical_output: HeadlessPhysicalAudioEvidence,
}

/// Scalar projection of the exact observation accepted by the Playback Engine.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HeadlessAudioObservationEvidence {
    epoch: u64,
    stream_generation: u64,
    sample_rate: u32,
    consumed_frames: u64,
    media_anchor: HeadlessAudioSampleAnchor,
    observed_at_ns: u128,
    grade: AudioClockObservationGrade,
    estimated_latency_frames: u32,
    uncertainty_frames: u32,
    underrun_frames: u64,
    state: AudioDeviceClockState,
}

/// Engine-produced phase qualification; no new phase tolerance is introduced.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HeadlessAudioHandoffEvidence {
    stream_generation: u64,
    phase_error_ns: i64,
    uncertainty_ns: u128,
    proven_phase_error_ns: u128,
    status: HeadlessAudioHandoffStatus,
}

#[derive(Debug, Clone, Copy, Serialize)]
enum HeadlessAudioHandoffStatus {
    Accepted,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct HeadlessAudioSampleAnchor {
    sample: i64,
    sample_rate: u32,
}

impl From<AudioSamplePosition> for HeadlessAudioSampleAnchor {
    fn from(position: AudioSamplePosition) -> Self {
        Self {
            sample: position.sample(),
            sample_rate: position.rate().hz(),
        }
    }
}

/// Actual newer physical callback counters, kept distinct from the Engine observation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HeadlessPhysicalAudioEvidence {
    render_generation: u64,
    stream_generation: u64,
    media_anchor: HeadlessAudioSampleAnchor,
    sample_rate: u32,
    channels: u8,
    capture_age_at_completion_ns: u128,
    callback_consumed_frames: u64,
    active_callback_consumed_frames: u64,
    active_duration_ns: Option<u128>,
    callback_count: u64,
    underrun_frames: u64,
    last_callback_frames: u32,
    last_callback_playback_delay_ns: Option<u128>,
    last_callback_age_ns: Option<u128>,
    buffered_frames: usize,
    stream_failed: bool,
    active: bool,
    activation_preroll_satisfied: bool,
    render_substitution_count: u64,
    render_generation_recovery_count: u64,
    active_interval_underrun_frames: u64,
    underrun_recovery_count: u64,
}

/// Permit clock-driven retargeting only after the initial exact picture is proved.
/// This initialization/recovery rule is not a realtime interval skip allowance.
pub(crate) fn can_retarget_after_initial_picture(
    proven: bool,
    original: HeadlessGpuCandidateIntent,
    previous: HeadlessGpuCandidateIntent,
    next: HeadlessGpuCandidateIntent,
    playback: PlaybackSnapshot,
) -> bool {
    proven
        && original.epoch == previous.epoch
        && previous.epoch == next.epoch
        && original.quality_revision <= previous.quality_revision
        && previous.quality_revision <= next.quality_revision
        && next.epoch == playback.epoch
        && next.quality_revision == playback.quality_revision
        && next.frame == playback.position.frame
        && matches!(
            playback.state,
            TransportState::Playing | TransportState::Recovering
        )
        && if playback.rate.is_forward() {
            next.frame >= previous.frame
                || (next.frame == original.frame && previous.frame >= original.frame)
        } else {
            next.frame <= previous.frame
                || (next.frame == original.frame && previous.frame <= original.frame)
        }
}

impl HeadlessAvPictureCompletion {
    /// Bind an exact resolved picture to the Engine's accepted same-stream handoff.
    /// The caller supplies its actual physical Ready sample; this constructor
    /// cannot create GPU or presentation authority from callback evidence.
    pub(crate) fn new(
        original: HeadlessGpuCandidateIntent,
        resolved: HeadlessGpuCandidateIntent,
        sample: HeadlessPreviewSample,
        original_picture_proven: bool,
        playback: PlaybackSnapshot,
        audio: AudioPlaybackSnapshot,
    ) -> anyhow::Result<Self> {
        ensure!(
            can_retarget_after_initial_picture(
                original_picture_proven,
                original,
                original,
                resolved,
                playback,
            ),
            "Audio picture completion changed unproved playback identity or direction: initial={original:?}, resolved={resolved:?}, playback={playback:?}"
        );
        ensure!(
            sample.current_gpu_ready && !sample.unavailable,
            "Audio completion lacks exact current physical picture readiness: {sample:?}"
        );
        ensure!(
            playback.state == TransportState::Playing
                && playback.clock_master == Some(ClockMaster::AudioDevice)
                && playback.rate.supports_realtime_audio(),
            "Audio Device is not the authoritative realtime clock"
        );
        let observation =
            playback.audio_clock_observation.context("missing Engine Audio observation")?;
        let handoff =
            playback.audio_handoff.context("missing Engine Audio handoff qualification")?;
        ensure!(
            observation.epoch == playback.epoch
                && observation.state == AudioDeviceClockState::Running,
            "Audio observation is not Running in the resolved epoch: {observation:?}"
        );
        ensure!(
            handoff.status == AudioClockHandoffStatus::Accepted
                && handoff.stream_generation == observation.stream_generation,
            "Audio handoff does not qualify the observed stream: {handoff:?}"
        );
        let output = audio.output.context("missing physical Audio output")?;
        let media_anchor = audio.media_anchor.context("missing physical Audio media anchor")?;
        ensure!(
            audio.state == AudioPlaybackState::Active
                && audio.activation_preroll_satisfied
                && output.active
                && !output.stream_failed,
            "physical Audio output is not actively consuming qualified PCM: {audio:?}"
        );
        ensure!(
            output.stream_generation == observation.stream_generation,
            "physical Audio stream generation differs from accepted handoff"
        );
        ensure!(
            media_anchor == observation.media_anchor
                && observation.sample_rate == output.contract.sample_rate
                && media_anchor.rate().hz() == output.contract.sample_rate,
            "physical Audio sample anchor or rate differs from accepted observation"
        );
        // The physical snapshot is newer than the observation fed into the
        // Engine. Callback progress may increase; equality would reject normal
        // playback. Stream/anchor changes are rejected above, not normalized.
        ensure!(
            output.callback_count > 0
                && output.last_callback_frames > 0
                && output.active_callback_consumed_frames >= observation.consumed_frames
                && output.active_callback_consumed_frames <= output.callback_consumed_frames,
            "physical Audio callback evidence regressed or is unavailable"
        );
        let capture_age = Instant::now()
            .checked_duration_since(output.captured_at)
            .context("physical Audio snapshot capture lies in the future")?;
        Ok(Self {
            initial_epoch: original.epoch.get(),
            initial_frame: original.frame,
            initial_quality_revision: original.quality_revision,
            resolved_epoch: resolved.epoch.get(),
            resolved_frame: resolved.frame,
            resolved_quality_revision: resolved.quality_revision,
            original_picture_proven,
            sample,
            transport_state: playback.state,
            playback_rate: playback.rate,
            audio_observation: HeadlessAudioObservationEvidence {
                epoch: observation.epoch.get(),
                stream_generation: observation.stream_generation,
                sample_rate: observation.sample_rate,
                consumed_frames: observation.consumed_frames,
                media_anchor: observation.media_anchor.into(),
                observed_at_ns: observation.observed_at.duration_since_origin().as_nanos(),
                grade: observation.grade,
                estimated_latency_frames: observation.estimated_latency_frames,
                uncertainty_frames: observation.uncertainty_frames,
                underrun_frames: observation.underrun_frames,
                state: observation.state,
            },
            audio_handoff: HeadlessAudioHandoffEvidence {
                stream_generation: handoff.stream_generation,
                phase_error_ns: handoff.phase_error_ns,
                uncertainty_ns: handoff.uncertainty_ns,
                proven_phase_error_ns: handoff.proven_phase_error_ns,
                status: HeadlessAudioHandoffStatus::Accepted,
            },
            physical_output: HeadlessPhysicalAudioEvidence {
                render_generation: audio.generation,
                stream_generation: output.stream_generation,
                media_anchor: media_anchor.into(),
                sample_rate: output.contract.sample_rate,
                channels: output.contract.channels(),
                capture_age_at_completion_ns: capture_age.as_nanos(),
                callback_consumed_frames: output.callback_consumed_frames,
                active_callback_consumed_frames: output.active_callback_consumed_frames,
                active_duration_ns: output.active_duration.map(|value| value.as_nanos()),
                callback_count: output.callback_count,
                underrun_frames: output.underrun_frames,
                last_callback_frames: output.last_callback_frames,
                last_callback_playback_delay_ns: output
                    .last_callback_playback_delay
                    .map(|value| value.as_nanos()),
                last_callback_age_ns: output.last_callback_age.map(|value| value.as_nanos()),
                buffered_frames: output.buffered_frames,
                stream_failed: output.stream_failed,
                active: output.active,
                activation_preroll_satisfied: audio.activation_preroll_satisfied,
                render_substitution_count: audio.render_substitution_count,
                render_generation_recovery_count: audio.render_generation_recovery_count,
                active_interval_underrun_frames: audio.active_interval_underrun_frames,
                underrun_recovery_count: audio.underrun_recovery_count,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_core::{AudioChannelLayout, AudioSampleRate};
    use mondrian_media::{
        RealtimeAudioCandidateCounts, RealtimeAudioChannelSemantics, RealtimeAudioOutputContract,
        RealtimeAudioOutputSnapshot, RealtimeAudioSampleFormat, RealtimeAudioSupportedBufferSize,
    };
    use mondrian_playback::{
        AudioClockHandoffEvidence, AudioDeviceClockObservation, MonotonicTimestamp, PlaybackEngine,
    };
    use std::time::Duration;

    struct Fixture {
        original: HeadlessGpuCandidateIntent,
        resolved: HeadlessGpuCandidateIntent,
        sample: HeadlessPreviewSample,
        playback: PlaybackSnapshot,
        audio: AudioPlaybackSnapshot,
    }

    impl Fixture {
        fn completion(self) -> anyhow::Result<HeadlessAvPictureCompletion> {
            HeadlessAvPictureCompletion::new(
                self.original,
                self.resolved,
                self.sample,
                true,
                self.playback,
                self.audio,
            )
        }
    }

    fn fixture() -> Fixture {
        let mut playback = PlaybackEngine::default().snapshot();
        playback.state = TransportState::Playing;
        playback.clock_master = Some(ClockMaster::AudioDevice);
        playback.position.frame = 1;
        let anchor = AudioSamplePosition::new(0, AudioSampleRate::new(48_000).expect("rate"));
        playback.audio_clock_observation = Some(AudioDeviceClockObservation {
            epoch: playback.epoch,
            stream_generation: 5,
            sample_rate: 48_000,
            consumed_frames: 960,
            media_anchor: anchor,
            observed_at: MonotonicTimestamp::from_duration(Duration::from_millis(20)),
            grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
            estimated_latency_frames: 100,
            uncertainty_frames: 480,
            underrun_frames: 0,
            state: AudioDeviceClockState::Running,
        });
        playback.audio_handoff = Some(AudioClockHandoffEvidence {
            stream_generation: 5,
            phase_error_ns: 0,
            uncertainty_ns: 10_000_000,
            proven_phase_error_ns: 10_000_000,
            status: AudioClockHandoffStatus::Accepted,
        });
        let mut audio = AudioPlaybackSnapshot::execution_unavailable();
        audio.generation = 17; // Render generation intentionally differs from the device generation.
        audio.state = AudioPlaybackState::Active;
        audio.activation_preroll_satisfied = true;
        audio.media_anchor = Some(anchor);
        audio.output = Some(RealtimeAudioOutputSnapshot {
            captured_at: Instant::now(),
            stream_generation: 5,
            contract: RealtimeAudioOutputContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                sample_format: RealtimeAudioSampleFormat::F32,
                channel_semantics: RealtimeAudioChannelSemantics::StereoConvention,
                supported_buffer_size: RealtimeAudioSupportedBufferSize::Unknown,
                candidates: RealtimeAudioCandidateCounts {
                    enumerated: 1,
                    matching_channels: 1,
                    matching_sample_rate: 1,
                    executable: 1,
                },
            },
            callback_consumed_frames: 1_440,
            active_callback_consumed_frames: 1_440,
            active_duration: Some(Duration::from_millis(30)),
            callback_count: 3,
            underrun_frames: 0,
            last_callback_frames: 480,
            last_callback_playback_delay: Some(Duration::from_millis(10)),
            last_callback_age: Some(Duration::from_millis(1)),
            buffered_frames: 5_760,
            stream_failed: false,
            active: true,
        });
        let original = HeadlessGpuCandidateIntent {
            epoch: playback.epoch,
            quality_revision: playback.quality_revision,
            frame: 0,
            pending_demand: None,
        };
        Fixture {
            original,
            resolved: HeadlessGpuCandidateIntent { frame: 1, ..original },
            sample: HeadlessPreviewSample {
                current_gpu_ready: true,
                stale_output_available: true,
                unavailable: false,
            },
            playback,
            audio,
        }
    }

    #[test]
    fn audio_handoff_completion_preserves_original_picture_and_newer_callback_progress() {
        let completion = fixture().completion().expect("accepted real-clock advancement");
        assert_eq!(completion.initial_frame, 0);
        assert_eq!(completion.resolved_frame, 1);
        let json = serde_json::to_value(completion).expect("durable evidence");
        assert_eq!(json["audio_observation"]["consumed_frames"], 960);
        assert_eq!(
            json["physical_output"]["active_callback_consumed_frames"],
            1_440
        );
        assert_eq!(json["physical_output"]["render_generation"], 17);
        assert_eq!(json["audio_handoff"]["stream_generation"], 5);
    }

    #[test]
    fn audio_handoff_completion_rejects_unbound_or_unready_evidence() {
        let mutations: [fn(&mut Fixture); 20] = [
            |f| f.resolved.quality_revision += 1,
            |f| f.resolved.frame += 1,
            |f| f.sample.current_gpu_ready = false,
            |f| f.sample.unavailable = true,
            |f| {
                f.sample.current_gpu_ready = false;
                f.sample.stale_output_available = true;
            },
            |f| {
                f.playback.audio_handoff.as_mut().expect("handoff").status =
                    AudioClockHandoffStatus::PhaseRejected
            },
            |f| f.playback.audio_handoff.as_mut().expect("handoff").stream_generation += 1,
            |f| {
                f.playback.audio_clock_observation.as_mut().expect("observation").state =
                    AudioDeviceClockState::Uncertain
            },
            |f| f.audio.output.as_mut().expect("output").stream_generation += 1,
            |f| f.audio.output.as_mut().expect("output").stream_failed = true,
            |f| {
                f.audio.media_anchor = Some(AudioSamplePosition::new(
                    1,
                    AudioSampleRate::new(48_000).expect("rate"),
                ))
            },
            |f| f.audio.output.as_mut().expect("output").active_callback_consumed_frames = 900,
            |f| f.playback.audio_clock_observation = None,
            |f| f.playback.audio_handoff = None,
            |f| f.playback.clock_master = Some(ClockMaster::Synthetic),
            |f| f.audio.output = None,
            |f| f.audio.media_anchor = None,
            |f| f.audio.output.as_mut().expect("output").active = false,
            |f| f.audio.activation_preroll_satisfied = false,
            |f| f.audio.output.as_mut().expect("output").contract.sample_rate = 44_100,
        ];
        for (index, mutate) in mutations.into_iter().enumerate() {
            let mut value = fixture();
            mutate(&mut value);
            assert!(
                value.completion().is_err(),
                "invalid evidence mutation {index}"
            );
        }
        let mut value = fixture();
        value.resolved.epoch =
            serde_json::from_value(serde_json::json!(value.original.epoch.get() + 1))
                .expect("different epoch");
        assert!(value.completion().is_err());
        let value = fixture();
        assert!(HeadlessAvPictureCompletion::new(
            value.original,
            value.resolved,
            value.sample,
            false,
            value.playback,
            value.audio
        )
        .is_err());
    }

    #[test]
    fn retarget_rule_requires_proven_picture_and_follows_exact_transport_direction() {
        let mut value = fixture();
        assert!(can_retarget_after_initial_picture(
            true,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
        assert!(!can_retarget_after_initial_picture(
            false,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
        value.playback.rate = PlaybackRate::REVERSE_1X;
        assert!(!can_retarget_after_initial_picture(
            true,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
        std::mem::swap(&mut value.original, &mut value.resolved);
        value.playback.position.frame = value.resolved.frame;
        assert!(can_retarget_after_initial_picture(
            true,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
        value.playback.state = TransportState::Priming;
        assert!(!can_retarget_after_initial_picture(
            true,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
    }

    #[test]
    fn startup_may_follow_engine_quality_recovery_but_cannot_complete_while_recovering() {
        let mut value = fixture();
        value.playback.state = TransportState::Recovering;
        value.playback.quality_revision += 1;
        value.resolved.quality_revision = value.playback.quality_revision;
        assert!(can_retarget_after_initial_picture(
            true,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
        assert!(value.completion().is_err());

        let mut value = fixture();
        value.playback.quality_revision += 1;
        value.resolved.quality_revision = value.playback.quality_revision;
        let completion = value.completion().expect("new exact picture after recovery");
        assert!(completion.resolved_quality_revision > completion.initial_quality_revision);

        let mut value = fixture();
        value.original.quality_revision = value.resolved.quality_revision + 1;
        assert!(!can_retarget_after_initial_picture(
            true,
            value.original,
            value.original,
            value.resolved,
            value.playback
        ));
        assert!(value.completion().is_err());
    }

    #[test]
    fn audio_handoff_may_return_only_to_the_proven_original_picture() {
        let mut value = fixture();
        let previous = value.resolved;
        let next = value.original;
        value.playback.position.frame = next.frame;
        assert!(can_retarget_after_initial_picture(
            true,
            value.original,
            previous,
            next,
            value.playback
        ));

        let mut invalid = next;
        invalid.frame -= 1;
        value.playback.position.frame = invalid.frame;
        assert!(!can_retarget_after_initial_picture(
            true,
            value.original,
            previous,
            invalid,
            value.playback
        ));
    }
}
