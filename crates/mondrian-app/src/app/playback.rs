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
        let snapshot = self.playback_engine.snapshot();
        let demand = self.playback_engine.frame_demand();
        if let Err(error) =
            self.playback_evidence.observe_snapshot(self.playback_now, snapshot, demand)
        {
            tracing::warn!(%error, "rejected Playback Evidence snapshot");
        }
    }

    /// Return a stable bounded report for diagnostics and headless/perf Adapters.
    pub fn playback_evidence_report(&self) -> PlaybackEvidenceReport {
        self.playback_evidence.report()
    }

    pub fn play(&mut self) {
        let end_frame = self.last_content_frame();
        let mut frames = self.current_frame();
        let prior_state = self.playback_engine.snapshot().state;
        if prior_state == TransportState::Stopped {
            frames = 0;
        }
        if prior_state == TransportState::Ended || (end_frame >= 0 && frames > end_frame) {
            frames = 0;
        }
        if !self.reset_playback_timeline(frames, end_frame) {
            return;
        }
        if let Err(error) = self.playback_engine.play(end_frame, self.playback_now) {
            tracing::error!(%error, "failed to start Playback Session");
            return;
        }
        self.prepare_audio_playback(TimeCode::new(frames, self.playback_time_base()));
        self.capture_playback_evidence();
    }

    pub fn pause(&mut self) {
        let frames = self.current_frame();
        self.settle_preview_access_source();
        if let Err(error) = self.playback_engine.pause(self.playback_now) {
            tracing::error!(%error, "failed to pause Playback Session");
        }
        self.audio_playback.reprime(TimeCode::new(frames, self.playback_time_base()));
        self.capture_playback_evidence();
    }

    pub fn stop(&mut self) {
        self.settle_preview_access_source();
        if let Err(error) = self.playback_engine.stop(self.playback_now) {
            tracing::error!(%error, "failed to stop Playback Session");
        }
        self.audio_playback.reprime(TimeCode::new(0, self.playback_time_base()));
        self.capture_playback_evidence();
    }

    pub fn seek(&mut self, frame: i64) {
        self.seek_with_source(frame, TimelineSeekSource::Settled);
    }

    pub(crate) fn settle_preview_access_source(&mut self) {
        self.last_timeline_seek_source = TimelineSeekSource::Settled;
    }

    pub fn seek_with_source(&mut self, frame: i64, source: TimelineSeekSource) {
        let was_running = self.is_playing();
        self.last_timeline_seek_source = source;
        let end_frame = self.last_content_frame();
        if !self.reset_playback_timeline(frame, end_frame) {
            return;
        }
        let time_base =
            self.sequence.as_ref().map(Sequence::time_base).unwrap_or(Rational::new(1, 25));
        if let Err(error) =
            self.playback_engine.seek(TimeCode::new(frame, time_base), self.playback_now)
        {
            tracing::error!(%error, "failed to seek Playback Session");
            return;
        }
        if was_running {
            if let Err(error) = self.playback_engine.play(end_frame, self.playback_now) {
                tracing::error!(%error, "failed to resume Playback Session after seek");
                return;
            }
        }
        if was_running {
            self.prepare_audio_playback(TimeCode::new(frame, time_base));
        } else {
            self.audio_playback.reprime(TimeCode::new(frame, time_base));
        }
        let seek_kind = match source {
            TimelineSeekSource::PointerDrag => PlaybackSeekKind::Warm,
            TimelineSeekSource::Settled => PlaybackSeekKind::Accurate,
        };
        if let Err(error) = self.playback_evidence.begin_seek(
            self.playback_now,
            self.playback_engine.snapshot().epoch,
            seek_kind,
        ) {
            tracing::warn!(%error, "rejected Playback Evidence seek start");
        }
        self.capture_playback_evidence();
    }

    pub fn set_playback_frame_running(&mut self, frame: i64) {
        self.seek(frame.max(0));
        self.play();
    }

    pub fn pump_audio_output(&mut self) {
        let playing = self.is_playing();
        let position = self.playback_engine.snapshot().position;
        let poll = self.audio_playback.poll(playing, position);
        for event in poll.events {
            self.handle_audio_playback_event(event);
        }
        if !playing {
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
                    self.playback_now,
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
                    self.playback_now,
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
        media_anchor: TimeCode,
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
                    && engine_snapshot.clock_master != Some(ClockMaster::AudioDevice) =>
            {
                self.audio_playback.reprime(engine_snapshot.position);
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "rejected audio-device Clock Master observation"),
        }
        self.capture_playback_evidence();
    }

    fn prepare_audio_playback(&mut self, anchor: TimeCode) {
        let renderer = self
            .sequence
            .as_ref()
            .filter(|sequence| sequence_has_audible_audio(sequence))
            .zip(self.asset_library.as_ref())
            .map(|(sequence, library)| -> Arc<dyn AudioPcmRenderer> {
                Arc::new(TimelineAudioPcmRenderer::new(
                    sequence.clone(),
                    Arc::clone(library),
                    Arc::clone(&self.audio_source_cache),
                    self.audio_sample_rate,
                    AUDIO_OUTPUT_CHANNELS,
                ))
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

        let center_secs = self.current_frame().max(0) as f64 / self.fps();
        let chunk = AUDIO_IDLE_WARMUP_CHUNK_SECS;

        let _ = self.render_audio_chunk(seq, library.as_ref(), center_secs, chunk);
        let before = (center_secs - chunk).max(0.0);
        let _ = self.render_audio_chunk(seq, library.as_ref(), before, chunk);
        let _ = self.render_audio_chunk(seq, library.as_ref(), center_secs + chunk, chunk);

        self.audio_idle_warmup_last = Some(now);
    }

    fn render_audio_chunk(
        &self,
        seq: &Sequence,
        library: &AssetLibrary,
        window_start_secs: f64,
        duration_secs: f64,
    ) -> mondrian_core::Result<AudioBuffer> {
        render_audio_chunk_with_cache(
            seq,
            library,
            self.audio_source_cache.as_ref(),
            self.audio_sample_rate,
            AUDIO_OUTPUT_CHANNELS,
            window_start_secs,
            duration_secs,
        )
    }

    pub fn audio_developer_metrics_summary(&self) -> String {
        let snapshot = self.audio_playback.snapshot(self.is_playing());
        let buffered_frames = snapshot.output.map_or(0, |output| output.buffered_frames);
        let buffered_ms = buffered_frames as f64 / self.audio_sample_rate as f64 * 1000.0;
        let source_cache_entries = self.audio_source_cache.cache_entry_count();
        format!(
            "Aud out:{:.0}ms inflight:{} srcCache:{}",
            buffered_ms, snapshot.in_flight, source_cache_entries
        )
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

    pub fn current_time_code(&self) -> Option<TimeCode> {
        let seq = self.sequence.as_ref()?;
        Some(TimeCode::new(self.current_frame().max(0), seq.time_base()))
    }

    pub fn is_playing(&self) -> bool {
        matches!(
            self.playback_engine.snapshot().state,
            TransportState::Priming | TransportState::Playing | TransportState::Recovering
        )
    }

    /// Current authoritative Clock Master exposed to diagnostics/UI adapters.
    pub fn playback_clock_master(&self) -> Option<mondrian_playback::ClockMaster> {
        self.playback_engine.snapshot().clock_master
    }

    /// Remaining useful lifetime of the authoritative current Frame Demand.
    pub fn playback_frame_deadline_budget_us(&self) -> Option<u64> {
        let demand = self.playback_engine.frame_demand()?;
        let deadline = demand.deadline.duration_since_origin();
        let now = self.playback_now.duration_since_origin();
        let remaining = deadline.saturating_sub(now);
        Some(remaining.as_micros().min(u64::MAX as u128) as u64)
    }

    /// Identity preview adapters must return with terminal current-frame work.
    pub fn playback_frame_demand_identity(&self) -> Option<mondrian_playback::FrameDemandIdentity> {
        self.playback_engine.frame_demand().map(|demand| demand.identity())
    }

    /// Feed an exact terminal preview observation into the Playback Session.
    pub fn observe_frame_delivery(&mut self, delivery: FrameDelivery) -> bool {
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
                .observe_delivery(self.playback_now, after, delivery, accepted)
        {
            tracing::warn!(%error, "rejected Playback Evidence Frame Delivery");
        }
        self.capture_playback_evidence();
        accepted && after != before
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

    fn reset_playback_timeline(&mut self, frame: i64, end_frame: i64) -> bool {
        let time_base =
            self.sequence.as_ref().map(Sequence::time_base).unwrap_or(Rational::new(1, 25));
        let sequence_id = self.sequence.as_ref().map(|sequence| sequence.id);
        match self.playback_engine.reset_timeline(
            sequence_id,
            self.project_document_revision,
            TimeCode::new(frame, time_base),
            end_frame,
            self.playback_now,
        ) {
            Ok(_) => {
                self.capture_playback_evidence();
                true
            }
            Err(error) => {
                tracing::error!(%error, "failed to reset Playback Session timeline");
                false
            }
        }
    }

    pub fn in_point_frame(&self) -> i64 {
        self.sequence.as_ref().map(|sequence| sequence.in_point_frame()).unwrap_or(0)
    }

    pub fn out_point_frame(&self) -> Option<i64> {
        self.sequence.as_ref().and_then(|sequence| sequence.out_point_frame())
    }
}

fn sequence_has_audible_audio(sequence: &Sequence) -> bool {
    let has_solo = sequence.audio_tracks.iter().any(|track| track.is_solo && !track.is_muted);
    sequence.audio_tracks.iter().any(|track| {
        !track.is_muted
            && (!has_solo || track.is_solo)
            && track.clips.iter().any(|clip| !clip.is_disabled)
    })
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
    media_anchor: Option<TimeCode>,
    activation_preroll_satisfied: bool,
) -> AudioDeviceClockObservation {
    let callback_fresh =
        snapshot.last_callback_age.is_some_and(|age| age <= AUDIO_CALLBACK_STALE_AFTER);
    let usable = snapshot.active
        && !snapshot.stream_failed
        && snapshot.active_callback_consumed_frames > 0
        && callback_fresh
        && media_anchor.is_some()
        && (already_audio_master || activation_preroll_satisfied);
    let callback_age_frames = snapshot
        .last_callback_age
        .map(|age| duration_sample_frames(age, snapshot.sample_rate))
        .unwrap_or(u64::MAX);
    let uncertainty_frames = callback_age_frames
        .saturating_add(snapshot.last_callback_frames as u64)
        .min(u32::MAX as u64) as u32;
    AudioDeviceClockObservation {
        epoch,
        stream_generation: snapshot.stream_generation,
        sample_rate: snapshot.sample_rate,
        consumed_frames: snapshot.active_callback_consumed_frames,
        media_anchor: media_anchor.unwrap_or_else(|| {
            TimeCode::new(0, Rational::new(1, i64::from(snapshot.sample_rate.max(1))))
        }),
        observed_at,
        grade: AudioClockObservationGrade::CallbackConsumptionEstimate,
        estimated_latency_frames: snapshot.last_callback_frames,
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

    fn state_with_sequence(duration_frames: i64) -> AppState {
        let mut state = AppState::new();
        let mut sequence = Sequence::new("playback");
        let tb = sequence.time_base();
        let clip = Clip::new_solid_color(
            AssetId::new(),
            Color::from_rgba8(255, 0, 0, 255),
            TimeCode::new(0, tb),
            TimeCode::new(duration_frames, tb),
        );
        sequence.video_tracks[0].add_clip(clip).expect("add clip");
        state.active_sequence_id = Some(sequence.id);
        state.default_sequence_id = Some(sequence.id);
        state.sequences = vec![sequence.clone()];
        state.sequence = Some(sequence);
        state
    }

    fn play_ready(state: &mut AppState) {
        state.play();
        assert!(state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready));
    }

    fn audio_snapshot() -> RealtimeAudioOutputSnapshot {
        RealtimeAudioOutputSnapshot {
            stream_generation: 3,
            sample_rate: 48_000,
            channels: 2,
            callback_consumed_frames: 960,
            active_callback_consumed_frames: 480,
            callback_count: 2,
            underrun_frames: 0,
            last_callback_frames: 480,
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
            Some(TimeCode::new(0, Rational::new(1, 25))),
            true,
        );
        assert_eq!(ready.state, AudioDeviceClockState::Running);
        assert_eq!(
            ready.grade,
            AudioClockObservationGrade::CallbackConsumptionEstimate
        );

        let mut stale = audio_snapshot();
        stale.last_callback_age = Some(Duration::from_millis(101));
        assert_eq!(
            audio_device_clock_observation(
                stale,
                epoch,
                MonotonicTimestamp::ZERO,
                false,
                Some(TimeCode::new(0, Rational::new(1, 25))),
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
                Some(TimeCode::new(0, Rational::new(1, 25))),
                false,
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
            Some(TimeCode::new(0, Rational::new(1, 48_000))),
            true,
        );
        state.playback_engine.observe_audio_device_clock(initial).expect("audio master");

        let mut final_output = audio_snapshot();
        final_output.callback_consumed_frames = 2_880;
        final_output.active_callback_consumed_frames = 2_400;
        final_output.underrun_frames = 960;
        state.observe_final_audio_clock_before_recovery(
            final_output,
            TimeCode::new(0, Rational::new(1, 48_000)),
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
        assert!(state.observe_viewer_frame_delivery(FrameDeliveryKind::Ready));

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
        assert_ne!(state.playback_engine.snapshot().epoch, old_epoch);
    }

    #[test]
    fn completion_for_pre_seek_demand_cannot_mutate_new_session() {
        let mut state = state_with_sequence(30);
        state.play();
        let old_identity = state.playback_frame_demand_identity().expect("priming demand identity");

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
