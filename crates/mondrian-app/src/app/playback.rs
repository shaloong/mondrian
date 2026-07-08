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

        if matches!(self.playback, PlaybackState::Stopped) {
            frames = 0;
        }

        // 回到起点的条件：
        // 1. 播放自然到达终点后再次按 Play（playback_reached_end 标志）
        // 2. 当前帧严格超过有效内容结束帧（用户 seek 到内容之外）
        // 注意：frames == end_frame 时不再自动跳回开头——允许从最后一帧开始播放，
        // advance_playback_clock 会在推进后正常停在该帧。
        // 入/出点不影响播放逻辑，仅影响导出。
        if self.playback_reached_end || (end_frame >= 0 && frames > end_frame) {
            frames = 0;
        }
        self.playback_reached_end = false;
        self.playback_buffering = false;
        self.playback_frame_accumulator = 0.0;

        self.playback = PlaybackState::Playing { timecode_frames: frames };
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
        }
    }

    pub fn pause(&mut self) {
        let frames = self.current_frame();
        self.settle_preview_access_source();
        self.playback_buffering = false;
        self.playback_frame_accumulator = 0.0;
        self.playback = PlaybackState::Paused { timecode_frames: frames };
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
    }

    pub fn stop(&mut self) {
        self.settle_preview_access_source();
        self.playback = PlaybackState::Stopped;
        self.playback_reached_end = false;
        self.playback_buffering = false;
        self.playback_frame_accumulator = 0.0;
        self.av_drift_ms = 0.0;
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
        // 任何手动跳帧操作都清除「自然到达终点」标志，
        // 这样下一次 play() 不会误跳回 in_point。
        self.last_timeline_seek_source = source;
        self.playback_reached_end = false;
        self.playback_buffering = false;
        self.playback_frame_accumulator = 0.0;
        self.playback = match &self.playback {
            PlaybackState::Playing { .. } => PlaybackState::Playing { timecode_frames: frame },
            _ => PlaybackState::Paused { timecode_frames: frame },
        };
        self.sync_audio_clock_to_frame(frame);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
    }

    pub fn set_playback_frame_running(&mut self, frame: i64) {
        self.playback_frame_accumulator = 0.0;
        self.playback = PlaybackState::Playing { timecode_frames: frame.max(0) };
    }

    pub fn set_playback_buffering(&mut self, buffering: bool) {
        if self.playback_buffering == buffering {
            return;
        }
        self.playback_buffering = buffering;
        if buffering {
            if let Some(output) = &self.audio_output {
                output.set_muted(true);
                output.clear();
            }
        } else {
            let frame = self.current_frame();
            self.sync_audio_clock_to_frame(frame);
            self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
            if let Some(output) = &self.audio_output {
                output.set_muted(false);
            }
        }
    }

    pub fn is_playback_buffering(&self) -> bool {
        self.playback_buffering
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
        let (target_high_secs, max_in_flight) = if self.playback_buffering {
            (
                audio_buffer_target_high_secs_buffering(),
                audio_render_max_in_flight_buffering(),
            )
        } else {
            (
                audio_buffer_target_high_secs_playing(),
                audio_render_max_in_flight_playing(),
            )
        };
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

    pub fn update_av_sync(&mut self) -> f64 {
        if !self.is_playing() {
            self.av_drift_ms = 0.0;
            return 1.0;
        }

        let video_secs = self.current_frame().max(0) as f64 / self.fps();
        let audio_secs = self.audio_clock.now_seconds();
        let correction =
            self.audio_sync
                .compute_correction(video_secs, audio_secs, self.audio_sample_rate);
        self.av_drift_ms = correction.drift_seconds * 1000.0;

        correction.playback_rate
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
        if self.playback_buffering {
            return PlaybackAdvance {
                previous_frame,
                current_frame: previous_frame,
                frames_advanced: 0,
                status: PlaybackAdvanceStatus::WaitingForFrame,
            };
        }

        let fps = self.fps();
        let target_frame = match self.audio_sync.role {
            ClockRole::AudioMaster => {
                let audio_frame = (self.audio_clock.now_seconds().max(0.0) * fps).floor() as i64;
                audio_frame.max(previous_frame)
            }
            ClockRole::VideoMaster => {
                let playback_rate = self.update_av_sync();
                let elapsed_frames = elapsed.as_secs_f64() * fps * playback_rate;
                if !elapsed_frames.is_finite() || elapsed_frames <= 0.0 {
                    return PlaybackAdvance {
                        previous_frame,
                        current_frame: previous_frame,
                        frames_advanced: 0,
                        status: PlaybackAdvanceStatus::WaitingForFrame,
                    };
                }
                self.playback_frame_accumulator += elapsed_frames;
                let whole_frames = self.playback_frame_accumulator.floor() as i64;
                if whole_frames <= 0 {
                    return PlaybackAdvance {
                        previous_frame,
                        current_frame: previous_frame,
                        frames_advanced: 0,
                        status: PlaybackAdvanceStatus::WaitingForFrame,
                    };
                }
                self.playback_frame_accumulator -= whole_frames as f64;
                previous_frame.saturating_add(whole_frames)
            }
        };

        if target_frame <= previous_frame {
            return PlaybackAdvance {
                previous_frame,
                current_frame: previous_frame,
                frames_advanced: 0,
                status: PlaybackAdvanceStatus::WaitingForFrame,
            };
        }

        let end_frame = self.last_content_frame().max(0);
        if target_frame >= end_frame {
            self.settle_preview_access_source();
            self.playback = PlaybackState::Paused { timecode_frames: end_frame };
            self.playback_reached_end = true;
            self.playback_buffering = false;
            self.playback_frame_accumulator = 0.0;
            self.sync_audio_clock_to_frame(end_frame);
            self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
            if let Some(output) = &self.audio_output {
                output.set_muted(false);
                output.clear();
            }
            return PlaybackAdvance {
                previous_frame,
                current_frame: end_frame,
                frames_advanced: (end_frame - previous_frame).max(0),
                status: PlaybackAdvanceStatus::ReachedEnd,
            };
        }

        self.playback = PlaybackState::Playing { timecode_frames: target_frame };
        self.playback_reached_end = false;
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

        let fps = self.fps();
        let secs = match self.audio_sync.role {
            ClockRole::AudioMaster => {
                let next_frame = self.current_frame().saturating_add(1).max(0) as f64;
                (next_frame / fps - self.audio_clock.now_seconds()).max(0.0)
            }
            ClockRole::VideoMaster => ((1.0 - self.playback_frame_accumulator).max(0.0)) / fps,
        };
        Some(clamp_playback_wake_delay(Duration::from_secs_f64(
            secs.max(0.0),
        )))
    }

    pub fn current_frame(&self) -> i64 {
        match &self.playback {
            PlaybackState::Stopped => 0,
            PlaybackState::Playing { timecode_frames } => *timecode_frames,
            PlaybackState::Paused { timecode_frames } => *timecode_frames,
        }
    }

    pub fn current_time_code(&self) -> Option<TimeCode> {
        let seq = self.sequence.as_ref()?;
        Some(TimeCode::new(self.current_frame().max(0), seq.time_base()))
    }

    pub fn is_playing(&self) -> bool {
        matches!(self.playback, PlaybackState::Playing { .. })
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
        state.audio_sync.role = ClockRole::VideoMaster;
        state
    }

    #[test]
    fn advance_playback_clock_accumulates_subframe_ticks() {
        let mut state = state_with_sequence(20);
        state.play();

        let waiting = state.advance_playback_clock(Duration::from_millis(10));
        assert_eq!(waiting.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(state.current_frame(), 0);

        for _ in 0..3 {
            state.advance_playback_clock(Duration::from_millis(10));
        }

        assert_eq!(state.current_frame(), 1);
        assert!(state.is_playing());
        assert!(!state.playback_reached_end);
    }

    #[test]
    fn advance_playback_clock_waits_while_preview_is_buffering() {
        let mut state = state_with_sequence(20);
        state.play();
        state.set_playback_buffering(true);

        let waiting = state.advance_playback_clock(Duration::from_secs(1));

        assert_eq!(waiting.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(waiting.current_frame, 0);
        assert_eq!(waiting.frames_advanced, 0);
        assert_eq!(state.current_frame(), 0);
        assert!(state.is_playing());
        assert!(state.is_playback_buffering());

        state.set_playback_buffering(false);
        let advanced = state.advance_playback_clock(Duration::from_millis(40));

        assert_eq!(advanced.status, PlaybackAdvanceStatus::Advanced);
        assert_eq!(advanced.current_frame, 1);
        assert!(!state.is_playback_buffering());
    }

    #[test]
    fn advance_playback_clock_reaches_end_and_pauses() {
        let mut state = state_with_sequence(5);
        state.play();

        let outcome = state.advance_playback_clock(Duration::from_secs(1));

        assert_eq!(outcome.status, PlaybackAdvanceStatus::ReachedEnd);
        assert_eq!(outcome.current_frame, 4);
        assert_eq!(state.current_frame(), 4);
        assert!(!state.is_playing());
        assert!(state.playback_reached_end);
    }

    #[test]
    fn play_after_reaching_end_restarts_from_zero() {
        let mut state = state_with_sequence(5);
        state.play();
        state.advance_playback_clock(Duration::from_secs(1));

        state.play();

        assert_eq!(state.current_frame(), 0);
        assert!(state.is_playing());
        assert!(!state.playback_reached_end);
    }

    #[test]
    fn seek_clears_reached_end_and_frame_accumulator() {
        let mut state = state_with_sequence(30);
        state.play();
        state.advance_playback_clock(Duration::from_millis(20));
        state.playback_reached_end = true;

        state.seek(10);
        let outcome = state.advance_playback_clock(Duration::from_millis(20));

        assert_eq!(outcome.status, PlaybackAdvanceStatus::WaitingForFrame);
        assert_eq!(state.current_frame(), 10);
        assert!(!state.playback_reached_end);
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
        state.play();

        let delay = state.playback_next_frame_delay().expect("next frame delay");

        assert!(delay >= MIN_PLAYBACK_WAKE_DELAY);
        assert!(delay <= MAX_PLAYBACK_WAKE_DELAY);
    }
}
