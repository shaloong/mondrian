//! Non-blocking lifecycle owner for the concrete realtime audio output stream.

use crate::audio::{
    AudioBuffer, RealtimeAudioOutput, RealtimeAudioOutputControlError,
    RealtimeAudioOutputEnqueueError, RealtimeAudioOutputHandle, RealtimeAudioOutputQuiescenceToken,
    RealtimeAudioOutputSnapshot,
};
use crate::audio_device::current_default_realtime_audio_output_device_id;
use crate::owner_lifetime::{abandon_io_error, dispose_canonical_or_abandon_opaque_panic_payload};
use crate::{
    RealtimeAudioOutputDeviceEvidence, RealtimeAudioOutputDeviceId,
    RealtimeAudioOutputDeviceSelection, RealtimeAudioOutputOpenFailure,
};
use mondrian_core::AudioChannelLayout;
use parking_lot::Mutex;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

const INITIAL_RETRY_DELAY: Duration = Duration::from_millis(250);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
const DEVICE_HEALTH_POLL: Duration = Duration::from_millis(20);
const DEFAULT_DEVICE_IDENTITY_POLL: Duration = Duration::from_secs(1);
const DEVICE_WORKER_TERMINAL_UNKNOWN: u8 = 0;
const DEVICE_WORKER_TERMINAL_NORMAL: u8 = 1;
const DEVICE_WORKER_TERMINAL_PANICKED: u8 = 2;
const DEVICE_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED: u8 = 3;

/// One lifecycle transition emitted while polling [`RealtimeAudioOutputManager`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeAudioOutputEvent {
    /// A new concrete stream is ready but remains inactive for PCM preroll.
    Opened {
        stream_generation: u64,
        evidence: RealtimeAudioOutputDeviceEvidence,
    },
    /// The concrete stream was destroyed and its evidence is now frozen.
    Lost {
        reason: RealtimeAudioOutputLossReason,
        final_snapshot: RealtimeAudioOutputSnapshot,
    },
    /// One background open attempt failed and a bounded retry was scheduled.
    OpenFailed {
        retry_after: Duration,
        failure: RealtimeAudioOutputOpenFailure,
    },
    /// The owned device lifecycle worker could not be created.
    WorkerStartFailed { reason: String },
    /// The owned device lifecycle worker exited without an explicit shutdown.
    WorkerStoppedUnexpectedly { reason: String },
}

/// Why one concrete stream was retired before the lifecycle worker reopened it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RealtimeAudioOutputLossReason {
    /// The concrete physical stream reported an asynchronous backend failure.
    BackendFailure,
    /// A validation-only exact-generation recycle was accepted.
    ControlledRecycle,
    /// Callback deactivation failed, so retirement forced silence before drop.
    DeactivationFailed,
    /// A `SystemDefault` selection observed a different stable default identity.
    DefaultDeviceChanged,
    /// The user/runtime selected a different explicit device intent.
    DeviceSelectionChanged,
}

/// Failure to create the owned realtime device lifecycle worker.
#[derive(Debug, Error)]
#[error("failed to spawn realtime audio device worker: {0}")]
pub struct RealtimeAudioOutputWorkerStartError(#[source] io::Error);

/// Lifetime closure evidence for concrete audio-device lifecycle workers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RealtimeAudioOutputShutdownEvidence {
    /// Evidence schema version.
    pub schema_version: u32,
    /// Workers successfully started over this manager lifetime.
    pub workers_started: u32,
    /// Started workers synchronously joined by polling or final shutdown.
    pub workers_terminated: u32,
    /// Worker creation attempts that returned an error or panicked.
    pub worker_start_failures: u32,
    /// Worker spawners that panicked before returning a handle.
    pub worker_spawner_panics: u32,
    /// Joined workers whose thread body panicked.
    pub worker_panics: u32,
    /// Workers detached because the join was requested on that same thread.
    pub current_thread_detachments: u32,
    /// Foreign startup errors or opaque panic payloads deliberately abandoned.
    pub worker_owner_abandonments: u32,
    /// Joined workers whose supervisor terminal stamp was absent or invalid.
    pub worker_terminal_evidence_missing: u32,
}

impl RealtimeAudioOutputShutdownEvidence {
    /// Current-schema clean evidence for an Adapter that never owned a worker.
    pub const fn no_worker_owner() -> Self {
        Self {
            schema_version: 2,
            workers_started: 0,
            workers_terminated: 0,
            worker_start_failures: 0,
            worker_spawner_panics: 0,
            worker_panics: 0,
            current_thread_detachments: 0,
            worker_owner_abandonments: 0,
            worker_terminal_evidence_missing: 0,
        }
    }

    /// Whether every started device worker returned synchronously without panic.
    pub const fn all_workers_terminated(self) -> bool {
        self.schema_version == 2
            && self.workers_started == self.workers_terminated
            && self.worker_start_failures == 0
            && self.worker_spawner_panics == 0
            && self.worker_panics == 0
            && self.current_thread_detachments == 0
            && self.worker_owner_abandonments == 0
            && self.worker_terminal_evidence_missing == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceWorkerJoinOutcome {
    Terminated,
    Panicked,
    TerminalEvidenceMissing,
    CurrentThreadSkipped,
}

enum WorkerEvent {
    Opened(RealtimeAudioOutputHandle),
    Lost {
        reason: RealtimeAudioOutputLossReason,
        final_snapshot: RealtimeAudioOutputSnapshot,
    },
    OpenFailed {
        retry_after: Duration,
        failure: RealtimeAudioOutputOpenFailure,
    },
}

enum WorkerCommand {
    #[cfg(feature = "validation")]
    ControlledRecycle { expected_stream_generation: u64 },
}

#[cfg(feature = "validation")]
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RealtimeAudioOutputRecycleError {
    #[error("no concrete realtime output stream is available")]
    OutputUnavailable,
    #[error("expected stream generation {expected} but current generation is {actual}")]
    StreamGenerationMismatch { expected: u64, actual: u64 },
    #[error("realtime audio device worker command channel is unavailable")]
    WorkerUnavailable,
}

/// Deep Module owning background open, failure detection, and bounded reopen.
///
/// The device thread retains concrete CPAL/WASAPI stream ownership for its
/// entire lifetime and publishes only a sendable lock-free control/observation
/// handle. The caller never blocks on device discovery, stream creation,
/// failure polling, or retry delay.
pub struct RealtimeAudioOutputManager {
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    device_selection: Arc<Mutex<RealtimeAudioOutputDeviceSelection>>,
    handle: Option<RealtimeAudioOutputHandle>,
    event_rx: Option<Receiver<WorkerEvent>>,
    command_tx: Option<Sender<WorkerCommand>>,
    worker: Option<JoinHandle<()>>,
    worker_terminal: Arc<AtomicU8>,
    shutdown: Arc<AtomicBool>,
    next_worker_retry_at: Option<Instant>,
    consecutive_worker_failures: u32,
    shutdown_evidence: RealtimeAudioOutputShutdownEvidence,
}

impl RealtimeAudioOutputManager {
    /// Create a dormant manager. The first poll starts its device lifecycle thread.
    pub fn new(sample_rate: u32, channel_layout: AudioChannelLayout) -> Self {
        Self::new_with_device_selection(
            sample_rate,
            channel_layout,
            RealtimeAudioOutputDeviceSelection::SystemDefault,
        )
    }

    /// Create a dormant manager bound to one explicit runtime device intent.
    pub fn new_with_device_selection(
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        device_selection: RealtimeAudioOutputDeviceSelection,
    ) -> Self {
        Self {
            sample_rate,
            channel_layout,
            device_selection: Arc::new(Mutex::new(device_selection)),
            handle: None,
            event_rx: None,
            command_tx: None,
            worker: None,
            worker_terminal: Arc::new(AtomicU8::new(DEVICE_WORKER_TERMINAL_UNKNOWN)),
            shutdown: Arc::new(AtomicBool::new(false)),
            next_worker_retry_at: None,
            consecutive_worker_failures: 0,
            shutdown_evidence: RealtimeAudioOutputShutdownEvidence::no_worker_owner(),
        }
    }

    /// Start the owned device lifecycle worker without blocking on device open.
    pub fn start(&mut self) -> Result<(), RealtimeAudioOutputWorkerStartError> {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(RealtimeAudioOutputWorkerStartError(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "realtime audio output is shutting down",
            )));
        }
        if self.next_worker_retry_at.is_some_and(|retry_at| Instant::now() < retry_at) {
            return Err(RealtimeAudioOutputWorkerStartError(io::Error::new(
                io::ErrorKind::WouldBlock,
                "realtime audio device worker restart is in bounded backoff",
            )));
        }
        self.ensure_worker_started_with(spawn_device_worker)
    }

    fn ensure_worker_started_with(
        &mut self,
        spawner: impl FnOnce(
            u32,
            AudioChannelLayout,
            Arc<Mutex<RealtimeAudioOutputDeviceSelection>>,
            Arc<AtomicBool>,
            Sender<WorkerEvent>,
            Receiver<WorkerCommand>,
            Arc<AtomicU8>,
        ) -> io::Result<JoinHandle<()>>,
    ) -> Result<(), RealtimeAudioOutputWorkerStartError> {
        if self.worker.is_some() {
            return Ok(());
        }
        let (event_tx, event_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel();
        let worker_shutdown = Arc::clone(&self.shutdown);
        self.worker_terminal.store(DEVICE_WORKER_TERMINAL_UNKNOWN, Ordering::Release);
        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawner(
                self.sample_rate,
                self.channel_layout,
                Arc::clone(&self.device_selection),
                worker_shutdown,
                event_tx,
                command_rx,
                Arc::clone(&self.worker_terminal),
            )
        }));
        let worker = match spawn_result {
            Ok(Ok(worker)) => worker,
            Ok(Err(error)) => {
                let (kind, owner_abandoned) = abandon_io_error(error);
                self.shutdown_evidence.worker_start_failures =
                    self.shutdown_evidence.worker_start_failures.saturating_add(1);
                self.shutdown_evidence.worker_owner_abandonments = self
                    .shutdown_evidence
                    .worker_owner_abandonments
                    .saturating_add(u32::from(owner_abandoned));
                self.schedule_worker_retry();
                return Err(RealtimeAudioOutputWorkerStartError(io::Error::from(kind)));
            }
            Err(payload) => {
                self.shutdown_evidence.worker_start_failures =
                    self.shutdown_evidence.worker_start_failures.saturating_add(1);
                self.shutdown_evidence.worker_spawner_panics =
                    self.shutdown_evidence.worker_spawner_panics.saturating_add(1);
                if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                    self.shutdown_evidence.worker_owner_abandonments =
                        self.shutdown_evidence.worker_owner_abandonments.saturating_add(1);
                }
                self.schedule_worker_retry();
                return Err(RealtimeAudioOutputWorkerStartError(io::Error::other(
                    "realtime audio device worker spawner panicked",
                )));
            }
        };
        self.event_rx = Some(event_rx);
        self.command_tx = Some(command_tx);
        self.worker = Some(worker);
        self.shutdown_evidence.workers_started =
            self.shutdown_evidence.workers_started.saturating_add(1);
        self.next_worker_retry_at = None;
        Ok(())
    }

    /// Apply at most one pending lifecycle transition without blocking.
    pub fn poll(&mut self) -> Option<RealtimeAudioOutputEvent> {
        if self.shutdown.load(Ordering::Acquire) {
            return None;
        }
        if self.next_worker_retry_at.is_some_and(|retry_at| Instant::now() < retry_at) {
            return None;
        }
        if let Err(error) = self.start() {
            return Some(RealtimeAudioOutputEvent::WorkerStartFailed { reason: error.to_string() });
        }
        let event = match self.event_rx.as_ref()?.try_recv() {
            Ok(event) => event,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.event_rx = None;
                self.handle = None;
                let reason = match self.worker.take().map(|worker| self.join_worker(worker)) {
                    Some(DeviceWorkerJoinOutcome::Terminated) => {
                        "device worker exited without shutdown".to_owned()
                    }
                    Some(DeviceWorkerJoinOutcome::Panicked) => "device worker panicked".to_owned(),
                    Some(DeviceWorkerJoinOutcome::TerminalEvidenceMissing) => {
                        "device worker exited without supervisor terminal evidence".to_owned()
                    }
                    Some(DeviceWorkerJoinOutcome::CurrentThreadSkipped) => {
                        "device worker could not join itself".to_owned()
                    }
                    None => "device worker ownership was lost".to_owned(),
                };
                self.schedule_worker_retry();
                return Some(RealtimeAudioOutputEvent::WorkerStoppedUnexpectedly { reason });
            }
        };
        match event {
            WorkerEvent::Opened(handle) => {
                self.consecutive_worker_failures = 0;
                self.next_worker_retry_at = None;
                let stream_generation = handle.snapshot().stream_generation;
                let evidence = handle.device_evidence();
                self.handle = Some(handle);
                Some(RealtimeAudioOutputEvent::Opened { stream_generation, evidence })
            }
            WorkerEvent::Lost { reason, final_snapshot } => {
                self.handle = None;
                Some(RealtimeAudioOutputEvent::Lost { reason, final_snapshot })
            }
            WorkerEvent::OpenFailed { retry_after, failure } => {
                Some(RealtimeAudioOutputEvent::OpenFailed { retry_after, failure })
            }
        }
    }

    pub(crate) fn shutdown_and_wait(&mut self) -> RealtimeAudioOutputShutdownEvidence {
        self.stop_worker()
    }

    /// Close device admission and request worker exit without joining it.
    pub(crate) fn begin_shutdown(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.handle = None;
        self.event_rx = None;
        self.command_tx = None;
    }

    /// Clone the allocation-free cooperative stop signal captured by Audio Playback.
    pub(crate) fn shutdown_signal(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    fn schedule_worker_retry(&mut self) {
        self.consecutive_worker_failures = self.consecutive_worker_failures.saturating_add(1);
        self.next_worker_retry_at =
            Instant::now().checked_add(retry_delay(self.consecutive_worker_failures.max(1)));
    }

    fn stop_worker(&mut self) -> RealtimeAudioOutputShutdownEvidence {
        self.begin_shutdown();
        if let Some(worker) = self.worker.take() {
            self.join_worker(worker);
        }
        self.shutdown_evidence
    }

    /// Request ordinary-drop shutdown without waiting for foreign device code.
    ///
    /// Returns `true` when a still-running worker was detached. Its started
    /// count deliberately remains unmatched by a terminated count, so this
    /// path can never manufacture clean lifetime evidence.
    fn stop_worker_without_waiting(&mut self) -> bool {
        self.begin_shutdown();
        let Some(worker) = self.worker.take() else {
            return false;
        };
        if worker.is_finished() {
            self.join_worker(worker);
            false
        } else {
            drop(worker);
            true
        }
    }

    fn join_worker(&mut self, worker: JoinHandle<()>) -> DeviceWorkerJoinOutcome {
        let outcome = if worker.thread().id() == thread::current().id() {
            drop(worker);
            DeviceWorkerJoinOutcome::CurrentThreadSkipped
        } else {
            match worker.join() {
                Ok(()) => match self.worker_terminal.load(Ordering::Acquire) {
                    DEVICE_WORKER_TERMINAL_NORMAL => DeviceWorkerJoinOutcome::Terminated,
                    DEVICE_WORKER_TERMINAL_PANICKED => DeviceWorkerJoinOutcome::Panicked,
                    DEVICE_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED => {
                        self.shutdown_evidence.worker_owner_abandonments =
                            self.shutdown_evidence.worker_owner_abandonments.saturating_add(1);
                        DeviceWorkerJoinOutcome::Panicked
                    }
                    _ => DeviceWorkerJoinOutcome::TerminalEvidenceMissing,
                },
                Err(payload) => {
                    if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                        self.shutdown_evidence.worker_owner_abandonments =
                            self.shutdown_evidence.worker_owner_abandonments.saturating_add(1);
                    }
                    DeviceWorkerJoinOutcome::Panicked
                }
            }
        };
        match outcome {
            DeviceWorkerJoinOutcome::Terminated => {
                self.shutdown_evidence.workers_terminated =
                    self.shutdown_evidence.workers_terminated.saturating_add(1);
            }
            DeviceWorkerJoinOutcome::Panicked => {
                self.shutdown_evidence.workers_terminated =
                    self.shutdown_evidence.workers_terminated.saturating_add(1);
                self.shutdown_evidence.worker_panics =
                    self.shutdown_evidence.worker_panics.saturating_add(1);
            }
            DeviceWorkerJoinOutcome::TerminalEvidenceMissing => {
                self.shutdown_evidence.workers_terminated =
                    self.shutdown_evidence.workers_terminated.saturating_add(1);
                self.shutdown_evidence.worker_terminal_evidence_missing =
                    self.shutdown_evidence.worker_terminal_evidence_missing.saturating_add(1);
            }
            DeviceWorkerJoinOutcome::CurrentThreadSkipped => {
                self.shutdown_evidence.current_thread_detachments =
                    self.shutdown_evidence.current_thread_detachments.saturating_add(1);
            }
        }
        outcome
    }

    /// Queue rendered PCM on the current stream, if one exists.
    pub fn enqueue(&mut self, buffer: &AudioBuffer) -> Result<(), RealtimeAudioOutputEnqueueError> {
        let Some(handle) = self.handle.as_mut() else {
            return Err(RealtimeAudioOutputEnqueueError::OutputUnavailable);
        };
        handle.enqueue(buffer)
    }

    /// Drop all queued PCM without affecting lifecycle retries.
    pub fn clear(&self) {
        if let Some(handle) = &self.handle {
            handle.clear();
        }
    }

    /// Request callback quiescence for the current stream, if one exists.
    pub fn validate_deactivation(&self) -> Result<(), RealtimeAudioOutputControlError> {
        self.handle
            .as_ref()
            .map(RealtimeAudioOutputHandle::validate_deactivation)
            .transpose()
            .map(|_| ())
    }

    /// Request callback quiescence for the current stream, if one exists.
    pub fn deactivate(
        &self,
    ) -> Result<Option<RealtimeAudioOutputQuiescenceToken>, RealtimeAudioOutputControlError> {
        self.handle.as_ref().map(RealtimeAudioOutputHandle::deactivate).transpose()
    }

    /// Observe whether a deactivation token is fully acknowledged.
    pub fn is_quiescent(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
    ) -> Result<bool, RealtimeAudioOutputControlError> {
        let Some(handle) = &self.handle else {
            return Ok(false);
        };
        handle.is_quiescent(token)
    }

    /// Atomically trim an exact prefix and activate callback PCM consumption.
    pub fn activate_after_discard(
        &self,
        token: RealtimeAudioOutputQuiescenceToken,
        frames: usize,
    ) -> Result<(), RealtimeAudioOutputControlError> {
        let Some(handle) = &self.handle else {
            return Err(RealtimeAudioOutputControlError::OutputUnavailable);
        };
        handle.activate_after_discard(token, frames)
    }

    /// Return PCM frames currently queued on the current stream.
    pub fn buffered_frames(&self) -> usize {
        self.handle.as_ref().map_or(0, RealtimeAudioOutputHandle::buffered_frames)
    }

    /// Fixed complete-frame capacity of the current stream queue.
    pub fn capacity_frames(&self) -> Option<usize> {
        self.handle.as_ref().map(RealtimeAudioOutputHandle::capacity_frames)
    }

    /// Capture immutable callback and stream health evidence.
    pub fn snapshot(&self) -> Option<RealtimeAudioOutputSnapshot> {
        self.handle.as_ref().map(RealtimeAudioOutputHandle::snapshot)
    }

    /// Replace the latest-wins runtime device intent.
    ///
    /// The device worker observes this low-frequency value outside the audio
    /// callback, retires any stream opened for the old intent, and publishes a
    /// normal Lost/Open generation handoff. Returns `false` for a no-op.
    pub fn set_device_selection(&self, selection: RealtimeAudioOutputDeviceSelection) -> bool {
        let mut current = self.device_selection.lock();
        if *current == selection {
            return false;
        }
        *current = selection;
        true
    }

    #[cfg(feature = "validation")]
    pub(crate) fn request_controlled_recycle(
        &self,
        expected_stream_generation: u64,
    ) -> Result<(), RealtimeAudioOutputRecycleError> {
        validate_controlled_recycle_generation(
            self.snapshot().map(|snapshot| snapshot.stream_generation),
            expected_stream_generation,
        )?;
        self.command_tx
            .as_ref()
            .ok_or(RealtimeAudioOutputRecycleError::WorkerUnavailable)?
            .send(WorkerCommand::ControlledRecycle { expected_stream_generation })
            .map_err(|_| RealtimeAudioOutputRecycleError::WorkerUnavailable)
    }
}

#[cfg(feature = "validation")]
fn validate_controlled_recycle_generation(
    current_stream_generation: Option<u64>,
    expected_stream_generation: u64,
) -> Result<(), RealtimeAudioOutputRecycleError> {
    let current =
        current_stream_generation.ok_or(RealtimeAudioOutputRecycleError::OutputUnavailable)?;
    if current != expected_stream_generation {
        return Err(RealtimeAudioOutputRecycleError::StreamGenerationMismatch {
            expected: expected_stream_generation,
            actual: current,
        });
    }
    Ok(())
}

impl Drop for RealtimeAudioOutputManager {
    fn drop(&mut self) {
        let worker_detached = self.stop_worker_without_waiting();
        if self.shutdown_evidence.worker_panics > 0 {
            tracing::error!("realtime audio device worker panicked during shutdown");
        }
        if self.shutdown_evidence.current_thread_detachments > 0 {
            tracing::error!("realtime audio device worker could not synchronously join itself");
        }
        if worker_detached {
            tracing::warn!(
                "realtime audio device worker was still running and detached during ordinary drop"
            );
        }
    }
}

fn spawn_device_worker(
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    device_selection: Arc<Mutex<RealtimeAudioOutputDeviceSelection>>,
    worker_shutdown: Arc<AtomicBool>,
    event_tx: Sender<WorkerEvent>,
    command_rx: Receiver<WorkerCommand>,
    terminal: Arc<AtomicU8>,
) -> io::Result<JoinHandle<()>> {
    thread::Builder::new().name("mondrian-audio-device".to_owned()).spawn(move || {
        supervise_device_worker(&terminal, || {
            run_device_worker(
                sample_rate,
                channel_layout,
                device_selection,
                &worker_shutdown,
                &event_tx,
                &command_rx,
            )
        });
    })
}

fn supervise_device_worker(terminal: &AtomicU8, work: impl FnOnce()) {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
    let terminal_outcome = match outcome {
        Ok(()) => DEVICE_WORKER_TERMINAL_NORMAL,
        Err(payload) => {
            if dispose_canonical_or_abandon_opaque_panic_payload(payload) {
                DEVICE_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
            } else {
                DEVICE_WORKER_TERMINAL_PANICKED
            }
        }
    };
    terminal.store(terminal_outcome, Ordering::Release);
}

fn run_device_worker(
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    device_selection: Arc<Mutex<RealtimeAudioOutputDeviceSelection>>,
    worker_shutdown: &AtomicBool,
    event_tx: &Sender<WorkerEvent>,
    command_rx: &Receiver<WorkerCommand>,
) {
    let mut consecutive_failures = 0_u32;
    while !worker_shutdown.load(Ordering::Acquire) {
        let opened_selection = device_selection.lock().clone();
        match RealtimeAudioOutput::try_new(&opened_selection, sample_rate, channel_layout) {
            Ok((output, handle, observer)) => {
                consecutive_failures = 0;
                let stream_generation = handle.snapshot().stream_generation;
                let selected_device_id = handle.device_evidence().device_id;
                if event_tx.send(WorkerEvent::Opened(handle)).is_err() {
                    break;
                }
                let Some(mut loss_reason) = wait_for_stream_retirement(
                    &output,
                    stream_generation,
                    &opened_selection,
                    &device_selection,
                    &selected_device_id,
                    worker_shutdown,
                    command_rx,
                ) else {
                    break;
                };
                if let Err(error) = output.deactivate() {
                    output.force_inactive_for_retirement();
                    tracing::error!(%error, "callback deactivation failed before concrete stream retirement");
                    loss_reason = RealtimeAudioOutputLossReason::DeactivationFailed;
                }
                if drop_freeze_and_publish(
                    output,
                    || observer.snapshot(),
                    |final_snapshot| {
                        event_tx
                            .send(WorkerEvent::Lost { reason: loss_reason, final_snapshot })
                            .map_err(|_| ())
                    },
                )
                .is_err()
                {
                    break;
                }
            }
            Err(error) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let retry_after = retry_delay(consecutive_failures);
                let failure = error.into_open_failure(sample_rate, channel_layout);
                if event_tx.send(WorkerEvent::OpenFailed { retry_after, failure }).is_err() {
                    break;
                }
                interruptible_sleep(retry_after, worker_shutdown);
            }
        }
    }
}

fn wait_for_stream_retirement(
    output: &RealtimeAudioOutput,
    stream_generation: u64,
    opened_selection: &RealtimeAudioOutputDeviceSelection,
    desired_selection: &Mutex<RealtimeAudioOutputDeviceSelection>,
    selected_device_id: &RealtimeAudioOutputDeviceId,
    worker_shutdown: &AtomicBool,
    command_rx: &Receiver<WorkerCommand>,
) -> Option<RealtimeAudioOutputLossReason> {
    #[cfg(not(feature = "validation"))]
    let _ = stream_generation;
    let mut last_default_device_poll = Instant::now();
    loop {
        if worker_shutdown.load(Ordering::Acquire) {
            return None;
        }
        if output.snapshot().stream_failed {
            return Some(RealtimeAudioOutputLossReason::BackendFailure);
        }
        if &*desired_selection.lock() != opened_selection {
            return Some(RealtimeAudioOutputLossReason::DeviceSelectionChanged);
        }
        if last_default_device_poll.elapsed() >= DEFAULT_DEVICE_IDENTITY_POLL {
            last_default_device_poll = Instant::now();
            if let Ok(observed) = current_default_realtime_audio_output_device_id()
                && should_rebind_system_default(
                    opened_selection,
                    selected_device_id,
                    observed.as_ref(),
                )
            {
                return Some(RealtimeAudioOutputLossReason::DefaultDeviceChanged);
            }
        }
        match command_rx.recv_timeout(DEVICE_HEALTH_POLL) {
            #[cfg(feature = "validation")]
            Ok(WorkerCommand::ControlledRecycle { expected_stream_generation })
                if expected_stream_generation == stream_generation =>
            {
                return Some(RealtimeAudioOutputLossReason::ControlledRecycle);
            }
            #[cfg(feature = "validation")]
            Ok(WorkerCommand::ControlledRecycle { .. }) => {}
            #[cfg(not(feature = "validation"))]
            Ok(command) => match command {},
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

fn should_rebind_system_default(
    selection: &RealtimeAudioOutputDeviceSelection,
    selected_device_id: &RealtimeAudioOutputDeviceId,
    observed_default: Option<&RealtimeAudioOutputDeviceId>,
) -> bool {
    matches!(selection, RealtimeAudioOutputDeviceSelection::SystemDefault)
        && observed_default.is_some_and(|observed| observed != selected_device_id)
}

fn drop_freeze_and_publish<T, Snapshot, PublishError>(
    concrete_output: T,
    observe_after_drop: impl FnOnce() -> Snapshot,
    publish: impl FnOnce(Snapshot) -> Result<(), PublishError>,
) -> Result<(), PublishError> {
    drop(concrete_output);
    let final_snapshot = observe_after_drop();
    publish(final_snapshot)
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
    use parking_lot::Mutex;
    use std::fmt;

    #[derive(Debug)]
    struct DropProbe {
        dropped: Arc<AtomicBool>,
    }

    impl fmt::Display for DropProbe {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("opaque test owner")
        }
    }

    impl std::error::Error for DropProbe {}

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    #[test]
    fn shutdown_evidence_rejects_default_and_stale_schema() {
        assert!(!RealtimeAudioOutputShutdownEvidence::default().all_workers_terminated());
        let clean = RealtimeAudioOutputShutdownEvidence::no_worker_owner();
        assert!(clean.all_workers_terminated());
        assert!(
            !RealtimeAudioOutputShutdownEvidence { schema_version: 0, ..clean }
                .all_workers_terminated()
        );
    }

    #[test]
    fn only_system_default_intent_rebinds_to_a_new_observed_identity() {
        let selected =
            RealtimeAudioOutputDeviceId::new("wasapi:selected").expect("selected identity");
        let replacement =
            RealtimeAudioOutputDeviceId::new("wasapi:replacement").expect("replacement identity");
        assert!(should_rebind_system_default(
            &RealtimeAudioOutputDeviceSelection::SystemDefault,
            &selected,
            Some(&replacement),
        ));
        assert!(!should_rebind_system_default(
            &RealtimeAudioOutputDeviceSelection::SystemDefault,
            &selected,
            Some(&selected),
        ));
        assert!(!should_rebind_system_default(
            &RealtimeAudioOutputDeviceSelection::SystemDefault,
            &selected,
            None,
        ));
        assert!(!should_rebind_system_default(
            &RealtimeAudioOutputDeviceSelection::Specific { device_id: selected.clone() },
            &selected,
            Some(&replacement),
        ));
    }

    #[test]
    fn manager_device_selection_is_latest_wins_and_rejects_noop() {
        let manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        assert!(!manager.set_device_selection(RealtimeAudioOutputDeviceSelection::SystemDefault));
        let specific = RealtimeAudioOutputDeviceSelection::Specific {
            device_id: RealtimeAudioOutputDeviceId::new("wasapi:selected")
                .expect("specific identity"),
        };
        assert!(manager.set_device_selection(specific.clone()));
        assert!(!manager.set_device_selection(specific));
        assert!(manager.set_device_selection(RealtimeAudioOutputDeviceSelection::SystemDefault));
    }

    #[test]
    fn retry_delay_is_exponential_and_bounded() {
        assert_eq!(retry_delay(1), Duration::from_millis(250));
        assert_eq!(retry_delay(2), Duration::from_millis(500));
        assert_eq!(retry_delay(3), Duration::from_secs(1));
        assert_eq!(retry_delay(10), MAX_RETRY_DELAY);
    }

    #[test]
    fn injected_device_worker_spawn_failure_remains_a_structured_start_error() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        let error_dropped = Arc::new(AtomicBool::new(false));
        let observed_error_drop = Arc::clone(&error_dropped);

        let result = manager.ensure_worker_started_with(|_, _, _, _, _, _, _| {
            Err(io::Error::other(DropProbe { dropped: observed_error_drop }))
        });

        assert!(result.is_err());
        assert!(manager.worker.is_none());
        assert!(manager.event_rx.is_none());
        assert!(manager.handle.is_none());
        assert!(!error_dropped.load(Ordering::Acquire));
        assert!(manager.next_worker_retry_at.is_some_and(|retry_at| retry_at > Instant::now()));
        assert!(manager.poll().is_none());
        assert_eq!(manager.shutdown_evidence.worker_start_failures, 1);
        let evidence = manager.shutdown_and_wait();
        assert_eq!(evidence.worker_start_failures, 1);
        assert_eq!(evidence.worker_spawner_panics, 0);
        assert_eq!(evidence.worker_owner_abandonments, 1);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn device_worker_spawner_opaque_panic_is_abandoned_and_fails_closed() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let observed_payload_drop = Arc::clone(&payload_dropped);

        let result = manager.ensure_worker_started_with(|_, _, _, _, _, _, _| {
            std::panic::panic_any(DropProbe { dropped: observed_payload_drop })
        });

        assert!(result.is_err());
        assert!(!payload_dropped.load(Ordering::Acquire));
        let evidence = manager.shutdown_and_wait();
        assert_eq!(evidence.worker_start_failures, 1);
        assert_eq!(evidence.worker_spawner_panics, 1);
        assert_eq!(evidence.worker_owner_abandonments, 1);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn shutdown_joins_the_owned_device_worker() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::clone(&exited);
        manager
            .ensure_worker_started_with(move |_, _, _, shutdown, _, _, terminal| {
                thread::Builder::new().name("mondrian-audio-device-test".to_owned()).spawn(
                    move || {
                        supervise_device_worker(&terminal, || {
                            while !shutdown.load(Ordering::Acquire) {
                                thread::yield_now();
                            }
                            worker_exited.store(true, Ordering::Release);
                        });
                    },
                )
            })
            .expect("spawn injected device worker");

        let evidence = manager.shutdown_and_wait();

        assert!(exited.load(Ordering::Acquire));
        assert_eq!(evidence.workers_started, 1);
        assert_eq!(evidence.workers_terminated, 1);
        assert_eq!(evidence.worker_panics, 0);
        assert!(evidence.all_workers_terminated());
        assert!(manager.poll().is_none());
        assert_eq!(manager.shutdown_evidence.workers_started, 1);
    }

    #[test]
    fn joined_device_worker_without_supervisor_stamp_fails_closed() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        manager
            .ensure_worker_started_with(|_, _, _, _, _, _, _| {
                thread::Builder::new()
                    .name("mondrian-audio-device-unstamped-test".to_owned())
                    .spawn(|| {})
            })
            .expect("spawn unstamped device worker");

        let evidence = manager.shutdown_and_wait();

        assert_eq!(evidence.workers_started, 1);
        assert_eq!(evidence.workers_terminated, 1);
        assert_eq!(evidence.worker_terminal_evidence_missing, 1);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn ordinary_drop_detaches_a_device_worker_that_has_not_finished() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::clone(&exited);
        manager
            .ensure_worker_started_with(move |_, _, _, shutdown, _, _, terminal| {
                thread::Builder::new().name("mondrian-audio-device-drop-test".to_owned()).spawn(
                    move || {
                        supervise_device_worker(&terminal, || {
                            while !shutdown.load(Ordering::Acquire) {
                                thread::yield_now();
                            }
                            while !worker_release.load(Ordering::Acquire) {
                                thread::yield_now();
                            }
                            worker_exited.store(true, Ordering::Release);
                        });
                    },
                )
            })
            .expect("spawn injected device worker");

        let (drop_complete_tx, drop_complete_rx) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(manager);
            drop_complete_tx.send(()).expect("publish drop completion");
        });
        let returned_without_worker_exit =
            drop_complete_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        release.store(true, Ordering::Release);
        dropper.join().expect("ordinary manager drop must not panic");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !exited.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            returned_without_worker_exit,
            "ordinary drop blocked on the device worker"
        );
        assert!(exited.load(Ordering::Acquire));
    }

    #[test]
    fn detached_device_worker_abandons_opaque_panic_inside_worker_runtime() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        let entered = Arc::new(AtomicBool::new(false));
        let worker_entered = Arc::clone(&entered);
        let release = Arc::new(AtomicBool::new(false));
        let worker_release = Arc::clone(&release);
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let observed_payload_drop = Arc::clone(&payload_dropped);
        let terminal = Arc::clone(&manager.worker_terminal);
        manager
            .ensure_worker_started_with(move |_, _, _, _, event_tx, _, worker_terminal| {
                thread::Builder::new()
                    .name("mondrian-audio-device-detached-panic-test".to_owned())
                    .spawn(move || {
                        drop(event_tx);
                        supervise_device_worker(&worker_terminal, || {
                            worker_entered.store(true, Ordering::Release);
                            while !worker_release.load(Ordering::Acquire) {
                                thread::yield_now();
                            }
                            std::panic::panic_any(DropProbe { dropped: observed_payload_drop });
                        });
                    })
            })
            .expect("spawn supervised device worker");

        let deadline = Instant::now() + Duration::from_secs(2);
        while !entered.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(entered.load(Ordering::Acquire));
        let (drop_complete_tx, drop_complete_rx) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(manager);
            drop_complete_tx.send(()).expect("publish manager drop completion");
        });
        let returned = drop_complete_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        release.store(true, Ordering::Release);
        dropper.join().expect("ordinary manager drop returns");

        let deadline = Instant::now() + Duration::from_secs(2);
        while terminal.load(Ordering::Acquire) != DEVICE_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        assert_eq!(
            terminal.load(Ordering::Acquire),
            DEVICE_WORKER_TERMINAL_OPAQUE_PANIC_ABANDONED
        );
        assert!(
            returned,
            "ordinary manager drop blocked on the device worker"
        );
        assert!(!payload_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn worker_panic_is_retained_after_poll_joins_disconnected_worker() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        manager
            .ensure_worker_started_with(|_, _, _, _, event_tx, _, _| {
                thread::Builder::new()
                    .name("mondrian-audio-device-panic-test".to_owned())
                    .spawn(move || {
                        drop(event_tx);
                        panic!("injected device worker panic");
                    })
            })
            .expect("spawn injected device worker");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(
                manager.poll(),
                Some(RealtimeAudioOutputEvent::WorkerStoppedUnexpectedly { .. })
            ) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "device worker panic was not observed"
            );
            thread::yield_now();
        }
        assert!(manager.next_worker_retry_at.is_some_and(|retry_at| retry_at > Instant::now()));
        assert!(manager.poll().is_none());
        assert_eq!(manager.shutdown_evidence.workers_started, 1);
        let evidence = manager.shutdown_and_wait();

        assert_eq!(evidence.workers_started, 1);
        assert_eq!(evidence.workers_terminated, 1);
        assert_eq!(evidence.worker_panics, 1);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn opaque_device_worker_panic_payload_never_drops_on_poll_caller() {
        let mut manager = RealtimeAudioOutputManager::new(48_000, AudioChannelLayout::Stereo);
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let observed_payload_drop = Arc::clone(&payload_dropped);
        manager
            .ensure_worker_started_with(move |_, _, _, _, event_tx, _, _| {
                thread::Builder::new()
                    .name("mondrian-audio-device-opaque-panic-test".to_owned())
                    .spawn(move || {
                        drop(event_tx);
                        std::panic::panic_any(DropProbe { dropped: observed_payload_drop });
                    })
            })
            .expect("spawn injected device worker");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if matches!(
                manager.poll(),
                Some(RealtimeAudioOutputEvent::WorkerStoppedUnexpectedly { .. })
            ) {
                break;
            }
            assert!(Instant::now() < deadline, "device panic was not observed");
            thread::yield_now();
        }
        assert!(!payload_dropped.load(Ordering::Acquire));
        let evidence = manager.shutdown_and_wait();
        assert_eq!(evidence.worker_panics, 1);
        assert_eq!(evidence.worker_owner_abandonments, 1);
        assert!(!evidence.all_workers_terminated());
    }

    #[test]
    fn concrete_output_is_dropped_before_final_observation_and_loss_publication() {
        struct DropProbe(Arc<Mutex<Vec<&'static str>>>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.lock().push("drop");
            }
        }

        let order = Arc::new(Mutex::new(Vec::new()));
        let observe_order = Arc::clone(&order);
        let publish_order = Arc::clone(&order);

        drop_freeze_and_publish(
            DropProbe(Arc::clone(&order)),
            move || {
                observe_order.lock().push("observe");
                42_u64
            },
            move |snapshot| {
                assert_eq!(snapshot, 42);
                publish_order.lock().push("publish");
                Ok::<(), ()>(())
            },
        )
        .expect("publish frozen loss evidence");

        assert_eq!(&*order.lock(), &["drop", "observe", "publish"]);
    }

    #[cfg(feature = "validation")]
    #[test]
    fn controlled_recycle_preflight_accepts_only_the_exact_current_generation() {
        assert_eq!(
            validate_controlled_recycle_generation(None, 7),
            Err(RealtimeAudioOutputRecycleError::OutputUnavailable)
        );
        assert_eq!(
            validate_controlled_recycle_generation(Some(8), 7),
            Err(RealtimeAudioOutputRecycleError::StreamGenerationMismatch {
                expected: 7,
                actual: 8,
            })
        );
        assert_eq!(validate_controlled_recycle_generation(Some(7), 7), Ok(()));
    }
}
