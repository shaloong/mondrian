//! Non-blocking lifecycle owner for the concrete realtime audio output stream.

use crate::audio::{
    AudioBuffer, RealtimeAudioOutput, RealtimeAudioOutputHandle, RealtimeAudioOutputSnapshot,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
const DEVICE_HEALTH_POLL: Duration = Duration::from_millis(20);

/// One lifecycle transition emitted while polling [`RealtimeAudioOutputManager`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeAudioOutputEvent {
    /// A new concrete stream is ready but remains inactive for PCM preroll.
    Opened { stream_generation: u64 },
    /// The concrete stream reported an asynchronous backend failure.
    Lost { stream_generation: u64 },
    /// One background open attempt failed and a bounded retry was scheduled.
    OpenFailed {
        retry_after: Duration,
        reason: String,
    },
}

enum WorkerEvent {
    Opened(RealtimeAudioOutputHandle),
    Lost {
        stream_generation: u64,
    },
    OpenFailed {
        retry_after: Duration,
        reason: String,
    },
}

/// Deep Module owning background open, failure detection, and bounded reopen.
///
/// CPAL streams are deliberately `!Send`; the device thread therefore retains
/// concrete stream ownership for its entire lifetime and publishes only a
/// sendable lock-free control/observation handle. The caller never blocks on
/// device discovery, stream creation, failure polling, or retry delay.
pub struct RealtimeAudioOutputManager {
    handle: Option<RealtimeAudioOutputHandle>,
    event_rx: Receiver<WorkerEvent>,
    shutdown: Arc<AtomicBool>,
}

impl RealtimeAudioOutputManager {
    /// Start a dedicated output-device lifecycle thread.
    pub fn new(sample_rate: u32, channels: u8) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let _ = thread::Builder::new().name("mondrian-audio-device".to_owned()).spawn(move || {
            let mut consecutive_failures = 0_u32;
            while !worker_shutdown.load(Ordering::Acquire) {
                match RealtimeAudioOutput::try_new(sample_rate, channels) {
                    Ok(output) => {
                        consecutive_failures = 0;
                        let handle = output.handle();
                        let stream_generation = handle.snapshot().stream_generation;
                        if event_tx.send(WorkerEvent::Opened(handle)).is_err() {
                            break;
                        }
                        while !worker_shutdown.load(Ordering::Acquire)
                            && !output.snapshot().stream_failed
                        {
                            thread::sleep(DEVICE_HEALTH_POLL);
                        }
                        if worker_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        if event_tx.send(WorkerEvent::Lost { stream_generation }).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        let retry_after = retry_delay(consecutive_failures);
                        if event_tx
                            .send(WorkerEvent::OpenFailed {
                                retry_after,
                                reason: error.to_string(),
                            })
                            .is_err()
                        {
                            break;
                        }
                        interruptible_sleep(retry_after, &worker_shutdown);
                    }
                }
            }
        });
        Self { handle: None, event_rx, shutdown }
    }

    /// Apply at most one pending lifecycle transition without blocking.
    pub fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        match self.event_rx.try_recv().ok()? {
            WorkerEvent::Opened(handle) => {
                let stream_generation = handle.snapshot().stream_generation;
                self.handle = Some(handle);
                Some(RealtimeAudioOutputEvent::Opened { stream_generation })
            }
            WorkerEvent::Lost { stream_generation } => {
                self.handle = None;
                Some(RealtimeAudioOutputEvent::Lost { stream_generation })
            }
            WorkerEvent::OpenFailed { retry_after, reason } => {
                Some(RealtimeAudioOutputEvent::OpenFailed { retry_after, reason })
            }
        }
    }

    /// Queue rendered PCM on the current stream, if one exists.
    pub fn enqueue(&self, buffer: &AudioBuffer) {
        if let Some(handle) = &self.handle {
            handle.enqueue(buffer);
        }
    }

    /// Drop all queued PCM without affecting lifecycle retries.
    pub fn clear(&self) {
        if let Some(handle) = &self.handle {
            handle.clear();
        }
    }

    /// Enable or disable callback consumption on the current stream.
    pub fn set_active(&self, active: bool) {
        if let Some(handle) = &self.handle {
            handle.set_active(active);
        }
    }

    /// Apply output mute without conflating it with transport activation.
    pub fn set_muted(&self, muted: bool) {
        if let Some(handle) = &self.handle {
            handle.set_muted(muted);
        }
    }

    /// Return PCM frames currently queued on the current stream.
    pub fn buffered_frames(&self) -> usize {
        self.handle.as_ref().map_or(0, RealtimeAudioOutputHandle::buffered_frames)
    }

    /// Capture immutable callback and stream health evidence.
    pub fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        self.handle.as_ref().map(RealtimeAudioOutputHandle::snapshot)
    }
}

impl Drop for RealtimeAudioOutputManager {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
    }
}

fn interruptible_sleep(duration: Duration, shutdown: &AtomicBool) {
    let mut remaining = duration;
    while !remaining.is_zero() && !shutdown.load(Ordering::Acquire) {
        let step = remaining.min(Duration::from_millis(50));
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
}

fn retry_delay(consecutive_failures: u32) -> Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(5);
    INITIAL_RETRY_DELAY.saturating_mul(1_u32 << exponent).min(MAX_RETRY_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_exponential_and_bounded() {
        assert_eq!(retry_delay(1), Duration::from_millis(250));
        assert_eq!(retry_delay(2), Duration::from_millis(500));
        assert_eq!(retry_delay(3), Duration::from_secs(1));
        assert_eq!(retry_delay(10), MAX_RETRY_DELAY);
    }
}
