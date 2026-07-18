//! Bounded completion, expiry, and terminal-delivery pump for Preview media work.

use super::*;

impl AppUiPreviewService {
    /// Poll completed background media preview decodes.
    pub fn poll_finished(
        &self,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> bool {
        self.poll_finished_outcome(pending_playback_demand).visible_change
    }

    pub(super) fn poll_finished_outcome(
        &self,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        self.poll_finished_outcome_with_budget(
            MEDIA_PREVIEW_MAX_COMPLETED_RESULTS_PER_POLL,
            Duration::from_micros(MEDIA_PREVIEW_COMPLETED_RESULTS_POLL_BUDGET_US),
            pending_playback_demand,
        )
    }

    pub(super) fn expire_stalled_realtime_current(
        &self,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        self.expire_stalled_realtime_current_with_timeout(
            Duration::from_micros(MEDIA_PREVIEW_PLAYBACK_BUFFERING_STALL_TIMEOUT_US),
            pending_playback_demand,
        )
    }

    pub(super) fn expire_stalled_realtime_current_with_timeout(
        &self,
        timeout: Duration,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        let expired = self.scheduler.expire_realtime_current_older_than(timeout);
        if expired.is_empty() {
            return PreviewWorkPoll::default();
        }
        let canceled_queued_jobs = expired.len() as u64;
        // Scheduler-current work can outlive the Playback Session demand that
        // created it (for example after a GPU presentation completed first).
        // Only the still-pending Playback identity has terminal authority, and
        // multiple redundant jobs for it collapse to one Late observation.
        let frame_deliveries = pending_playback_demand
            .and_then(|pending_identity| {
                expired.iter().find_map(|request| {
                    (request.access_mode == PreviewDecodeAccessMode::PlaybackCursor
                        && request.demand_identity == Some(pending_identity))
                    .then_some(pending_identity)
                })
            })
            .map(|identity| {
                mondrian_playback::FrameDelivery::for_demand(
                    identity,
                    mondrian_playback::FrameDeliveryKind::Late,
                )
            })
            .into_iter()
            .collect::<Vec<_>>();
        add_cell(
            &self.metrics.playback_current_stalled_expirations,
            frame_deliveries.len() as u64,
        );
        self.record_playback_current_late_drop(frame_deliveries.len() as u64);
        add_cell(&self.metrics.queue_canceled_jobs, canceled_queued_jobs);
        self.execution.borrow_mut().set_pending(false);
        PreviewWorkPoll {
            visible_change: false,
            transport_change: true,
            needs_follow_up_poll: false,
            frame_deliveries,
        }
    }

    #[cfg(test)]
    pub(super) fn poll_finished_with_budget(
        &self,
        max_results: usize,
        time_budget: Duration,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> bool {
        self.poll_finished_outcome_with_budget(max_results, time_budget, pending_playback_demand)
            .visible_change
    }

    pub(super) fn poll_finished_outcome_with_budget(
        &self,
        max_results: usize,
        time_budget: Duration,
        pending_playback_demand: Option<mondrian_playback::FrameDemandIdentity>,
    ) -> PreviewWorkPoll {
        let poll_started = Instant::now();
        bump(&self.metrics.completion_poll_calls);
        self.metrics
            .completion_poll_max_results_per_poll
            .set(self.metrics.completion_poll_max_results_per_poll.get().max(max_results as u64));
        let mut outcome = PreviewWorkPoll::default();
        let mut drained = 0usize;
        while drained < max_results {
            if drained > 0 && poll_started.elapsed() >= time_budget {
                bump(&self.metrics.completion_poll_time_budget_exhaustions);
                outcome.needs_follow_up_poll = true;
                break;
            }
            let result = match self.results.borrow().try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            let completion_resolution = if let Some(execution_id) = result.execution_id {
                self.scheduler.resolve_execution(execution_id, !result.canceled)
            } else {
                self.scheduler.resolve_unleased(
                    &result.key,
                    result.generation,
                    result.access_mode,
                    result.demand_identity,
                    !result.canceled,
                )
            };
            let completion = completion_resolution.status;
            let completion_demand_identity =
                completion_resolution.demand_identity.or(result.demand_identity);
            let owns_pending_playback_demand = completion.is_current()
                && completion_demand_identity.is_some()
                && completion_demand_identity == pending_playback_demand;
            let presentation_current = completion.is_current()
                && (result.access_mode != PreviewDecodeAccessMode::PlaybackCursor
                    || owns_pending_playback_demand);
            let startup_preroll = media_preview_result_is_startup_preroll(&result);
            if !startup_preroll {
                self.record_preview_decode_queue_wait(
                    result.priority,
                    result.access_mode,
                    result.queue_wait_us,
                );
            }
            if result.canceled {
                if result.cancellation_phase == Some(MediaPreviewCancellationPhase::Queued) {
                    // A request that expired before codec execution is a
                    // scheduler deadline drop, not cooperative-cancellation
                    // latency evidence. Complete the matching demand as Late
                    // so playback can advance without waiting for its stall
                    // timeout; broker expiry counters retain the diagnosis.
                    if owns_pending_playback_demand {
                        if let Some(identity) = completion_demand_identity {
                            outcome.frame_deliveries.push(
                                mondrian_playback::FrameDelivery::for_demand(
                                    identity,
                                    mondrian_playback::FrameDeliveryKind::Late,
                                ),
                            );
                        }
                        self.record_playback_current_late_drop(1);
                    }
                    continue;
                }
                self.record_preview_decode_cancel(
                    result.access_mode,
                    result.cancel_reason,
                    result.decode_elapsed_us,
                    result.cancel_observed_elapsed_us,
                    result.cancel_request_to_observed_us,
                    owns_pending_playback_demand,
                );
                // Decode work cancellation is scheduler evidence, not a frame
                // presentation. Emitting it as a terminal FrameDelivery races
                // the Playback Session's newer demand and turns expected
                // latest-wins cleanup into a rejected stale delivery.
                // A settled scrub/still request can be cooperatively canceled
                // while the render generation changes. Its completion releases
                // the scheduler entry, so request one more render pass to submit
                // the stable current frame. Playback deadline cancellation is
                // intentionally excluded to avoid a realtime retry loop.
                if result.priority == MediaPreviewRequestPriority::Current
                    && result.access_mode != PreviewDecodeAccessMode::PlaybackCursor
                {
                    outcome.visible_change = true;
                }
                continue;
            }
            let completed_after_playback_deadline =
                completion_resolution.deadline_status.is_missed();
            if presentation_current {
                if let Some(identity) = completion_demand_identity {
                    let kind = playback_frame_delivery_kind(
                        completed_after_playback_deadline,
                        result.frame.is_some(),
                        result.priority,
                        result.decode_diagnostics.as_ref().map(PlaybackDecodeExecution::from),
                    );
                    match kind {
                        mondrian_playback::FrameDeliveryKind::Ready => {
                            // Successful decode is non-terminal: the Presentation Adapter
                            // finishes the demand after its output is actually usable.
                        }
                        mondrian_playback::FrameDeliveryKind::Degraded => {}
                        _ => outcome
                            .frame_deliveries
                            .push(mondrian_playback::FrameDelivery::for_demand(identity, kind)),
                    }
                }
            }
            if let Some(diagnostics) = result.decode_diagnostics {
                self.scrub_adaptation.borrow_mut().observe_decode(diagnostics);
                if startup_preroll {
                    self.record_startup_preroll_decode(diagnostics, result.queue_wait_us);
                } else {
                    self.record_preview_decode(
                        diagnostics,
                        result.priority,
                        result.queue_wait_us,
                        !completed_after_playback_deadline,
                    );
                }
            }
            if completed_after_playback_deadline && owns_pending_playback_demand {
                self.record_playback_current_late_drop(1);
            }
            match result.frame {
                Some(frame) => {
                    bump(&self.metrics.decode_successes);
                    if let Some(diagnostics) = result.color_diagnostics {
                        self.record_color_transform(diagnostics);
                    }
                    if let Some(diagnostics) = result.color_stage_diagnostics {
                        self.record_color_stage(diagnostics);
                    }
                    if completed_after_playback_deadline {
                        continue;
                    }
                    if completion.should_cache() {
                        let mut frame_store = self.frame_store.borrow_mut();
                        frame_store.insert_media_frame(
                            result.key.clone(),
                            frame,
                            presentation_current,
                        );
                        frame_store.forget_failure(&result.key);
                    }
                    outcome.visible_change |= presentation_current;
                }
                None => {
                    if let Some(reason) = result.failure_reason {
                        self.scrub_adaptation
                            .borrow_mut()
                            .observe_failure(result.access_mode, reason);
                    }
                    let terminal_failure =
                        result.failure_reason == Some(MediaPreviewFailureReason::DecodeError);
                    if presentation_current && terminal_failure {
                        self.frame_store.borrow_mut().clear_pinned_media_frame();
                    }
                    self.record_preview_decode_failure(result.access_mode, result.failure_reason);
                    if let Some(error) = result.error {
                        tracing::debug!(
                            asset_id = %result.key.asset_id,
                            path = %result.key.path.display(),
                            "viewer preview decode failed: {error}"
                        );
                    }
                    if completed_after_playback_deadline {
                        continue;
                    }
                    if completion.should_cache() && terminal_failure {
                        self.frame_store.borrow_mut().remember_failure(result.key);
                    }
                    // A current retryable failure changed adaptive decode policy.
                    // Request one refresh so the same target can recover through
                    // the conservative keyframe path instead of stalling forever.
                    outcome.visible_change |= presentation_current;
                }
            }
        }
        if max_results > 0 && drained == max_results {
            bump(&self.metrics.completion_poll_count_budget_exhaustions);
            outcome.needs_follow_up_poll = true;
        }
        if drained > 0 {
            add_cell(&self.metrics.completion_poll_results, drained as u64);
        }
        self.record_completion_poll_duration(poll_started.elapsed());
        outcome
    }

    fn record_completion_poll_duration(&self, duration: Duration) {
        let duration_us = app_duration_us(duration);
        add_cell(&self.metrics.completion_poll_total_duration_us, duration_us);
        self.metrics
            .completion_poll_max_duration_us
            .set(self.metrics.completion_poll_max_duration_us.get().max(duration_us));
        self.metrics.completion_poll_last_duration_us.set(duration_us);
    }
}
