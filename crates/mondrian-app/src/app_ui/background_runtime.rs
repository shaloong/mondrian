//! Bounded ownership for the Tokio runtime used by Window background work.
//!
//! The Window thread receives only a runtime Handle. A native supervisor owns
//! Runtime destruction so a blocking Tokio task cannot turn UI shutdown into
//! an unbounded `Runtime::drop` wait.

use std::time::Instant;

use crate::app::owned_worker_lifecycle::OwnedWorkerShutdown;

pub(crate) const APP_UI_BACKGROUND_WORKERS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AppUiBackgroundRuntimeShutdownEvidence {
    runtime_handoff_completed: bool,
    configured_worker_threads: Option<usize>,
    shutdown_signal_delivered: bool,
    supervisor: OwnedWorkerShutdown,
}

impl AppUiBackgroundRuntimeShutdownEvidence {
    pub(super) fn all_created_resources_released(&self) -> bool {
        match self.supervisor {
            OwnedWorkerShutdown::NotStarted => !self.runtime_handoff_completed,
            OwnedWorkerShutdown::Terminated => true,
            OwnedWorkerShutdown::Panicked
            | OwnedWorkerShutdown::PanickedPayloadAbandoned
            | OwnedWorkerShutdown::CurrentThreadSkipped
            | OwnedWorkerShutdown::TimedOutDetached => false,
        }
    }

    fn qualifies_normal_runtime(&self) -> bool {
        self.runtime_handoff_completed
            && self.configured_worker_threads == Some(APP_UI_BACKGROUND_WORKERS)
            && self.shutdown_signal_delivered
            && self.supervisor == OwnedWorkerShutdown::Terminated
    }

    pub(super) fn qualification_failure(&self) -> Option<String> {
        (!self.qualifies_normal_runtime())
            .then(|| format!("Window background runtime did not close cleanly: {self:?}"))
    }
}

pub(super) struct AppUiBackgroundRuntimeOwner {
    handle: Option<tokio::runtime::Handle>,
    shutdown: Option<std::sync::mpsc::Sender<()>>,
    supervisor: Option<std::thread::JoinHandle<()>>,
    runtime_handoff_completed: bool,
}

impl AppUiBackgroundRuntimeOwner {
    pub(super) fn start(deadline: Instant) -> Result<Self, AppUiBackgroundRuntimeStartupFailure> {
        if Instant::now() >= deadline {
            return Err(AppUiBackgroundRuntimeStartupFailure {
                diagnostic: "Window background runtime startup deadline elapsed".to_owned(),
                owner: Self {
                    handle: None,
                    shutdown: None,
                    supervisor: None,
                    runtime_handoff_completed: false,
                },
            });
        }
        let (startup_tx, startup_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
        let supervisor = match std::thread::Builder::new()
            .name("mondrian-bg-supervisor".to_owned())
            .spawn(move || {
                let runtime = match build_app_ui_background_runtime() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = startup_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let handle = runtime.handle().clone();
                if startup_tx.send(Ok(handle)).is_err() {
                    return;
                }
                let _ = shutdown_rx.recv();
                drop(runtime);
            }) {
            Ok(supervisor) => supervisor,
            Err(error) => {
                return Err(AppUiBackgroundRuntimeStartupFailure {
                    diagnostic: format!("could not spawn Window background runtime owner: {error}"),
                    owner: Self {
                        handle: None,
                        shutdown: None,
                        supervisor: None,
                        runtime_handoff_completed: false,
                    },
                });
            }
        };
        let mut owner = Self {
            handle: None,
            shutdown: Some(shutdown_tx),
            supervisor: Some(supervisor),
            runtime_handoff_completed: false,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        match startup_rx.recv_timeout(remaining) {
            Ok(Ok(handle)) => {
                owner.handle = Some(handle);
                owner.runtime_handoff_completed = true;
                Ok(owner)
            }
            Ok(Err(error)) => Err(AppUiBackgroundRuntimeStartupFailure {
                diagnostic: format!("could not build Window background runtime: {error}"),
                owner,
            }),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(AppUiBackgroundRuntimeStartupFailure {
                    diagnostic: "Window background runtime startup deadline elapsed".to_owned(),
                    owner,
                })
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(AppUiBackgroundRuntimeStartupFailure {
                    diagnostic: "Window background runtime owner exited before startup handoff"
                        .to_owned(),
                    owner,
                })
            }
        }
    }

    pub(super) fn enter(&self) -> tokio::runtime::EnterGuard<'_> {
        self.handle
            .as_ref()
            .expect("started Window background runtime retains its Handle")
            .enter()
    }

    pub(super) fn shutdown_until(
        mut self,
        deadline: Instant,
    ) -> AppUiBackgroundRuntimeShutdownEvidence {
        drop(self.handle.take());
        let shutdown_signal_delivered =
            self.shutdown.take().is_some_and(|shutdown| shutdown.send(()).is_ok());
        let supervisor = self.supervisor.take().map_or(OwnedWorkerShutdown::NotStarted, |worker| {
            OwnedWorkerShutdown::join_until(worker, deadline)
        });
        AppUiBackgroundRuntimeShutdownEvidence {
            runtime_handoff_completed: self.runtime_handoff_completed,
            configured_worker_threads: self
                .runtime_handoff_completed
                .then_some(APP_UI_BACKGROUND_WORKERS),
            shutdown_signal_delivered,
            supervisor,
        }
    }
}

impl Drop for AppUiBackgroundRuntimeOwner {
    fn drop(&mut self) {
        drop(self.handle.take());
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        // A fallback Drop cannot wait without the caller's deadline and is not
        // qualification evidence. The supervisor owns any still-running Tokio
        // destructor rather than blocking this Window thread indefinitely.
        drop(self.supervisor.take());
    }
}

pub(super) struct AppUiBackgroundRuntimeStartupFailure {
    diagnostic: String,
    owner: AppUiBackgroundRuntimeOwner,
}

impl AppUiBackgroundRuntimeStartupFailure {
    pub(super) fn shutdown_until(self, deadline: Instant) -> AppUiBackgroundRuntimeStartupClosed {
        AppUiBackgroundRuntimeStartupClosed {
            diagnostic: self.diagnostic,
            shutdown: self.owner.shutdown_until(deadline),
        }
    }
}

pub(super) struct AppUiBackgroundRuntimeStartupClosed {
    pub(super) diagnostic: String,
    pub(super) shutdown: AppUiBackgroundRuntimeShutdownEvidence,
}

fn build_app_ui_background_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(APP_UI_BACKGROUND_WORKERS)
        .thread_name("mondrian-bg")
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn background_runtime_uses_product_worker_count() {
        assert_eq!(APP_UI_BACKGROUND_WORKERS, 4);
        let deadline = Instant::now() + Duration::from_secs(2);
        let runtime = match AppUiBackgroundRuntimeOwner::start(deadline) {
            Ok(runtime) => runtime,
            Err(failure) => {
                let closed = failure.shutdown_until(deadline);
                panic!(
                    "runtime should build: {} ({:?})",
                    closed.diagnostic, closed.shutdown
                );
            }
        };
        {
            let _guard = runtime.enter();
            tokio::runtime::Handle::current().block_on(async {});
        }
        let evidence = runtime.shutdown_until(deadline);
        assert!(evidence.qualifies_normal_runtime(), "{evidence:?}");
        assert!(evidence.all_created_resources_released());
    }

    #[test]
    fn background_runtime_timeout_evidence_fails_closed() {
        let evidence = AppUiBackgroundRuntimeShutdownEvidence {
            runtime_handoff_completed: true,
            configured_worker_threads: Some(APP_UI_BACKGROUND_WORKERS),
            shutdown_signal_delivered: true,
            supervisor: OwnedWorkerShutdown::TimedOutDetached,
        };

        assert!(!evidence.qualifies_normal_runtime());
        assert!(!evidence.all_created_resources_released());
        assert!(evidence.qualification_failure().is_some());
    }

    #[test]
    fn expired_deadline_does_not_start_background_runtime_owner() {
        let deadline = Instant::now();
        let failure = match AppUiBackgroundRuntimeOwner::start(deadline) {
            Ok(runtime) => {
                let evidence = runtime.shutdown_until(deadline);
                panic!("expired deadline unexpectedly started runtime: {evidence:?}");
            }
            Err(failure) => failure,
        };
        let closed = failure.shutdown_until(deadline);

        assert_eq!(closed.shutdown.supervisor, OwnedWorkerShutdown::NotStarted);
        assert!(!closed.shutdown.runtime_handoff_completed);
        assert_eq!(closed.shutdown.configured_worker_threads, None);
        assert!(closed.shutdown.all_created_resources_released());
        assert!(!closed.shutdown.qualifies_normal_runtime());
    }

    #[test]
    fn background_runtime_shutdown_deadline_does_not_wait_for_blocking_task() {
        let startup_deadline = Instant::now() + Duration::from_secs(2);
        let runtime = match AppUiBackgroundRuntimeOwner::start(startup_deadline) {
            Ok(runtime) => runtime,
            Err(failure) => {
                let closed = failure.shutdown_until(startup_deadline);
                panic!(
                    "runtime should build: {} ({:?})",
                    closed.diagnostic, closed.shutdown
                );
            }
        };
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        {
            let _guard = runtime.enter();
            drop(tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv();
            }));
        }
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocking task should start");

        let shutdown_started = Instant::now();
        let evidence = runtime.shutdown_until(shutdown_started + Duration::from_millis(25));
        let elapsed = shutdown_started.elapsed();
        let _ = release_tx.send(());

        assert_eq!(evidence.supervisor, OwnedWorkerShutdown::TimedOutDetached);
        assert!(!evidence.qualifies_normal_runtime());
        assert!(!evidence.all_created_resources_released());
        assert!(elapsed < Duration::from_millis(500), "elapsed={elapsed:?}");
    }
}
