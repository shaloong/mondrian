//! Bounded completion, expiry, and terminal-candidate pump for Preview media work.

use super::*;
use crate::app::preview_media_task::{MediaPreviewCancellationPhase, MediaPreviewQueueDisposition};

impl<O: Clone> PreviewProductionRuntime<O> {
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
        let expired = self.scheduler.expire_playback_current_older_than(timeout);
        if expired.is_empty() {
            return PreviewWorkPoll::default();
        }
        let canceled_queued_jobs = expired.iter().fold(0u64, |total, request| {
            total.saturating_add(request.removed_queued_work as u64)
        });
        // Scheduler-current work can outlive the Playback Session demand that
        // created it (for example after a GPU presentation completed first).
        // Only the still-pending Playback identity has terminal authority, and
        // multiple redundant jobs for it collapse to one Late observation.
        let frame_delivery_candidates = pending_playback_demand
            .and_then(|pending_identity| {
                expired.iter().find_map(|request| {
                    (request.access_mode == PreviewDecodeAccessMode::PlaybackCursor
                        && request.demand_identity == Some(pending_identity))
                    .then_some(pending_identity)
                })
            })
            .map(|identity| {
                mondrian_playback::FrameDeliveryCandidate::for_demand(
                    identity,
                    mondrian_playback::FrameDeliveryKind::Late,
                )
            })
            .into_iter()
            .collect::<Vec<_>>();
        add_cell(
            &self.metrics.playback_current_stalled_expirations,
            frame_delivery_candidates.len() as u64,
        );
        self.record_playback_current_late_drop(frame_delivery_candidates.len() as u64);
        add_cell(&self.metrics.queue_canceled_jobs, canceled_queued_jobs);
        self.execution.borrow_mut().set_pending(false);
        PreviewWorkPoll {
            visible_change: false,
            transport_change: true,
            candidate_retry_required: false,
            needs_follow_up_poll: false,
            frame_delivery_candidates,
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
            let mut result = match self.results.borrow().try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.observe_media_worker_result_disconnect() {
                        outcome.visible_change = true;
                        if let Some(identity) = pending_playback_demand {
                            outcome.frame_delivery_candidates.push(
                                mondrian_playback::FrameDeliveryCandidate::for_demand(
                                    identity,
                                    mondrian_playback::FrameDeliveryKind::Failed,
                                ),
                            );
                        }
                    }
                    break;
                }
            };
            drained += 1;
            let reusable_completion = !result.canceled
                && result.frame.as_ref().is_some_and(MediaPreviewFrame::permits_cross_call_reuse);
            let completion_resolution = if let Some(execution_id) = result.execution_id {
                self.scheduler.resolve_execution(execution_id, reusable_completion)
            } else {
                self.scheduler.resolve_unleased(
                    &result.key,
                    result.generation,
                    result.access_mode,
                    result.demand_identity,
                    reusable_completion,
                )
            };
            let completion = completion_resolution.status;
            let authoritative_failure_generation = completion
                .is_current()
                .then_some(completion_resolution.binding_generation)
                .flatten();
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
                match result.queue_disposition {
                    MediaPreviewQueueDisposition::Ready => {
                        self.record_preview_decode_queue_wait(
                            result.priority,
                            result.access_mode,
                            result.queue_wait_us,
                        );
                    }
                    MediaPreviewQueueDisposition::Expired => {
                        self.record_expired_preview_decode_queue_wait(
                            result.priority,
                            result.access_mode,
                            result.queue_wait_us,
                        );
                    }
                }
            }
            if result.canceled {
                self.invalidate_evaluations_for_asset(result.key.asset_id);
                if result.cancellation_phase == Some(MediaPreviewCancellationPhase::Queued) {
                    // A request that expired before codec execution is a
                    // scheduler deadline drop, not cooperative-cancellation
                    // latency evidence. Complete the matching demand as Late
                    // so playback can advance without waiting for its stall
                    // timeout; broker expiry counters retain the diagnosis.
                    if owns_pending_playback_demand {
                        if let Some(identity) = completion_demand_identity {
                            outcome.frame_delivery_candidates.push(
                                mondrian_playback::FrameDeliveryCandidate::for_demand(
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
                    result.concrete_media_checkpoint,
                    result.decode_elapsed_us,
                    result.logical_cancellation_observed,
                    owns_pending_playback_demand,
                );
                // Decode work cancellation is scheduler evidence, not a frame
                // presentation. Emitting it as a terminal candidate races
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
            if presentation_current && let Some(identity) = completion_demand_identity {
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
                    _ => outcome.frame_delivery_candidates.push(
                        mondrian_playback::FrameDeliveryCandidate::for_demand(identity, kind),
                    ),
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
                        let residency_admission = match result.residency_work.take() {
                            Some(work) => Some(self.frame_store.borrow_mut().insert_media_frame(
                                result.key.clone(),
                                frame,
                                work,
                            )),
                            None => None,
                        };
                        if !residency_admission
                            .is_some_and(mondrian_playback::FrameStoreAdmission::is_admitted)
                        {
                            tracing::warn!(
                                asset_id = %result.key.asset_id,
                                source_sample = ?result.key.source_sample(),
                                admission = ?residency_admission,
                                "decoded Preview result could not transfer its physical residency lease"
                            );
                            let reason = if residency_admission.is_none() {
                                MediaPreviewFailureReason::ResidencyContractViolation
                            } else {
                                MediaPreviewFailureReason::ResidencyCapacityRejected
                            };
                            self.scrub_adaptation
                                .borrow_mut()
                                .observe_failure(result.access_mode, reason);
                            self.record_preview_decode_failure(result.access_mode, Some(reason));
                            if let Some(generation) = authoritative_failure_generation {
                                self.remember_media_execution_failure(
                                    &result.key,
                                    generation,
                                    reason,
                                );
                            }
                            if owns_pending_playback_demand
                                && let Some(identity) = completion_demand_identity
                            {
                                let kind = if reason
                                    == MediaPreviewFailureReason::ResidencyCapacityRejected
                                {
                                    mondrian_playback::FrameDeliveryKind::Blocked
                                } else {
                                    mondrian_playback::FrameDeliveryKind::Failed
                                };
                                outcome.frame_delivery_candidates.push(
                                    mondrian_playback::FrameDeliveryCandidate::for_demand(
                                        identity, kind,
                                    ),
                                );
                            }
                            outcome.visible_change |= presentation_current;
                            continue;
                        }
                        self.frame_store.borrow_mut().forget_failure(&result.key);
                        self.media_execution_failures.borrow_mut().remove(&result.key);
                    }
                    outcome.visible_change |= presentation_current;
                    // A decoded frame became available; evaluations waiting
                    // on this exact asset (and, transitionally, every ready
                    // evaluation) must re-resolve instead of serving stale
                    // media content.
                    self.invalidate_evaluations_for_asset(result.key.asset_id);
                }
                None => {
                    // A retained timeline evaluation may be waiting on this
                    // exact producer. Once it completes without publishing a
                    // frame (cancellation, timeout, or failure), that wait is
                    // no longer actionable. Re-resolve so the current
                    // generation can re-admit work or project the retained
                    // terminal failure instead of waiting forever.
                    self.invalidate_evaluations_for_asset(result.key.asset_id);
                    if let Some(reason) = result.failure_reason {
                        self.scrub_adaptation
                            .borrow_mut()
                            .observe_failure(result.access_mode, reason);
                    }
                    let terminal_failure =
                        result.failure_reason == Some(MediaPreviewFailureReason::DecodeError);
                    if let (
                        Some(generation),
                        Some(
                            reason @ (MediaPreviewFailureReason::WorkerPanicked
                            | MediaPreviewFailureReason::ResidencyContractViolation
                            | MediaPreviewFailureReason::ResidencyCapacityRejected
                            | MediaPreviewFailureReason::TemporalMismatch),
                        ),
                    ) = (authoritative_failure_generation, result.failure_reason)
                    {
                        self.remember_media_execution_failure(&result.key, generation, reason);
                    }
                    self.record_preview_decode_failure(result.access_mode, result.failure_reason);
                    if let Some(error) = result.error {
                        tracing::debug!(
                            asset_id = %result.key.asset_id,
                            path = %result.key.decode.source().path().display(),
                            "viewer preview decode failed: {error}"
                        );
                    }
                    if completed_after_playback_deadline {
                        continue;
                    }
                    if completion.should_cache() && terminal_failure {
                        self.frame_store.borrow_mut().remember_failure(result.key);
                    }
                    // Publish the current failure once. Adaptive failures may
                    // retry through a changed decode policy; deterministic
                    // generation-scoped failures are retained above, so the
                    // media Adapter projects Unavailable instead of submitting
                    // the same work on every repaint.
                    outcome.visible_change |= presentation_current;
                }
            }
        }
        let aggregate_capacity_retry =
            drained > 0 && self.media_aggregate_capacity_waiting.replace(false);
        if aggregate_capacity_retry {
            // Only the consumer that explicitly observed aggregate-capacity
            // blocking may turn a generic completion into a candidate retry.
            // Presentation-current results already publish `visible_change`,
            // while exact reused work has its own owner-bound waiter below.
            // Treating every drained background/canceled result as actionable
            // creates a retry/supersession loop for an unchanged Viewer intent.
            outcome.candidate_retry_required = true;
        }
        // A queued cancellation or another lifecycle owner can release the
        // aggregate lease without producing a media result. Once every owner
        // that justified transient pressure has settled, publish the same
        // one-shot retry edge. A still-occupied external lease is classified
        // on that retry as a stable aggregate blocker.
        outcome.candidate_retry_required |= self.consume_settled_aggregate_capacity_retry();
        // Rebinding to queued/in-flight work transfers no new payload and can
        // therefore settle through cancellation, expiry, or another result's
        // resolution. Its exact Broker owner is the level predicate; unrelated
        // work cannot suppress or manufacture this one-shot candidate retry.
        outcome.candidate_retry_required |= self.consume_settled_existing_work_retry();
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

    pub(super) fn remember_media_execution_failure(
        &self,
        key: &MediaPreviewKey,
        generation: u64,
        reason: MediaPreviewFailureReason,
    ) {
        let mut failures = self.media_execution_failures.borrow_mut();
        if failures
            .get(key)
            .is_none_or(|(retained_generation, _)| generation >= *retained_generation)
        {
            failures.insert(key.clone(), (generation, reason));
        }
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
