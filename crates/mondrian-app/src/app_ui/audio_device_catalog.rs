//! Non-blocking Window Adapter for realtime audio-output discovery.
//!
//! Native device enumeration may enter an operating-system audio service and
//! therefore never runs on the winit thread or in the realtime audio callback.
//! The Adapter owns at most one bounded, low-frequency discovery attempt.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

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
}

impl AudioOutputDeviceCatalogAdapter {
    /// Start with an immediate production discovery attempt.
    pub fn new() -> Self {
        let adapter = Self {
            state: AudioOutputDeviceCatalogState::Loading,
            result_rx: None,
            worker: None,
            discover: discover_realtime_audio_output_devices,
        };
        #[cfg(not(test))]
        let adapter = {
            let mut adapter = adapter;
            adapter.request_refresh();
            adapter
        };
        adapter
    }

    /// Latest immutable catalog state.
    pub fn state(&self) -> &AudioOutputDeviceCatalogState {
        &self.state
    }

    /// Request a fresh observation. A running attempt retains sole authority.
    pub fn request_refresh(&mut self) -> bool {
        if self.worker.is_some() {
            return false;
        }
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let discover = self.discover;
        self.state = AudioOutputDeviceCatalogState::Loading;
        self.result_rx = Some(result_rx);
        match thread::Builder::new().name("mondrian-audio-device-discovery".to_owned()).spawn(
            move || {
                let result = discover();
                let _ = result_tx.send(result);
            },
        ) {
            Ok(worker) => {
                self.worker = Some(worker);
                true
            }
            Err(error) => {
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
        let Some(result_rx) = self.result_rx.as_ref() else {
            return false;
        };
        let next = match result_rx.try_recv() {
            Ok(result) => Some(match result {
                Ok(catalog) => AudioOutputDeviceCatalogState::Ready(catalog),
                Err(failure) => AudioOutputDeviceCatalogState::Failed(failure.to_string()),
            }),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(AudioOutputDeviceCatalogState::Failed(
                "audio output discovery worker terminated without evidence".to_owned(),
            )),
        };
        let Some(next) = next else {
            return false;
        };
        self.result_rx = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let changed = self.state != next;
        self.state = next;
        changed
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
        self.result_rx = None;
        self.worker = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture_discovery() -> DiscoveryResult {
        Ok(RealtimeAudioOutputDeviceCatalog {
            host_name: "fixture".to_owned(),
            devices: Vec::new(),
        })
    }

    #[test]
    fn discovery_runs_off_thread_and_publishes_one_immutable_snapshot() {
        let mut adapter = AudioOutputDeviceCatalogAdapter {
            state: AudioOutputDeviceCatalogState::Loading,
            result_rx: None,
            worker: None,
            discover: fixture_discovery,
        };
        assert!(adapter.request_refresh());
        assert!(
            !adapter.request_refresh(),
            "one attempt owns discovery authority"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !adapter.poll_finished() {
            assert!(Instant::now() < deadline, "discovery worker did not finish");
            std::thread::yield_now();
        }
        assert_eq!(
            adapter.state(),
            &AudioOutputDeviceCatalogState::Ready(RealtimeAudioOutputDeviceCatalog {
                host_name: "fixture".to_owned(),
                devices: Vec::new(),
            })
        );
    }
}
