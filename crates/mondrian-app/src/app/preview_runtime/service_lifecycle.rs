//! Preview worker and interactive-work lifecycle owned by the production Runtime.

use super::*;

impl<O: Clone> PreviewProductionRuntime<O> {
    /// Observe the transport-family boundary and retire the opposite family
    /// before it may accumulate another hardware decoder surface pool.
    pub(super) fn observe_transport_activity(&self, playing: bool) {
        self.transport_playing.set(playing);
        let family = if playing {
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
    pub fn cancel_interactive_work(&self) {
        let pending_requests = self.scheduler.diagnostics().pending_requests as u64;
        let (generation, queued_jobs) = self.scheduler.cancel_all();
        let queued_jobs = queued_jobs as u64;
        bump(&self.metrics.interactive_cancel_requests);
        add_cell(
            &self.metrics.interactive_cancel_scheduler_requests,
            pending_requests,
        );
        add_cell(&self.metrics.interactive_cancel_queued_jobs, queued_jobs);
        add_cell(&self.metrics.queue_canceled_jobs, queued_jobs);
        self.execution.borrow_mut().invalidate(|| generation);
        self.frame_store.borrow_mut().clear_all();
    }

    /// Shut down preview workers for application exit.
    pub fn shutdown(&self) {
        self.jobs.close();
        let already_shutdown = self.shutdown.request();
        self.cancel_interactive_work();
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
