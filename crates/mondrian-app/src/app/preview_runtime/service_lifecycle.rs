//! Preview worker and interactive-work lifecycle owned by the production Runtime.

use super::*;

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Bind prepared visual execution to the exact open Authoring Session.
    ///
    /// Durable IDs and author revisions may intentionally recur after closing
    /// and reopening a Project. They therefore cannot identify the lifetime of
    /// process-local prepared programs or final Viewer output.
    pub(super) fn synchronize_visual_program_authoring_session(
        &self,
        snapshot: &PreviewExecutionSnapshot<'_>,
    ) {
        let current = snapshot.authoring_session_id();
        let previous = self.visual_program_authoring_session.get();
        if previous == current {
            return;
        }

        if previous.is_some() {
            // One mutable cache operation both rotates identity and clears
            // residency. Retire all dependent jobs and outputs before the new
            // Session can request work.
            self.visual_programs.borrow_mut().rotate_scope();
            self.transport_epoch.set(None);
            self.retire_obsolete_transport_work();
            self.clear_all_preview_residency();
        }
        self.future_media_window.borrow_mut().clear();
        self.last_gallery_capture.borrow_mut().take();
        self.visual_program_authoring_session.set(current);
    }

    /// Synchronize the latest authoritative transport intent.
    ///
    /// The Runtime compares the transport family and Playback Epoch, then
    /// retires obsolete work exactly once for a play/pause family transition
    /// or a seek/restart discontinuity. Presentation Adapters only forward the
    /// current typed intent; they do not classify Actions or own cancellation
    /// policy.
    pub(crate) fn synchronize_transport_intent(&self, intent: PreviewTransportIntent) {
        let previous_epoch = self.transport_epoch.replace(Some(intent.epoch()));
        let was_playing = self.transport_playing.get();
        let family_changed = previous_epoch.is_some() && was_playing != intent.playing();
        let epoch_changed = previous_epoch.is_some_and(|previous| previous != intent.epoch());
        if family_changed || epoch_changed {
            // A stopped seek can rotate transport authority before
            // `activate_preview_generation` observes the new Viewer key. Retry
            // the old generation's independently usable output proof first,
            // after any raced worker completion became visible. Starting or
            // stopping playback cannot use the other transport family's output
            // to evict otherwise reusable CPU frames.
            if !was_playing && !intent.playing() {
                self.try_release_settled_transport_media_residency();
            }
            self.retire_obsolete_transport_work();
        }

        self.transport_playing.set(intent.playing());
        let family = if intent.playing() {
            PreviewDecodeResidencyFamily::Playback
        } else {
            PreviewDecodeResidencyFamily::Interactive
        };
        if !self.decode_residency.activate(family) {
            return;
        }

        // Final Viewer outputs have independent ownership. Dropping decoded
        // media here releases native-output leases before worker-owned codec
        // contexts acknowledge retirement.
        self.clear_decoder_resource_preview_residency();
        self.jobs.interrupt_workers_for_lifecycle();
    }

    /// Release decoder-backed media residency after all Preview work is idle.
    ///
    /// This preserves Viewer output and failure memory. A retained native
    /// media frame can pin its decoder's entire hardware-surface pool, so entry
    /// and byte budgets alone are not sufficient at a transport-idle boundary.
    /// The operation fails closed if queued, in-flight, or unresolved work is
    /// still visible to the Broker.
    #[cfg(test)]
    pub(crate) fn try_release_idle_media_residency(&self) -> bool {
        if !self.preview_media_work_is_idle() {
            return false;
        }
        self.clear_media_preview_residency();
        true
    }

    fn preview_media_work_is_idle(&self) -> bool {
        let scheduler = self.scheduler.diagnostics();
        let queue = self.jobs.diagnostics();
        !self.execution.borrow().is_pending()
            && scheduler.pending_requests == 0
            && queue.queued_jobs == 0
            && queue.in_flight_jobs == 0
    }

    /// Release idle decoder-native resources after a stopped transport has a
    /// durable final GPU Viewer output under the active Preview generation.
    ///
    /// Bounded CPU frames remain in the Frame Store so immediate playback can
    /// reuse the exact paused frame without another cold source open. Native
    /// frames are retired because even one can pin the decoder surface pool.
    pub(crate) fn try_release_settled_transport_media_residency(&self) -> bool {
        if self.transport_playing.get()
            || !self.execution.borrow().has_exact_current_output()
            || !self.preview_media_work_is_idle()
        {
            return false;
        }
        self.clear_decoder_resource_preview_residency();
        true
    }

    /// Release CPU evaluation reuse after the Adapter proves physical current completion.
    /// The caller must have finalized Presented/NoDemand for this exact GPU output.
    /// Registration alone does not authorize release. In-flight candidate clones,
    /// unrelated evaluations, native owners and Frame Store entries remain intact.
    pub(crate) fn release_completed_gpu_evaluation(
        &self,
        output_key: &PreviewOutputKey,
        intent: crate::app::preview_execution::PreviewPlaybackIntent,
    ) -> bool {
        if !self
            .execution
            .borrow()
            .exact_current_output()
            .is_some_and(|(current, _)| current == output_key)
        {
            return false;
        }
        self.evaluation_working_set
            .borrow_mut()
            .release_completed_gpu_evaluation(output_key, intent)
    }

    /// Release all idle decoded media at an explicit validation/lifecycle seam.
    #[cfg(test)]
    pub(crate) fn try_release_settled_transport_all_media_residency(&self) -> bool {
        if self.transport_playing.get() || !self.execution.borrow().has_exact_current_output() {
            return false;
        }
        self.try_release_idle_media_residency()
    }

    /// Cancel outstanding preview decode work without shutting down workers.
    ///
    /// Closing a project, switching projects, or quitting should make any
    /// queued/in-flight frame immediately obsolete so decode workers can
    /// cooperatively stop instead of continuing to consume CPU for invisible
    /// media.
    pub(crate) fn cancel_all_work_for_lifecycle(&self) {
        self.visual_program_authoring_session.set(None);
        self.visual_programs.borrow_mut().rotate_scope();
        self.future_media_window.borrow_mut().clear();
        self.transport_epoch.set(None);
        self.retire_obsolete_transport_work();
        self.clear_all_preview_residency();
    }

    fn retire_obsolete_transport_work(&self) {
        let (pending_requests, queued_jobs) = self.retire_all_preview_work();
        bump(&self.metrics.interactive_cancel_requests);
        add_cell(
            &self.metrics.interactive_cancel_scheduler_requests,
            pending_requests,
        );
        add_cell(&self.metrics.interactive_cancel_queued_jobs, queued_jobs);
        add_cell(&self.metrics.queue_canceled_jobs, queued_jobs);
    }

    /// Retire every decode/output binding owned by a superseded renderer
    /// device root while preserving device-independent CPU frame residency.
    pub(super) fn retire_decoder_device_generation(&self) {
        self.future_media_window.borrow_mut().clear();
        let _ = self.retire_all_preview_work();
        self.clear_decoder_resource_preview_residency();
        // Wake idle workers and force active workers through their Broker
        // cancellation checkpoint before another Session can be reused.
        self.jobs.interrupt_workers_for_lifecycle();
    }

    fn retire_all_preview_work(&self) -> (u64, u64) {
        self.decode_residency_waiting.set(None);
        self.media_aggregate_capacity_waiting.set(false);
        self.media_existing_work_waiters.borrow_mut().clear();
        self.media_execution_pressure_waiters.borrow_mut().clear();
        self.media_retry_pending.set(false);
        let pending_requests = self.scheduler.diagnostics().pending_requests as u64;
        let (generation, queued_jobs) = self.scheduler.cancel_all();
        if let Some(task) = &self.visual_execution {
            task.prune_before(generation);
        }
        self.visual_ready.borrow_mut().clear();
        self.visual_failures.borrow_mut().clear();
        self.visual_terminal_candidates.borrow_mut().clear();
        self.cpu_fallback_in_flight.borrow_mut().take();
        self.cpu_fallback_failure.borrow_mut().take();
        let queued_jobs = queued_jobs as u64;
        self.execution.borrow_mut().invalidate(|| generation);
        (pending_requests, queued_jobs)
    }

    /// Shut down preview workers for application exit.
    pub fn shutdown(&self) {
        let already_shutdown = self.begin_shutdown();
        if !already_shutdown {
            self.reap_workers_async();
        }
    }

    /// Stop admission and synchronously reclaim every Preview-owned worker.
    pub fn shutdown_and_wait(mut self) -> PreviewRuntimeShutdownEvidence {
        self.begin_shutdown();
        let handles = self.workers.borrow_mut().drain(..).collect::<Vec<_>>();
        let mut evidence = join_preview_workers(handles);
        let unverified_async_reaps = self.unverified_async_worker_reaps.get();
        evidence.workers_started = evidence.workers_started.saturating_add(unverified_async_reaps);
        evidence.unverified_async_reaps = unverified_async_reaps;
        evidence.record(
            self.visual_execution
                .take()
                .map_or(PreviewOwnedWorkerShutdown::NotStarted, |task| {
                    task.shutdown_and_wait()
                }),
        );
        evidence.record(
            self.cpu_fallback_task
                .take()
                .map_or(PreviewOwnedWorkerShutdown::NotStarted, |task| {
                    task.shutdown_and_wait()
                }),
        );
        evidence.record(self.title_task.borrow_mut().shutdown_and_wait());
        let observer = self.visual_dependencies.shutdown_and_wait();
        evidence.record(observer);
        evidence.visual_dependency_worker = Some(observer);
        let render_cache = self.timeline_render_cache.borrow_mut().shutdown_and_wait();
        evidence.record(render_cache.aggregate_outcome);
        evidence.timeline_render_cache = render_cache;
        let callbacks = self.work_watch.shutdown_and_wait();
        if let Some(worker) = callbacks.worker {
            evidence.record(worker);
        }
        evidence.work_callbacks = Some(callbacks);
        evidence
    }

    /// Close Preview admission without waiting for worker or cache teardown.
    ///
    /// Qualification calls this before waiting on any execution owner so every
    /// cooperative cancellation observes the same absolute shutdown window.
    pub(crate) fn begin_endurance_shutdown(&mut self) {
        self.begin_shutdown();
        if let Some(task) = self.visual_execution.as_mut() {
            task.begin_shutdown();
        }
        if let Some(task) = self.cpu_fallback_task.as_mut() {
            task.begin_shutdown();
        }
        self.title_task.borrow_mut().begin_shutdown();
        self.visual_dependencies.begin_shutdown();
        self.timeline_render_cache.borrow_mut().begin_shutdown();
    }

    /// Consume Preview while bounding every owned worker by one absolute deadline.
    ///
    /// Preview contains thread-affine OCIO processor sessions and therefore
    /// cannot be moved wholesale to a shutdown coordinator. Admission closes on
    /// the owner thread, then each movable worker handle is polled and joined
    /// only when it has completed. Any worker still running at `deadline` is
    /// detached and recorded fail-closed.
    pub(crate) fn shutdown_until(self, deadline: Instant) -> PreviewRuntimeShutdownEvidence {
        self.shutdown_until_with_inventory(deadline).0
    }

    /// The same consuming path retains per-owner outcomes for failed construction.
    pub(super) fn shutdown_until_with_inventory(
        mut self,
        deadline: Instant,
    ) -> (
        PreviewRuntimeShutdownEvidence,
        crate::app::preview_shutdown_evidence::PreviewStartupWorkerShutdown,
    ) {
        self.begin_endurance_shutdown();
        let handles = self.workers.borrow_mut().drain(..).collect::<Vec<_>>();
        let (mut evidence, media) = join_preview_workers_until(handles, deadline);
        let unverified_async_reaps = self.unverified_async_worker_reaps.get();
        evidence.workers_started = evidence.workers_started.saturating_add(unverified_async_reaps);
        evidence.unverified_async_reaps = unverified_async_reaps;
        let visual = self
            .visual_execution
            .take()
            .map_or(PreviewOwnedWorkerShutdown::NotStarted, |task| {
                task.shutdown_until(deadline)
            });
        evidence.record(visual);
        let cpu_fallback = self
            .cpu_fallback_task
            .take()
            .map_or(PreviewOwnedWorkerShutdown::NotStarted, |task| {
                task.shutdown_until(deadline)
            });
        evidence.record(cpu_fallback);
        let title = self.title_task.borrow_mut().shutdown_until(deadline);
        evidence.record(title);
        let observer = self.visual_dependencies.shutdown_until(deadline);
        evidence.record(observer);
        evidence.visual_dependency_worker = Some(observer);
        let render_cache = self.timeline_render_cache.borrow_mut().shutdown_until(deadline);
        evidence.record(render_cache.aggregate_outcome);
        evidence.timeline_render_cache = render_cache;
        let callbacks = self.work_watch.shutdown_until(deadline);
        if let Some(worker) = callbacks.worker {
            evidence.record(worker);
        }
        evidence.work_callbacks = Some(callbacks);
        (
            evidence,
            crate::app::preview_shutdown_evidence::PreviewStartupWorkerShutdown {
                media,
                visual,
                cpu_fallback,
                title,
            },
        )
    }

    fn begin_shutdown(&self) -> bool {
        self.work_watch.begin_shutdown();
        self.jobs.close();
        self.visual_dependencies.begin_shutdown();
        let already_shutdown = self.shutdown.request();
        // Disconnect the completion sink before waiting for media workers.
        // A decode can finish after the foreground stores were cleared and
        // publish a native AVFrame into this channel. Keeping the Receiver in
        // `self` while joining that worker creates a lifetime cycle: codec
        // teardown waits for the queued surface, while the Receiver is not
        // dropped until after the join. Replacing it makes every raced publish
        // fail on the worker and drop its payload before decoder-session clear.
        let (_replacement_sender, replacement_receiver) = mpsc::sync_channel(1);
        drop(self.results.replace(replacement_receiver));
        self.future_media_window.borrow_mut().clear();
        self.retire_obsolete_transport_work();
        self.clear_all_preview_residency();
        already_shutdown
    }

    fn reap_workers_async(&self) {
        let handles = self.workers.borrow_mut().drain(..).collect::<Vec<_>>();
        if handles.is_empty() {
            return;
        }
        let reaped_workers = u32::try_from(handles.len()).unwrap_or(u32::MAX);
        self.unverified_async_worker_reaps
            .set(self.unverified_async_worker_reaps.get().saturating_add(reaped_workers));

        if let Err(err) = thread::Builder::new()
            .name("mondrian-ui-viewer-preview-reaper".to_owned())
            .spawn(move || {
                let _ = join_preview_workers(handles);
            })
        {
            tracing::warn!(
                "failed to start production preview reaper; workers will finish detached: {err}"
            );
        }
    }

    /// Atomically retire every owner of native decoder surfaces known to the
    /// Preview Runtime. Evaluation entries are dropped first because their
    /// frame handles are clones of Store payloads.
    pub(super) fn clear_decoder_resource_preview_residency(&self) {
        // The future-window cold-activation owner may hold the final external
        // clone of a native surface. Retire it before asking the Store and
        // decoder Session to prove that native residency is gone. Lowered
        // plans and source-to-worker Session ownership remain valid across an
        // in-family trim and therefore stay resident.
        self.future_media_window.borrow_mut().release_decoded_residency();
        self.evaluation_working_set.borrow_mut().clear_decoder_resource_entries();
        self.frame_store.borrow_mut().clear_decoder_resource_media_frames();
    }

    /// Atomically retire every decoded-media owner while preserving final
    /// Viewer outputs and failure memory.
    pub(super) fn clear_media_preview_residency(&self) {
        self.future_media_window.borrow_mut().release_decoded_residency();
        self.evaluation_working_set.borrow_mut().clear();
        self.frame_store.borrow_mut().clear_media_frames();
    }

    /// Retire all Preview residency at an Authoring Session boundary.
    pub(super) fn clear_all_preview_residency(&self) {
        self.future_media_window.borrow_mut().clear();
        self.evaluation_working_set.borrow_mut().clear();
        self.frame_store.borrow_mut().clear_all();
    }
}
