use super::*;

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

        self.playback = PlaybackState::Playing { timecode_frames: frames };
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
        }
    }

    pub fn pause(&mut self) {
        let frames = self.current_frame();
        self.playback_buffering = false;
        self.playback = PlaybackState::Paused { timecode_frames: frames };
        self.sync_audio_clock_to_frame(frames);
        self.reset_audio_render_pipeline(self.audio_clock.now_seconds().max(0.0));
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
    }

    pub fn stop(&mut self) {
        self.playback = PlaybackState::Stopped;
        self.playback_reached_end = false;
        self.playback_buffering = false;
        self.av_drift_ms = 0.0;
        self.reset_audio_render_pipeline(0.0);
        if let Some(output) = &self.audio_output {
            output.set_muted(false);
            output.clear();
        }
    }

    pub fn seek(&mut self, frame: i64) {
        // 任何手动跳帧操作都清除「自然到达终点」标志，
        // 这样下一次 play() 不会误跳回 in_point。
        self.playback_reached_end = false;
        self.playback_buffering = false;
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
        self.playback = PlaybackState::Playing { timecode_frames: frame.max(0) };
    }

    pub fn set_playback_buffering(&mut self, buffering: bool) {
        self.playback_buffering = buffering;
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
        self.project_in_point.unwrap_or(0).max(0)
    }

    pub fn out_point_frame(&self) -> Option<i64> {
        self.project_out_point.map(|f| f.max(0)).filter(|&f| f >= self.in_point_frame())
    }
}
