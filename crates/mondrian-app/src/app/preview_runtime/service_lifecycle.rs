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
            self.frame_store.borrow_mut().clear_all();
        }
        self.future_media_window.borrow_mut().clear();
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
        self.frame_store.borrow_mut().clear_decoder_resource_media_frames();
        self.jobs.interrupt_workers_for_lifecycle();
    }

    /// Release decoder-backed media residency after all Preview work is idle.
    ///
    /// This preserves Viewer output and failure memory. A retained native
    /// media frame can pin its decoder's entire hardware-surface pool, so entry
    /// and byte budgets alone are not sufficient at a transport-idle boundary.
    /// The operation fails closed if queued, in-flight, or unresolved work is
    /// still visible to the Broker.
    pub(crate) fn try_release_idle_media_residency(&self) -> bool {
        let scheduler = self.scheduler.diagnostics();
        let queue = self.jobs.diagnostics();
        if self.execution.borrow().is_pending()
            || scheduler.pending_requests != 0
            || queue.queued_jobs != 0
            || queue.in_flight_jobs != 0
        {
            return false;
        }
        self.frame_store.borrow_mut().clear_media_frames();
        true
    }

    /// Release idle media only after a stopped transport has a durable final
    /// GPU Viewer output proved under the active Preview generation.
    pub(crate) fn try_release_settled_transport_media_residency(&self) -> bool {
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
        self.frame_store.borrow_mut().clear_all();
    }

    fn retire_obsolete_transport_work(&self) {
        self.decode_residency_waiting.set(None);
        self.media_aggregate_capacity_waiting.set(false);
        self.media_existing_work_waiters.borrow_mut().clear();
        self.media_existing_work_retry_pending.set(false);
        let pending_requests = self.scheduler.diagnostics().pending_requests as u64;
        let (generation, queued_jobs) = self.scheduler.cancel_all();
        if let Some(task) = &self.visual_execution {
            task.prune_before(generation);
        }
        self.visual_ready.borrow_mut().clear();
        self.visual_failures.borrow_mut().clear();
        self.visual_terminal_candidates.borrow_mut().clear();
        let queued_jobs = queued_jobs as u64;
        bump(&self.metrics.interactive_cancel_requests);
        add_cell(
            &self.metrics.interactive_cancel_scheduler_requests,
            pending_requests,
        );
        add_cell(&self.metrics.interactive_cancel_queued_jobs, queued_jobs);
        add_cell(&self.metrics.queue_canceled_jobs, queued_jobs);
        self.execution.borrow_mut().invalidate(|| generation);
    }

    /// Shut down preview workers for application exit.
    pub fn shutdown(&self) {
        self.jobs.close();
        let already_shutdown = self.shutdown.request();
        self.future_media_window.borrow_mut().clear();
        self.retire_obsolete_transport_work();
        self.frame_store.borrow_mut().clear_all();
        if !already_shutdown {
            self.reap_workers_async();
        }
    }

    fn reap_workers_async(&self) {
        let handles = self.workers.borrow_mut().drain(..).collect::<Vec<_>>();
        if handles.is_empty() {
            return;
        }

        if let Err(err) = thread::Builder::new()
            .name("mondrian-ui-viewer-preview-reaper".to_owned())
            .spawn(move || join_preview_workers(handles))
        {
            tracing::warn!(
                "failed to start production preview reaper; workers will finish detached: {err}"
            );
        }
    }
}
