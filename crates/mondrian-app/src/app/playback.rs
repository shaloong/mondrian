use super::*;

const MIN_PLAYBACK_WAKE_DELAY: Duration = Duration::from_millis(1);
const MAX_PLAYBACK_WAKE_DELAY: Duration = Duration::from_millis(100);

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
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
        }
    }

    pub fn pause(&mut self) {
        let frames = self.current_frame();
        self.settle_preview_access_source();
        if let Err(error) = self.playback_engine.pause(self.playback_now) {
            tracing::error!(%error, "failed to pause Playback Session");
        }
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
    }

    pub fn stop(&mut self) {
        self.settle_preview_access_source();
        if let Err(error) = self.playback_engine.stop(self.playback_now) {
            tracing::error!(%error, "failed to stop Playback Session");
        }
        self.reset_audio_render_pipeline(0.0);
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
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
        self.sync_audio_clock_to_frame(frame);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
    }

    pub fn set_playback_frame_running(&mut self, frame: i64) {
        self.seek(frame.max(0));
        self.play();
    }

    pub fn pump_audio_output(&mut self) {
        let Some(output) = self.audio_output.as_ref() else {
            return;
        };

        while let Ok(done) = self.audio_render_rx.try_recv() {
            self.audio_render_in_flight = self.audio_render_in_flight.saturating_sub(1);
            if done.generation != self.audio_render_generation {
                continue;
            }
            match done.chunk {
                Ok(chunk) => output.enqueue(&chunk),
                Err(err) => tracing::debug!("音频后台渲染块失败: {}", err),
            }
        }

        if !self.is_playing() {
            output.set_muted(false);
            output.clear();
            if audio_idle_warmup_enabled() {
                self.warm_audio_cache_when_idle();
            }
            return;
        }

        self.audio_idle_warmup_last = None;

        let Some(seq) = self.sequence.as_ref() else {
            return;
        };
        let Some(library) = self.asset_library.as_ref() else {
            return;
        };

        let sample_rate_f64 = self.audio_sample_rate as f64;
        let chunk_frames = ((self.audio_chunk_secs * sample_rate_f64).round() as usize).max(1);
        let target_high_secs = audio_buffer_target_high_secs_playing();
        let max_in_flight = audio_render_max_in_flight_playing();
        let target_high_frames = (sample_rate_f64 * target_high_secs).round() as usize;

        while output.buffered_frames() + self.audio_render_in_flight * chunk_frames
            < target_high_frames
        {
            if self.audio_render_in_flight >= max_in_flight {
                break;
            }

            let request = AudioRenderRequest {
                generation: self.audio_render_generation,
                window_start_secs: self.audio_render_next_start_secs.max(0.0),
                duration_secs: self.audio_chunk_secs,
                sequence: seq.clone(),
                library: Arc::clone(library),
            };

            if self.audio_render_tx.send(request).is_err() {
                break;
            }

            self.audio_render_in_flight += 1;
            self.audio_render_next_start_secs += self.audio_chunk_secs;
        }
    }

    pub(super) fn reset_audio_render_pipeline(&mut self, anchor_secs: f64) {
        self.audio_render_generation = self.audio_render_generation.saturating_add(1);
        self.audio_render_in_flight = 0;
        self.audio_render_next_start_secs = anchor_secs.max(0.0);
        while self.audio_render_rx.try_recv().is_ok() {}
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
        let chunk = self.audio_chunk_secs.max(0.08);

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
            self.audio_mixer.output_channels,
            window_start_secs,
            duration_secs,
        )
    }

    pub fn audio_developer_metrics_summary(&self) -> String {
        let buffered_frames = self.audio_output.as_ref().map(|o| o.buffered_frames()).unwrap_or(0);
        let buffered_ms = buffered_frames as f64 / self.audio_sample_rate as f64 * 1000.0;
        let source_cache_entries = self.audio_source_cache.cache_entry_count();
        format!(
            "Aud out:{:.0}ms inflight:{} srcCache:{}",
            buffered_ms, self.audio_render_in_flight, source_cache_entries
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
        let target_frame = snapshot.position.frame;
        if snapshot.state == TransportState::Ended {
            self.settle_preview_access_source();
            self.sync_audio_clock_to_frame(target_frame);
            self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
            if let Some(output) = &self.audio_output {
                output.set_muted(false);
                output.clear();
            }
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
        match self.playback_engine.observe_frame_delivery(delivery) {
            Ok(true) => self.playback_engine.snapshot() != before,
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(%error, ?delivery, "rejected Viewer Frame Delivery");
                false
            }
        }
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
            Ok(_) => true,
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

fn clamp_playback_wake_delay(delay: Duration) -> Duration {
    delay.clamp(MIN_PLAYBACK_WAKE_DELAY, MAX_PLAYBACK_WAKE_DELAY)
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
