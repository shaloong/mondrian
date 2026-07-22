//! Preview prefetch, preroll, adaptive-hint, and Broker-admission Adapter.

use super::*;
use crate::app::preview_timeline_execution::collect_preview_timeline_media_demands;

const MEDIA_PREVIEW_COLD_ACTIVATION_LOOKAHEAD_US: u64 = 2_000_000;
const MEDIA_PREVIEW_COLD_ACTIVATION_MAX_FRAMES: i64 = 120;

impl<O: Clone> PreviewProductionRuntime<O> {
    pub(super) fn schedule_media_prefetches(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
    ) {
        if !state.is_playing() {
            return;
        }
        if self.execution.borrow().is_pending() {
            bump(&self.metrics.prefetch_skipped_current_pending);
            return;
        }
        if self.playback_sustained_pressure_active() {
            bump(&self.metrics.playback_prefetch_skipped_sustained_pressure);
            return;
        }
        let worker_queue = self.jobs.diagnostics();
        if worker_queue.queued_current_jobs > 0 || worker_queue.in_flight_current_jobs > 0 {
            bump(&self.metrics.prefetch_skipped_current_work);
            return;
        }
        let prefetch_pressure = worker_queue
            .queued_prefetch_jobs
            .saturating_add(worker_queue.in_flight_prefetch_jobs);
        let prefetch_window_frames =
            media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate);
        self.record_playback_forward_prefetch_window(prefetch_window_frames);
        let Some(prefetch_window_frames) = prefetch_window_frames else {
            return;
        };
        let prefetch_slots_available = prefetch_window_frames.saturating_sub(prefetch_pressure);
        if prefetch_slots_available == 0 {
            bump(&self.metrics.prefetch_skipped_prefetch_backlog);
            return;
        }
        let display_snapshot = self.display_snapshot.borrow();
        let Ok(display_color_space) = preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        ) else {
            return;
        };
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let preroll_deadline_at = state
            .is_playback_priming()
            .then(|| state.playback_frame_deadline_at(Instant::now()))
            .flatten();
        let mut remaining_prefetch_jobs = prefetch_slots_available;
        let mut steady_window_has_media = false;
        for offset in 1..=prefetch_window_frames as i64 {
            if remaining_prefetch_jobs == 0 {
                break;
            }
            steady_window_has_media |= self.schedule_media_prefetch_for_sequence(
                state,
                sequence,
                frame.saturating_add(offset),
                target_width,
                target_height,
                color_context.clone(),
                &mut remaining_prefetch_jobs,
                preroll_deadline_at,
            );
        }
        if remaining_prefetch_jobs > 0 && !steady_window_has_media {
            let activation_horizon =
                media_preview_cold_activation_lookahead_frames(sequence.settings.frame_rate);
            let after_frame = frame.saturating_add(prefetch_window_frames as i64);
            let horizon_frame = frame.saturating_add(activation_horizon);
            if let Some(activation_frame) =
                next_root_media_activation_frame(sequence, after_frame, horizon_frame)
            {
                self.schedule_media_prefetch_for_sequence(
                    state,
                    sequence,
                    activation_frame,
                    target_width,
                    target_height,
                    color_context,
                    &mut remaining_prefetch_jobs,
                    None,
                );
            }
        }
    }

    pub(super) fn schedule_media_prefetch_for_sequence(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
        color_context: ColorContext,
        remaining_prefetch_jobs: &mut usize,
        preroll_deadline_at: Option<Instant>,
    ) -> bool {
        if *remaining_prefetch_jobs == 0 {
            return false;
        }
        let demands = collect_preview_timeline_media_demands(
            sequence,
            &state.sequences,
            frame,
            Resolution { width: target_width, height: target_height },
            state.playback_preview_resolution_scale(),
            color_context,
        );
        let Ok(demands) = demands else {
            return false;
        };
        let has_media = !demands.is_empty();

        for demand in demands {
            if *remaining_prefetch_jobs == 0 {
                break;
            }
            let Ok(key) = self.media_preview_key_for_asset(
                state,
                &demand.asset_id,
                demand.color_space_override,
                demand.alpha_interpretation,
                demand.source_time,
                demand.target_resolution.width,
                demand.target_resolution.height,
                &demand.color_context,
                false,
                false,
            ) else {
                continue;
            };
            if self.cached_media_frame(&key).is_none() && !self.failed_media_key(&key) {
                let enqueued = self.request_media_preview(
                    key,
                    MediaPreviewRequestPriority::Prefetch,
                    PreviewDecodeAccessMode::PlaybackCursor,
                    preroll_deadline_at,
                    None,
                    PreviewDecodeAdaptiveHints::default(),
                );
                if enqueued {
                    *remaining_prefetch_jobs = (*remaining_prefetch_jobs).saturating_sub(1);
                }
            }
        }
        has_media
    }

    /// Inspect the same bounded forward media window used by playback prefetch.
    ///
    /// This does not claim that a Viewer output is presented. It reports only
    /// whether immediate future frames have media payloads and how much of the
    /// media-bearing prefix is resident; the Playback Engine separately
    /// requires current-frame presentation before releasing its clock anchor.
    pub(super) fn playback_video_preroll_readiness(
        &self,
        state: &AppState,
    ) -> Option<PreviewVideoPreroll> {
        if !state.is_playback_priming() {
            return None;
        }
        let sequence = state.sequence.as_ref()?;
        let current_frame = state.current_frame().max(0);
        let end_frame = state.last_content_frame().ok()?.max(0);
        if current_frame >= end_frame {
            return Some(PreviewVideoPreroll { ready_media_frames: 0, available_media_frames: 0 });
        }
        let (width, height) = preview_dimensions_for_state(state, sequence);
        let display_snapshot = self.display_snapshot.borrow();
        let display_color_space = preview_display_color_space(
            sequence,
            &state.project_settings.color_management,
            display_snapshot.as_ref(),
        )
        .ok()?;
        let color_context = sequence.settings.root_preview_color_context(
            &state.project_settings.color_management,
            display_color_space,
        );
        let window = media_preview_forward_prefetch_window_frames(sequence.settings.frame_rate)?;
        let mut ready_media_frames = 0usize;
        let mut available_media_frames = 0usize;
        let mut ready_prefix = true;
        for offset in 1..=window as i64 {
            let future_frame = current_frame.saturating_add(offset);
            if future_frame > end_frame {
                break;
            }
            let readiness = self.media_preroll_frame_readiness(
                state,
                sequence,
                future_frame,
                width,
                height,
                color_context.clone(),
            );
            if !readiness.has_media {
                continue;
            }
            available_media_frames = available_media_frames.saturating_add(1);
            ready_prefix &= readiness.ready;
            if ready_prefix {
                ready_media_frames = ready_media_frames.saturating_add(1);
            }
        }
        Some(PreviewVideoPreroll { ready_media_frames, available_media_frames })
    }

    pub(super) fn media_preroll_frame_readiness(
        &self,
        state: &AppState,
        sequence: &Sequence,
        frame: i64,
        target_width: u32,
        target_height: u32,
        color_context: ColorContext,
    ) -> MediaPrerollFrameReadiness {
        let Ok(demands) = collect_preview_timeline_media_demands(
            sequence,
            &state.sequences,
            frame,
            Resolution { width: target_width, height: target_height },
            state.playback_preview_resolution_scale(),
            color_context,
        ) else {
            return MediaPrerollFrameReadiness::required_not_ready();
        };

        let mut readiness = MediaPrerollFrameReadiness::default();
        for demand in demands {
            readiness.has_media = true;
            let cached = self
                .media_preview_key_for_asset(
                    state,
                    &demand.asset_id,
                    demand.color_space_override,
                    demand.alpha_interpretation,
                    demand.source_time,
                    demand.target_resolution.width,
                    demand.target_resolution.height,
                    &demand.color_context,
                    false,
                    false,
                )
                .is_ok_and(|key| self.frame_store.borrow_mut().media_frame(&key).is_some());
            readiness.ready &= cached;
        }
        readiness
    }

    pub(super) fn preview_decode_adaptive_hints(
        &self,
        access_mode: PreviewDecodeAccessMode,
        key: &MediaPreviewKey,
    ) -> PreviewDecodeAdaptiveHints {
        if access_mode != PreviewDecodeAccessMode::ScrubCursor {
            return PreviewDecodeAdaptiveHints::default();
        }
        let hints = self.scrub_adaptation.borrow_mut().observe_request(
            key.asset_id,
            key.source_time,
            Instant::now(),
        );
        match hints.scrub_class {
            PreviewScrubAdaptiveClass::Normal => {
                bump(&self.metrics.scrub_adaptive_normal_requests);
            }
            PreviewScrubAdaptiveClass::HotRegion => {
                bump(&self.metrics.scrub_adaptive_hot_region_requests);
            }
            PreviewScrubAdaptiveClass::SlowLatency => {
                bump(&self.metrics.scrub_adaptive_slow_latency_requests);
            }
            PreviewScrubAdaptiveClass::Recovery => {
                bump(&self.metrics.scrub_adaptive_recovery_requests);
            }
        }
        hints
    }

    pub(super) fn request_media_preview(
        &self,
        key: MediaPreviewKey,
        priority: MediaPreviewRequestPriority,
        access_mode: PreviewDecodeAccessMode,
        playback_current_deadline_at: Option<Instant>,
        demand_identity: Option<mondrian_playback::FrameDemandIdentity>,
        adaptive_hints: PreviewDecodeAdaptiveHints,
    ) -> bool {
        if !self.decode_residency.admits(access_mode) {
            return false;
        }
        let generation = self.execution.borrow().generation();
        let is_current_playback = priority == MediaPreviewRequestPriority::Current
            && access_mode == PreviewDecodeAccessMode::PlaybackCursor;
        if is_current_playback
            && self.playback_sustained_pressure_active()
            && self.playback_realtime_work_pending()
        {
            self.record_playback_current_sustained_pressure_skip();
            tracing::trace!(
                asset_id = %key.asset_id,
                source_time = %key.source_time,
                "viewer preview skipped current playback decode while sustained pressure recovery has realtime work pending"
            );
            return false;
        }
        if is_current_playback {
            self.record_playback_current_deadline_budget(playback_deadline_remaining_us(
                playback_current_deadline_at,
            ));
        }
        let hardware_decode_request = self.hardware_decode_request_for_key(access_mode, &key);
        let hardware_decode_device_selector =
            self.hardware_decode_device_selector_for_access_mode(access_mode);
        let submission = self.scheduler.submit_job(MediaPreviewJob {
            key: key.clone(),
            generation,
            priority,
            access_mode,
            adaptive_hints,
            hardware_decode_request,
            hardware_decode_device_selector,
            enqueued_at: Instant::now(),
            deadline_at: if access_mode == PreviewDecodeAccessMode::PlaybackCursor {
                playback_current_deadline_at
            } else {
                None
            },
            demand_identity,
            execution_id: None,
        });
        match submission {
            MediaPreviewRequestStatus::Scheduled { evicted_prefetch, evicted_still } => {
                bump(&self.metrics.enqueued_jobs);
                if evicted_prefetch.is_some() {
                    bump(&self.metrics.queue_evicted_prefetch_jobs);
                    bump(&self.metrics.queue_canceled_jobs);
                }
                if evicted_still.is_some() {
                    bump(&self.metrics.queue_evicted_still_jobs);
                    bump(&self.metrics.queue_canceled_jobs);
                }
                true
            }
            MediaPreviewRequestStatus::UpdatedQueued {
                priority_promoted,
                access_mode_changed: _,
                generation_changed: _,
            } => {
                if priority_promoted {
                    bump(&self.metrics.queue_promoted_current_jobs);
                }
                false
            }
            MediaPreviewRequestStatus::ReusedInFlight => false,
            #[cfg(test)]
            MediaPreviewRequestStatus::AlreadyPending { .. } => false,
            MediaPreviewRequestStatus::DroppedBackpressure => {
                bump(&self.metrics.queue_full_drops);
                tracing::trace!(
                    asset_id = %key.asset_id,
                    source_time = %key.source_time,
                    "viewer preview request dropped by backpressure"
                );
                false
            }
            MediaPreviewRequestStatus::DroppedInvalidAccessMode => {
                bump(&self.metrics.queue_invalid_access_mode_drops);
                tracing::warn!(
                    asset_id = %key.asset_id,
                    source_time = %key.source_time,
                    priority = ?priority,
                    access_mode = access_mode.as_str(),
                    "viewer preview request dropped because priority/access-mode pair is invalid"
                );
                false
            }
            MediaPreviewRequestStatus::Closed => {
                bump(&self.metrics.worker_disconnected_drops);
                tracing::debug!("viewer preview worker unavailable");
                false
            }
        }
    }
}

fn media_preview_cold_activation_lookahead_frames(frame_rate: mondrian_core::Rational) -> i64 {
    let fps = frame_rate.to_f64();
    if !fps.is_finite() || fps <= 0.0 {
        return 0;
    }
    (((MEDIA_PREVIEW_COLD_ACTIVATION_LOOKAHEAD_US as f64 / 1_000_000.0) * fps).ceil() as i64)
        .clamp(1, MEDIA_PREVIEW_COLD_ACTIVATION_MAX_FRAMES)
}

fn next_root_media_activation_frame(
    sequence: &Sequence,
    after_frame: i64,
    horizon_frame: i64,
) -> Option<i64> {
    sequence
        .video_tracks
        .iter()
        .filter(|track| track.is_visible && !track.is_muted)
        .flat_map(|track| track.clips.iter())
        .filter(|clip| {
            !clip.is_disabled
                && clip.duration > mondrian_core::TimelineTime::ZERO
                && matches!(
                    clip.kind,
                    mondrian_core::timeline_data::ClipKind::Media
                        | mondrian_core::timeline_data::ClipKind::NestedSequence
                )
        })
        .filter_map(|clip| {
            clip.position
                .to_frame_position(
                    sequence.settings.frame_rate,
                    mondrian_core::FrameRounding::Ceil,
                )
                .ok()
                .map(|position| position.frame)
        })
        .filter(|frame| *frame > after_frame && *frame <= horizon_frame)
        .min()
}
