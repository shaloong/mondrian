use super::product_action::TimelineSeekPayload;
use super::timeline_position::lower_nearest_sequence_frame;
use super::*;

const MAX_PLAYBACK_WAKE_DELAY: Duration = Duration::from_millis(100);
const AUDIO_CALLBACK_STALE_AFTER: Duration = Duration::from_millis(100);

struct PreparedTimelineAudioSource {
    renderer: Arc<dyn AudioPcmRenderer>,
    meter_observer: mondrian_audio::AudioMeterObserver,
    delivery_evidence: mondrian_audio::AudioDeliveryEvidence,
    authoring_session_id: AuthoringSessionId,
    sequence_id: SequenceId,
}

pub(super) enum AppAudioPlayback {
    Available(Box<AudioPlayback>),
    Unavailable { sample_rate: u32, reason: String },
}

impl AppAudioPlayback {
    pub(super) fn product_default(sample_rate: u32) -> Self {
        match AudioPlayback::product_default() {
            Ok(playback) => Self::Available(Box::new(playback)),
            Err(error) => {
                let reason = error.to_string();
                tracing::error!(%reason, "Audio Playback execution is unavailable; transport will remain Synthetic-mastered");
                Self::Unavailable { sample_rate, reason }
            }
        }
    }

    fn execution_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Available(_) => None,
            Self::Unavailable { reason, .. } => Some(reason),
        }
    }

    fn validate_anchor(&self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        match self {
            Self::Available(playback) => playback.validate_anchor(anchor),
            Self::Unavailable { sample_rate, .. } => {
                mondrian_media::validate_audio_playback_anchor(anchor, *sample_rate)
            }
        }
    }

    fn prepare(
        &mut self,
        anchor: AudioSamplePosition,
        renderer: Arc<dyn AudioPcmRenderer>,
    ) -> Result<(), AudioPlaybackError> {
        match self {
            Self::Available(playback) => playback.prepare(anchor, renderer),
            Self::Unavailable { sample_rate, .. } => {
                drop(renderer);
                mondrian_media::validate_audio_playback_anchor(anchor, *sample_rate)
            }
        }
    }

    fn clear_source(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        match self {
            Self::Available(playback) => playback.clear_source(anchor),
            Self::Unavailable { sample_rate, .. } => {
                mondrian_media::validate_audio_playback_anchor(anchor, *sample_rate)
            }
        }
    }

    fn reprime(&mut self, anchor: AudioSamplePosition) -> Result<(), AudioPlaybackError> {
        match self {
            Self::Available(playback) => playback.reprime(anchor),
            Self::Unavailable { sample_rate, .. } => {
                mondrian_media::validate_audio_playback_anchor(anchor, *sample_rate)
            }
        }
    }

    fn poll(
        &mut self,
        mode: AudioPlaybackMode,
        position: AudioSamplePosition,
    ) -> Result<AudioPlaybackPoll, AudioPlaybackError> {
        match self {
            Self::Available(playback) => playback.poll(mode, position),
            Self::Unavailable { sample_rate, .. } => {
                mondrian_media::validate_audio_playback_anchor(position, *sample_rate)?;
                Ok(AudioPlaybackPoll {
                    snapshot: AudioPlaybackSnapshot::execution_unavailable(),
                    events: Vec::new(),
                })
            }
        }
    }

    fn snapshot(&self, mode: AudioPlaybackMode) -> AudioPlaybackSnapshot {
        match self {
            Self::Available(playback) => playback.snapshot(mode),
            Self::Unavailable { .. } => AudioPlaybackSnapshot::execution_unavailable(),
        }
    }

    fn latest_output_device_evidence(
        &self,
    ) -> Option<mondrian_media::RealtimeAudioOutputDeviceEvidence> {
        match self {
            Self::Available(playback) => playback.latest_output_device_evidence().cloned(),
            Self::Unavailable { .. } => None,
        }
    }

    fn set_output_device_selection(
        &self,
        selection: mondrian_media::RealtimeAudioOutputDeviceSelection,
    ) -> bool {
        match self {
            Self::Available(playback) => playback.set_output_device_selection(selection),
            Self::Unavailable { .. } => false,
        }
    }

    #[cfg(all(feature = "validation", test))]
    fn request_controlled_output_recycle(
        &self,
        expected_stream_generation: u64,
    ) -> Result<(), mondrian_media::AudioPlaybackValidationError> {
        match self {
            Self::Available(playback) => {
                playback.request_controlled_output_recycle(expected_stream_generation)
            }
            Self::Unavailable { .. } => {
                Err(mondrian_media::AudioPlaybackValidationError::UnsupportedAdapter)
            }
        }
    }
}

impl AppState {
    /// Publish a latest-wins user/runtime audio-output selection.
    ///
    /// This preference is deliberately outside Project authoring. A changed
    /// selection rotates the concrete stream through the existing
    /// Audio→Synthetic→Audio generation handoff on subsequent playback polls.
    pub fn set_audio_output_device_selection(
        &self,
        selection: mondrian_media::RealtimeAudioOutputDeviceSelection,
    ) -> bool {
        self.audio_playback.set_output_device_selection(selection)
    }
}

/// Result category for one playback clock advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackAdvanceStatus {
    /// Playback is not currently running.
    Idle,
    /// Playback is running, but elapsed time has not crossed a frame boundary.
    WaitingForFrame,
    /// Playback advanced to another timeline frame.
    Advanced,
    /// Playback reached the final content frame and paused there.
    ReachedEnd,
}

/// Observable result of advancing the playback clock once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackAdvance {
    /// Frame before the clock tick.
    pub previous_frame: i64,
    /// Frame after the clock tick.
    pub current_frame: i64,
    /// Number of timeline frames crossed by this tick.
    pub frames_advanced: i64,
    /// High-level outcome.
    pub status: PlaybackAdvanceStatus,
}

/// Accepted result of one exact Frame Presentation Ticket completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePresentationCompletion {
    delivery: FrameDelivery,
    transport_changed: bool,
}

impl FramePresentationCompletion {
    /// Authoritative terminal delivery classified by the Playback ticket.
    pub const fn delivery(self) -> FrameDelivery {
        self.delivery
    }

    /// Whether accepting the delivery changed the public transport snapshot.
    pub const fn transport_changed(self) -> bool {
        self.transport_changed
    }
}

/// Result of atomically arbitrating output publication and one exact Frame
/// Presentation Ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FramePresentationDisposition {
    /// Output publication succeeded and consumed an exact presentable demand.
    Presented(FramePresentationCompletion),
    /// Output publication succeeded while no Playback demand existed.
    NoDemand,
    /// The exact demand was consumed as Late and the output was not published.
    DroppedLate(FramePresentationCompletion),
    /// The output adapter rejected publication; the demand remains pending.
    OutputRejected,
    /// The ticket was absent or stale while another demand retained authority.
    LostAuthority,
}

/// Prepared, single-consumption publication at the presentation commit seam.
///
/// `prepare` must finish every fallible or potentially blocking operation
/// before constructing this value. The carried commit is then a bounded,
/// infallible visibility change such as replacing the current Viewer output.
/// Keeping preparation outside [`AppState::finalize_frame_presentation`]
/// ensures that the authoritative timestamp is sampled immediately before the
/// Playback delivery and output become current together.
pub(crate) enum FramePresentationPublication<C> {
    /// A fully prepared output whose commit cannot reject publication.
    Prepared(C),
    /// Preparation rejected the output before anything became current.
    Rejected,
}

impl<C> FramePresentationPublication<C> {
    /// Bind one bounded, infallible visibility commit to a prepared output.
    pub(crate) const fn prepared(commit: C) -> Self {
        Self::Prepared(commit)
    }
}

impl FramePresentationPublication<fn()> {
    /// Reject publication before the presentation commit boundary.
    pub(crate) const fn rejected() -> Self {
        Self::Rejected
    }
}

/// Result of checking exact presentation authority before GPU work starts.
///
/// This seam never publishes output. It only permits useful work, consumes an
/// already-expired exact demand as `Late`, or reports that the captured ticket
/// no longer owns terminal authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FramePresentationPreflight {
    /// The ticket remains presentable, or no Playback demand exists.
    MaySubmit,
    /// The exact demand was already late and was consumed without GPU work.
    DroppedLate(FramePresentationCompletion),
    /// The ticket is absent/stale while another demand retains authority.
    LostAuthority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppliedFrameDelivery {
    accepted: bool,
    snapshot_changed: bool,
}

impl PlaybackAdvance {
    /// Return whether UI models should refresh for this playback tick.
    pub fn requires_refresh(self) -> bool {
        matches!(
            self.status,
            PlaybackAdvanceStatus::Advanced | PlaybackAdvanceStatus::ReachedEnd
        )
    }
}

impl AppState {
    fn playback_time_base(&self) -> Rational {
        self.active_sequence().map(Sequence::time_base).unwrap_or(Rational::new(1, 25))
    }

    fn authoritative_audio_anchor(
        &self,
        engine: &PlaybackEngine,
        observed_at: MonotonicTimestamp,
        action: &str,
    ) -> mondrian_core::Result<AudioSamplePosition> {
        let sample_rate = AudioSampleRate::new(self.audio_sample_rate)
            .map_err(|error| transport_action_error(action, error))?;
        engine
            .authoritative_audio_sample_position_at(
                engine.snapshot().epoch,
                observed_at,
                sample_rate,
            )
            .map_err(|error| transport_action_error(action, error))
    }

    fn capture_playback_evidence(&mut self) {
        let observed_at = self.playback_engine.monotonic_high_water();
        self.playback_evidence_now = observed_at;
        let snapshot = self.playback_engine.snapshot();
        let demand = self.playback_engine.frame_demand();
        if let Err(error) = self.playback_evidence.observe_snapshot(observed_at, snapshot, demand) {
            tracing::warn!(%error, "rejected Playback Evidence snapshot");
        }
    }

    fn synchronize_playback_observation_clock(&mut self, observed_at: Instant) {
        if self.is_playing() {
            let _ = self.advance_playback_clock_at(observed_at);
        }
        self.reanchor_playback_observation_projection(observed_at);
    }

    /// Return a stable bounded report for diagnostics and headless/perf Adapters.
    pub fn playback_evidence_report(&self) -> PlaybackEvidenceReport {
        self.playback_evidence.report()
    }

    /// Start one isolated headless/performance evidence run with explicit
    /// retention sized for its real execution workload.
    #[cfg(test)]
    pub(crate) fn begin_playback_evidence_run(
        &mut self,
        config: mondrian_playback::PlaybackEvidenceConfig,
    ) -> Result<(), mondrian_playback::PlaybackEvidenceError> {
        self.playback_evidence = PlaybackEvidenceCollector::new(config)?;
        self.playback_evidence_now = self.playback_engine.monotonic_high_water();
        self.capture_playback_evidence();
        Ok(())
    }

    pub fn play(&mut self) -> mondrian_core::Result<()> {
        let observed_at = Instant::now();
        self.synchronize_playback_observation_clock(observed_at);
        self.require_transport_sequence("play")?;
        let playback_now = self.playback_engine.monotonic_high_water();
        let end_frame = self.last_content_frame()?;
        let mut frames = self.current_frame();
        let prior_state = self.playback_engine.snapshot().state;
        if prior_state == TransportState::Stopped {
            frames = 0;
        }
        if prior_state == TransportState::Ended || (end_frame >= 0 && frames > end_frame) {
            frames = 0;
        }
        let time_base = self.playback_time_base();
        let binding = self
            .playback_timeline_binding(end_frame)
            .map_err(|error| transport_action_error("play", error))?;
        let timeline_anchor = FramePosition::new(frames, time_base);
        let renderer = self.audio_playback_renderer("play")?;
        let mut engine = self.playback_engine.clone();
        engine
            .play_timeline(binding, timeline_anchor, playback_now)
            .map_err(|error| transport_action_error("play", error))?;
        let audio_anchor = self.authoritative_audio_anchor(&engine, playback_now, "play")?;
        self.audio_playback
            .validate_anchor(audio_anchor)
            .map_err(|error| transport_action_error("play", error))?;
        self.commit_audio_playback("play", audio_anchor, renderer)?;
        self.playback_engine = engine;
        self.audio_idle_warmup.set_automatic_policy_enabled(false);
        self.audio_idle_warmup.set_dispatch_enabled(false);
        self.reanchor_playback_observation_projection(observed_at);
        self.capture_playback_evidence();
        self.refresh_internal_execution_resource_decision();
        Ok(())
    }

    pub fn pause(&mut self) -> mondrian_core::Result<()> {
        let observed_at = Instant::now();
        self.synchronize_playback_observation_clock(observed_at);
        self.require_transport_sequence("pause")?;
        let playback_now = self.playback_engine.monotonic_high_water();
        let mut engine = self.playback_engine.clone();
        engine
            .pause(playback_now)
            .map_err(|error| transport_action_error("pause", error))?;
        let audio_anchor = self.authoritative_audio_anchor(&engine, playback_now, "pause")?;
        self.audio_playback
            .reprime(audio_anchor)
            .map_err(|error| transport_action_error("pause", error))?;
        self.playback_engine = engine;
        self.audio_idle_warmup.set_automatic_policy_enabled(false);
        self.audio_idle_warmup.set_dispatch_enabled(false);
        self.settle_preview_access_source();
        self.reanchor_playback_observation_projection(observed_at);
        self.capture_playback_evidence();
        self.refresh_internal_execution_resource_decision();
        Ok(())
    }

    pub fn stop(&mut self) -> mondrian_core::Result<()> {
        let observed_at = Instant::now();
        self.synchronize_playback_observation_clock(observed_at);
        self.require_transport_sequence("stop")?;
        let playback_now = self.playback_engine.monotonic_high_water();
        let mut engine = self.playback_engine.clone();
        engine
            .stop(playback_now)
            .map_err(|error| transport_action_error("stop", error))?;
        let audio_anchor = self.authoritative_audio_anchor(&engine, playback_now, "stop")?;
        self.audio_playback
            .reprime(audio_anchor)
            .map_err(|error| transport_action_error("stop", error))?;
        self.playback_engine = engine;
        self.audio_idle_warmup.set_automatic_policy_enabled(false);
        self.audio_idle_warmup.set_dispatch_enabled(false);
        self.settle_preview_access_source();
        self.reanchor_playback_observation_projection(observed_at);
        self.capture_playback_evidence();
        self.refresh_internal_execution_resource_decision();
        Ok(())
    }

    pub fn seek(&mut self, frame: i64) -> mondrian_core::Result<()> {
        self.seek_with_source(frame, TimelineSeekSource::Settled)
    }

    /// Whether one typed Timeline seek has a valid active Sequence coordinate.
    pub fn can_seek_from_product_action(&self, payload: TimelineSeekPayload) -> bool {
        self.active_sequence().is_some_and(|sequence| {
            lower_nearest_sequence_frame(sequence, payload.position, "timeline_seek").is_ok()
        })
    }

    /// Lower one externally gridded Timeline seek exactly once, then enter the
    /// Playback-owned transport Interface.
    pub fn seek_from_product_action(
        &mut self,
        payload: TimelineSeekPayload,
    ) -> mondrian_core::Result<()> {
        let frame = {
            let sequence = self
                .active_sequence()
                .ok_or_else(|| transport_action_error("seek", "there is no active Sequence"))?;
            lower_nearest_sequence_frame(sequence, payload.position, "timeline_seek")?
        };
        self.seek_with_source(frame, payload.source)
    }

    pub(crate) fn settle_preview_access_source(&mut self) {
        self.last_timeline_seek_source = TimelineSeekSource::Settled;
    }

    pub fn seek_with_source(
        &mut self,
        frame: i64,
        source: TimelineSeekSource,
    ) -> mondrian_core::Result<()> {
        let observed_at = Instant::now();
        self.synchronize_playback_observation_clock(observed_at);
        self.require_transport_sequence("seek")?;
        let playback_now = self.playback_engine.monotonic_high_water();
        if frame < 0 {
            return Err(transport_action_error(
                "seek",
                "timeline frame must be non-negative",
            ));
        }
        let was_running = self.is_playing();
        let end_frame = self.last_content_frame()?;
        let time_base = self.playback_time_base();
        let binding = self
            .playback_timeline_binding(end_frame)
            .map_err(|error| transport_action_error("seek", error))?;
        let timeline_anchor = FramePosition::new(frame, time_base);
        let renderer =
            was_running.then(|| self.audio_playback_renderer("seek")).transpose()?.flatten();
        let mut engine = self.playback_engine.clone();
        engine
            .seek_timeline(binding, timeline_anchor, playback_now)
            .map_err(|error| transport_action_error("seek", error))?;
        let audio_anchor = self.authoritative_audio_anchor(&engine, playback_now, "seek")?;
        self.audio_playback
            .validate_anchor(audio_anchor)
            .map_err(|error| transport_action_error("seek", error))?;
        if was_running {
            self.commit_audio_playback("seek", audio_anchor, renderer)?;
        } else {
            self.audio_playback
                .reprime(audio_anchor)
                .map_err(|error| transport_action_error("seek", error))?;
        }
        self.playback_engine = engine;
        self.audio_idle_warmup.set_automatic_policy_enabled(false);
        self.audio_idle_warmup.set_dispatch_enabled(false);
        self.last_timeline_seek_source = source;
        self.reanchor_playback_observation_projection(observed_at);
        let seek_kind = match source {
            TimelineSeekSource::PointerDrag => PlaybackSeekKind::Warm,
            TimelineSeekSource::Settled => PlaybackSeekKind::Accurate,
        };
        if let Err(error) = self.playback_evidence.begin_seek(
            self.playback_engine.monotonic_high_water(),
            self.playback_engine.snapshot().epoch,
            seek_kind,
        ) {
            tracing::warn!(%error, "rejected Playback Evidence seek start");
        }
        self.capture_playback_evidence();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_playback_frame_running(&mut self, frame: i64) {
        let frame = frame.max(0);
        let end_frame = self.last_content_frame().map_or(frame, |end| end.max(frame));
        let time_base = self.playback_time_base();
        let binding = match self.playback_timeline_binding(end_frame) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::error!(%error, "failed to bind simulated Playback Session timeline");
                return;
            }
        };
        if let Err(error) = self.playback_engine.play_timeline(
            binding,
            FramePosition::new(frame, time_base),
            self.playback_engine.monotonic_high_water(),
        ) {
            tracing::error!(%error, "failed to start simulated Playback Session");
        }
    }

    pub fn pump_audio_output(&mut self) -> mondrian_core::Result<()> {
        self.poll_audio_idle_warmup();
        let mode = self.audio_playback_mode();
        let poll_started_at = Instant::now();
        let poll_timestamp = self
            .playback_timestamp_for_observation(poll_started_at)
            .map(|timestamp| timestamp.max(self.playback_engine.monotonic_high_water()))
            .map_err(|error| transport_action_error("pump_audio_output", error))?;
        let audio_anchor = self.authoritative_audio_anchor(
            &self.playback_engine,
            poll_timestamp,
            "pump_audio_output",
        )?;
        let poll = self
            .audio_playback
            .poll(mode, audio_anchor)
            .map_err(|error| transport_action_error("pump_audio_output", error))?;
        for event in poll.events {
            self.handle_audio_playback_event(event, Instant::now())?;
        }
        if mode == AudioPlaybackMode::Idle {
            self.request_audio_idle_warmup();
            return Ok(());
        }
        self.audio_idle_warmup.set_automatic_policy_enabled(false);
        self.audio_idle_warmup.set_dispatch_enabled(false);
        self.observe_audio_output_clock(poll.snapshot, Instant::now())?;
        Ok(())
    }

    fn handle_audio_playback_event(
        &mut self,
        event: AudioPlaybackEvent,
        handled_at: Instant,
    ) -> mondrian_core::Result<()> {
        match event {
            AudioPlaybackEvent::DeviceOpened { stream_generation, evidence } => {
                tracing::info!(
                    stream_generation,
                    host = %evidence.host_name,
                    device = ?evidence.device_name,
                    sample_rate = evidence.contract.sample_rate,
                    channels = evidence.contract.channels(),
                    sample_format = ?evidence.contract.sample_format,
                    channel_semantics = ?evidence.contract.channel_semantics,
                    "audio output stream opened; starting preroll"
                );
                Ok(())
            }
            AudioPlaybackEvent::DeviceLost { reason, final_output, final_media_anchor } => {
                tracing::warn!(
                    stream_generation = final_output.stream_generation,
                    ?reason,
                    "audio output stream lost; using Synthetic Clock Master"
                );
                if let Some(final_media_anchor) = final_media_anchor {
                    self.observe_final_audio_clock_before_recovery(
                        final_output,
                        final_media_anchor,
                        handled_at,
                    )
                } else {
                    self.handoff_audio_device_to_synthetic("audio_device_lost", handled_at)
                }
            }
            AudioPlaybackEvent::DeviceOpenFailed { retry_after, failure } => {
                tracing::debug!(
                    ?retry_after,
                    code = ?failure.code,
                    requested_sample_rate = failure.requested_sample_rate,
                    requested_layout = %failure.requested_layout,
                    candidates = ?failure.candidates,
                    detail = %failure.detail,
                    "audio output open failed; retry scheduled"
                );
                Ok(())
            }
            AudioPlaybackEvent::DeviceWorkerStartFailed { reason } => {
                self.handoff_audio_device_to_synthetic(
                    "audio_device_worker_start_failed",
                    handled_at,
                )?;
                Err(transport_action_error(
                    "pump_audio_output",
                    format!("audio device worker failed to start: {reason}"),
                ))
            }
            AudioPlaybackEvent::DeviceWorkerStoppedUnexpectedly { reason } => {
                self.handoff_audio_device_to_synthetic("audio_device_worker_stopped", handled_at)?;
                Err(transport_action_error(
                    "pump_audio_output",
                    format!("audio device worker stopped unexpectedly: {reason}"),
                ))
            }
            AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly { reason } => {
                let unavailable_reason =
                    format!("audio render worker stopped unexpectedly: {reason}");
                let unavailable = AppAudioPlayback::Unavailable {
                    sample_rate: self.audio_sample_rate,
                    reason: unavailable_reason.clone(),
                };
                drop(std::mem::replace(&mut self.audio_playback, unavailable));
                self.handoff_audio_device_to_synthetic("audio_render_worker_stopped", handled_at)?;
                Err(transport_action_error(
                    "pump_audio_output",
                    unavailable_reason,
                ))
            }
            AudioPlaybackEvent::RenderSubstitutedWithSilence {
                generation,
                start_sample,
                reason,
            } => {
                tracing::warn!(
                    generation,
                    start_sample,
                    %reason,
                    "audio render window replaced with exact-duration silence"
                );
                Ok(())
            }
            AudioPlaybackEvent::RenderGenerationInvalidated {
                failed_generation,
                restart_generation,
                failed_start_sample,
                restart_anchor,
                final_output,
                final_media_anchor,
                reason,
                disposition,
            } => {
                if let (Some(final_output), Some(final_media_anchor)) =
                    (final_output, final_media_anchor)
                {
                    self.observe_final_audio_clock_before_recovery(
                        final_output,
                        final_media_anchor,
                        handled_at,
                    )?;
                } else {
                    self.handoff_audio_device_to_synthetic(
                        "audio_render_generation_invalidated",
                        handled_at,
                    )?;
                }
                tracing::warn!(
                    failed_generation,
                    restart_generation,
                    failed_start_sample,
                    restart_sample = restart_anchor.sample(),
                    ?disposition,
                    %reason,
                    "stateful audio render generation invalidated; using Synthetic Clock Master"
                );
                Ok(())
            }
            AudioPlaybackEvent::UnderrunObserved {
                stream_generation,
                delta_frames,
                interval_total_frames,
            } => {
                if let Err(error) = self.playback_evidence.observe_audio_underrun(
                    self.playback_engine.monotonic_high_water(),
                    self.playback_engine.snapshot().epoch,
                    delta_frames,
                    false,
                ) {
                    tracing::warn!(%error, "rejected Playback Evidence underrun observation");
                }
                tracing::debug!(
                    stream_generation,
                    delta_frames,
                    interval_total_frames,
                    "audio output underrun observed"
                );
                Ok(())
            }
            AudioPlaybackEvent::UnderrunRecoveryStarted {
                stream_generation,
                missing_frames,
                threshold_frames,
                final_output,
                final_media_anchor,
            } => {
                if let Err(error) = self.playback_evidence.observe_audio_underrun(
                    self.playback_engine.monotonic_high_water(),
                    self.playback_engine.snapshot().epoch,
                    0,
                    true,
                ) {
                    tracing::warn!(%error, "rejected Playback Evidence underrun recovery");
                }
                self.observe_final_audio_clock_before_recovery(
                    final_output,
                    final_media_anchor,
                    handled_at,
                )?;
                tracing::warn!(
                    stream_generation,
                    missing_frames,
                    threshold_frames,
                    "sustained audio underrun; using Synthetic Clock Master during reprime"
                );
                Ok(())
            }
        }
    }

    fn handoff_audio_device_to_synthetic(
        &mut self,
        action: &str,
        observed_at: Instant,
    ) -> mondrian_core::Result<()> {
        let observed_timestamp = self
            .playback_timestamp_for_observation(observed_at)
            .map(|timestamp| timestamp.max(self.playback_engine.monotonic_high_water()))
            .map_err(|error| transport_action_error(action, error))?;
        let mut engine = self.playback_engine.clone();
        engine
            .audio_device_lost(observed_timestamp)
            .map_err(|error| transport_action_error(action, error))?;
        self.playback_engine = engine;
        self.reanchor_playback_observation_projection_at(
            observed_at,
            self.playback_engine.monotonic_high_water(),
        );
        self.capture_playback_evidence();
        Ok(())
    }

    fn observe_final_audio_clock_before_recovery(
        &mut self,
        output: RealtimeAudioOutputSnapshot,
        media_anchor: AudioSamplePosition,
        handled_at: Instant,
    ) -> mondrian_core::Result<()> {
        let loss_timestamp = self
            .playback_timestamp_for_observation(handled_at)
            .map(|timestamp| timestamp.max(self.playback_engine.monotonic_high_water()))
            .map_err(|error| transport_action_error("audio_recovery_handoff", error))?;
        let already_audio_master =
            self.playback_engine.snapshot().clock_master == Some(ClockMaster::AudioDevice);
        let final_observation = self
            .playback_timestamp_for_observation(output.captured_at)
            .and_then(|captured_at| {
                audio_device_clock_observation(
                    output,
                    self.playback_engine.snapshot().epoch,
                    captured_at,
                    already_audio_master,
                    media_anchor,
                    false,
                    true,
                )
            })
            .map(Some)
            .unwrap_or_else(|error| {
                tracing::warn!(
                    %error,
                    stream_generation = output.stream_generation,
                    "ignored invalid frozen audio-clock evidence before mandatory Synthetic handoff"
                );
                None
            });
        let mut engine = self.playback_engine.clone();
        let application = engine
            .audio_device_lost_with_final_observation(
                output.stream_generation,
                final_observation,
                loss_timestamp,
            )
            .map_err(|error| transport_action_error("audio_recovery_handoff", error))?;
        if final_observation.is_some() && !application.final_observation_applied() {
            tracing::warn!(
                stream_generation = output.stream_generation,
                "frozen audio-clock evidence did not match current authoritative stream generation"
            );
        }
        self.playback_engine = engine;
        self.reanchor_playback_observation_projection_at(
            handled_at,
            self.playback_engine.monotonic_high_water(),
        );
        self.capture_playback_evidence();
        Ok(())
    }

    fn observe_audio_output_clock(
        &mut self,
        audio: AudioPlaybackSnapshot,
        handled_at: Instant,
    ) -> mondrian_core::Result<()> {
        let Some(mut snapshot) = audio.output else {
            if self.playback_engine.snapshot().clock_master == Some(ClockMaster::AudioDevice) {
                self.handoff_audio_device_to_synthetic("audio_output_unavailable", handled_at)?;
            }
            return Ok(());
        };
        if !self.is_playing() {
            return Ok(());
        }
        let Some(media_anchor) = audio.media_anchor else {
            if self.playback_engine.snapshot().clock_master == Some(ClockMaster::AudioDevice) {
                self.handoff_audio_device_to_synthetic(
                    "audio_media_anchor_unavailable",
                    handled_at,
                )?;
            }
            return Ok(());
        };
        let captured_timestamp = self
            .playback_timestamp_for_observation(snapshot.captured_at)
            .map_err(|error| transport_action_error("observe_audio_output_clock", error))?;
        let engine_high_water = self.playback_engine.monotonic_high_water();
        let captured_after_high_water = captured_timestamp >= engine_high_water;
        let observed_timestamp = captured_timestamp.max(engine_high_water);
        snapshot = rebase_audio_output_snapshot(snapshot, captured_timestamp, observed_timestamp)
            .map_err(|error| transport_action_error("observe_audio_output_clock", error))?;
        let already_audio_master =
            self.playback_engine.snapshot().clock_master == Some(ClockMaster::AudioDevice);
        let observation = audio_device_clock_observation(
            snapshot,
            self.playback_engine.snapshot().epoch,
            observed_timestamp,
            already_audio_master,
            media_anchor,
            audio.activation_preroll_satisfied,
            false,
        )
        .map_err(|error| transport_action_error("observe_audio_output_clock", error))?;
        let mut engine = self.playback_engine.clone();
        let engine_snapshot = engine
            .observe_audio_device_clock(observation)
            .map_err(|error| transport_action_error("observe_audio_output_clock", error))?;
        let phase_rejected = observation.state == AudioDeviceClockState::Running
            && !already_audio_master
            && engine_snapshot.clock_master != Some(ClockMaster::AudioDevice)
            && engine_snapshot.audio_handoff.is_some_and(|handoff| {
                handoff.stream_generation == observation.stream_generation
                    && handoff.status == mondrian_playback::AudioClockHandoffStatus::PhaseRejected
            });
        if phase_rejected {
            let restart_anchor = self.authoritative_audio_anchor(
                &engine,
                engine.monotonic_high_water(),
                "audio_phase_rejection",
            )?;
            self.audio_playback
                .validate_anchor(restart_anchor)
                .map_err(|error| transport_action_error("audio_phase_rejection", error))?;
            self.audio_playback
                .reprime(restart_anchor)
                .map_err(|error| transport_action_error("audio_phase_rejection", error))?;
        }
        self.playback_engine = engine;
        if captured_after_high_water {
            self.reanchor_playback_observation_projection_at(
                snapshot.captured_at,
                self.playback_engine.monotonic_high_water(),
            );
        }
        self.capture_playback_evidence();
        Ok(())
    }

    fn require_transport_sequence(&self, action: &str) -> mondrian_core::Result<()> {
        self.active_sequence()
            .map(|_| ())
            .ok_or_else(|| transport_action_error(action, "there is no active Sequence to bind"))
    }

    fn audio_playback_renderer(
        &self,
        action: &str,
    ) -> mondrian_core::Result<Option<PreparedTimelineAudioSource>> {
        let sequence = self.active_sequence().ok_or_else(|| {
            transport_action_error(action, "there is no active Sequence to compile")
        })?;
        if !self.audio_playback.execution_available() {
            return Ok(None);
        }
        let Some(library) = self.asset_library_handle() else {
            // Direct headless/UI model fixtures may carry a Sequence without a
            // Project Session. Such a fixture is silent only when it has no
            // authored audio placement; real Projects always own a library.
            if sequence.audio_tracks.iter().all(|track| track.clips.is_empty()) {
                return Ok(None);
            }
            return Err(transport_action_error(
                action,
                "the active Sequence has audio placements but no Asset Library authority",
            ));
        };
        let authoring_session_id = self.authoring_session_id().ok_or_else(|| {
            transport_action_error(action, "there is no Authoring Session to bind")
        })?;
        let runtime_grant = self.execution_resources.decision().audio.runtime_grant;
        let audition = self.audio_monitoring.audition_overlay(authoring_session_id, sequence);
        let renderer = TimelineAudioPcmRenderer::new(
            sequence.clone(),
            self.sequences().to_vec(),
            library,
            Arc::clone(&self.audio_source_cache),
            runtime_grant,
            audition,
            self.audio_sample_rate,
            AUDIO_OUTPUT_LAYOUT,
        )
        .map_err(|error| transport_action_error(action, error))?;
        let requires_execution = renderer.execution_demand().requires_execution();
        let meter_observer = renderer.meter_observer();
        let delivery_evidence = renderer.delivery_evidence();
        Ok(requires_execution.then(|| PreparedTimelineAudioSource {
            renderer: Arc::new(renderer),
            meter_observer,
            delivery_evidence,
            authoring_session_id,
            sequence_id: sequence.id,
        }))
    }

    fn commit_audio_playback(
        &mut self,
        action: &str,
        anchor: AudioSamplePosition,
        source: Option<PreparedTimelineAudioSource>,
    ) -> mondrian_core::Result<()> {
        if let Some(source) = source {
            self.audio_playback
                .prepare(anchor, source.renderer)
                .map_err(|error| transport_action_error(action, error))?;
            self.audio_monitoring.bind_meter(
                source.authoring_session_id,
                source.sequence_id,
                source.meter_observer,
                source.delivery_evidence,
            );
        } else {
            self.audio_playback
                .clear_source(anchor)
                .map_err(|error| transport_action_error(action, error))?;
            self.audio_monitoring.clear_meter();
        }
        Ok(())
    }

    fn prepare_audio_playback(&mut self, anchor: AudioSamplePosition) -> mondrian_core::Result<()> {
        let renderer = self.audio_playback_renderer("prepare_audio_playback")?;
        self.commit_audio_playback("prepare_audio_playback", anchor, renderer)
    }

    /// Invalidate prepared audio after Timeline audio authoring or Asset source
    /// binding changes without changing transport authority.
    pub(super) fn refresh_audio_playback_after_program_change(
        &mut self,
    ) -> mondrian_core::Result<()> {
        let anchor = self.authoritative_audio_anchor(
            &self.playback_engine,
            self.playback_engine.monotonic_high_water(),
            "refresh_audio_playback",
        )?;
        if self.is_playing() {
            self.prepare_audio_playback(anchor)
        } else {
            self.commit_audio_playback("refresh_audio_playback", anchor, None)
        }
    }

    /// Reconcile runtime audio after an author transaction that is already
    /// committed and therefore cannot be rolled back by an Adapter failure.
    /// Failure clears the old source so stale PCM cannot masquerade as the new
    /// Sequence, then records both the primary and cleanup outcome explicitly.
    pub(super) fn reconcile_audio_after_committed_authoring_change(&mut self, context: &str) {
        if let Err(error) = self.refresh_audio_playback_after_program_change() {
            tracing::error!(%error, context, "failed to prepare audio after committed authoring change");
            let cleanup = self
                .authoritative_audio_anchor(
                    &self.playback_engine,
                    self.playback_engine.monotonic_high_water(),
                    "clear_stale_audio",
                )
                .and_then(|anchor| {
                    self.audio_playback
                        .clear_source(anchor)
                        .map_err(|cleanup_error| {
                            transport_action_error("clear_stale_audio", cleanup_error)
                        })
                        .map(|()| self.audio_monitoring.clear_meter())
                });
            if let Err(cleanup_error) = cleanup {
                tracing::error!(
                    %cleanup_error,
                    context,
                    "failed to clear stale audio after committed authoring change"
                );
            }
        }
    }

    /// Apply a post-commit playhead convenience move without misreporting the
    /// already-completed Author Transaction as failed. Transport rejection is
    /// retained as explicit diagnostics and leaves the committed author state
    /// untouched.
    pub(super) fn reconcile_playhead_after_committed_authoring_change(
        &mut self,
        frame: i64,
        context: &str,
    ) {
        if let Err(error) = self.seek(frame) {
            tracing::error!(%error, context, "failed to move playhead after committed authoring change");
        }
    }

    fn request_audio_idle_warmup(&mut self) {
        let resource_decision = self.execution_resources.decision();
        let audio_decision = resource_decision.audio;
        let warmup_windows = audio_decision.idle_warmup_windows;
        let automatic_policy_enabled = audio_idle_warmup_enabled() && warmup_windows > 0;
        let dispatch_enabled = audio_decision.idle_warmup_dispatch_enabled;
        self.audio_idle_warmup.set_automatic_policy_enabled(automatic_policy_enabled);
        self.audio_idle_warmup.set_dispatch_enabled(dispatch_enabled);
        self.synchronize_audio_idle_warmup_binding();
        if !automatic_policy_enabled {
            return;
        }

        let Some(seq) = self.active_sequence() else {
            return;
        };
        let Some(library) = self.asset_library_handle() else {
            self.audio_idle_warmup.bind_authoring(None);
            return;
        };
        let Ok(asset_library_revision) = library.database_revision() else {
            self.audio_idle_warmup.bind_authoring(None);
            return;
        };
        let Some(project_id) = self.project_id() else {
            return;
        };
        let Some(authoring_session_id) = self.authoring_session_id() else {
            return;
        };
        let binding = AudioIdleWarmupAuthorBinding {
            project_id,
            authoring_session_id,
            author_generation: self.project_author_generation(),
            asset_library_revision,
        };
        self.audio_idle_warmup.bind_authoring(Some(binding));

        let Ok(rate) = AudioSampleRate::new(self.audio_sample_rate) else {
            return;
        };
        let Ok(center_time) = TimelineTime::from_frame_position(FramePosition::new(
            self.current_frame().max(0),
            seq.time_base(),
        )) else {
            return;
        };
        let Ok(center) = AudioSamplePosition::from_timeline_time(
            center_time,
            rate,
            AudioSampleRounding::Nearest,
        ) else {
            return;
        };
        let chunk_frames = u64::from(self.audio_sample_rate)
            .saturating_mul(u64::from(AUDIO_IDLE_WARMUP_CHUNK_MILLIS))
            .saturating_add(500)
            / 1_000;
        let chunk_frames = (chunk_frames as usize).max(1);
        let key = AudioIdleWarmupDemand::key(
            binding,
            seq,
            center.sample(),
            warmup_windows,
            chunk_frames,
            self.audio_sample_rate,
            AUDIO_OUTPUT_LAYOUT,
            audio_decision.runtime_grant,
        );
        if !self.audio_idle_warmup.wants_demand(key) {
            return;
        }
        let demand = AudioIdleWarmupDemand::from_key(
            key,
            seq.clone(),
            self.sequences().to_vec(),
            library,
            Arc::clone(&self.audio_source_cache),
        );
        if self.audio_idle_warmup.submit(demand) == AudioIdleWarmupSubmitOutcome::Queued {
            // Publishing queued demand lets the fair cross-domain allocator
            // grant this worker without compiling/rendering on the UI thread.
            let _ = self.refresh_internal_execution_resource_decision();
        }
    }

    pub(super) fn synchronize_audio_idle_warmup_binding(&self) {
        let binding = self
            .project_id()
            .zip(self.authoring_session_id())
            .zip(self.asset_library_handle())
            .and_then(|((project_id, authoring_session_id), library)| {
                let asset_library_revision = library.database_revision().ok()?;
                Some(AudioIdleWarmupAuthorBinding {
                    project_id,
                    authoring_session_id,
                    author_generation: self.project_author_generation(),
                    asset_library_revision,
                })
            });
        self.audio_idle_warmup.bind_authoring(binding);
    }

    /// Poll bounded terminal evidence from the idle-audio preparation worker.
    pub fn poll_audio_idle_warmup(&mut self) -> bool {
        let delta = self
            .audio_idle_warmup
            .terminal_delta_after(self.audio_idle_warmup_terminal_cursor);
        if delta.records.is_empty() && !delta.retention_gap {
            return false;
        }
        if delta.retention_gap {
            tracing::warn!(
                observed_terminal_sequence = self.audio_idle_warmup_terminal_cursor,
                next_terminal_sequence = delta.next_cursor,
                "Audio idle-warmup terminal consumer crossed bounded retention"
            );
        }
        for terminal in &delta.records {
            if terminal.evidence.disposition == mondrian_core::ExecutionTerminalDisposition::Failed
            {
                tracing::debug!(
                    request_id = terminal.identity.request_id,
                    author_generation = terminal.identity.author_generation,
                    sequence_id = %terminal.identity.sequence_id,
                    detail = terminal.failure_detail.as_deref().unwrap_or("unknown failure"),
                    "speculative Audio idle warmup failed"
                );
            }
        }
        self.audio_idle_warmup_terminal_cursor = delta.next_cursor;
        true
    }

    /// Return bounded diagnostics for speculative paused-audio preparation.
    pub fn audio_idle_warmup_diagnostics(&self) -> AudioIdleWarmupDiagnostics {
        self.audio_idle_warmup.diagnostics()
    }

    pub fn audio_developer_metrics_summary(&self) -> String {
        let snapshot = self.audio_playback.snapshot(self.audio_playback_mode());
        let buffered_frames = snapshot.output.map_or(0, |output| output.buffered_frames);
        let buffered_ms = buffered_frames as f64 / self.audio_sample_rate as f64 * 1000.0;
        let source_cache = self.audio_source_cache.diagnostics();
        let idle_warmup = self.audio_idle_warmup.diagnostics();
        format!(
            "Aud out:{:.0}ms inflight:{} srcCache:{}/{}MiB win:{}/{} sess:{}/{} trim:{}+{} fail:{} idle:{}/{} done:{} cancel:{} fail:{}",
            buffered_ms,
            snapshot.in_flight,
            source_cache.reserved_bytes / (1024 * 1024),
            source_cache.byte_budget / (1024 * 1024),
            source_cache.entries,
            source_cache.entry_capacity,
            source_cache.decoder_sessions,
            source_cache.decoder_session_capacity,
            source_cache.budget_trimmed_entries,
            source_cache.decoder_capacity_trim_evictions,
            source_cache.decode_failures,
            idle_warmup.queued,
            usize::from(idle_warmup.running.is_some()),
            idle_warmup.completions,
            idle_warmup.cancellations,
            idle_warmup.failures,
        )
    }

    /// Return bounded decoded-audio source residency and execution evidence.
    pub fn audio_source_cache_diagnostics(&self) -> AudioSourceCacheDiagnostics {
        self.audio_source_cache.diagnostics()
    }

    /// Return the current production Audio Playback lifecycle and CPAL callback evidence.
    pub fn audio_playback_snapshot(&self) -> AudioPlaybackSnapshot {
        self.audio_playback.snapshot(self.audio_playback_mode())
    }

    /// Latest successful CPAL host/device/configuration negotiation evidence.
    ///
    /// This remains available after device loss so diagnostics can explain
    /// the exact physical contract preceding Synthetic Clock handoff.
    pub fn latest_audio_output_device_evidence(
        &self,
    ) -> Option<mondrian_media::RealtimeAudioOutputDeviceEvidence> {
        self.audio_playback.latest_output_device_evidence()
    }

    /// Request a real destroy-and-reopen cycle for the exact current CPAL
    /// generation. This exists only in validation builds and uses the normal
    /// device worker, callback quiescence, stream destruction, and reopen path.
    #[cfg(all(feature = "validation", test))]
    pub(crate) fn request_controlled_audio_output_recycle(
        &self,
        expected_stream_generation: u64,
    ) -> Result<(), mondrian_media::AudioPlaybackValidationError> {
        self.audio_playback
            .request_controlled_output_recycle(expected_stream_generation)
    }

    /// Explain why realtime PCM execution is unavailable, when App startup
    /// degraded transport to Synthetic Clock Master.
    pub fn audio_playback_unavailable_reason(&self) -> Option<&str> {
        self.audio_playback.unavailable_reason()
    }

    /// Advance playback by a deterministic interval from the current
    /// observation projection.
    ///
    /// Production Adapters should pass their one absolute process-monotonic
    /// observation to [`Self::advance_playback_clock_at`]. This relative seam
    /// is retained for deterministic tests and fixed-step validation only.
    pub fn advance_playback_clock(&mut self, elapsed: Duration) -> PlaybackAdvance {
        let Some(observed_at) = self.playback_observation_instant_anchor.checked_add(elapsed)
        else {
            let current_frame = self.current_frame().max(0);
            tracing::error!("rejected overflowing deterministic Playback clock advance");
            return PlaybackAdvance {
                previous_frame: current_frame,
                current_frame,
                frames_advanced: 0,
                status: PlaybackAdvanceStatus::WaitingForFrame,
            };
        };
        self.advance_playback_clock_at(observed_at)
    }

    /// Advance playback at one absolute process-monotonic observation.
    ///
    /// The App projects this instant once from its current Engine-time anchor.
    /// Viewer, audio, or preroll observations may move the Engine high-water
    /// and reanchor that projection between ticks; no caller-owned elapsed
    /// accumulator is therefore allowed to overlap-count the same interval.
    pub(crate) fn advance_playback_clock_at(&mut self, observed_at: Instant) -> PlaybackAdvance {
        let previous_frame = self.current_frame().max(0);
        if !self.is_playing() {
            return PlaybackAdvance {
                previous_frame,
                current_frame: previous_frame,
                frames_advanced: 0,
                status: PlaybackAdvanceStatus::Idle,
            };
        }
        let observed_timestamp = match self.playback_timestamp_for_observation(observed_at) {
            Ok(timestamp) => timestamp.max(self.playback_engine.monotonic_high_water()),
            Err(error) => {
                tracing::error!(%error, "rejected invalid Playback clock observation");
                return PlaybackAdvance {
                    previous_frame,
                    current_frame: previous_frame,
                    frames_advanced: 0,
                    status: PlaybackAdvanceStatus::WaitingForFrame,
                };
            }
        };
        let mut engine = self.playback_engine.clone();
        let snapshot = match engine.tick(observed_timestamp) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::error!(%error, "failed to advance Playback Session");
                return PlaybackAdvance {
                    previous_frame,
                    current_frame: previous_frame,
                    frames_advanced: 0,
                    status: PlaybackAdvanceStatus::WaitingForFrame,
                };
            }
        };
        self.playback_engine = engine;
        self.reanchor_playback_observation_projection(observed_at);
        self.capture_playback_evidence();
        let target_frame = snapshot.position.frame;
        if snapshot.state == TransportState::Ended {
            self.settle_preview_access_source();
            // End-of-program reprime is runtime cleanup after the Engine has
            // already committed `Ended`; it cannot retroactively reject the
            // clock tick, so record cleanup failure explicitly.
            let cleanup = self
                .authoritative_audio_anchor(
                    &self.playback_engine,
                    self.playback_engine.monotonic_high_water(),
                    "playback_end",
                )
                .and_then(|anchor| {
                    self.audio_playback
                        .reprime(anchor)
                        .map_err(|error| transport_action_error("playback_end", error))
                });
            if let Err(error) = cleanup {
                tracing::error!(%error, "failed to reprime audio at end of Playback Session");
            }
            self.refresh_internal_execution_resource_decision();
            return PlaybackAdvance {
                previous_frame,
                current_frame: target_frame,
                frames_advanced: (target_frame - previous_frame).max(0),
                status: PlaybackAdvanceStatus::ReachedEnd,
            };
        }
        if target_frame <= previous_frame {
            return PlaybackAdvance {
                previous_frame,
                current_frame: previous_frame,
                frames_advanced: 0,
                status: PlaybackAdvanceStatus::WaitingForFrame,
            };
        }

        PlaybackAdvance {
            previous_frame,
            current_frame: target_frame,
            frames_advanced: target_frame - previous_frame,
            status: PlaybackAdvanceStatus::Advanced,
        }
    }

    /// Estimate how long an execution loop can wait before advancing playback.
    pub fn playback_next_wake_delay(&self) -> Option<Duration> {
        self.playback_next_wake().map(|wake| clamp_playback_wake_delay(wake.after()))
    }

    /// Return the next Engine wake together with its authoritative reason.
    pub fn playback_next_wake(&self) -> Option<mondrian_playback::PlaybackWake> {
        if !self.is_playing() {
            return None;
        }
        self.playback_engine
            .next_wake(self.playback_engine.monotonic_high_water())
            .ok()
            .flatten()
    }

    pub fn current_frame(&self) -> i64 {
        self.playback_engine.snapshot().position.frame
    }

    pub fn current_timeline_time(&self) -> mondrian_core::Result<Option<TimelineTime>> {
        let Some(sequence) = self.active_sequence() else {
            return Ok(None);
        };
        Ok(Some(TimelineTime::from_frame_position(
            FramePosition::new(self.current_frame().max(0), sequence.time_base()),
        )?))
    }

    pub fn is_playing(&self) -> bool {
        matches!(
            self.playback_engine.snapshot().state,
            TransportState::Priming | TransportState::Playing | TransportState::Recovering
        )
    }

    /// Whether transport is holding its clock anchor for bounded startup preroll.
    pub fn is_playback_priming(&self) -> bool {
        self.playback_engine.snapshot().state == TransportState::Priming
    }

    /// Feed current-session video lookahead into the authoritative Playback Engine.
    pub fn observe_video_preroll(
        &mut self,
        ready_media_frames: usize,
        preservable_media_frames: usize,
    ) -> bool {
        let Some(demand) = self.playback_engine.frame_demand().map(|demand| demand.identity())
        else {
            return false;
        };
        self.observe_video_preroll_at_wall(
            demand,
            ready_media_frames,
            preservable_media_frames,
            Instant::now(),
        )
    }

    pub(crate) fn observe_video_preroll_at_wall(
        &mut self,
        demand: FrameDemandIdentity,
        ready_media_frames: usize,
        preservable_media_frames: usize,
        observed_at: Instant,
    ) -> bool {
        let observation = VideoPrerollObservation {
            demand,
            ready_media_frames,
            preservable_media_frames,
        };
        let observed_timestamp = match self.playback_timestamp_for_observation(observed_at) {
            Ok(timestamp) => timestamp.max(self.playback_engine.monotonic_high_water()),
            Err(error) => {
                tracing::warn!(%error, ?observation, "rejected video preroll with an invalid observation timestamp");
                return false;
            }
        };
        let changed =
            match self.playback_engine.observe_video_preroll(observation, observed_timestamp) {
                Ok(changed) => changed,
                Err(error) => {
                    tracing::warn!(%error, ?observation, "rejected video preroll observation");
                    false
                }
            };
        let engine_now = self.playback_engine.monotonic_high_water();
        if changed {
            self.reanchor_playback_observation_projection_at(observed_at, engine_now);
        }
        self.capture_playback_evidence();
        changed
    }

    fn audio_playback_mode(&self) -> AudioPlaybackMode {
        audio_playback_mode_for_transport(self.playback_engine.snapshot().state)
    }

    /// Current authoritative Clock Master exposed to diagnostics/UI adapters.
    pub fn playback_clock_master(&self) -> Option<mondrian_playback::ClockMaster> {
        self.playback_engine.snapshot().clock_master
    }

    /// Identity of the current contiguous Playback Session.
    ///
    /// Preview execution uses this to retain forward work across ordinary
    /// frame advances while still invalidating it atomically on seek, stop,
    /// restart, or another transport discontinuity.
    pub(crate) fn playback_epoch(&self) -> mondrian_playback::PlaybackEpoch {
        self.playback_engine.snapshot().epoch
    }

    /// Minimal current transport identity consumed by Preview execution.
    pub(crate) fn preview_transport_intent(
        &self,
    ) -> crate::app::playback_preview::PreviewTransportIntent {
        crate::app::playback_preview::PreviewTransportIntent::new(
            self.is_playing(),
            self.is_playback_priming(),
            self.playback_epoch(),
        )
    }

    /// Runtime-only Viewer scale selected by the Playback Quality Policy.
    pub fn playback_preview_resolution_scale(&self) -> PreviewResolutionScale {
        let playback_scale = if self.is_playing() {
            self.playback_engine.snapshot().preview_scale
        } else {
            PreviewResolutionScale::Full
        };
        let resource_scale = self.execution_resource_decision().preview.minimum_runtime_scale;
        if resource_scale.dimension_divisor() > playback_scale.dimension_divisor() {
            resource_scale
        } else {
            playback_scale
        }
    }

    /// Project the current Frame Demand deadline into the production monotonic
    /// domain at the exact sampling instant used by a Preview Adapter.
    ///
    /// The projected Adapter deadline includes the demand's bounded
    /// late-presentation grace so decode and GPU work that lands inside the
    /// grace window is still handed off for a degraded presentation instead of
    /// being dropped as already-late. Presentation classification itself stays
    /// on the exact [`FramePresentationTicket`] deadline.
    pub fn playback_frame_deadline_at(&self, sampled_at: Instant) -> Option<Instant> {
        let demand = self.playback_engine.pending_frame_demand()?;
        let deadline = demand.deadline?;
        let sampled_timestamp = self
            .playback_timestamp_for_observation(sampled_at)
            .ok()?
            .max(self.playback_engine.monotonic_high_water());
        let remaining = deadline
            .duration_since_origin()
            .checked_sub(sampled_timestamp.duration_since_origin())
            .unwrap_or(Duration::ZERO);
        let grace = Duration::from_nanos(demand.late_presentation_grace_ns);
        sampled_at.checked_add(remaining.saturating_add(grace))
    }

    /// Identity preview adapters may return only while the current demand still
    /// accepts a terminal presentation/decode observation.
    pub fn pending_playback_frame_demand_identity(
        &self,
    ) -> Option<mondrian_playback::FrameDemandIdentity> {
        self.playback_engine.pending_frame_demand().map(|demand| demand.identity())
    }

    /// Create exact authority for a Presentation Adapter to finish the current demand.
    pub fn playback_frame_presentation_ticket(
        &self,
        quality: FramePresentationQuality,
    ) -> Option<FramePresentationTicket> {
        self.playback_engine
            .pending_frame_demand()
            .map(|demand| FramePresentationTicket::for_demand(demand, quality))
    }

    fn frame_presentation_delivery_kind_at_timestamp(
        &self,
        ticket: FramePresentationTicket,
        completion_timestamp: MonotonicTimestamp,
    ) -> Option<FrameDeliveryKind> {
        if self.pending_playback_frame_demand_identity() != Some(ticket.identity()) {
            return None;
        }
        Some(ticket.delivery_kind_at(completion_timestamp))
    }

    /// Atomically commit one prepared output and its exact presentation ticket.
    ///
    /// All fallible or blocking preparation must happen before this call. The
    /// commit timestamp is sampled inside this seam, after preparation and
    /// immediately before terminal Playback acceptance. Only an accepted
    /// presentable ticket (or the proven absence of any demand) runs the
    /// bounded, infallible visibility commit. Late, stale, and rejected
    /// publications therefore cannot become current and never require
    /// best-effort rollback.
    pub(crate) fn finalize_frame_presentation<C: FnOnce()>(
        &mut self,
        ticket: Option<FramePresentationTicket>,
        publication: FramePresentationPublication<C>,
    ) -> FramePresentationDisposition {
        self.finalize_frame_presentation_at(ticket, Instant::now(), publication)
    }

    /// Complete a demand whose exact physical artifact was already visible at
    /// the supplied observation instant.
    ///
    /// This narrow seam is only valid for an exact prepared successor that
    /// aliases the current physical output. The commit may synchronize
    /// semantic ownership metadata, but it must not replace pixels or perform
    /// any fallible work. A distinct prepared buffer must use ordinary
    /// presentation completion at its real visibility-commit instant.
    pub(crate) fn finalize_already_visible_frame_presentation<C: FnOnce()>(
        &mut self,
        ticket: Option<FramePresentationTicket>,
        already_visible_at: Instant,
        publication: FramePresentationPublication<C>,
    ) -> FramePresentationDisposition {
        self.finalize_frame_presentation_at(ticket, already_visible_at, publication)
    }

    fn finalize_frame_presentation_at<C: FnOnce()>(
        &mut self,
        ticket: Option<FramePresentationTicket>,
        committed_at: Instant,
        publication: FramePresentationPublication<C>,
    ) -> FramePresentationDisposition {
        let Some(ticket) = ticket else {
            // A terminal Failed/Canceled/Late observation consumes ticket
            // authority but does not erase the Engine's bound demand. An
            // unbound publication must never overwrite Viewer output while
            // any demand identity still owns the current transport frame.
            if self.playback_engine.frame_demand().is_some() {
                return FramePresentationDisposition::LostAuthority;
            }
            return match publication {
                FramePresentationPublication::Prepared(commit) => {
                    commit();
                    FramePresentationDisposition::NoDemand
                }
                FramePresentationPublication::Rejected => {
                    FramePresentationDisposition::OutputRejected
                }
            };
        };
        let completion_timestamp = match self.playback_timestamp_for_observation(committed_at) {
            Ok(timestamp) => timestamp.max(self.playback_engine.monotonic_high_water()),
            Err(error) => {
                tracing::warn!(%error, "rejected frame publication with an invalid presentation timestamp");
                return FramePresentationDisposition::LostAuthority;
            }
        };
        let Some(kind) =
            self.frame_presentation_delivery_kind_at_timestamp(ticket, completion_timestamp)
        else {
            return FramePresentationDisposition::LostAuthority;
        };
        if kind == FrameDeliveryKind::Late {
            return self
                .complete_frame_presentation_at_timestamp(
                    ticket,
                    committed_at,
                    completion_timestamp,
                )
                .filter(|completion| {
                    completion.delivery().identity() == ticket.identity()
                        && completion.delivery().kind() == FrameDeliveryKind::Late
                })
                .map_or(
                    FramePresentationDisposition::LostAuthority,
                    FramePresentationDisposition::DroppedLate,
                );
        }
        if !matches!(kind, FrameDeliveryKind::Ready | FrameDeliveryKind::Degraded) {
            return FramePresentationDisposition::LostAuthority;
        }
        let FramePresentationPublication::Prepared(commit) = publication else {
            return FramePresentationDisposition::OutputRejected;
        };
        let Some(completion) = self.complete_frame_presentation_at_timestamp(
            ticket,
            committed_at,
            completion_timestamp,
        ) else {
            return FramePresentationDisposition::LostAuthority;
        };
        if completion.delivery().identity() != ticket.identity()
            || completion.delivery().kind() != kind
        {
            return FramePresentationDisposition::LostAuthority;
        }
        commit();
        FramePresentationDisposition::Presented(completion)
    }

    #[cfg(test)]
    pub(crate) fn finalize_frame_presentation_at_for_test<C: FnOnce()>(
        &mut self,
        ticket: Option<FramePresentationTicket>,
        committed_at: Instant,
        publication: FramePresentationPublication<C>,
    ) -> FramePresentationDisposition {
        self.finalize_frame_presentation_at(ticket, committed_at, publication)
    }

    /// Reject already-expired work before an Adapter records or submits GPU
    /// commands.
    ///
    /// Adapters must still call [`Self::finalize_frame_presentation`] after
    /// actual completion because a ticket that is current here can cross its
    /// deadline while GPU work is in flight.
    pub(crate) fn preflight_frame_presentation(
        &mut self,
        ticket: Option<FramePresentationTicket>,
        observed_at: Instant,
    ) -> FramePresentationPreflight {
        let Some(ticket) = ticket else {
            return if self.playback_engine.frame_demand().is_some() {
                FramePresentationPreflight::LostAuthority
            } else {
                FramePresentationPreflight::MaySubmit
            };
        };
        let observed_timestamp = match self.playback_timestamp_for_observation(observed_at) {
            Ok(timestamp) => timestamp.max(self.playback_engine.monotonic_high_water()),
            Err(error) => {
                tracing::warn!(%error, "rejected frame preflight with an invalid presentation timestamp");
                return FramePresentationPreflight::LostAuthority;
            }
        };
        let Some(kind) =
            self.frame_presentation_delivery_kind_at_timestamp(ticket, observed_timestamp)
        else {
            return FramePresentationPreflight::LostAuthority;
        };
        if kind != FrameDeliveryKind::Late {
            return FramePresentationPreflight::MaySubmit;
        }
        self.complete_frame_presentation_at_timestamp(ticket, observed_at, observed_timestamp)
            .filter(|completion| {
                completion.delivery().identity() == ticket.identity()
                    && completion.delivery().kind() == FrameDeliveryKind::Late
            })
            .map_or(
                FramePresentationPreflight::LostAuthority,
                FramePresentationPreflight::DroppedLate,
            )
    }

    /// Preflight the currently pending demand before building a Viewer
    /// candidate. Presentation quality cannot affect an already-late result,
    /// so a Ready ticket is sufficient for this early rejection seam.
    pub(crate) fn preflight_pending_frame_presentation(
        &mut self,
        observed_at: Instant,
    ) -> FramePresentationPreflight {
        let ticket = self.playback_frame_presentation_ticket(FramePresentationQuality::Ready);
        self.preflight_frame_presentation(ticket, observed_at)
    }

    /// Complete a presentation against the demand's final deadline using the
    /// monotonic-runtime/Playback timestamp mapping owned by the app Adapter.
    ///
    /// Returns the accepted terminal delivery with its authoritative
    /// Ready/Degraded/Late classification. `None` means the ticket lost
    /// authority before completion or the Playback Engine rejected it.
    pub fn complete_frame_presentation(
        &mut self,
        ticket: FramePresentationTicket,
        completed_at: Instant,
    ) -> Option<FramePresentationCompletion> {
        let completion_timestamp = self
            .playback_timestamp_for_observation(completed_at)
            .ok()?
            .max(self.playback_engine.monotonic_high_water());
        self.complete_frame_presentation_at_timestamp(ticket, completed_at, completion_timestamp)
    }

    fn complete_frame_presentation_at_timestamp(
        &mut self,
        ticket: FramePresentationTicket,
        completed_at: Instant,
        completion_timestamp: MonotonicTimestamp,
    ) -> Option<FramePresentationCompletion> {
        if self.pending_playback_frame_demand_identity() != Some(ticket.identity()) {
            // GPU completion may race a newer frame demand. It is a stale
            // presentation completion, not a terminal observation for the
            // newer demand and must not pollute rejected-delivery evidence.
            return None;
        }
        let delivery = ticket.complete_at(completion_timestamp);
        let applied = self.observe_frame_delivery_at_wall(delivery, completed_at);
        applied.accepted.then_some(FramePresentationCompletion {
            delivery,
            transport_changed: applied.snapshot_changed,
        })
    }

    /// Bind a timestamp-free terminal candidate at the App observation seam.
    ///
    /// Preview workers may classify failure/cancellation policy, but only the
    /// App owns the wall-clock to Playback timestamp mapping. The candidate is
    /// therefore not terminal authority until this method observes it.
    pub(crate) fn observe_frame_delivery_candidate(
        &mut self,
        candidate: FrameDeliveryCandidate,
        observed_at: Instant,
    ) -> bool {
        let observed_timestamp = match self.playback_timestamp_for_observation(observed_at) {
            Ok(timestamp) => timestamp.max(self.playback_engine.monotonic_high_water()),
            Err(error) => {
                tracing::warn!(%error, ?candidate, "rejected Frame Delivery candidate with an invalid observation timestamp");
                return false;
            }
        };
        let applied = self
            .observe_frame_delivery_at_wall(candidate.complete_at(observed_timestamp), observed_at);
        applied.accepted && applied.snapshot_changed
    }

    fn observe_frame_delivery_at_wall(
        &mut self,
        delivery: FrameDelivery,
        observed_at: Instant,
    ) -> AppliedFrameDelivery {
        let completed_at = delivery.completed_at();
        let engine_high_water = self.playback_engine.monotonic_high_water();
        if completed_at < engine_high_water {
            tracing::warn!(
                ?delivery,
                ?engine_high_water,
                "rejected Viewer Frame Delivery older than the Engine high-water mark"
            );
            return AppliedFrameDelivery { accepted: false, snapshot_changed: false };
        }
        let before = self.playback_engine.snapshot();
        let application = match self.playback_engine.observe_frame_delivery(delivery) {
            Ok(application) => application,
            Err(error) => {
                tracing::warn!(%error, ?delivery, "rejected Viewer Frame Delivery");
                return AppliedFrameDelivery { accepted: false, snapshot_changed: false };
            }
        };
        let accepted = application.accepted();
        let after = application.snapshot();
        if let Err(error) = self.playback_evidence.observe_delivery(application) {
            tracing::warn!(%error, "rejected Playback Evidence Frame Delivery");
        }
        self.capture_playback_evidence();
        if accepted {
            self.reanchor_playback_observation_projection_at(observed_at, completed_at);
        }
        AppliedFrameDelivery { accepted, snapshot_changed: after != before }
    }

    fn reanchor_playback_observation_projection(&mut self, observed_at: Instant) {
        self.reanchor_playback_observation_projection_at(
            observed_at,
            self.playback_engine.monotonic_high_water(),
        );
    }

    fn reanchor_playback_observation_projection_at(
        &mut self,
        observed_at: Instant,
        timestamp: MonotonicTimestamp,
    ) {
        self.playback_observation_instant_anchor = observed_at;
        self.playback_observation_time_anchor = timestamp;
    }

    fn playback_timestamp_for_observation(
        &self,
        observed_at: Instant,
    ) -> Result<MonotonicTimestamp, mondrian_playback::PlaybackError> {
        let elapsed = observed_at
            .checked_duration_since(self.playback_observation_instant_anchor)
            .ok_or(mondrian_playback::PlaybackError::NonMonotonicTimestamp)?;
        self.playback_observation_time_anchor.checked_add(elapsed)
    }

    /// Feed one terminal Viewer observation into the authoritative Playback Session.
    pub fn observe_viewer_frame_delivery(&mut self, kind: FrameDeliveryKind) -> bool {
        let before = self.playback_engine.snapshot();
        let Some(demand) = self.playback_engine.frame_demand() else {
            return false;
        };
        let changed = self.observe_frame_delivery_candidate(
            FrameDeliveryCandidate::for_demand(demand.identity(), kind),
            Instant::now(),
        );
        changed || self.playback_engine.snapshot() != before
    }

    fn playback_timeline_binding(
        &self,
        end_frame: i64,
    ) -> Result<mondrian_playback::PlaybackTimelineBinding, mondrian_playback::PlaybackError> {
        let sequence_revision =
            self.active_sequence().map_or(0, |sequence| sequence.revision.get());
        mondrian_playback::PlaybackTimelineBinding::new(
            self.active_sequence().map(|sequence| sequence.id),
            sequence_revision,
            self.playback_time_base(),
            end_frame,
        )
    }

    pub fn in_point_frame(&self) -> mondrian_core::Result<i64> {
        let Some(sequence) = self.active_sequence() else {
            return Ok(0);
        };
        Ok(sequence
            .in_point()
            .to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)?
            .frame)
    }

    pub fn out_point_frame(&self) -> mondrian_core::Result<Option<i64>> {
        let Some(sequence) = self.active_sequence() else {
            return Ok(None);
        };
        sequence
            .out_point()
            .map(|time| {
                time.to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)
                    .map(|position| position.frame)
            })
            .transpose()
            .map_err(Into::into)
    }
}

fn audio_playback_mode_for_transport(state: TransportState) -> AudioPlaybackMode {
    match state {
        TransportState::Priming => AudioPlaybackMode::Preroll,
        TransportState::Playing | TransportState::Recovering => AudioPlaybackMode::Consume,
        TransportState::Stopped
        | TransportState::Paused
        | TransportState::Ended
        | TransportState::Blocked => AudioPlaybackMode::Idle,
    }
}

fn transport_action_error(
    action: &str,
    reason: impl std::fmt::Display,
) -> mondrian_core::MondrianError {
    mondrian_core::MondrianError::ActionNotExecuted {
        action: action.to_owned(),
        reason: reason.to_string(),
    }
}

fn clamp_playback_wake_delay(delay: Duration) -> Duration {
    delay.min(MAX_PLAYBACK_WAKE_DELAY)
}

fn duration_sample_frames_ceil(
    duration: Duration,
    sample_rate: AudioSampleRate,
) -> Result<u64, mondrian_playback::PlaybackError> {
    let numerator = duration
        .as_nanos()
        .checked_mul(u128::from(sample_rate.hz()))
        .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
    let quotient = numerator / 1_000_000_000;
    let remainder = numerator % 1_000_000_000;
    u64::try_from(
        quotient
            .checked_add(u128::from(remainder != 0))
            .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?,
    )
    .map_err(|_| mondrian_playback::PlaybackError::TransportArithmeticOverflow)
}

fn sample_frames_duration_ceil(
    frames: u64,
    sample_rate: AudioSampleRate,
) -> Result<Duration, mondrian_playback::PlaybackError> {
    let numerator = u128::from(frames)
        .checked_mul(1_000_000_000)
        .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
    let denominator = u128::from(sample_rate.hz());
    let nanos = (numerator / denominator)
        .checked_add(u128::from(numerator % denominator != 0))
        .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
    let seconds = nanos / 1_000_000_000;
    let subsecond_nanos = nanos % 1_000_000_000;
    Ok(Duration::new(
        u64::try_from(seconds)
            .map_err(|_| mondrian_playback::PlaybackError::TransportArithmeticOverflow)?,
        u32::try_from(subsecond_nanos)
            .map_err(|_| mondrian_playback::PlaybackError::TransportArithmeticOverflow)?,
    ))
}

fn rebase_audio_output_snapshot(
    mut snapshot: RealtimeAudioOutputSnapshot,
    captured_at: MonotonicTimestamp,
    observed_at: MonotonicTimestamp,
) -> Result<RealtimeAudioOutputSnapshot, mondrian_playback::PlaybackError> {
    let elapsed = observed_at
        .duration_since_origin()
        .checked_sub(captured_at.duration_since_origin())
        .ok_or(mondrian_playback::PlaybackError::NonMonotonicTimestamp)?;
    if elapsed.is_zero() {
        return Ok(snapshot);
    }
    snapshot.last_callback_age = snapshot
        .last_callback_age
        .map(|age| {
            age.checked_add(elapsed)
                .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)
        })
        .transpose()?;
    snapshot.active_duration = snapshot
        .active_duration
        .map(|duration| {
            duration
                .checked_add(elapsed)
                .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)
        })
        .transpose()?;
    Ok(snapshot)
}

fn audio_device_clock_observation(
    snapshot: RealtimeAudioOutputSnapshot,
    epoch: mondrian_playback::PlaybackEpoch,
    observed_at: MonotonicTimestamp,
    already_audio_master: bool,
    media_anchor: AudioSamplePosition,
    activation_preroll_satisfied: bool,
    terminal_frozen: bool,
) -> Result<AudioDeviceClockObservation, mondrian_playback::PlaybackError> {
    let sample_rate = AudioSampleRate::new(snapshot.contract.sample_rate)
        .map_err(|_| mondrian_playback::PlaybackError::InvalidAudioSampleRate)?;
    if media_anchor.rate() != sample_rate {
        return Err(mondrian_playback::PlaybackError::MismatchedAudioSampleRate);
    }
    if media_anchor.sample() < 0 {
        return Err(mondrian_playback::PlaybackError::InvalidAudioClockPosition);
    }
    let callback_period_frames = u64::from(snapshot.last_callback_frames);
    let callback_age_frames = snapshot
        .last_callback_age
        .map(|age| duration_sample_frames_ceil(age, sample_rate))
        .transpose()?;
    let freshness_frame_limit = callback_period_frames
        .checked_mul(3)
        .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
    let callback_fresh = snapshot.last_callback_age.is_some_and(|age| {
        age <= AUDIO_CALLBACK_STALE_AFTER
            && callback_age_frames.is_some_and(|frames| frames <= freshness_frame_limit)
    });
    let callback_position_plausible = terminal_frozen
        || match (snapshot.active_duration, snapshot.last_callback_age) {
            (Some(active_duration), Some(callback_age)) => {
                let observed_span = active_duration
                    .checked_add(callback_age.min(AUDIO_CALLBACK_STALE_AFTER))
                    .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
                let maximum_consumed_frames =
                    duration_sample_frames_ceil(observed_span, sample_rate)?
                        .checked_add(callback_period_frames)
                        .ok_or(mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
                snapshot.active_callback_consumed_frames <= maximum_consumed_frames
            }
            _ => false,
        };
    let stream_available = terminal_frozen || (snapshot.active && !snapshot.stream_failed);
    let playback_delay = snapshot
        .last_callback_playback_delay
        .map(Ok)
        .unwrap_or_else(|| sample_frames_duration_ceil(callback_period_frames, sample_rate))?;
    let remaining_playback_delay = snapshot
        .last_callback_age
        .map_or(Duration::ZERO, |age| playback_delay.saturating_sub(age));
    let estimated_latency_frames = u32::try_from(duration_sample_frames_ceil(
        remaining_playback_delay,
        sample_rate,
    )?)
    .map_err(|_| mondrian_playback::PlaybackError::TransportArithmeticOverflow)?;
    // Callback consumption names frames accepted by the host, not frames that
    // have necessarily crossed the device boundary. Until consumption covers
    // the remaining host-reported playback delay, the effective device
    // position would be negative relative to this activation interval. Keep
    // Synthetic authority instead of clamping or publishing a false position.
    let effective_position_available =
        snapshot.active_callback_consumed_frames >= u64::from(estimated_latency_frames);
    let usable = stream_available
        && snapshot.callback_count > 0
        && callback_period_frames > 0
        && snapshot.active_callback_consumed_frames > 0
        && snapshot.active_callback_consumed_frames <= snapshot.callback_consumed_frames
        && effective_position_available
        && callback_fresh
        && callback_position_plausible
        && (already_audio_master || activation_preroll_satisfied);
    let uncertainty_frames = match callback_age_frames {
        Some(frames) => u32::try_from(frames.max(callback_period_frames))
            .map_err(|_| mondrian_playback::PlaybackError::TransportArithmeticOverflow)?,
        None => u32::MAX,
    };
    Ok(AudioDeviceClockObservation {
        epoch,
        stream_generation: snapshot.stream_generation,
        sample_rate: snapshot.contract.sample_rate,
        consumed_frames: snapshot.active_callback_consumed_frames,
        media_anchor,
        observed_at,
        grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
        estimated_latency_frames,
        uncertainty_frames,
        underrun_frames: snapshot.underrun_frames,
        state: if usable {
            AudioDeviceClockState::Running
        } else if stream_available {
            AudioDeviceClockState::Uncertain
        } else {
            AudioDeviceClockState::Unavailable
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tt(frame: i64, time_base: mondrian_core::Rational) -> mondrian_core::TimelineTime {
        let numerator = frame.checked_mul(time_base.num).expect("test time fits i64");
        mondrian_core::TimelineTime::new(numerator, time_base.den).expect("valid test time")
    }

    fn state_with_sequence(duration_frames: i64) -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("playback");
        let tb = sequence.time_base();
        let clip = Clip::new_solid_color(
            AssetId::new(),
            Color::from_rgba8(255, 0, 0, 255),
            tt(0, tb),
            tt(duration_frames, tb),
        )
        .expect("valid clip");
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.test_set_active_sequence(sequence.id);
        state.test_set_default_sequence(sequence.id);
        state.test_set_sequences(vec![sequence.clone()]);
        state.test_set_sequence(Some(sequence));
        state
    }

    fn play_ready(state: &mut AppState) {
        state.play().expect("play");
        assert!(!state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready));
        let demand = state
            .playback_engine
            .frame_demand()
            .map(|demand| demand.identity())
            .expect("frame demand after play");
        let observed_at = state.playback_observation_instant_anchor;
        assert!(state.observe_video_preroll_at_wall(demand, 0, 0, observed_at));
        assert_eq!(
            state.advance_playback_clock(Duration::ZERO).status,
            PlaybackAdvanceStatus::WaitingForFrame,
            "the first Window clock sample adopts preroll's monotonic high-water mark"
        );
    }

    fn observe_current_frame_delivery_at_projection(
        state: &mut AppState,
        kind: FrameDeliveryKind,
    ) -> bool {
        let identity =
            state.pending_playback_frame_demand_identity().expect("pending Frame Demand");
        state.observe_frame_delivery_candidate(
            FrameDeliveryCandidate::for_demand(identity, kind),
            state.playback_observation_instant_anchor,
        )
    }

    #[test]
    fn direct_and_dispatched_play_fail_without_sequence_and_preserve_transport() {
        let mut state = AppState::new();
        let engine_before = state.playback_engine.snapshot();
        let audio_before = state.audio_playback.snapshot(AudioPlaybackMode::Idle);

        assert!(state.play().is_err());
        assert_eq!(state.playback_engine.snapshot(), engine_before);
        assert_eq!(
            state.audio_playback.snapshot(AudioPlaybackMode::Idle),
            audio_before
        );

        assert!(state.dispatch_action(mondrian_editor_state::Action::Play).is_err());
        assert_eq!(state.playback_engine.snapshot(), engine_before);
        assert_eq!(
            state.audio_playback.snapshot(AudioPlaybackMode::Idle),
            audio_before
        );
    }

    #[test]
    fn unavailable_audio_execution_is_explicit_and_keeps_transport_synthetic() {
        let mut state = state_with_sequence(20);
        state.audio_playback = AppAudioPlayback::Unavailable {
            sample_rate: state.audio_sample_rate,
            reason: "injected render-worker spawn failure".to_owned(),
        };

        state.play().expect("video transport can use Synthetic master");
        state
            .pump_audio_output()
            .expect("explicit unavailable audio has no false device transition");

        assert_eq!(
            state.audio_playback_snapshot().state,
            mondrian_media::AudioPlaybackState::ExecutionUnavailable
        );
        assert_eq!(
            state.audio_playback_unavailable_reason(),
            Some("injected render-worker spawn failure")
        );
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
    }

    #[test]
    fn unresolved_audio_dependency_cannot_commit_play_transport() {
        let mut state = state_with_sequence(20);
        let sequence = state.active_sequence().expect("sequence").clone();
        let mut sequence = sequence;
        let tb = sequence.time_base();
        sequence.audio_tracks[0]
            .add_clip(Clip::new(AssetId::new(), tt(0, tb), tt(10, tb)).expect("audio placement"))
            .expect("add audio placement");
        state.test_set_sequence(Some(sequence));
        let engine_before = state.playback_engine.snapshot();
        let audio_before = state.audio_playback.snapshot(AudioPlaybackMode::Idle);

        let error = state.play().expect_err("unresolved audio dependency must fail closed");
        let mondrian_core::MondrianError::ActionNotExecuted { action, reason } = error else {
            panic!("play preparation must return a typed action failure");
        };
        assert_eq!(action, "play");
        assert!(reason.contains("Asset "));
        assert!(reason.contains(" is unavailable"));
        assert_eq!(state.playback_engine.snapshot(), engine_before);
        assert_eq!(
            state.audio_playback.snapshot(AudioPlaybackMode::Idle),
            audio_before
        );
    }

    #[test]
    fn play_binds_timeline_and_starts_one_epoch() {
        let mut state = state_with_sequence(20);
        let before = state.playback_engine.snapshot().epoch;
        state.test_advance_project_generation();
        let sequence_revision = state.active_sequence().expect("sequence").revision.get();

        state.play().expect("play");

        let after = state.playback_engine.snapshot();
        assert_eq!(after.epoch.get(), before.get() + 1);
        assert_eq!(after.state, TransportState::Priming);
        assert_eq!(state.playback_engine.timeline_revision(), sequence_revision);
        assert_ne!(state.playback_engine.timeline_revision(), 91);
        assert!(state.pending_playback_frame_demand_identity().is_some());
    }

    fn audio_snapshot() -> RealtimeAudioOutputSnapshot {
        RealtimeAudioOutputSnapshot {
            captured_at: Instant::now(),
            stream_generation: 3,
            contract: mondrian_media::RealtimeAudioOutputContract {
                sample_rate: 48_000,
                channel_layout: AudioChannelLayout::Stereo,
                sample_format: mondrian_media::RealtimeAudioSampleFormat::F32,
                channel_semantics: mondrian_media::RealtimeAudioChannelSemantics::StereoConvention,
                supported_buffer_size: mondrian_media::RealtimeAudioSupportedBufferSize::Unknown,
                candidates: mondrian_media::RealtimeAudioCandidateCounts {
                    enumerated: 1,
                    matching_channels: 1,
                    matching_sample_rate: 1,
                    executable: 1,
                },
            },
            callback_consumed_frames: 960,
            active_callback_consumed_frames: 480,
            active_duration: Some(Duration::from_millis(10)),
            callback_count: 2,
            underrun_frames: 0,
            last_callback_frames: 480,
            last_callback_playback_delay: Some(Duration::from_millis(10)),
            last_callback_age: Some(Duration::from_millis(1)),
            buffered_frames: 5_760,
            stream_failed: false,
            active: true,
        }
    }

    fn audio_anchor(sample: i64) -> AudioSamplePosition {
        AudioSamplePosition::new(
            sample,
            AudioSampleRate::new(48_000).expect("test sample rate"),
        )
    }

    #[test]
    fn audio_adapter_distinguishes_transient_uncertainty_from_device_loss() {
        let state = state_with_sequence(20);
        let epoch = state.playback_engine.snapshot().epoch;

        let ready = audio_device_clock_observation(
            audio_snapshot(),
            epoch,
            MonotonicTimestamp::ZERO,
            false,
            audio_anchor(0),
            true,
            false,
        )
        .expect("valid audio observation");
        assert_eq!(ready.state, AudioDeviceClockState::Running);
        assert_eq!(
            ready.grade,
            AudioClockObservationGrade::CallbackConsumptionEstimate
        );
        assert_eq!(ready.estimated_latency_frames, 432);
        assert_eq!(ready.uncertainty_frames, 480);

        let mut stale = audio_snapshot();
        stale.last_callback_age = Some(Duration::from_millis(101));
        assert_eq!(
            audio_device_clock_observation(
                stale,
                epoch,
                MonotonicTimestamp::ZERO,
                false,
                audio_anchor(0),
                true,
                false,
            )
            .expect("valid stale observation")
            .state,
            AudioDeviceClockState::Uncertain
        );

        let mut unprimed = audio_snapshot();
        unprimed.buffered_frames = 5_759;
        assert_eq!(
            audio_device_clock_observation(
                unprimed,
                epoch,
                MonotonicTimestamp::ZERO,
                false,
                audio_anchor(0),
                false,
                false,
            )
            .expect("valid unprimed observation")
            .state,
            AudioDeviceClockState::Uncertain
        );

        let mut implausibly_fast = audio_snapshot();
        implausibly_fast.active_callback_consumed_frames = 480_000;
        implausibly_fast.active_duration = Some(Duration::from_millis(100));
        assert_eq!(
            audio_device_clock_observation(
                implausibly_fast,
                epoch,
                MonotonicTimestamp::ZERO,
                false,
                audio_anchor(0),
                true,
                false,
            )
            .expect("valid implausible observation")
            .state,
            AudioDeviceClockState::Uncertain
        );

        let mut failed = audio_snapshot();
        failed.stream_failed = true;
        assert_eq!(
            audio_device_clock_observation(
                failed,
                epoch,
                MonotonicTimestamp::ZERO,
                true,
                audio_anchor(0),
                true,
                false,
            )
            .expect("valid failed-stream observation")
            .state,
            AudioDeviceClockState::Unavailable
        );
        assert_eq!(
            audio_device_clock_observation(
                failed,
                epoch,
                MonotonicTimestamp::ZERO,
                true,
                audio_anchor(0),
                true,
                true,
            )
            .expect("post-drop frozen callback evidence remains measurable")
            .state,
            AudioDeviceClockState::Running
        );
    }

    #[test]
    fn audio_adapter_waits_until_consumption_covers_reported_latency() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);
        let mut snapshot = audio_snapshot();
        snapshot.callback_consumed_frames = 14_950;
        snapshot.active_callback_consumed_frames = 512;
        snapshot.active_duration = Some(Duration::from_micros(1_078));
        snapshot.callback_count = 28;
        snapshot.last_callback_frames = 512;
        snapshot.last_callback_playback_delay = Some(Duration::from_micros(12_792));
        snapshot.last_callback_age = Some(Duration::from_micros(521));

        let observation = audio_device_clock_observation(
            snapshot,
            state.playback_engine.snapshot().epoch,
            state.playback_engine.monotonic_high_water(),
            false,
            audio_anchor(38_796),
            true,
            false,
        )
        .expect("internally consistent early callback observation");

        assert_eq!(observation.estimated_latency_frames, 590);
        assert_eq!(observation.state, AudioDeviceClockState::Uncertain);
        state
            .playback_engine
            .observe_audio_device_clock(observation)
            .expect("early device position remains non-authoritative");
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
    }

    #[test]
    fn underrun_recovery_consumes_final_device_position_before_synthetic_handoff() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);
        let epoch = state.playback_engine.snapshot().epoch;
        let initial = audio_device_clock_observation(
            audio_snapshot(),
            epoch,
            state.playback_engine.monotonic_high_water(),
            false,
            audio_anchor(0),
            true,
            false,
        )
        .expect("valid initial audio observation");
        state.playback_engine.observe_audio_device_clock(initial).expect("audio master");
        state.advance_playback_clock(Duration::from_millis(30));
        assert_eq!(state.current_frame(), 0);

        let mut final_output = audio_snapshot();
        let handled_at = state.playback_observation_instant_anchor + Duration::from_millis(1);
        final_output.captured_at = handled_at;
        final_output.callback_consumed_frames = 2_880;
        final_output.active_callback_consumed_frames = 2_400;
        final_output.active_duration = Some(Duration::from_millis(50));
        final_output.callback_count = 6;
        final_output.underrun_frames = 960;
        state
            .observe_final_audio_clock_before_recovery(final_output, audio_anchor(0), handled_at)
            .expect("final audio observation and Synthetic handoff");
        assert_eq!(state.current_frame(), 1);
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
        state.advance_playback_clock(Duration::from_millis(40));
        assert_eq!(state.current_frame(), 2);
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
    }

    #[test]
    fn invalid_final_device_evidence_cannot_block_confirmed_loss_handoff() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);
        let observation = audio_device_clock_observation(
            audio_snapshot(),
            state.playback_engine.snapshot().epoch,
            state.playback_engine.monotonic_high_water(),
            false,
            audio_anchor(0),
            true,
            false,
        )
        .expect("valid audio observation");
        state
            .playback_engine
            .observe_audio_device_clock(observation)
            .expect("qualify audio master");
        state
            .playback_engine
            .tick(MonotonicTimestamp::from_duration(Duration::from_millis(10)))
            .expect("advance Engine timestamp beyond App adapter timestamp");
        let result = state.handle_audio_playback_event(
            AudioPlaybackEvent::DeviceLost {
                reason: mondrian_media::RealtimeAudioOutputLossReason::BackendFailure,
                final_output: audio_snapshot(),
                final_media_anchor: Some(audio_anchor(0)),
            },
            Instant::now(),
        );

        assert!(result.is_ok());
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
    }

    #[test]
    fn render_worker_exit_becomes_explicitly_unavailable_and_synthetic_mastered() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);
        let observation = audio_device_clock_observation(
            audio_snapshot(),
            state.playback_engine.snapshot().epoch,
            state.playback_engine.monotonic_high_water(),
            false,
            audio_anchor(0),
            true,
            false,
        )
        .expect("valid audio observation");
        state
            .playback_engine
            .observe_audio_device_clock(observation)
            .expect("qualify audio master");

        let result = state.handle_audio_playback_event(
            AudioPlaybackEvent::RenderWorkerStoppedUnexpectedly {
                reason: "injected early exit".to_owned(),
            },
            Instant::now(),
        );

        assert!(result.is_err());
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
        assert_eq!(
            state.audio_playback_snapshot().state,
            mondrian_media::AudioPlaybackState::ExecutionUnavailable
        );
        assert_eq!(
            state.audio_playback_unavailable_reason(),
            Some("audio render worker stopped unexpectedly: injected early exit")
        );
    }

    #[test]
    fn production_adapter_reports_warm_seek_and_delivery_latency() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        state.seek_with_source(10, TimelineSeekSource::PointerDrag).expect("seek");
        let completed_at = state.playback_observation_instant_anchor + Duration::from_millis(120);
        state.advance_playback_clock_at(completed_at);
        assert_eq!(state.playback_observation_instant_anchor, completed_at);
        assert_eq!(
            state.playback_observation_time_anchor,
            state.playback_engine.monotonic_high_water()
        );
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("warm seek presentation ticket");
        assert_eq!(
            state
                .complete_frame_presentation(ticket, completed_at)
                .map(FramePresentationCompletion::delivery)
                .map(FrameDelivery::kind),
            Some(FrameDeliveryKind::Ready)
        );
        let demand = state.playback_engine.frame_demand().expect("warm seek demand").identity();
        assert!(state.observe_video_preroll_at_wall(demand, 0, 0, completed_at));

        let report = state.playback_evidence_report();

        assert!(report.snapshot_count >= 4);
        assert!(report.demand_count >= 2);
        assert_eq!(report.deliveries.ready, 2);
        assert_eq!(report.warm_seek_latency.count, 1);
        assert_eq!(report.warm_seek_latency.p95_us, 120_000);
        assert!(report.clock_residency.synthetic_us >= 40_000);
        assert_eq!(
            report.schema_version,
            mondrian_playback::PLAYBACK_EVIDENCE_SCHEMA_VERSION
        );
    }

    #[test]
    fn paused_seek_presentation_records_real_warm_latency() {
        let mut state = state_with_sequence(40);

        state.seek_with_source(10, TimelineSeekSource::PointerDrag).expect("seek");
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("paused seek presentation ticket");
        let completed_at = state.playback_observation_instant_anchor + Duration::from_millis(120);
        assert_eq!(
            state
                .complete_frame_presentation(ticket, completed_at)
                .map(FramePresentationCompletion::delivery)
                .map(FrameDelivery::kind),
            Some(FrameDeliveryKind::Ready)
        );

        let report = state.playback_evidence_report();
        assert_eq!(report.deliveries.ready, 1);
        assert_eq!(report.warm_seek_latency.count, 1);
        assert_eq!(report.warm_seek_latency.p95_us, 120_000);

        state.play().expect("play");
        assert_eq!(
            state.playback_engine.monotonic_high_water(),
            state.playback_evidence_now
        );
    }

    #[test]
    fn advance_playback_clock_accumulates_subframe_ticks() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);

        let waiting = state.advance_playback_clock(Duration::from_millis(10));
        assert_eq!(waiting.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(state.current_frame(), 0);

        for _ in 0..3 {
            state.advance_playback_clock(Duration::from_millis(10));
        }

        assert_eq!(state.current_frame(), 1);
        assert!(state.is_playing());
    }

    #[test]
    fn late_viewer_delivery_does_not_hold_synthetic_clock() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        observe_current_frame_delivery_at_projection(&mut state, FrameDeliveryKind::Late);

        let advanced = state.advance_playback_clock(Duration::from_millis(40));

        assert_eq!(advanced.status, PlaybackAdvanceStatus::Advanced);
        assert_eq!(advanced.current_frame, 2);
        assert!(state.is_playing());
    }

    #[test]
    fn priming_grants_audio_preroll_before_consumption() {
        let mut state = state_with_sequence(20);
        state.play().expect("play");

        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Priming
        );
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Preroll);

        state.advance_playback_clock(Duration::from_millis(1499));
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Priming
        );
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Preroll);

        state.advance_playback_clock(Duration::from_millis(2));
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Playing
        );
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Consume);

        state.pause().expect("pause");
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Idle);
    }

    #[test]
    fn sustained_late_deliveries_expose_lower_runtime_preview_scale_to_adapters() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);

        for _ in 0..12 {
            state.advance_playback_clock(Duration::from_millis(40));
            observe_current_frame_delivery_at_projection(&mut state, FrameDeliveryKind::Late);
        }

        // The sustained-late window degrades one step per full pressure
        // window. macOS's monotonic clock lands an extra pressure sample in
        // the same deterministic tick sequence, so the engine reaches a
        // second degradation step there. The behavior contract is the same:
        // sustained late delivery must drop the runtime preview scale below
        // Full and enter Recovering.
        let degraded_scale = state.playback_preview_resolution_scale();
        assert_ne!(
            degraded_scale,
            PreviewResolutionScale::Full,
            "sustained late deliveries must degrade the runtime preview scale"
        );
        assert!(
            degraded_scale.dimension_divisor() >= PreviewResolutionScale::Half.dimension_divisor(),
            "sustained late deliveries must degrade to at least Half scale"
        );
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Recovering
        );
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Consume);

        state.pause().expect("pause");
        assert_eq!(
            state.playback_preview_resolution_scale(),
            PreviewResolutionScale::Full,
            "paused still-frame work must return to the authored preview scale"
        );
    }

    #[test]
    fn presentation_completion_crossing_deadline_is_late_in_policy_and_evidence() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("playing presentation ticket");
        let completed_at = state.playback_observation_instant_anchor + Duration::from_millis(41);

        let completion = state
            .complete_frame_presentation(ticket, completed_at)
            .expect("accepted late presentation");
        assert_eq!(completion.delivery().kind(), FrameDeliveryKind::Late);

        let report = state.playback_evidence_report();
        assert_eq!(report.deliveries.ready, 1);
        assert_eq!(report.deliveries.late, 1);
        assert_eq!(report.demand_latency.count, 2);
        assert!(report.demand_latency.p95_us >= 41_000);
    }

    #[test]
    fn prepared_publication_crossing_deadline_cannot_become_current() {
        let mut state = state_with_sequence(40);
        state.set_playback_frame_running(4);
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("playing presentation ticket");
        let evidence_before = state.playback_evidence_report();
        let published = std::cell::Cell::new(false);
        let publication = FramePresentationPublication::prepared(|| published.set(true));
        state.playback_observation_instant_anchor = Instant::now() - Duration::from_secs(10);

        let disposition = state.finalize_frame_presentation(Some(ticket), publication);

        assert!(
            matches!(
                disposition,
                FramePresentationDisposition::DroppedLate(completion)
                    if completion.delivery().kind() == FrameDeliveryKind::Late
            ),
            "the publication commit instant, not its earlier preparation instant, decides timeliness"
        );
        assert!(
            !published.get(),
            "a prepared output that missed the commit deadline must never become current"
        );
        let evidence_after = state.playback_evidence_report();
        assert_eq!(
            evidence_after.deliveries.ready, evidence_before.deliveries.ready,
            "crossing the publication deadline cannot create false Ready evidence"
        );
        assert_eq!(
            evidence_after.deliveries.late,
            evidence_before.deliveries.late + 1
        );
    }

    #[test]
    fn stale_epoch_publication_cannot_replace_current_output_or_evidence() {
        let mut state = state_with_sequence(40);
        state.set_playback_frame_running(4);
        let stale_ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("playing presentation ticket");
        let stale_epoch = state.playback_epoch();

        state.set_playback_frame_running(5);
        assert_ne!(state.playback_epoch(), stale_epoch);
        let current_identity = state
            .pending_playback_frame_demand_identity()
            .expect("seek issues a replacement demand");
        assert_ne!(current_identity, stale_ticket.identity());
        let rejected_before = state.playback_evidence_report().deliveries.rejected;
        let published = std::cell::Cell::new(false);
        let committed_at = state.playback_observation_instant_anchor;

        let disposition = state.finalize_frame_presentation_at(
            Some(stale_ticket),
            committed_at,
            FramePresentationPublication::prepared(|| published.set(true)),
        );

        assert_eq!(disposition, FramePresentationDisposition::LostAuthority);
        assert!(!published.get());
        assert_eq!(
            state.pending_playback_frame_demand_identity(),
            Some(current_identity)
        );
        assert_eq!(
            state.playback_evidence_report().deliveries.rejected,
            rejected_before,
            "a stale epoch is retired silently and cannot pollute the replacement demand"
        );
    }

    #[test]
    fn presentation_preflight_consumes_late_demand_before_gpu_submission() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("playing presentation ticket");
        let observed_at = state.playback_observation_instant_anchor + Duration::from_millis(41);
        let gpu_submitted = std::cell::Cell::new(false);

        let preflight = state.preflight_frame_presentation(Some(ticket), observed_at);
        if preflight == FramePresentationPreflight::MaySubmit {
            gpu_submitted.set(true);
        }

        assert!(matches!(
            preflight,
            FramePresentationPreflight::DroppedLate(completion)
                if completion.delivery().kind() == FrameDeliveryKind::Late
                    && completion.delivery().identity() == ticket.identity()
        ));
        assert!(
            !gpu_submitted.get(),
            "expired work must not reach the GPU Adapter"
        );
        assert_ne!(
            state.pending_playback_frame_demand_identity(),
            Some(ticket.identity()),
            "preflight owns the one terminal Late completion"
        );
        assert_eq!(state.playback_evidence_report().deliveries.late, 1);

        let unbound_published = std::cell::Cell::new(false);
        assert_eq!(
            state.preflight_frame_presentation(None, observed_at),
            FramePresentationPreflight::LostAuthority,
            "a consumed ticket does not erase the Engine-bound frame demand"
        );
        assert_eq!(
            state.finalize_frame_presentation_at(
                None,
                observed_at,
                FramePresentationPublication::prepared(|| unbound_published.set(true)),
            ),
            FramePresentationDisposition::LostAuthority
        );
        assert!(!unbound_published.get());
    }

    #[test]
    fn presentation_preflight_leaves_presentable_demand_pending_until_publication() {
        let mut state = state_with_sequence(40);
        state.set_playback_frame_running(4);
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("playing presentation ticket");
        let observed_at = state.playback_observation_instant_anchor + Duration::from_millis(10);
        let ready_before = state.playback_evidence_report().deliveries.ready;

        assert_eq!(
            state.preflight_frame_presentation(Some(ticket), observed_at),
            FramePresentationPreflight::MaySubmit
        );
        assert_eq!(
            state.pending_playback_frame_demand_identity(),
            Some(ticket.identity()),
            "preflight permission is not terminal evidence"
        );
        assert_eq!(
            state.playback_evidence_report().deliveries.ready,
            ready_before
        );

        let published = std::cell::Cell::new(false);
        assert!(matches!(
            state.finalize_frame_presentation_at(
                Some(ticket),
                observed_at,
                FramePresentationPublication::prepared(|| published.set(true))
            ),
            FramePresentationDisposition::Presented(completion)
                if completion.delivery().kind() == FrameDeliveryKind::Ready
        ));
        assert!(published.get());
        assert_eq!(
            state.playback_evidence_report().deliveries.ready,
            ready_before + 1
        );
    }

    #[test]
    fn superseded_gpu_presentation_ticket_is_silently_retired() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        let stale_ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("current presentation ticket");
        state.advance_playback_clock(Duration::from_millis(40));
        let rejected_before = state.playback_evidence_report().deliveries.rejected;

        assert_eq!(
            state.complete_frame_presentation(
                stale_ticket,
                state.playback_observation_instant_anchor,
            ),
            None
        );

        assert_eq!(
            state.playback_evidence_report().deliveries.rejected,
            rejected_before
        );
    }

    #[test]
    fn worker_deadline_projection_does_not_regrant_time_spent_before_enqueue() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        let demand_anchor = state.playback_observation_instant_anchor;
        let sampled_at = demand_anchor + Duration::from_millis(10);
        let grace_ns = state
            .playback_engine
            .pending_frame_demand()
            .expect("active playback demand")
            .late_presentation_grace_ns;
        let grace = Duration::from_nanos(grace_ns);

        let deadline_at =
            state.playback_frame_deadline_at(sampled_at).expect("projected worker deadline");

        assert_eq!(
            deadline_at,
            demand_anchor + Duration::from_millis(20) + grace,
            "the projected Adapter deadline preserves the phase budget and adds the bounded late-presentation grace"
        );
        assert_eq!(
            deadline_at.duration_since(sampled_at),
            Duration::from_millis(10) + grace
        );
    }

    #[test]
    fn advance_playback_clock_reaches_end_and_pauses() {
        let mut state = state_with_sequence(5);
        play_ready(&mut state);

        let outcome = state.advance_playback_clock(Duration::from_secs(1));

        assert_eq!(outcome.status, PlaybackAdvanceStatus::ReachedEnd);
        assert_eq!(outcome.current_frame, 4);
        assert_eq!(state.current_frame(), 4);
        assert!(!state.is_playing());
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Ended
        );
    }

    #[test]
    fn play_after_reaching_end_restarts_from_zero() {
        let mut state = state_with_sequence(5);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_secs(1));

        play_ready(&mut state);

        assert_eq!(state.current_frame(), 0);
        assert!(state.is_playing());
    }

    #[test]
    fn seek_starts_a_new_epoch_and_reanchors_subframe_time() {
        let mut state = state_with_sequence(30);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(20));
        let old_epoch = state.playback_engine.snapshot().epoch;

        state.seek(10).expect("seek");
        let outcome = state.advance_playback_clock(Duration::from_millis(20));

        assert_eq!(outcome.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(state.current_frame(), 10);
        assert_eq!(
            state.playback_engine.snapshot().epoch.get(),
            old_epoch.get() + 1
        );
        assert!(state.playback_engine.pending_frame_demand().is_some());
    }

    #[test]
    fn completion_for_pre_seek_demand_cannot_mutate_new_session() {
        let mut state = state_with_sequence(30);
        state.play().expect("play");
        let old_identity =
            state.pending_playback_frame_demand_identity().expect("priming demand identity");

        state.seek(10).expect("seek");
        let before = state.playback_engine.snapshot();
        let accepted = state.observe_frame_delivery_candidate(
            FrameDeliveryCandidate::for_demand(old_identity, FrameDeliveryKind::Ready),
            Instant::now(),
        );

        assert!(!accepted);
        assert_eq!(state.playback_engine.snapshot(), before);
        assert_eq!(before.state, TransportState::Priming);
    }

    #[test]
    fn seek_records_preview_access_source() {
        let mut state = state_with_sequence(30);

        state.seek_with_source(12, TimelineSeekSource::PointerDrag).expect("seek");
        assert_eq!(state.current_frame(), 12);
        assert_eq!(
            state.last_timeline_seek_source,
            TimelineSeekSource::PointerDrag
        );

        state.seek(18).expect("seek");
        assert_eq!(state.current_frame(), 18);
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);
    }

    #[test]
    fn pause_stop_and_reached_end_settle_preview_access_source() {
        let mut state = state_with_sequence(5);

        state.seek_with_source(2, TimelineSeekSource::PointerDrag).expect("seek");
        state.play().expect("play");
        state.pause().expect("pause");
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);

        state.seek_with_source(3, TimelineSeekSource::PointerDrag).expect("seek");
        state.stop().expect("stop");
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);

        state.seek_with_source(4, TimelineSeekSource::PointerDrag).expect("seek");
        state.play().expect("play");
        let outcome = state.advance_playback_clock(Duration::from_secs(2));
        assert_eq!(outcome.status, PlaybackAdvanceStatus::ReachedEnd);
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);
    }

    #[test]
    fn playback_next_wake_delay_is_bounded_while_playing() {
        let mut state = state_with_sequence(30);
        play_ready(&mut state);

        let delay = state.playback_next_wake_delay().expect("next playback wake");

        assert!(delay <= MAX_PLAYBACK_WAKE_DELAY);
    }

    #[test]
    fn playback_next_wake_delay_keeps_unpresented_priming_bounded() {
        let mut state = state_with_sequence(30);
        state.play().expect("play");
        assert!(state.is_playback_priming());

        let delay =
            state.playback_next_wake_delay().expect("priming fallback must schedule a wake");

        assert!(delay <= MAX_PLAYBACK_WAKE_DELAY);
    }

    #[test]
    fn playback_wake_delay_preserves_submillisecond_phase_deadlines() {
        assert_eq!(
            clamp_playback_wake_delay(Duration::from_micros(250)),
            Duration::from_micros(250)
        );
        assert_eq!(clamp_playback_wake_delay(Duration::ZERO), Duration::ZERO);
        assert_eq!(
            clamp_playback_wake_delay(Duration::from_secs(1)),
            MAX_PLAYBACK_WAKE_DELAY
        );
    }

    #[test]
    fn absolute_clock_continues_after_unpresented_priming_budget_expires() {
        let mut state = state_with_sequence(30);
        state.play().expect("play");
        let observation_anchor = state.playback_observation_instant_anchor;

        let advance =
            state.advance_playback_clock_at(observation_anchor + Duration::from_millis(1_600));

        assert_eq!(advance.status, PlaybackAdvanceStatus::Advanced);
        assert!(
            state.current_frame() > 0,
            "the clock must continue after the unpresented priming budget expires"
        );
        assert!(state.pending_playback_frame_demand_identity().is_some());
    }

    #[test]
    fn absolute_clock_observation_does_not_recount_viewer_completion_interval() {
        let mut state = state_with_sequence(30);
        play_ready(&mut state);
        let observation_origin = state.playback_observation_instant_anchor;
        let playback_origin = state.playback_observation_time_anchor;

        state.advance_playback_clock_at(observation_origin + Duration::from_millis(40));
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("next-frame presentation ticket");
        state
            .complete_frame_presentation(ticket, observation_origin + Duration::from_millis(45))
            .expect("on-time Viewer completion");

        state.advance_playback_clock_at(observation_origin + Duration::from_millis(50));

        assert_eq!(
            state.playback_engine.monotonic_high_water(),
            playback_origin
                .checked_add(Duration::from_millis(50))
                .expect("bounded expected timestamp"),
            "the next absolute tick must not add its pre-completion interval twice"
        );
    }

    #[test]
    fn first_clock_tick_adopts_observed_priming_high_water_without_double_counting() {
        let mut state = state_with_sequence(30);
        state.play().expect("play");
        let observation_anchor = state.playback_observation_instant_anchor;
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("priming ticket");

        assert!(state
            .complete_frame_presentation(ticket, observation_anchor + Duration::from_millis(5))
            .is_some());
        let priming_completed_at = observation_anchor + Duration::from_millis(17);
        let demand = state.playback_engine.frame_demand().expect("priming demand").identity();
        assert!(state.observe_video_preroll_at_wall(demand, 0, 0, priming_completed_at));
        assert_eq!(
            state.playback_engine.monotonic_high_water(),
            MonotonicTimestamp::from_duration(Duration::from_millis(17))
        );
        assert_eq!(
            state.playback_evidence_now,
            MonotonicTimestamp::from_duration(Duration::from_millis(17))
        );

        let stationary = state.advance_playback_clock_at(priming_completed_at);
        assert_eq!(stationary.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(
            state.playback_engine.monotonic_high_water(),
            MonotonicTimestamp::from_duration(Duration::from_millis(17))
        );
        assert_eq!(
            state.playback_next_wake_delay(),
            Some(Duration::from_millis(40))
        );

        let subframe =
            state.advance_playback_clock_at(priming_completed_at + Duration::from_millis(3));
        assert_eq!(subframe.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(
            state.playback_next_wake_delay(),
            Some(Duration::from_millis(37))
        );

        let boundary =
            state.advance_playback_clock_at(priming_completed_at + Duration::from_millis(40));
        assert_eq!(boundary.status, PlaybackAdvanceStatus::Advanced);
        assert_eq!(state.current_frame(), 1);
    }
}
