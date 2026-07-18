//! Preview worker and interactive-work lifecycle owned by the service.

use super::*;

impl AppUiPreviewService {
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
                "failed to start app UI viewer preview reaper; workers will finish detached: {err}"
            );
        }
    }
}
