use super::*;

const MIN_PLAYBACK_WAKE_DELAY: Duration = Duration::from_millis(1);
const MAX_PLAYBACK_WAKE_DELAY: Duration = Duration::from_millis(100);
const AUDIO_CALLBACK_STALE_AFTER: Duration = Duration::from_millis(100);

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
        self.sequence.as_ref().map(Sequence::time_base).unwrap_or(Rational::new(1, 25))
    }

    fn capture_playback_evidence(&mut self) {
        let observed_at = self.playback_now.max(self.playback_evidence_now);
        self.capture_playback_evidence_at(observed_at);
    }

    fn capture_playback_evidence_at(&mut self, observed_at: MonotonicTimestamp) {
        self.playback_evidence_now = self.playback_evidence_now.max(observed_at);
        let snapshot = self.playback_engine.snapshot();
        let demand = self.playback_engine.frame_demand();
        if let Err(error) = self.playback_evidence.observe_snapshot(observed_at, snapshot, demand) {
            tracing::warn!(%error, "rejected Playback Evidence snapshot");
        }
    }

    fn synchronize_playback_runtime_to_evidence(&mut self) {
        self.playback_now = self.playback_now.max(self.playback_evidence_now);
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
        self.playback_evidence_now = self.playback_now;
        self.capture_playback_evidence();
        Ok(())
    }

    pub fn play(&mut self) {
        self.synchronize_playback_runtime_to_evidence();
        let Ok(end_frame) = self.last_content_frame() else {
            tracing::error!("failed to resolve exact Sequence duration onto playback frame grid");
            return;
        };
        let mut frames = self.current_frame();
        let prior_state = self.playback_engine.snapshot().state;
        if prior_state == TransportState::Stopped {
            frames = 0;
        }
        if prior_state == TransportState::Ended || (end_frame >= 0 && frames > end_frame) {
            frames = 0;
        }
        let time_base = self.playback_time_base();
        let binding = match self.playback_timeline_binding(end_frame) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::error!(%error, "failed to bind Playback Session timeline");
                return;
            }
        };
        if let Err(error) = self.playback_engine.play_timeline(
            binding,
            FramePosition::new(frames, time_base),
            self.playback_now,
        ) {
            tracing::error!(%error, "failed to start Playback Session");
            return;
        }
        self.reanchor_playback_presentation_clock(Instant::now());
        self.prepare_audio_playback(FramePosition::new(frames, time_base));
        self.capture_playback_evidence();
    }

    pub fn pause(&mut self) {
        self.synchronize_playback_runtime_to_evidence();
        let frames = self.current_frame();
        self.settle_preview_access_source();
        if let Err(error) = self.playback_engine.pause(self.playback_now) {
            tracing::error!(%error, "failed to pause Playback Session");
        }
        self.audio_playback
            .reprime(FramePosition::new(frames, self.playback_time_base()));
        self.capture_playback_evidence();
    }

    pub fn stop(&mut self) {
        self.synchronize_playback_runtime_to_evidence();
        self.settle_preview_access_source();
        if let Err(error) = self.playback_engine.stop(self.playback_now) {
            tracing::error!(%error, "failed to stop Playback Session");
        }
        self.audio_playback.reprime(FramePosition::new(0, self.playback_time_base()));
        self.capture_playback_evidence();
    }

    pub fn seek(&mut self, frame: i64) {
        self.seek_with_source(frame, TimelineSeekSource::Settled);
    }

    pub(crate) fn settle_preview_access_source(&mut self) {
        self.last_timeline_seek_source = TimelineSeekSource::Settled;
    }

    pub fn seek_with_source(&mut self, frame: i64, source: TimelineSeekSource) {
        self.synchronize_playback_runtime_to_evidence();
        let was_running = self.is_playing();
        self.last_timeline_seek_source = source;
        let Ok(end_frame) = self.last_content_frame() else {
            tracing::error!("failed to resolve exact Sequence duration onto playback frame grid");
            return;
        };
        let time_base = self.playback_time_base();
        let binding = match self.playback_timeline_binding(end_frame) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::error!(%error, "failed to bind Playback Session timeline");
                return;
            }
        };
        if let Err(error) = self.playback_engine.seek_timeline(
            binding,
            FramePosition::new(frame, time_base),
            self.playback_now,
        ) {
            tracing::error!(%error, "failed to seek Playback Session");
            return;
        }
        self.reanchor_playback_presentation_clock(Instant::now());
        if was_running {
            self.prepare_audio_playback(FramePosition::new(frame, time_base));
        } else {
            self.audio_playback.reprime(FramePosition::new(frame, time_base));
        }
        let seek_kind = match source {
            TimelineSeekSource::PointerDrag => PlaybackSeekKind::Warm,
            TimelineSeekSource::Settled => PlaybackSeekKind::Accurate,
        };
        if let Err(error) = self.playback_evidence.begin_seek(
            self.playback_now.max(self.playback_evidence_now),
            self.playback_engine.snapshot().epoch,
            seek_kind,
        ) {
            tracing::warn!(%error, "rejected Playback Evidence seek start");
        }
        self.capture_playback_evidence();
    }

    pub fn set_playback_frame_running(&mut self, frame: i64) {
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
            self.playback_now,
        ) {
            tracing::error!(%error, "failed to start simulated Playback Session");
        }
    }

    pub fn pump_audio_output(&mut self) {
        let mode = self.audio_playback_mode();
        let position = self.playback_engine.snapshot().position;
        let poll = self.audio_playback.poll(mode, position);
        for event in poll.events {
            self.handle_audio_playback_event(event);
        }
        if mode == AudioPlaybackMode::Idle {
            if audio_idle_warmup_enabled() {
                self.warm_audio_cache_when_idle();
            }
            return;
        }
        self.audio_idle_warmup_last = None;
        self.observe_audio_output_clock(poll.snapshot);
    }

    fn handle_audio_playback_event(&mut self, event: AudioPlaybackEvent) {
        match event {
            AudioPlaybackEvent::DeviceOpened { stream_generation } => {
                tracing::info!(
                    stream_generation,
                    "audio output stream opened; starting preroll"
                );
            }
            AudioPlaybackEvent::DeviceLost { stream_generation } => {
                tracing::warn!(
                    stream_generation,
                    "audio output stream lost; using Synthetic Clock Master"
                );
                if let Err(error) = self.playback_engine.audio_device_lost(self.playback_now) {
                    tracing::warn!(%error, "failed to hand off lost audio stream");
                }
                self.capture_playback_evidence();
            }
            AudioPlaybackEvent::DeviceOpenFailed { retry_after, reason } => {
                tracing::debug!(?retry_after, %reason, "audio output open failed; retry scheduled");
            }
            AudioPlaybackEvent::RenderSubstitutedWithSilence {
                generation,
                start_sample,
                reason,
            } => tracing::warn!(
                generation,
                start_sample,
                %reason,
                "audio render window replaced with exact-duration silence"
            ),
            AudioPlaybackEvent::UnderrunObserved {
                stream_generation,
                delta_frames,
                interval_total_frames,
            } => {
                if let Err(error) = self.playback_evidence.observe_audio_underrun(
                    self.playback_now.max(self.playback_evidence_now),
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
            }
            AudioPlaybackEvent::UnderrunRecoveryStarted {
                stream_generation,
                missing_frames,
                threshold_frames,
                final_output,
                final_media_anchor,
            } => {
                if let Err(error) = self.playback_evidence.observe_audio_underrun(
                    self.playback_now.max(self.playback_evidence_now),
                    self.playback_engine.snapshot().epoch,
                    0,
                    true,
                ) {
                    tracing::warn!(%error, "rejected Playback Evidence underrun recovery");
                }
                self.observe_final_audio_clock_before_recovery(final_output, final_media_anchor);
                tracing::warn!(
                    stream_generation,
                    missing_frames,
                    threshold_frames,
                    "sustained audio underrun; using Synthetic Clock Master during reprime"
                );
            }
        }
    }

    fn observe_final_audio_clock_before_recovery(
        &mut self,
        output: RealtimeAudioOutputSnapshot,
        media_anchor: FramePosition,
    ) {
        let observation = audio_device_clock_observation(
            output,
            self.playback_engine.snapshot().epoch,
            self.playback_now,
            true,
            Some(media_anchor),
            true,
        );
        if let Err(error) = self.playback_engine.observe_audio_device_clock(observation) {
            tracing::warn!(%error, "rejected final audio clock before underrun recovery");
        }
        self.capture_playback_evidence();
    }

    fn observe_audio_output_clock(&mut self, audio: AudioPlaybackSnapshot) {
        let Some(snapshot) = audio.output else {
            return;
        };
        if !self.is_playing() {
            return;
        }
        let already_audio_master =
            self.playback_engine.snapshot().clock_master == Some(ClockMaster::AudioDevice);
        let observation = audio_device_clock_observation(
            snapshot,
            self.playback_engine.snapshot().epoch,
            self.playback_now,
            already_audio_master,
            audio.media_anchor,
            audio.activation_preroll_satisfied,
        );
        match self.playback_engine.observe_audio_device_clock(observation) {
            Ok(engine_snapshot)
                if observation.state == AudioDeviceClockState::Running
                    && !already_audio_master
                    && engine_snapshot.clock_master != Some(ClockMaster::AudioDevice)
                    && engine_snapshot.audio_handoff.is_some_and(|handoff| {
                        handoff.stream_generation == observation.stream_generation
                            && handoff.status
                                == mondrian_playback::AudioClockHandoffStatus::PhaseRejected
                    }) =>
            {
                self.audio_playback.reprime(engine_snapshot.position);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "rejected audio-device Clock Master observation"),
        }
        self.capture_playback_evidence();
    }

    fn prepare_audio_playback(&mut self, anchor: FramePosition) {
        let renderer = self
            .sequence
            .as_ref()
            .filter(|sequence| sequence_has_audible_audio(sequence))
            .zip(self.asset_library.as_ref())
            .and_then(|(sequence, library)| {
                match TimelineAudioPcmRenderer::new(
                    sequence.clone(),
                    self.sequences.clone(),
                    Arc::clone(library),
                    Arc::clone(&self.audio_source_cache),
                    self.audio_sample_rate,
                    AUDIO_OUTPUT_CHANNELS,
                ) {
                    Ok(renderer) => Some(Arc::new(renderer) as Arc<dyn AudioPcmRenderer>),
                    Err(error) => {
                        tracing::error!(%error, "audio Program preparation failed closed");
                        None
                    }
                }
            });
        if let Some(renderer) = renderer {
            self.audio_playback.prepare(anchor, renderer);
        } else {
            self.audio_playback.clear_source(anchor);
        }
    }

    fn warm_audio_cache_when_idle(&mut self) {
        let now = std::time::Instant::now();
        if let Some(last) = self.audio_idle_warmup_last {
            if now.saturating_duration_since(last) < Duration::from_millis(900) {
                return;
            }
        }

        let Some(seq) = self.sequence.as_ref() else {
            return;
        };
        let Some(library) = self.asset_library.as_ref() else {
            return;
        };

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
        let before = center.sample().saturating_sub(chunk_frames as i64).max(0);

        let Ok(renderer) = TimelineAudioPcmRenderer::new(
            seq.clone(),
            self.sequences.clone(),
            Arc::clone(library),
            Arc::clone(&self.audio_source_cache),
            self.audio_sample_rate,
            AUDIO_OUTPUT_CHANNELS,
        ) else {
            return;
        };
        let cancellation = mondrian_core::ExecutionCancellationToken::new();
        for start_sample in [
            center.sample(),
            before,
            center.sample().saturating_add(chunk_frames as i64),
        ] {
            let _ = renderer.render(
                AudioPcmRenderRequest {
                    start_sample,
                    frame_count: chunk_frames,
                    sample_rate: self.audio_sample_rate,
                    channels: AUDIO_OUTPUT_CHANNELS,
                },
                &cancellation,
            );
        }

        self.audio_idle_warmup_last = Some(now);
    }

    pub fn audio_developer_metrics_summary(&self) -> String {
        let snapshot = self.audio_playback.snapshot(self.audio_playback_mode());
        let buffered_frames = snapshot.output.map_or(0, |output| output.buffered_frames);
        let buffered_ms = buffered_frames as f64 / self.audio_sample_rate as f64 * 1000.0;
        let source_cache = self.audio_source_cache.diagnostics();
        format!(
            "Aud out:{:.0}ms inflight:{} srcCache:{}/{}MiB win:{} missMax:{}ms evict:{} fail:{}",
            buffered_ms,
            snapshot.in_flight,
            source_cache.reserved_bytes / (1024 * 1024),
            source_cache.byte_budget / (1024 * 1024),
            source_cache.entries,
            source_cache.decode_max_duration_us / 1_000,
            source_cache.evictions,
            source_cache.decode_failures,
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

    /// Advance playback using the active clock source.
    pub fn advance_playback_clock(&mut self, elapsed: Duration) -> PlaybackAdvance {
        let previous_frame = self.current_frame().max(0);
        if !self.is_playing() {
            return PlaybackAdvance {
                previous_frame,
                current_frame: previous_frame,
                frames_advanced: 0,
                status: PlaybackAdvanceStatus::Idle,
            };
        }
        self.playback_now = MonotonicTimestamp::from_duration(
            self.playback_now.duration_since_origin().saturating_add(elapsed),
        );
        self.reanchor_playback_presentation_clock(Instant::now());
        let snapshot = match self.playback_engine.tick(self.playback_now) {
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
        self.capture_playback_evidence();
        let target_frame = snapshot.position.frame;
        if snapshot.state == TransportState::Ended {
            self.settle_preview_access_source();
            self.audio_playback.reprime(snapshot.position);
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

    /// Estimate how long the UI loop can wait before polling playback again.
    pub fn playback_next_frame_delay(&self) -> Option<Duration> {
        if !self.is_playing() {
            return None;
        }
        self.playback_engine
            .time_until_next_frame(self.playback_now)
            .ok()
            .flatten()
            .map(clamp_playback_wake_delay)
    }

    pub fn current_frame(&self) -> i64 {
        self.playback_engine.snapshot().position.frame
    }

    pub fn current_timeline_time(&self) -> mondrian_core::Result<Option<TimelineTime>> {
        let Some(sequence) = self.sequence.as_ref() else {
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
        available_media_frames: usize,
    ) -> bool {
        let before = self.playback_engine.snapshot();
        let observation = VideoPrerollObservation {
            epoch: before.epoch,
            ready_media_frames,
            available_media_frames,
        };
        let changed = match self.playback_engine.observe_video_preroll(observation) {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(%error, ?observation, "rejected video preroll observation");
                false
            }
        };
        if changed {
            self.reanchor_playback_presentation_clock(Instant::now());
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

    /// Runtime-only Viewer scale selected by the Playback Quality Policy.
    pub fn playback_preview_resolution_scale(&self) -> PreviewResolutionScale {
        if self.is_playing() {
            self.playback_engine.snapshot().preview_scale
        } else {
            PreviewResolutionScale::Full
        }
    }

    /// Project the current Frame Demand deadline into the production wall-clock
    /// domain at the exact sampling instant used by a Preview Adapter.
    pub fn playback_frame_deadline_at(&self, sampled_at: Instant) -> Option<Instant> {
        let demand = self.playback_engine.pending_frame_demand()?;
        let deadline = demand.deadline?;
        let sampled_timestamp = self
            .playback_presentation_timestamp_at(sampled_at)
            .max(self.playback_evidence_now);
        let remaining = deadline
            .duration_since_origin()
            .saturating_sub(sampled_timestamp.duration_since_origin());
        sampled_at.checked_add(remaining)
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

    /// Complete a presentation against the demand's final deadline using the
    /// wall-clock/Playback timestamp mapping owned by the app Adapter.
    pub fn complete_frame_presentation(
        &mut self,
        ticket: FramePresentationTicket,
        completed_at: Instant,
    ) -> bool {
        let completion_timestamp = self
            .playback_presentation_timestamp_at(completed_at)
            .max(self.playback_evidence_now);
        let delivery = ticket.complete_at(completion_timestamp);
        self.observe_frame_delivery_at_wall(delivery, completed_at)
    }

    /// Feed an exact terminal preview observation into the Playback Session.
    pub fn observe_frame_delivery(&mut self, delivery: FrameDelivery) -> bool {
        self.observe_frame_delivery_at_wall(delivery, Instant::now())
    }

    fn observe_frame_delivery_at_wall(
        &mut self,
        delivery: FrameDelivery,
        observed_at: Instant,
    ) -> bool {
        let observed_timestamp = self
            .playback_presentation_timestamp_at(observed_at)
            .max(self.playback_evidence_now);
        let before = self.playback_engine.snapshot();
        let accepted = match self.playback_engine.observe_frame_delivery(delivery) {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(%error, ?delivery, "rejected Viewer Frame Delivery");
                false
            }
        };
        let after = self.playback_engine.snapshot();
        if let Err(error) =
            self.playback_evidence
                .observe_delivery(observed_timestamp, after, delivery, accepted)
        {
            tracing::warn!(%error, "rejected Playback Evidence Frame Delivery");
        }
        self.playback_evidence_now = self.playback_evidence_now.max(observed_timestamp);
        self.capture_playback_evidence_at(observed_timestamp);
        self.reanchor_playback_presentation_clock_at(observed_at, observed_timestamp);
        accepted && after != before
    }

    fn reanchor_playback_presentation_clock(&mut self, wall_time: Instant) {
        self.reanchor_playback_presentation_clock_at(wall_time, self.playback_now);
    }

    fn reanchor_playback_presentation_clock_at(
        &mut self,
        wall_time: Instant,
        timestamp: MonotonicTimestamp,
    ) {
        self.playback_presentation_wall_anchor = wall_time;
        self.playback_presentation_time_anchor = timestamp;
    }

    fn playback_presentation_timestamp_at(&self, wall_time: Instant) -> MonotonicTimestamp {
        self.playback_presentation_time_anchor.saturating_add(
            wall_time.saturating_duration_since(self.playback_presentation_wall_anchor),
        )
    }

    /// Feed one terminal Viewer observation into the authoritative Playback Session.
    pub fn observe_viewer_frame_delivery(&mut self, kind: FrameDeliveryKind) -> bool {
        let before = self.playback_engine.snapshot();
        let Some(demand) = self.playback_engine.frame_demand() else {
            return false;
        };
        let delivery = FrameDelivery::for_demand(demand.identity(), kind);
        let changed = self.observe_frame_delivery(delivery);
        changed || self.playback_engine.snapshot() != before
    }

    fn playback_timeline_binding(
        &self,
        end_frame: i64,
    ) -> Result<mondrian_playback::PlaybackTimelineBinding, mondrian_playback::PlaybackError> {
        mondrian_playback::PlaybackTimelineBinding::new(
            self.sequence.as_ref().map(|sequence| sequence.id),
            self.project_document_revision,
            self.playback_time_base(),
            end_frame,
        )
    }

    pub fn in_point_frame(&self) -> mondrian_core::Result<i64> {
        let Some(sequence) = self.sequence.as_ref() else {
            return Ok(0);
        };
        Ok(sequence
            .in_point()
            .to_frame_position(sequence.settings.frame_rate, FrameRounding::Floor)?
            .frame)
    }

    pub fn out_point_frame(&self) -> mondrian_core::Result<Option<i64>> {
        let Some(sequence) = self.sequence.as_ref() else {
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

fn sequence_has_audible_audio(sequence: &Sequence) -> bool {
    sequence.audio_tracks.iter().any(|track| {
        !track.is_muted
            && track.clips.iter().any(|clip| {
                !clip.is_disabled && clip.audio_components.iter().any(|edit| edit.enabled)
            })
    })
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

fn clamp_playback_wake_delay(delay: Duration) -> Duration {
    delay.clamp(MIN_PLAYBACK_WAKE_DELAY, MAX_PLAYBACK_WAKE_DELAY)
}

fn duration_sample_frames(duration: Duration, sample_rate: u32) -> u64 {
    duration
        .as_nanos()
        .saturating_mul(sample_rate as u128)
        .checked_div(1_000_000_000)
        .unwrap_or(u128::MAX)
        .min(u64::MAX as u128) as u64
}

fn audio_device_clock_observation(
    snapshot: RealtimeAudioOutputSnapshot,
    epoch: mondrian_playback::PlaybackEpoch,
    observed_at: MonotonicTimestamp,
    already_audio_master: bool,
    media_anchor: Option<FramePosition>,
    activation_preroll_satisfied: bool,
) -> AudioDeviceClockObservation {
    let callback_age = snapshot.last_callback_age.unwrap_or(Duration::MAX);
    let callback_period_frames = u64::from(snapshot.last_callback_frames);
    let callback_age_frames = duration_sample_frames(callback_age, snapshot.sample_rate);
    let callback_fresh = callback_age <= AUDIO_CALLBACK_STALE_AFTER
        && callback_age_frames <= callback_period_frames.saturating_mul(3);
    let callback_position_plausible = snapshot.active_duration.is_some_and(|active_duration| {
        let maximum_consumed_frames = duration_sample_frames(
            active_duration.saturating_add(callback_age.min(AUDIO_CALLBACK_STALE_AFTER)),
            snapshot.sample_rate,
        )
        .saturating_add(u64::from(snapshot.last_callback_frames));
        snapshot.active_callback_consumed_frames <= maximum_consumed_frames
    });
    let usable = snapshot.active
        && !snapshot.stream_failed
        && snapshot.active_callback_consumed_frames > 0
        && callback_fresh
        && callback_position_plausible
        && media_anchor.is_some()
        && (already_audio_master || activation_preroll_satisfied);
    let playback_delay = snapshot.last_callback_playback_delay.unwrap_or_else(|| {
        Duration::from_nanos(
            callback_period_frames
                .saturating_mul(1_000_000_000)
                .checked_div(u64::from(snapshot.sample_rate.max(1)))
                .unwrap_or(u64::MAX),
        )
    });
    let remaining_playback_delay = playback_delay.saturating_sub(callback_age);
    let estimated_latency_frames =
        duration_sample_frames(remaining_playback_delay, snapshot.sample_rate).min(u32::MAX as u64)
            as u32;
    let uncertainty_frames =
        callback_age_frames.max(callback_period_frames).min(u32::MAX as u64) as u32;
    AudioDeviceClockObservation {
        epoch,
        stream_generation: snapshot.stream_generation,
        sample_rate: snapshot.sample_rate,
        consumed_frames: snapshot.active_callback_consumed_frames,
        media_anchor: media_anchor.unwrap_or_else(|| {
            FramePosition::new(0, Rational::new(1, i64::from(snapshot.sample_rate.max(1))))
        }),
        observed_at,
        grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
        estimated_latency_frames,
        uncertainty_frames,
        underrun_frames: snapshot.underrun_frames,
        state: if usable {
            AudioDeviceClockState::Running
        } else {
            AudioDeviceClockState::Unavailable
        },
    }
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
        state.active_sequence_id = Some(sequence.id);
        state.default_sequence_id = Some(sequence.id);
        state.sequences = vec![sequence.clone()];
        state.sequence = Some(sequence);
        state
    }

    fn play_ready(state: &mut AppState) {
        state.play();
        assert!(!state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready));
        assert!(state.observe_video_preroll(0, 0));
    }

    #[test]
    fn play_binds_timeline_and_starts_one_epoch() {
        let mut state = state_with_sequence(20);
        let before = state.playback_engine.snapshot().epoch;

        state.play();

        let after = state.playback_engine.snapshot();
        assert_eq!(after.epoch.get(), before.get() + 1);
        assert_eq!(after.state, TransportState::Priming);
        assert!(state.pending_playback_frame_demand_identity().is_some());
    }

    fn audio_snapshot() -> RealtimeAudioOutputSnapshot {
        RealtimeAudioOutputSnapshot {
            stream_generation: 3,
            sample_rate: 48_000,
            channels: 2,
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

    #[test]
    fn audio_adapter_requires_fresh_callback_and_preroll_before_handoff() {
        let state = state_with_sequence(20);
        let epoch = state.playback_engine.snapshot().epoch;

        let ready = audio_device_clock_observation(
            audio_snapshot(),
            epoch,
            MonotonicTimestamp::ZERO,
            false,
            Some(FramePosition::new(0, Rational::new(1, 25))),
            true,
        );
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
                Some(FramePosition::new(0, Rational::new(1, 25))),
                true,
            )
            .state,
            AudioDeviceClockState::Unavailable
        );

        let mut unprimed = audio_snapshot();
        unprimed.buffered_frames = 5_759;
        assert_eq!(
            audio_device_clock_observation(
                unprimed,
                epoch,
                MonotonicTimestamp::ZERO,
                false,
                Some(FramePosition::new(0, Rational::new(1, 25))),
                false,
            )
            .state,
            AudioDeviceClockState::Unavailable
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
                Some(FramePosition::new(0, Rational::new(1, 25))),
                true,
            )
            .state,
            AudioDeviceClockState::Unavailable
        );
    }

    #[test]
    fn underrun_recovery_consumes_final_device_position_before_synthetic_handoff() {
        let mut state = state_with_sequence(20);
        play_ready(&mut state);
        let epoch = state.playback_engine.snapshot().epoch;
        let initial = audio_device_clock_observation(
            audio_snapshot(),
            epoch,
            state.playback_now,
            false,
            Some(FramePosition::new(0, Rational::new(1, 48_000))),
            true,
        );
        state.playback_engine.observe_audio_device_clock(initial).expect("audio master");
        state.advance_playback_clock(Duration::from_millis(30));
        assert_eq!(state.current_frame(), 0);

        let mut final_output = audio_snapshot();
        final_output.callback_consumed_frames = 2_880;
        final_output.active_callback_consumed_frames = 2_400;
        final_output.active_duration = Some(Duration::from_millis(50));
        final_output.callback_count = 6;
        final_output.underrun_frames = 960;
        state.observe_final_audio_clock_before_recovery(
            final_output,
            FramePosition::new(0, Rational::new(1, 48_000)),
        );
        assert_eq!(state.current_frame(), 1);

        state
            .playback_engine
            .audio_device_lost(state.playback_now)
            .expect("synthetic handoff");
        state.advance_playback_clock(Duration::from_millis(40));
        assert_eq!(state.current_frame(), 2);
        assert_eq!(state.playback_clock_master(), Some(ClockMaster::Synthetic));
    }

    #[test]
    fn production_adapter_reports_warm_seek_and_delivery_latency() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        state.seek_with_source(10, TimelineSeekSource::PointerDrag);
        state.advance_playback_clock(Duration::from_millis(120));
        assert!(!state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready));
        assert!(state.observe_video_preroll(0, 0));

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

        state.seek_with_source(10, TimelineSeekSource::PointerDrag);
        let ticket = state
            .playback_frame_presentation_ticket(FramePresentationQuality::Ready)
            .expect("paused seek presentation ticket");
        let completed_at = state.playback_presentation_wall_anchor + Duration::from_millis(120);
        assert!(!state.complete_frame_presentation(ticket, completed_at));

        let report = state.playback_evidence_report();
        assert_eq!(report.deliveries.ready, 1);
        assert_eq!(report.warm_seek_latency.count, 1);
        assert_eq!(report.warm_seek_latency.p95_us, 120_000);

        state.play();
        assert_eq!(state.playback_now, state.playback_evidence_now);
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
        state.observe_viewer_frame_delivery(FrameDeliveryKind::Late);

        let advanced = state.advance_playback_clock(Duration::from_millis(40));

        assert_eq!(advanced.status, PlaybackAdvanceStatus::Advanced);
        assert_eq!(advanced.current_frame, 2);
        assert!(state.is_playing());
    }

    #[test]
    fn priming_grants_audio_preroll_before_consumption() {
        let mut state = state_with_sequence(20);
        state.play();

        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Priming
        );
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Preroll);

        state.advance_playback_clock(Duration::from_millis(499));
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

        state.pause();
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Idle);
    }

    #[test]
    fn sustained_late_deliveries_expose_lower_runtime_preview_scale_to_adapters() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);

        for _ in 0..12 {
            state.advance_playback_clock(Duration::from_millis(40));
            state.observe_viewer_frame_delivery(FrameDeliveryKind::Late);
        }

        assert_eq!(
            state.playback_preview_resolution_scale(),
            PreviewResolutionScale::Half
        );
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Recovering
        );
        assert_eq!(state.audio_playback_mode(), AudioPlaybackMode::Consume);

        state.pause();
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
        let completed_at = state.playback_presentation_wall_anchor + Duration::from_millis(41);

        state.complete_frame_presentation(ticket, completed_at);

        let report = state.playback_evidence_report();
        assert_eq!(report.deliveries.ready, 1);
        assert_eq!(report.deliveries.late, 1);
        assert_eq!(report.demand_latency.count, 2);
        assert!(report.demand_latency.p95_us >= 41_000);
    }

    #[test]
    fn worker_deadline_projection_does_not_regrant_time_spent_before_enqueue() {
        let mut state = state_with_sequence(40);
        play_ready(&mut state);
        state.advance_playback_clock(Duration::from_millis(40));
        let demand_anchor = state.playback_presentation_wall_anchor;
        let sampled_at = demand_anchor + Duration::from_millis(10);

        let deadline_at =
            state.playback_frame_deadline_at(sampled_at).expect("projected worker deadline");

        assert_eq!(deadline_at, demand_anchor + Duration::from_millis(40));
        assert_eq!(
            deadline_at.duration_since(sampled_at),
            Duration::from_millis(30)
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

        state.seek(10);
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
        state.play();
        let old_identity =
            state.pending_playback_frame_demand_identity().expect("priming demand identity");

        state.seek(10);
        let before = state.playback_engine.snapshot();
        let accepted = state.observe_frame_delivery(FrameDelivery::for_demand(
            old_identity,
            FrameDeliveryKind::Ready,
        ));

        assert!(!accepted);
        assert_eq!(state.playback_engine.snapshot(), before);
        assert_eq!(before.state, TransportState::Priming);
    }

    #[test]
    fn seek_records_preview_access_source() {
        let mut state = state_with_sequence(30);

        state.seek_with_source(12, TimelineSeekSource::PointerDrag);
        assert_eq!(state.current_frame(), 12);
        assert_eq!(
            state.last_timeline_seek_source,
            TimelineSeekSource::PointerDrag
        );

        state.seek(18);
        assert_eq!(state.current_frame(), 18);
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);
    }

    #[test]
    fn pause_stop_and_reached_end_settle_preview_access_source() {
        let mut state = state_with_sequence(5);

        state.seek_with_source(2, TimelineSeekSource::PointerDrag);
        state.play();
        state.pause();
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);

        state.seek_with_source(3, TimelineSeekSource::PointerDrag);
        state.stop();
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);

        state.seek_with_source(4, TimelineSeekSource::PointerDrag);
        state.play();
        let outcome = state.advance_playback_clock(Duration::from_secs(1));
        assert_eq!(outcome.status, PlaybackAdvanceStatus::ReachedEnd);
        assert_eq!(state.last_timeline_seek_source, TimelineSeekSource::Settled);
    }

    #[test]
    fn playback_next_frame_delay_is_bounded_while_playing() {
        let mut state = state_with_sequence(30);
        play_ready(&mut state);

        let delay = state.playback_next_frame_delay().expect("next frame delay");

        assert!(delay >= MIN_PLAYBACK_WAKE_DELAY);
        assert!(delay <= MAX_PLAYBACK_WAKE_DELAY);
    }
}
