//! Non-blocking Window Adapter for realtime audio-output discovery.
//!
//! Native device enumeration may enter an operating-system audio service and
//! therefore never runs on the winit thread or in the realtime audio callback.
//! The Adapter owns at most one bounded, low-frequency discovery attempt.

use crate::app::owned_worker_lifecycle::OwnedWorkerShutdown;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use mondrian_media::{
    discover_realtime_audio_output_devices, RealtimeAudioOutputDeviceCatalog,
    RealtimeAudioOutputDiscoveryFailure,
};

/// UI-facing state of the latest physical output-device observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioOutputDeviceCatalogState {
    /// One discovery attempt is running outside the UI thread.
    Loading,
    /// A complete observation from the current default CPAL host.
    Ready(RealtimeAudioOutputDeviceCatalog),
    /// Discovery completed but the host could not enumerate its devices.
    Failed(String),
}

type DiscoveryResult =
    Result<RealtimeAudioOutputDeviceCatalog, RealtimeAudioOutputDiscoveryFailure>;

/// Domain-owned one-shot worker used by the Window host.
pub struct AudioOutputDeviceCatalogAdapter {
    state: AudioOutputDeviceCatalogState,
    result_rx: Option<Receiver<DiscoveryResult>>,
    worker: Option<JoinHandle<()>>,
    discover: fn() -> DiscoveryResult,
    shutdown: AudioDeviceCatalogShutdownEvidence,
    closed: bool,
}

/// Bounded cumulative raw inventory for all device-discovery attempts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct AudioDeviceCatalogShutdownEvidence {
    /// Receipt schema.
    pub schema_version: u32,
    /// Further native discovery and result publication were revoked.
    pub admission_closed: bool,
    /// Production requires the initial discovery attempt; test-disabled is explicit.
    pub initial_discovery_required: bool,
    /// Requests that attempted native worker creation.
    pub startup_attempts: u64,
    /// Returned native worker handles.
    pub workers_started: u64,
    /// Ordinary native worker-creation failures.
    pub worker_start_failures: u64,
    /// Native handles joined, including panics.
    pub workers_joined: u64,
    /// Joined workers that panicked.
    pub worker_panics: u64,
    /// Opaque panic payloads whose destructors were not safely executed.
    pub panic_payloads_abandoned: u64,
    /// Workers still running at the original shutdown deadline.
    pub deadline_detachments: u64,
    /// Same-thread joins that could not be performed.
    pub current_thread_detachments: u64,
    /// Terminated workers that failed to publish one typed result.
    pub results_missing: u64,
}

/// Exact Host-startup state of one freshly prepared catalog Adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDeviceCatalogStartupState {
    /// The inert Adapter exists and native discovery has not been attempted.
    Prepared,
    /// Native discovery was entered but did not return a typed outcome.
    InProgress,
    /// Native worker creation returned an ordinary operating-system failure.
    OrdinaryFailed,
    /// A native discovery worker handle was returned.
    Started,
}

impl AudioDeviceCatalogShutdownEvidence {
    /// No unknown, unjoined, panicked or detached discovery attempt remains.
    pub fn all_resources_released(self) -> bool {
        self.schema_version == 1
            && self.admission_closed
            && (!self.initial_discovery_required || self.startup_attempts > 0)
            && self.startup_attempts
                == self.workers_started.saturating_add(self.worker_start_failures)
            && self.workers_started == self.workers_joined
            && self.worker_start_failures == 0
            && self.worker_panics == 0
            && self.panic_payloads_abandoned == 0
            && self.deadline_detachments == 0
            && self.current_thread_detachments == 0
            && self.results_missing == 0
    }

    /// Closure of exactly the attempts made before a later Host startup failure.
    /// A required but not-yet-attempted initial discovery is valid partial inventory.
    pub fn all_created_resources_released(self, startup: AudioDeviceCatalogStartupState) -> bool {
        let counts_match = match startup {
            AudioDeviceCatalogStartupState::Prepared => {
                self.startup_attempts == 0
                    && self.workers_started == 0
                    && self.worker_start_failures == 0
                    && self.workers_joined == 0
            }
            AudioDeviceCatalogStartupState::InProgress => false,
            AudioDeviceCatalogStartupState::OrdinaryFailed => {
                self.startup_attempts == 1
                    && self.workers_started == 0
                    && self.worker_start_failures == 1
                    && self.workers_joined == 0
            }
            AudioDeviceCatalogStartupState::Started => {
                self.startup_attempts == 1
                    && self.workers_started == 1
                    && self.worker_start_failures == 0
                    && self.workers_joined == 1
            }
        };
        counts_match
            && self.schema_version == 1
            && self.admission_closed
            && self.startup_attempts
                == self.workers_started.saturating_add(self.worker_start_failures)
            && self.workers_started == self.workers_joined
            && self.worker_panics == 0
            && self.panic_payloads_abandoned == 0
            && self.deadline_detachments == 0
            && self.current_thread_detachments == 0
            && self.results_missing == 0
    }

    fn record(&mut self, outcome: OwnedWorkerShutdown) {
        use OwnedWorkerShutdown::*;
        match outcome {
            NotStarted => {}
            Terminated | Panicked | PanickedPayloadAbandoned => {
                self.workers_joined = self.workers_joined.saturating_add(1);
                if outcome != Terminated {
                    self.worker_panics = self.worker_panics.saturating_add(1);
                }
                if outcome == PanickedPayloadAbandoned {
                    self.panic_payloads_abandoned = self.panic_payloads_abandoned.saturating_add(1);
                }
            }
            TimedOutDetached => {
                self.deadline_detachments = self.deadline_detachments.saturating_add(1)
            }
            CurrentThreadSkipped => {
                self.current_thread_detachments = self.current_thread_detachments.saturating_add(1)
            }
        }
    }
}

impl AudioOutputDeviceCatalogAdapter {
    /// Start with an immediate production discovery attempt.
    pub fn new() -> Self {
        let adapter = Self::prepare();
        #[cfg(not(test))]
        let adapter = {
            let mut adapter = adapter;
            adapter.request_refresh();
            adapter
        };
        adapter
    }

    /// Prepare a main-thread Adapter before starting any native discovery.
    pub(crate) fn prepare() -> Self {
        Self {
            state: AudioOutputDeviceCatalogState::Loading,
            result_rx: None,
            worker: None,
            discover: discover_realtime_audio_output_devices,
            shutdown: AudioDeviceCatalogShutdownEvidence {
                schema_version: 1,
                initial_discovery_required: !cfg!(test),
                ..Default::default()
            },
            closed: false,
        }
    }

    /// Latest immutable catalog state.
    pub fn state(&self) -> &AudioOutputDeviceCatalogState {
        &self.state
    }

    /// Request a fresh observation. A running attempt retains sole authority.
    pub fn request_refresh(&mut self) -> bool {
        self.start_discovery(self.discover)
    }

    fn start_discovery(
        &mut self,
        discover: impl FnOnce() -> DiscoveryResult + Send + 'static,
    ) -> bool {
        if self.closed || self.worker.is_some() {
            return false;
        }
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        self.state = AudioOutputDeviceCatalogState::Loading;
        self.result_rx = Some(result_rx);
        self.shutdown.startup_attempts = self.shutdown.startup_attempts.saturating_add(1);
        match thread::Builder::new().name("mondrian-audio-device-discovery".to_owned()).spawn(
            move || {
                let result = discover();
                let _ = result_tx.send(result);
            },
        ) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.shutdown.workers_started = self.shutdown.workers_started.saturating_add(1);
                true
            }
            Err(error) => {
                self.shutdown.worker_start_failures =
                    self.shutdown.worker_start_failures.saturating_add(1);
                self.result_rx = None;
                self.state = AudioOutputDeviceCatalogState::Failed(format!(
                    "audio output discovery worker could not start: {error}"
                ));
                false
            }
        }
    }

    /// Publish at most one completed observation to the Window model.
    pub fn poll_finished(&mut self) -> bool {
        if self.closed || !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return false;
        }
        let Some(result_rx) = self.result_rx.as_ref() else {
            return false;
        };
        let next = match result_rx.try_recv() {
            Ok(result) => Some(match result {
                Ok(catalog) => AudioOutputDeviceCatalogState::Ready(catalog),
                Err(failure) => AudioOutputDeviceCatalogState::Failed(failure.to_string()),
            }),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.shutdown.results_missing = self.shutdown.results_missing.saturating_add(1);
                Some(AudioOutputDeviceCatalogState::Failed(
                    "audio output discovery worker terminated without evidence".to_owned(),
                ))
            }
        };
        let Some(next) = next else {
            return false;
        };
        self.result_rx = None;
        if let Some(worker) = self.worker.take() {
            self.shutdown.record(OwnedWorkerShutdown::join(worker));
        }
        let changed = self.state != next;
        self.state = next;
        changed
    }

    /// Revoke publication and future discovery before any Host worker is joined.
    pub fn begin_shutdown(&mut self) {
        self.closed = true;
        self.result_rx = None;
        self.shutdown.admission_closed = true;
    }

    /// Join the current native attempt under the shared original Host deadline.
    pub fn shutdown_until(&mut self, deadline: Instant) -> AudioDeviceCatalogShutdownEvidence {
        self.begin_shutdown();
        if let Some(worker) = self.worker.take() {
            self.shutdown.record(OwnedWorkerShutdown::join_until(worker, deadline));
        }
        self.shutdown
    }
}

impl Default for AudioOutputDeviceCatalogAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioOutputDeviceCatalogAdapter {
    fn drop(&mut self) {
        // Native enumeration has no portable cancellation contract. Dropping
        // the receiver revokes publication authority immediately; detaching a
        // still-blocked one-shot worker keeps Window shutdown bounded. The
        // worker owns no App, Project, device stream, or other mutable state.
        self.begin_shutdown();
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(worker) = self.worker.take()
        {
            self.shutdown.record(OwnedWorkerShutdown::join(worker));
        }
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/audio_device_catalog.rs"]
mod tests;
