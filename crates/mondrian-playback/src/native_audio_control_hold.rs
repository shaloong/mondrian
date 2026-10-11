//! A retained native control wait is frozen transport, never recovery playback.

use super::*;

/// Exact Engine-owned identity of one pending native control wait.
///
/// This is clock authority only. Media/runtime still retain renderer, plan,
/// Session, occurrence, callback-quiescence and native-cohort identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAudioControlHoldTicket {
    epoch: PlaybackEpoch,
    pending_generation: u64,
    serial: u64,
}

impl NativeAudioControlHoldTicket {
    /// Playback Epoch in which the wait was admitted.
    pub const fn epoch(self) -> PlaybackEpoch {
        self.epoch
    }
    /// Media render generation blocked awaiting native reconfiguration.
    pub const fn pending_generation(self) -> u64 {
        self.pending_generation
    }
}

#[derive(Clone, Copy)]
pub(super) struct NativeAudioControlHold {
    pub(super) ticket: NativeAudioControlHoldTicket,
    pub(super) phase_ns: i128,
    began_at: MonotonicTimestamp,
    device: Option<AudioDeviceClockAnchor>,
    pub(super) finalized: bool,
}

impl PlaybackEngine {
    /// Freeze current play intent while a retained native owner is reconfigured.
    ///
    /// A supplied observation is provisional accepted-device evidence. The
    /// quiesced final observation is a separate exact-ticket operation. No timer,
    /// generic device loss, PCM completion or control completion releases this
    /// hold. A matching service completion can later end the finalized hold as paused;
    /// no method here grants automatic private-DSP continuation.
    pub fn hold_for_native_audio_control(
        &mut self,
        pending_generation: u64,
        observation: Option<AudioDeviceClockObservation>,
        now: MonotonicTimestamp,
    ) -> Result<NativeAudioControlHoldTicket, PlaybackError> {
        self.commit_candidate(move |candidate| {
            candidate.accept_timestamp(now)?;
            if pending_generation == 0 || !candidate.transport_intends_playback() {
                return Err(PlaybackError::InvalidNativeAudioControlHold);
            }
            if let Some(hold) = candidate.native_audio_control_hold {
                if hold.ticket.pending_generation == pending_generation {
                    return Ok(hold.ticket);
                }
                if pending_generation < hold.ticket.pending_generation {
                    return Err(PlaybackError::InvalidNativeAudioControlHold);
                }
                // A new Media generation supersedes the old wait without
                // releasing transport or reusing the old finalization ticket.
            }
            let ticket = NativeAudioControlHoldTicket {
                epoch: candidate.epoch,
                pending_generation,
                serial: candidate.next_native_audio_control_hold_serial,
            };
            candidate.next_native_audio_control_hold_serial =
                ticket.serial.checked_add(1).ok_or(PlaybackError::TransportArithmeticOverflow)?;
            let mut hold = NativeAudioControlHold {
                ticket,
                phase_ns: candidate.clock_phase_reference_ns(now)?,
                began_at: now,
                device: candidate
                    .native_audio_control_hold
                    .and_then(|previous| previous.device)
                    .or(candidate.audio_device_anchor),
                finalized: false,
            };
            if let Some(observation) = observation {
                // A snapshot captured before dispatch may predate `now`. It
                // must still belong to the accepted old epoch/stream, not a new
                // callback submitted after hold admission.
                hold.began_at =
                    candidate.last_audio_observation.map_or(now, |previous| previous.observed_at);
                if let Some((phase_ns, device)) =
                    candidate.native_hold_phase(hold, observation, now)?
                {
                    hold.phase_ns = phase_ns;
                    hold.device = Some(device);
                    candidate.last_audio_observation = Some(observation);
                }
                hold.began_at = now;
            }
            candidate.native_audio_control_hold = Some(hold);
            candidate.state = TransportState::HoldingForNativeAudioControl;
            candidate.clock_master = None;
            candidate.audio_device_anchor = None;
            candidate.position.frame =
                timeline_frame_at_ns(hold.phase_ns, candidate.position.time_base)?
                    .clamp(0, candidate.end_frame);
            candidate.reanchor_at_phase(now, hold.phase_ns);
            candidate.refresh_untimed_frame_demand()?;
            Ok(ticket)
        })
    }

    /// Current native hold identity; a pause/seek/stop or epoch change retires it.
    pub fn native_audio_control_hold_ticket(&self) -> Option<NativeAudioControlHoldTicket> {
        (self.state == TransportState::HoldingForNativeAudioControl)
            .then_some(self.native_audio_control_hold)
            .flatten()
            .map(|hold| hold.ticket)
    }

    /// Bind one coherent final callback observation after actual output quiescence.
    ///
    /// The caller owns the exact Media token/renderer proof. This method additionally
    /// checks the Engine ticket, epoch, old device anchor, counter and uncertainty.
    /// Rejected/stale evidence leaves the accepted frozen phase unchanged. Success
    /// updates the anchor once and keeps the hold; it does not resume playback.
    pub fn finalize_native_audio_control_hold(
        &mut self,
        ticket: NativeAudioControlHoldTicket,
        observation: Option<AudioDeviceClockObservation>,
        now: MonotonicTimestamp,
    ) -> Result<bool, PlaybackError> {
        self.commit_candidate(move |candidate| {
            candidate.accept_timestamp(now)?;
            let Some(mut hold) = candidate.native_audio_control_hold.filter(|hold| {
                candidate.state == TransportState::HoldingForNativeAudioControl
                    && hold.ticket == ticket
                    && ticket.epoch == candidate.epoch
                    && !hold.finalized
            }) else {
                return Ok(false);
            };
            if let Some(observation) = observation {
                let Some((phase_ns, device)) =
                    candidate.native_hold_phase(hold, observation, now)?
                else {
                    return Ok(false);
                };
                hold.phase_ns = phase_ns;
                hold.device = Some(device);
                candidate.last_audio_observation = Some(observation);
            }
            hold.finalized = true;
            candidate.native_audio_control_hold = Some(hold);
            candidate.position.frame =
                timeline_frame_at_ns(hold.phase_ns, candidate.position.time_base)?
                    .clamp(0, candidate.end_frame);
            candidate.reanchor_at_phase(now, hold.phase_ns);
            candidate.refresh_untimed_frame_demand()?;
            Ok(true)
        })
    }

    /// End an exact finalized native wait as paused, preserving its accepted phase.
    ///
    /// The product caller must already own the matching Media service result and
    /// its quiesced, validated configuration candidate. This operation is clock
    /// authority only: it does not configure a processor or admit PCM, and it
    /// grants no continuation of private DSP history. A stale or unfinalized
    /// ticket cannot leave the hold. Explicit later Play/seek starts a fresh
    /// generation through the ordinary preroll/device qualification path.
    pub fn finish_native_audio_control_hold_paused(
        &mut self,
        ticket: NativeAudioControlHoldTicket,
        now: MonotonicTimestamp,
    ) -> Result<bool, PlaybackError> {
        self.commit_candidate(move |candidate| {
            candidate.accept_timestamp(now)?;
            let Some(hold) = candidate.native_audio_control_hold.filter(|hold| {
                candidate.state == TransportState::HoldingForNativeAudioControl
                    && hold.ticket == ticket
                    && ticket.epoch == candidate.epoch
                    && hold.finalized
            }) else {
                return Ok(false);
            };
            candidate.state = TransportState::Paused;
            candidate.native_audio_control_hold = None;
            candidate.clock_master = None;
            candidate.audio_device_anchor = None;
            // Reanchoring from FramePosition would discard the accepted
            // subframe sample phase. Keep the finalized clock anchor itself.
            candidate.reanchor_at_phase(now, hold.phase_ns);
            candidate.ensure_pending_untimed_frame_demand()?;
            Ok(true)
        })
    }

    fn native_hold_phase(
        &self,
        hold: NativeAudioControlHold,
        observation: AudioDeviceClockObservation,
        now: MonotonicTimestamp,
    ) -> Result<Option<(i128, AudioDeviceClockAnchor)>, PlaybackError> {
        if !self.rate.supports_realtime_audio()
            || observation.epoch != hold.ticket.epoch
            || observation.state != AudioDeviceClockState::Running
            || observation.observed_at < hold.began_at
            || observation.observed_at > now
        {
            return Ok(None);
        }
        let rate = AudioSampleRate::new(observation.sample_rate)
            .map_err(|_| PlaybackError::InvalidAudioSampleRate)?;
        if observation.media_anchor.rate() != rate {
            return Ok(None);
        }
        let Some(mut effective) = observation
            .consumed_frames
            .checked_sub(u64::from(observation.estimated_latency_frames))
        else {
            return Ok(None);
        };
        if sample_frames_ns_ceil(u64::from(observation.uncertainty_frames), rate)?
            > self.policy.max_audio_clock_uncertainty.as_nanos()
        {
            return Ok(None);
        }
        let mut phase = audio_media_position_ns(observation, effective)?;
        let mut uncertainty_frames = observation.uncertainty_frames;
        if let Some(previous) = hold.device {
            if observation.stream_generation != previous.stream_generation
                || observation.media_anchor != previous.media_anchor
                || observation.consumed_frames < previous.last_consumed_frames
            {
                return Ok(None);
            }
            let adjacent_uncertainty_frames = u64::from(previous.last_uncertainty_frames)
                .checked_add(u64::from(observation.uncertainty_frames))
                .ok_or(PlaybackError::TransportArithmeticOverflow)?;
            let allowed = sample_frames_ns_ceil(adjacent_uncertainty_frames, rate)?;
            let observed_elapsed =
                observation.observed_at.checked_elapsed_since(previous.last_observed_at)?;
            let effective_delta = effective.saturating_sub(previous.last_effective_consumed_frames);
            let allowed_elapsed_ns = observed_elapsed
                .as_nanos()
                .checked_add(allowed)
                .ok_or(PlaybackError::TransportArithmeticOverflow)?;
            if sample_frames_exceed_duration_ns(effective_delta, rate, allowed_elapsed_ns)? {
                return Ok(None);
            }
            if phase < previous.last_media_position_ns {
                if previous.last_media_position_ns.abs_diff(phase) > allowed {
                    return Ok(None);
                }
                let correction_frames = previous
                    .last_media_position_ns
                    .abs_diff(phase)
                    .checked_mul(u128::from(observation.sample_rate))
                    .and_then(|value| value.checked_add(999_999_999))
                    .ok_or(PlaybackError::TransportArithmeticOverflow)?
                    / 1_000_000_000;
                uncertainty_frames = u128::from(uncertainty_frames)
                    .checked_add(correction_frames)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or(PlaybackError::TransportArithmeticOverflow)?;
                if sample_frames_ns_ceil(u64::from(uncertainty_frames), rate)?
                    > self.policy.max_audio_clock_uncertainty.as_nanos()
                {
                    return Ok(None);
                }
                phase = previous.last_media_position_ns;
                effective = effective.max(previous.last_effective_consumed_frames);
            }
        } else if phase.abs_diff(hold.phase_ns).saturating_add(sample_frames_ns_ceil(
            u64::from(observation.uncertainty_frames),
            rate,
        )?) > self.policy.max_audio_handoff_phase_error.as_nanos()
        {
            return Ok(None);
        }
        Ok(Some((
            phase,
            AudioDeviceClockAnchor {
                stream_generation: observation.stream_generation,
                media_anchor: observation.media_anchor,
                last_consumed_frames: observation.consumed_frames,
                last_effective_consumed_frames: effective,
                last_observed_at: observation.observed_at,
                last_media_position_ns: phase,
                last_uncertainty_frames: uncertainty_frames,
            },
        )))
    }
}

impl PlaybackEngine {
    /// Resume only the still-current finalized native hold through a fresh
    /// playback epoch and ordinary preroll. This preserves the exact accepted
    /// subframe phase, not private DSP continuity. Pause/seek/stop retire the
    /// ticket, so historical native completion cannot restart their transport.
    pub fn finish_native_audio_control_hold_reprime(
        &mut self,
        ticket: NativeAudioControlHoldTicket,
        binding: PlaybackTimelineBinding,
        now: MonotonicTimestamp,
    ) -> Result<bool, PlaybackError> {
        self.commit_candidate(move |candidate| {
            candidate.accept_timestamp(now)?;
            let Some(hold) = candidate.native_audio_control_hold.filter(|hold| {
                candidate.state == TransportState::HoldingForNativeAudioControl
                    && hold.ticket == ticket
                    && ticket.epoch == candidate.epoch
                    && hold.finalized
            }) else {
                return Ok(false);
            };
            if binding.sequence_id != candidate.sequence_id
                || binding.time_base != candidate.position.time_base
                || binding.end_frame != candidate.end_frame
            {
                return Err(PlaybackError::InvalidNativeAudioControlHold);
            }
            candidate.apply_timeline_binding(binding);
            candidate.bump_epoch()?;
            candidate.state = TransportState::Priming;
            candidate.clock_master = Some(ClockMaster::Synthetic);
            candidate.reset_runtime_policy()?;
            candidate.reanchor_at_phase(now, hold.phase_ns);
            candidate.refresh_frame_demand_with_duration(now, candidate.policy.priming_limit)?;
            Ok(true)
        })
    }
}
