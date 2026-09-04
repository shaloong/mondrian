//! Bounded persistent FFmpeg child-process sessions for decoded source windows.

use super::{
    audio_frame_timestamp, canceled_audio_decode, mapping::identity_pan_filter,
    AudioSourceIdentity, AudioWindowDecoder, AudioWindowDecoderDiagnostics,
    AudioWindowDecoderShutdownEvidence, AudioWindowDecoderShutdownSignal,
    AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX,
};
use crate::audio::AudioBuffer;
use crate::owner_lifetime::{abandon_io_error, dispose_canonical_or_abandon_opaque_panic_payload};
use mondrian_core::{AudioChannelLayout, ExecutionCancellationToken, MondrianError, Result};
use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::io::Read;
use std::mem::ManuallyDrop;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DEFAULT_SESSION_CAPACITY: usize = 2;
const EXACT_SEEK_PREROLL_SECONDS: i64 = 10;
const PUMP_CHUNK_BYTES: usize = 64 * 1024;
const PUMP_LOOKAHEAD_CHUNKS: usize = 2;
const STDERR_TAIL_BYTES: usize = 64 * 1024;
const CANCELLATION_POLL: Duration = Duration::from_millis(5);
const DECODER_TEARDOWN_QUEUE_CAPACITY: usize = AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX;
const DECODER_SHUTDOWN_SLOT_WAIT: Duration = Duration::from_millis(250);
const DECODER_TERMINAL_STATUS_WAIT: Duration = Duration::from_millis(250);
const DECODER_TEARDOWN_WORKER_RUNNING: u8 = 1;
const DECODER_TEARDOWN_WORKER_COMPLETED: u8 = 2;
const DECODER_TEARDOWN_WORKER_PANICKED: u8 = 3;

#[path = "session/startup.rs"]
mod startup;
use startup::StartupLane;

#[cfg(test)]
type NativeChildSpawner = Arc<dyn Fn(&mut Command) -> std::io::Result<Child> + Send + Sync>;

type DecoderTeardownWork = Box<dyn FnOnce() + Send + 'static>;
type DecoderTeardownSpawner =
    Arc<dyn Fn(DecoderTeardownWork) -> std::io::Result<JoinHandle<()>> + Send + Sync>;

fn product_decoder_teardown_spawner() -> DecoderTeardownSpawner {
    Arc::new(|work| {
        std::thread::Builder::new()
            .name("mondrian-audio-source-session-teardown".to_owned())
            .spawn(work)
    })
}

/// Product decoder that reuses one bounded FFmpeg stream per active source contract.
pub(super) struct PersistentFfmpegAudioWindowDecoder {
    state: Arc<Mutex<DecoderState>>,
    session_permits: Arc<DecoderSessionPermitPool>,
    shutdown_signal: Arc<AudioWindowDecoderShutdownSignal>,
    teardown_queue: Arc<DecoderTeardownQueue>,
    teardown_faulted: Arc<AtomicBool>,
    shutdown: Mutex<DecoderShutdownControl>,
    startup: Arc<StartupLane>,
    #[cfg(test)]
    native_spawn_for_test: Option<NativeChildSpawner>,
}

struct DecoderShutdownControl {
    worker: Option<JoinHandle<()>>,
    startup_worker: Option<JoinHandle<()>>,
    publication: Arc<Mutex<Option<AudioWindowDecoderShutdownEvidence>>>,
    terminal: Arc<AtomicU8>,
}

impl Default for PersistentFfmpegAudioWindowDecoder {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_SESSION_CAPACITY)
    }
}

struct DecoderTeardownQueue {
    state: Mutex<DecoderTeardownQueueState>,
    shutdown_signal: Arc<AudioWindowDecoderShutdownSignal>,
    capacity: usize,
}

struct DecoderTeardownQueueState {
    owners: VecDeque<DecoderTeardownOwner>,
    closed: bool,
}

enum DecoderTeardownOwner {
    Session(DecodeSession),
    PartialSession(PartialDecodeSession),
    FinalizeSession {
        session: DecodeSession,
        completion: SyncSender<std::result::Result<(), String>>,
    },
    Entry(DecoderEntry),
}

impl DecoderTeardownQueue {
    fn new(capacity: usize, shutdown_signal: Arc<AudioWindowDecoderShutdownSignal>) -> Self {
        Self {
            state: Mutex::new(DecoderTeardownQueueState { owners: VecDeque::new(), closed: false }),
            shutdown_signal,
            capacity: capacity.max(1),
        }
    }

    fn push(&self, mut owners: VecDeque<DecoderTeardownOwner>) -> VecDeque<DecoderTeardownOwner> {
        let mut state = self.state.lock();
        if state.closed {
            return owners;
        }
        let available = self.capacity.saturating_sub(state.owners.len());
        let accepted = available.min(owners.len());
        for _ in 0..accepted {
            if let Some(owner) = owners.pop_front() {
                state.owners.push_back(owner);
            }
        }
        drop(state);
        if accepted > 0 {
            self.shutdown_signal.notify_worker();
        }
        owners
    }

    fn wait_for_work(
        &self,
        shutdown_signal: &AudioWindowDecoderShutdownSignal,
        startup: &StartupLane,
        initial_sweep_pending: bool,
    ) -> VecDeque<DecoderTeardownOwner> {
        loop {
            let owners = std::mem::take(&mut self.state.lock().owners);
            if !owners.is_empty()
                || (shutdown_signal.is_requested()
                    && (initial_sweep_pending || startup.producers_closed()))
            {
                return owners;
            }
            // `unpark` retains a one-shot token when it races this call, so a
            // producer or shutdown signal cannot be lost between the empty
            // observation above and the actual park.
            std::thread::park();
        }
    }

    fn close_and_take_pending(&self) -> VecDeque<DecoderTeardownOwner> {
        let mut state = self.state.lock();
        state.closed = true;
        std::mem::take(&mut state.owners)
    }

    fn take_pending(&self) -> VecDeque<DecoderTeardownOwner> {
        std::mem::take(&mut self.state.lock().owners)
    }
}

struct DecoderState {
    session_capacity: usize,
    entries: VecDeque<DecoderEntry>,
    peak_sessions: usize,
    session_opens: u64,
    sequential_reuses: u64,
    random_seek_restarts: u64,
    session_evictions: u64,
    capacity_reconfigurations: u64,
    capacity_trim_evictions: u64,
    cancellations: u64,
    cold_window_max_duration_us: u64,
    sequential_window_max_duration_us: u64,
    random_seek_window_max_duration_us: u64,
    retired_shutdown: AudioWindowDecoderShutdownEvidence,
}

struct DecoderSessionPermitPool {
    state: Mutex<DecoderSessionPermitState>,
    available: Condvar,
}

struct DecoderSessionPermitState {
    capacity: usize,
    in_use: usize,
    peak_in_use: usize,
}

struct DecoderSessionPermit {
    pool: Arc<DecoderSessionPermitPool>,
}

impl DecoderSessionPermitPool {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(DecoderSessionPermitState {
                capacity: capacity.clamp(1, AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX),
                in_use: 0,
                peak_in_use: 0,
            }),
            available: Condvar::new(),
        }
    }

    fn acquire(
        self: &Arc<Self>,
        cancellation: &ExecutionCancellationToken,
        shutdown_signal: &AudioWindowDecoderShutdownSignal,
        teardown_faulted: &AtomicBool,
        source: &std::path::Path,
    ) -> Result<DecoderSessionPermit> {
        loop {
            if cancellation.is_canceled() || shutdown_signal.is_requested() {
                return Err(canceled_audio_decode(source));
            }
            if teardown_faulted.load(Ordering::Acquire) {
                return Err(MondrianError::DecodeFailed {
                    asset_id: source.display().to_string(),
                    reason: "persistent audio teardown worker is unavailable".to_owned(),
                });
            }
            let mut state = self.state.lock();
            if cancellation.is_canceled() || shutdown_signal.is_requested() {
                return Err(canceled_audio_decode(source));
            }
            if teardown_faulted.load(Ordering::Acquire) {
                return Err(MondrianError::DecodeFailed {
                    asset_id: source.display().to_string(),
                    reason: "persistent audio teardown worker is unavailable".to_owned(),
                });
            }
            if state.in_use < state.capacity {
                state.in_use = state.in_use.saturating_add(1);
                state.peak_in_use = state.peak_in_use.max(state.in_use);
                return Ok(DecoderSessionPermit { pool: Arc::clone(self) });
            }
            self.available.wait_for(&mut state, CANCELLATION_POLL);
        }
    }

    fn reconfigure(&self, capacity: usize) {
        self.state.lock().capacity = capacity.clamp(1, AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX);
        self.available.notify_all();
    }

    fn diagnostics(&self) -> (usize, usize, usize) {
        let state = self.state.lock();
        (state.in_use, state.capacity, state.peak_in_use)
    }
}

impl Drop for DecoderSessionPermit {
    fn drop(&mut self) {
        let mut state = self.pool.state.lock();
        state.in_use = state.in_use.saturating_sub(1);
        drop(state);
        self.pool.available.notify_all();
    }
}

impl DecoderState {
    fn new(session_capacity: usize) -> Self {
        Self {
            session_capacity: session_capacity.clamp(1, AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX),
            entries: VecDeque::new(),
            peak_sessions: 0,
            session_opens: 0,
            sequential_reuses: 0,
            random_seek_restarts: 0,
            session_evictions: 0,
            capacity_reconfigurations: 0,
            capacity_trim_evictions: 0,
            cancellations: 0,
            cold_window_max_duration_us: 0,
            sequential_window_max_duration_us: 0,
            random_seek_window_max_duration_us: 0,
            retired_shutdown: AudioWindowDecoderShutdownEvidence::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionKey {
    source: AudioSourceIdentity,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
}

struct DecoderEntry {
    key: SessionKey,
    slot: Arc<Mutex<Option<DecodeSession>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    Cold,
    Sequential,
    RandomSeek,
}

impl AudioWindowDecoder for PersistentFfmpegAudioWindowDecoder {
    fn shutdown_signal(&self) -> Option<Arc<AudioWindowDecoderShutdownSignal>> {
        Some(Arc::clone(&self.shutdown_signal))
    }

    fn decode_window(
        &self,
        source: &AudioSourceIdentity,
        start_frame: i64,
        frame_count: usize,
        sample_rate: u32,
        channel_layout: AudioChannelLayout,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<AudioBuffer> {
        if cancellation.is_canceled() || self.shutdown_signal.is_requested() {
            return Err(canceled_audio_decode(&source.path));
        }
        if self.teardown_faulted.load(Ordering::Acquire) {
            return Err(MondrianError::DecodeFailed {
                asset_id: source.path.display().to_string(),
                reason: "persistent audio teardown worker is unavailable".to_owned(),
            });
        }

        let key = SessionKey {
            source: source.clone(),
            sample_rate: sample_rate.max(8_000),
            channel_layout,
        };
        let slot = self.acquire_slot(key.clone(), cancellation)?;
        let mut session = self.lock_slot(&slot, source, cancellation)?;
        if self.shutdown_signal.is_requested() {
            drop(session);
            return Err(canceled_audio_decode(&source.path));
        }
        let start_frame = start_frame.max(0);
        let kind = match session.as_ref() {
            None => WindowKind::Cold,
            Some(existing) if existing.next_frame == start_frame => WindowKind::Sequential,
            Some(_) => WindowKind::RandomSeek,
        };

        if kind == WindowKind::RandomSeek
            && let Some(previous) = session.take()
            && !self.enqueue_teardown(VecDeque::from([DecoderTeardownOwner::Session(previous)]))
        {
            drop(session);
            self.remove_slot(&slot);
            self.record_window(kind, false, Duration::ZERO, cancellation.is_canceled());
            return Err(MondrianError::DecodeFailed {
                asset_id: source.path.display().to_string(),
                reason: "persistent audio teardown queue exceeded its proven capacity".to_owned(),
            });
        }
        if self.shutdown_signal.is_requested() {
            drop(session);
            self.remove_slot(&slot);
            return Err(canceled_audio_decode(&source.path));
        }

        let started = Instant::now();
        let mut opened = false;
        if session.is_none() {
            let permit = self.session_permits.acquire(
                cancellation,
                &self.shutdown_signal,
                &self.teardown_faulted,
                &source.path,
            )?;
            if self.shutdown_signal.is_requested() {
                drop(permit);
                drop(session);
                self.remove_slot(&slot);
                return Err(canceled_audio_decode(&source.path));
            }
            let mut completion = match self.startup.wait(
                &key,
                start_frame,
                permit,
                cancellation,
                #[cfg(test)]
                self.native_spawn_for_test.clone(),
            ) {
                Ok(completion) => completion,
                Err(error) => {
                    drop(session);
                    self.remove_slot(&slot);
                    self.record_window(kind, false, started.elapsed(), cancellation.is_canceled());
                    return Err(error);
                }
            };
            match completion.take() {
                Ok(created) => {
                    if cancellation.is_canceled() || self.shutdown_signal.is_requested() {
                        let _accepted =
                            self.enqueue_teardown(VecDeque::from([DecoderTeardownOwner::Session(
                                created,
                            )]));
                        drop(session);
                        self.remove_slot(&slot);
                        self.record_window(
                            kind,
                            false,
                            started.elapsed(),
                            cancellation.is_canceled(),
                        );
                        return Err(canceled_audio_decode(&source.path));
                    }
                    *session = Some(created);
                    opened = true;
                }
                Err(failure) => {
                    let DecodeSessionSpawnFailure { error, owner } = *failure;
                    if let Some(owner) = owner {
                        let _accepted = self.enqueue_teardown(VecDeque::from([
                            DecoderTeardownOwner::PartialSession(owner),
                        ]));
                    }
                    drop(session);
                    self.remove_slot(&slot);
                    self.record_window(kind, false, started.elapsed(), cancellation.is_canceled());
                    return Err(*error);
                }
            }
        }
        let mut result = match session.as_mut() {
            Some(active) => active.decode(frame_count, cancellation, &self.shutdown_signal),
            None => Err(MondrianError::DecodeFailed {
                asset_id: source.path.display().to_string(),
                reason: "persistent audio session was not created".to_owned(),
            }),
        };
        if (cancellation.is_canceled() || self.shutdown_signal.is_requested()) && result.is_ok() {
            result = Err(canceled_audio_decode(&source.path));
        }
        let shutting_down = self.shutdown_signal.is_requested();
        let ended = session.as_ref().is_some_and(|session| session.ended);
        if ended
            && !shutting_down
            && let Some(ended_session) = session.take()
        {
            if ended_session.awaiting_terminal_status {
                let (completion_tx, completion_rx) = mpsc::sync_channel(1);
                let accepted = self.enqueue_teardown(VecDeque::from([
                    DecoderTeardownOwner::FinalizeSession {
                        session: ended_session,
                        completion: completion_tx,
                    },
                ]));
                if accepted {
                    if let Err(error) = wait_for_terminal_finalization(
                        completion_rx,
                        cancellation,
                        &self.shutdown_signal,
                        &source.path,
                    ) {
                        result = Err(error);
                    }
                } else if result.is_ok() {
                    result = Err(MondrianError::DecodeFailed {
                        asset_id: source.path.display().to_string(),
                        reason: "persistent audio teardown queue exceeded its proven capacity"
                            .to_owned(),
                    });
                }
            } else if !self.enqueue_teardown(VecDeque::from([DecoderTeardownOwner::Session(
                ended_session,
            )])) && result.is_ok()
            {
                result = Err(MondrianError::DecodeFailed {
                    asset_id: source.path.display().to_string(),
                    reason: "persistent audio teardown queue exceeded its proven capacity"
                        .to_owned(),
                });
            }
        }
        let failed = result.is_err();
        if failed
            && !shutting_down
            && let Some(failed) = session.take()
        {
            self.enqueue_teardown(VecDeque::from([DecoderTeardownOwner::Session(failed)]));
        }
        drop(session);
        if (failed || ended) && !shutting_down {
            self.remove_slot(&slot);
        }
        drop(slot);
        self.converge_capacity();

        self.record_window(
            kind,
            opened,
            started.elapsed(),
            cancellation.is_canceled() || self.shutdown_signal.is_requested(),
        );
        result
    }

    fn diagnostics(&self) -> AudioWindowDecoderDiagnostics {
        let state = self.state.lock();
        let (live_sessions, session_capacity, peak_live_sessions) =
            self.session_permits.diagnostics();
        AudioWindowDecoderDiagnostics {
            sessions: live_sessions,
            session_capacity,
            peak_sessions: peak_live_sessions,
            session_opens: state.session_opens,
            sequential_reuses: state.sequential_reuses,
            random_seek_restarts: state.random_seek_restarts,
            session_evictions: state.session_evictions,
            capacity_reconfigurations: state.capacity_reconfigurations,
            capacity_trim_evictions: state.capacity_trim_evictions,
            sessions_above_capacity: live_sessions.saturating_sub(session_capacity),
            cancellations: state.cancellations,
            cold_window_max_duration_us: state.cold_window_max_duration_us,
            sequential_window_max_duration_us: state.sequential_window_max_duration_us,
            random_seek_window_max_duration_us: state.random_seek_window_max_duration_us,
        }
    }

    fn reconfigure_session_capacity(&self, session_capacity: usize) {
        let session_capacity = session_capacity.clamp(1, AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX);
        self.session_permits.reconfigure(session_capacity);
        let evicted = {
            let mut state = self.state.lock();
            if state.session_capacity == session_capacity {
                VecDeque::new()
            } else {
                state.session_capacity = session_capacity;
                state.capacity_reconfigurations = state.capacity_reconfigurations.saturating_add(1);
                trim_idle_sessions_to_capacity(&mut state)
            }
        };
        self.enqueue_entries(evicted);
    }

    fn shutdown_sessions(&self) -> AudioWindowDecoderShutdownEvidence {
        self.shutdown_signal.request();
        let permits_before = self.session_permits.diagnostics().0;
        // Serialize the complete consuming receipt publication. A follower
        // must not observe `worker = None` before the leader has joined and
        // published the terminal evidence.
        let mut shutdown = self.shutdown.lock();
        let worker = shutdown.worker.take();
        let publication = Arc::clone(&shutdown.publication);
        let terminal = Arc::clone(&shutdown.terminal);
        let joined_now = worker.is_some();
        let (current_thread_detachment, join_panicked, join_payload_abandonments) = match worker {
            Some(worker) if worker.thread().id() == std::thread::current().id() => {
                drop(worker);
                (true, false, 0)
            }
            Some(worker) => match worker.join() {
                Ok(()) => (false, false, 0),
                Err(payload) => (
                    false,
                    true,
                    u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload)),
                ),
            },
            None => (false, false, 0),
        };
        let mut evidence = (*publication.lock()).unwrap_or_default();
        evidence.startup = self.startup.join(shutdown.startup_worker.take());
        if joined_now && !current_thread_detachment {
            evidence.shutdown_workers_terminated =
                evidence.shutdown_workers_terminated.saturating_add(1);
            if terminal.load(Ordering::Acquire) != DECODER_TEARDOWN_WORKER_COMPLETED
                && terminal.load(Ordering::Acquire) != DECODER_TEARDOWN_WORKER_PANICKED
            {
                evidence.shutdown_worker_publication_missing =
                    evidence.shutdown_worker_publication_missing.saturating_add(1);
                evidence.resource_handles_remaining =
                    evidence.resource_handles_remaining.saturating_add(1);
            }
        }
        if current_thread_detachment || join_panicked {
            evidence.shutdown_worker_panics = evidence.shutdown_worker_panics.saturating_add(1);
        }
        if current_thread_detachment {
            evidence.resource_handles_remaining =
                evidence.resource_handles_remaining.saturating_add(1);
        }
        evidence.shutdown_worker_owner_abandonments = evidence
            .shutdown_worker_owner_abandonments
            .saturating_add(join_payload_abandonments);

        if !current_thread_detachment {
            // Once the owner worker is joined no producer may publish another
            // foreign owner after this fallback drain.
            let pending = self.teardown_queue.close_and_take_pending();
            abandon_teardown_owners(pending, &mut evidence);
        }
        let (entries, retired) = {
            let mut state = self.state.lock();
            let entries = std::mem::take(&mut state.entries);
            let retired = std::mem::take(&mut state.retired_shutdown);
            (entries, retired)
        };
        if !entries.is_empty() {
            evidence.sessions_before = evidence.sessions_before.saturating_add(entries.len());
            abandon_decoder_entries(entries, &mut evidence);
        }
        evidence.merge(retired);
        evidence.sessions_before = evidence.sessions_before.max(permits_before);
        let permits_remaining = self.session_permits.diagnostics().0;
        evidence.sessions_remaining = evidence.sessions_remaining.max(permits_remaining);
        evidence.resource_handles_remaining =
            evidence.resource_handles_remaining.max(permits_remaining);
        *publication.lock() = Some(evidence);
        drop(shutdown);
        evidence
    }
}

impl PersistentFfmpegAudioWindowDecoder {
    /// Transfer the concrete startup owner without invoking an erased hook.
    pub(super) fn into_cache_parts(
        self,
    ) -> (
        Arc<dyn AudioWindowDecoder>,
        Arc<AudioWindowDecoderShutdownSignal>,
    ) {
        let signal = Arc::clone(&self.shutdown_signal);
        (Arc::new(self), signal)
    }

    pub(super) fn with_capacity(session_capacity: usize) -> Self {
        Self::with_state_and_spawner(
            DecoderState::new(session_capacity),
            product_decoder_teardown_spawner(),
        )
    }

    #[cfg(test)]
    pub(super) fn with_native_spawn_for_test(capacity: usize, spawn: NativeChildSpawner) -> Self {
        let mut decoder = Self::with_capacity(capacity);
        decoder.native_spawn_for_test = Some(spawn);
        decoder
    }

    fn with_state_and_spawner(state: DecoderState, spawner: DecoderTeardownSpawner) -> Self {
        Self::with_state_and_spawners(state, spawner, startup::product_startup_spawner())
    }

    fn with_state_and_spawners(
        state: DecoderState,
        spawner: DecoderTeardownSpawner,
        startup_spawner: DecoderTeardownSpawner,
    ) -> Self {
        let session_permits = Arc::new(DecoderSessionPermitPool::new(state.session_capacity));
        let state = Arc::new(Mutex::new(state));
        let shutdown_signal = Arc::new(AudioWindowDecoderShutdownSignal::new());
        let teardown_queue = Arc::new(DecoderTeardownQueue::new(
            DECODER_TEARDOWN_QUEUE_CAPACITY,
            Arc::clone(&shutdown_signal),
        ));
        let teardown_faulted = Arc::new(AtomicBool::new(false));
        let startup = StartupLane::new(
            Arc::clone(&shutdown_signal),
            Arc::clone(&teardown_queue),
            Arc::clone(&state),
            Arc::clone(&teardown_faulted),
        );
        let publication = Arc::new(Mutex::new(None));
        let terminal = Arc::new(AtomicU8::new(0));

        let worker_state = Arc::clone(&state);
        let worker_signal = Arc::clone(&shutdown_signal);
        let worker_queue = Arc::clone(&teardown_queue);
        let worker_faulted = Arc::clone(&teardown_faulted);
        let worker_publication = Arc::clone(&publication);
        let worker_terminal = Arc::clone(&terminal);
        let worker_startup = Arc::clone(&startup);
        let work: DecoderTeardownWork = Box::new(move || {
            worker_terminal.store(DECODER_TEARDOWN_WORKER_RUNNING, Ordering::Release);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_decoder_teardown_worker(
                    &worker_state,
                    &worker_queue,
                    &worker_signal,
                    &worker_faulted,
                    &worker_startup,
                )
            }));
            let (mut evidence, terminal_state) = match result {
                Ok(evidence) => (evidence, DECODER_TEARDOWN_WORKER_COMPLETED),
                Err(payload) => {
                    worker_faulted.store(true, Ordering::Release);
                    let owner_abandonments =
                        u32::from(dispose_canonical_or_abandon_opaque_panic_payload(payload));
                    let mut evidence = AudioWindowDecoderShutdownEvidence {
                        shutdown_worker_panics: 1,
                        shutdown_worker_owner_abandonments: owner_abandonments,
                        resource_handles_remaining: 1,
                        ..AudioWindowDecoderShutdownEvidence::default()
                    };
                    abandon_teardown_owners(worker_queue.close_and_take_pending(), &mut evidence);
                    let mut state = worker_state.lock();
                    let entries = std::mem::take(&mut state.entries);
                    evidence.sessions_before =
                        evidence.sessions_before.saturating_add(entries.len());
                    abandon_decoder_entries(entries, &mut evidence);
                    evidence.merge(std::mem::take(&mut state.retired_shutdown));
                    (evidence, DECODER_TEARDOWN_WORKER_PANICKED)
                }
            };
            evidence.shutdown_workers_started = evidence.shutdown_workers_started.saturating_add(1);
            *worker_publication.lock() = Some(evidence);
            worker_terminal.store(terminal_state, Ordering::Release);
        });

        let spawn_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawner(work)));
        let worker = match spawn_result {
            Ok(Ok(worker)) => Some(worker),
            Ok(Err(error)) => {
                teardown_faulted.store(true, Ordering::Release);
                let (_kind, error_owner_abandoned) = abandon_io_error(error);
                *publication.lock() = Some(AudioWindowDecoderShutdownEvidence {
                    shutdown_worker_start_failures: 1,
                    shutdown_worker_owner_abandonments: u32::from(error_owner_abandoned),
                    ..AudioWindowDecoderShutdownEvidence::default()
                });
                None
            }
            Err(payload) => {
                teardown_faulted.store(true, Ordering::Release);
                *publication.lock() = Some(AudioWindowDecoderShutdownEvidence {
                    shutdown_worker_start_failures: 1,
                    shutdown_worker_panics: 1,
                    shutdown_worker_owner_abandonments: u32::from(
                        dispose_canonical_or_abandon_opaque_panic_payload(payload),
                    ),
                    ..AudioWindowDecoderShutdownEvidence::default()
                });
                None
            }
        };

        if teardown_faulted.load(Ordering::Acquire) {
            shutdown_signal.request();
        }
        let startup_worker = startup.start(startup_spawner);
        Self {
            state,
            session_permits,
            shutdown_signal,
            teardown_queue,
            teardown_faulted,
            shutdown: Mutex::new(DecoderShutdownControl {
                worker,
                startup_worker,
                publication,
                terminal,
            }),
            startup,
            #[cfg(test)]
            native_spawn_for_test: None,
        }
    }

    fn converge_capacity(&self) {
        if self.shutdown_signal.is_requested() {
            return;
        }
        self.enqueue_entries(trim_decoder_capacity(&self.state));
    }

    fn enqueue_entries(&self, entries: VecDeque<DecoderEntry>) -> bool {
        self.enqueue_teardown(entries.into_iter().map(DecoderTeardownOwner::Entry).collect())
    }

    fn enqueue_teardown(&self, owners: VecDeque<DecoderTeardownOwner>) -> bool {
        if owners.is_empty() {
            return true;
        }
        let overflow = self.teardown_queue.push(owners);
        if overflow.is_empty() {
            return true;
        }
        self.teardown_faulted.store(true, Ordering::Release);
        let mut evidence = AudioWindowDecoderShutdownEvidence::default();
        abandon_teardown_owners(overflow, &mut evidence);
        self.record_session_shutdown(evidence);
        false
    }

    fn record_session_shutdown(&self, evidence: AudioWindowDecoderShutdownEvidence) {
        self.state.lock().retired_shutdown.merge(evidence);
    }

    fn record_window(&self, kind: WindowKind, opened: bool, duration: Duration, canceled: bool) {
        let duration_us = duration.as_micros().min(u64::MAX as u128) as u64;
        let mut state = self.state.lock();
        if opened {
            state.session_opens = state.session_opens.saturating_add(1);
        }
        match kind {
            WindowKind::Cold => {
                state.cold_window_max_duration_us =
                    state.cold_window_max_duration_us.max(duration_us);
            }
            WindowKind::Sequential => {
                state.sequential_reuses = state.sequential_reuses.saturating_add(1);
                state.sequential_window_max_duration_us =
                    state.sequential_window_max_duration_us.max(duration_us);
            }
            WindowKind::RandomSeek => {
                state.random_seek_restarts = state.random_seek_restarts.saturating_add(1);
                state.random_seek_window_max_duration_us =
                    state.random_seek_window_max_duration_us.max(duration_us);
            }
        }
        if canceled {
            state.cancellations = state.cancellations.saturating_add(1);
        }
    }

    fn remove_slot(&self, slot: &Arc<Mutex<Option<DecodeSession>>>) {
        self.state.lock().entries.retain(|entry| !Arc::ptr_eq(&entry.slot, slot));
    }

    fn acquire_slot(
        &self,
        key: SessionKey,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<Arc<Mutex<Option<DecodeSession>>>> {
        loop {
            if cancellation.is_canceled() || self.shutdown_signal.is_requested() {
                return Err(canceled_audio_decode(&key.source.path));
            }
            if self.teardown_faulted.load(Ordering::Acquire) {
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: "persistent audio teardown worker is unavailable".to_owned(),
                });
            }

            let mut evicted = None;
            let selected = {
                let mut state = self.state.lock();
                if self.shutdown_signal.is_requested() {
                    return Err(canceled_audio_decode(&key.source.path));
                }
                let trimmed = trim_idle_sessions_to_capacity(&mut state);
                if !trimmed.is_empty() {
                    evicted = Some(trimmed);
                }
                if let Some(index) = state.entries.iter().position(|entry| entry.key == key) {
                    state.entries.remove(index).map(|entry| {
                        let slot = Arc::clone(&entry.slot);
                        state.entries.push_front(entry);
                        slot
                    })
                } else if state.entries.len() < state.session_capacity {
                    let slot = Arc::new(Mutex::new(None));
                    state
                        .entries
                        .push_front(DecoderEntry { key: key.clone(), slot: Arc::clone(&slot) });
                    state.peak_sessions = state.peak_sessions.max(state.entries.len());
                    Some(slot)
                } else if state.entries.len() == state.session_capacity {
                    if let Some(index) =
                        state.entries.iter().rposition(|entry| Arc::strong_count(&entry.slot) == 1)
                    {
                        let displaced = state.entries.remove(index);
                        if let Some(displaced) = displaced {
                            match evicted.as_mut() {
                                Some(entries) => entries.push_back(displaced),
                                None => evicted = Some(VecDeque::from([displaced])),
                            }
                        }
                        state.session_evictions = state.session_evictions.saturating_add(1);
                        let slot = Arc::new(Mutex::new(None));
                        state
                            .entries
                            .push_front(DecoderEntry { key: key.clone(), slot: Arc::clone(&slot) });
                        Some(slot)
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            if let Some(entries) = evicted
                && !self.enqueue_entries(entries)
            {
                return Err(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: "persistent audio teardown queue exceeded its proven capacity"
                        .to_owned(),
                });
            }
            if let Some(slot) = selected {
                return Ok(slot);
            }
            std::thread::sleep(CANCELLATION_POLL);
        }
    }

    fn lock_slot<'a>(
        &self,
        slot: &'a Mutex<Option<DecodeSession>>,
        source: &AudioSourceIdentity,
        cancellation: &ExecutionCancellationToken,
    ) -> Result<parking_lot::MutexGuard<'a, Option<DecodeSession>>> {
        loop {
            if self.shutdown_signal.is_requested() {
                return Err(canceled_audio_decode(&source.path));
            }
            if let Some(guard) = slot.try_lock() {
                return Ok(guard);
            }
            if cancellation.is_canceled() {
                return Err(canceled_audio_decode(&source.path));
            }
            std::thread::sleep(CANCELLATION_POLL);
        }
    }
}

fn trim_decoder_capacity(state: &Mutex<DecoderState>) -> VecDeque<DecoderEntry> {
    trim_idle_sessions_to_capacity(&mut state.lock())
}

fn trim_idle_sessions_to_capacity(state: &mut DecoderState) -> VecDeque<DecoderEntry> {
    let mut evicted = VecDeque::new();
    while state.entries.len() > state.session_capacity {
        let Some(index) =
            state.entries.iter().rposition(|entry| Arc::strong_count(&entry.slot) == 1)
        else {
            break;
        };
        if let Some(entry) = state.entries.remove(index) {
            evicted.push_back(entry);
        }
    }
    if !evicted.is_empty() {
        state.capacity_trim_evictions =
            state.capacity_trim_evictions.saturating_add(evicted.len() as u64);
    }
    evicted
}

fn wait_for_terminal_finalization(
    completion: Receiver<std::result::Result<(), String>>,
    cancellation: &ExecutionCancellationToken,
    shutdown_signal: &AudioWindowDecoderShutdownSignal,
    source: &std::path::Path,
) -> Result<()> {
    let deadline = Instant::now() + DECODER_TERMINAL_STATUS_WAIT;
    loop {
        if cancellation.is_canceled() || shutdown_signal.is_requested() {
            return Err(canceled_audio_decode(source));
        }
        match completion.recv_timeout(CANCELLATION_POLL) {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(reason)) => {
                return Err(MondrianError::DecodeFailed {
                    asset_id: source.display().to_string(),
                    reason,
                });
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(MondrianError::DecodeFailed {
                    asset_id: source.display().to_string(),
                    reason: "persistent audio terminal owner stopped without publication"
                        .to_owned(),
                });
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                return Err(MondrianError::DecodeFailed {
                    asset_id: source.display().to_string(),
                    reason: "persistent audio terminal owner exceeded its bounded EOF grace"
                        .to_owned(),
                });
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

impl Drop for PersistentFfmpegAudioWindowDecoder {
    fn drop(&mut self) {
        self.shutdown_signal.request();
        if let Some(worker) = self.shutdown.get_mut().worker.take() {
            // The pre-existing worker owns every potentially blocking child,
            // pipe, and JoinHandle teardown. Dropping this JoinHandle only
            // detaches supervision; it never transfers foreign Drop work back
            // to the caller.
            drop(worker);
            return;
        }

        // A construction-time spawn failure prevents decode admission. Tests
        // can still inject retained owners into that state, so abandon them
        // deliberately instead of running foreign Drop code on this caller.
        let mut abandoned = AudioWindowDecoderShutdownEvidence::default();
        abandon_teardown_owners(self.teardown_queue.take_pending(), &mut abandoned);
        let entries = std::mem::take(&mut self.state.lock().entries);
        abandon_decoder_entries(entries, &mut abandoned);
        if abandoned.resource_handles_remaining > 0 {
            tracing::error!(
                resource_handles_remaining = abandoned.resource_handles_remaining,
                "persistent audio decoder owners were abandoned after teardown-worker loss"
            );
        }
    }
}

fn run_decoder_teardown_worker(
    state: &Mutex<DecoderState>,
    queue: &DecoderTeardownQueue,
    shutdown_signal: &AudioWindowDecoderShutdownSignal,
    teardown_faulted: &AtomicBool,
    startup: &StartupLane,
) -> AudioWindowDecoderShutdownEvidence {
    shutdown_signal.register_worker();
    let mut evidence = AudioWindowDecoderShutdownEvidence::default();
    let mut initial_sweep_pending = true;
    loop {
        let retired = terminate_teardown_owners(queue.wait_for_work(
            shutdown_signal,
            startup,
            initial_sweep_pending,
        ));
        if teardown_retirement_failed(retired) {
            teardown_faulted.store(true, Ordering::Release);
        }
        evidence.merge(retired);
        if !shutdown_signal.is_requested() {
            continue;
        }

        if initial_sweep_pending {
            initial_sweep_pending = false;
            // Do not let an unrelated native spawn retain idle Sessions. Busy
            // slots keep their cooperative handoff and the final sweep; the
            // teardown queue remains open to every startup producer.
            let idle = {
                let mut state = state.lock();
                let entries = std::mem::take(&mut state.entries);
                let mut idle = VecDeque::new();
                for entry in entries {
                    if Arc::strong_count(&entry.slot) == 1 {
                        idle.push_back(DecoderTeardownOwner::Entry(entry));
                    } else {
                        state.entries.push_back(entry);
                    }
                }
                idle
            };
            evidence.sessions_before = evidence.sessions_before.saturating_add(idle.len());
            let retired = terminate_teardown_owners(idle);
            if teardown_retirement_failed(retired) {
                teardown_faulted.store(true, Ordering::Release);
            }
            evidence.merge(retired);
        }
        if !startup.producers_closed() {
            continue;
        }

        let (entries, retired) = {
            let mut state = state.lock();
            (
                std::mem::take(&mut state.entries),
                std::mem::take(&mut state.retired_shutdown),
            )
        };
        evidence.sessions_before = evidence.sessions_before.saturating_add(entries.len());
        evidence.merge(retired);
        let deadline = Instant::now() + DECODER_SHUTDOWN_SLOT_WAIT;
        evidence.merge(shutdown_entries_until(entries, deadline));
        // Sweep again after signal publication so an acquire that was already
        // inside the state critical section cannot escape terminal evidence.
        let (late_entries, late_retired) = {
            let mut state = state.lock();
            (
                std::mem::take(&mut state.entries),
                std::mem::take(&mut state.retired_shutdown),
            )
        };
        evidence.sessions_before = evidence.sessions_before.saturating_add(late_entries.len());
        evidence.merge(late_retired);
        evidence.merge(shutdown_entries_until(late_entries, deadline));
        // Closing and draining after every admitted slot has quiesced makes a
        // random-seek handoff racing the global signal either visible here or
        // explicitly rejected by the bounded queue.
        evidence.merge(terminate_teardown_owners(queue.close_and_take_pending()));
        return evidence;
    }
}

fn teardown_retirement_failed(evidence: AudioWindowDecoderShutdownEvidence) -> bool {
    evidence.child_process_termination_failures > 0
        || evidence.stdout_pump_threads_panicked > 0
        || evidence.stdout_pump_thread_owner_abandonments > 0
        || evidence.stderr_pump_threads_panicked > 0
        || evidence.stderr_pump_thread_owner_abandonments > 0
        || evidence.resource_handles_remaining > 0
        || evidence.shutdown_worker_panics > 0
        || evidence.shutdown_worker_owner_abandonments > 0
}

fn terminate_teardown_owners(
    owners: VecDeque<DecoderTeardownOwner>,
) -> AudioWindowDecoderShutdownEvidence {
    let mut evidence = AudioWindowDecoderShutdownEvidence::default();
    for owner in owners {
        evidence.merge(terminate_teardown_owner(owner));
    }
    evidence
}

fn terminate_teardown_owner(owner: DecoderTeardownOwner) -> AudioWindowDecoderShutdownEvidence {
    let external_references = match &owner {
        DecoderTeardownOwner::Session(_)
        | DecoderTeardownOwner::PartialSession(_)
        | DecoderTeardownOwner::FinalizeSession { .. } => 0,
        DecoderTeardownOwner::Entry(entry) => Arc::strong_count(&entry.slot).saturating_sub(1),
    };
    let mut owner = ManuallyDrop::new(owner);
    let terminated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &mut *owner {
        DecoderTeardownOwner::Session(session) => session.terminate(),
        DecoderTeardownOwner::PartialSession(session) => session.terminate(),
        DecoderTeardownOwner::FinalizeSession { session, completion } => {
            let result = session.finalize_after_stdout_on_owner_worker();
            let evidence = session.terminate();
            let _ = completion.send(result);
            evidence
        }
        DecoderTeardownOwner::Entry(entry) => {
            let mut slot = entry.slot.lock();
            match slot.as_mut() {
                Some(session) => session.terminate(),
                None => AudioWindowDecoderShutdownEvidence::default(),
            }
        }
    }));
    match terminated {
        Ok(mut terminated) => {
            terminated.external_session_slot_references =
                terminated.external_session_slot_references.saturating_add(external_references);
            drop(ManuallyDrop::into_inner(owner));
            terminated
        }
        Err(payload) => AudioWindowDecoderShutdownEvidence {
            sessions_remaining: 1,
            external_session_slot_references: external_references,
            resource_handles_remaining: 1,
            shutdown_worker_panics: 1,
            shutdown_worker_owner_abandonments: u32::from(
                dispose_canonical_or_abandon_opaque_panic_payload(payload),
            )
            .saturating_add(1),
            ..AudioWindowDecoderShutdownEvidence::default()
        },
    }
}

fn shutdown_entries_until(
    entries: VecDeque<DecoderEntry>,
    deadline: Instant,
) -> AudioWindowDecoderShutdownEvidence {
    let mut evidence = AudioWindowDecoderShutdownEvidence::default();
    let mut pending = entries;
    loop {
        let mut busy = VecDeque::new();
        for entry in pending {
            if let Some(terminated) = shutdown_entry_if_ready(entry, &mut busy) {
                evidence.merge(terminated);
            }
        }
        if busy.is_empty() {
            return evidence;
        }
        if Instant::now() >= deadline {
            for entry in busy {
                let external_references = Arc::strong_count(&entry.slot).saturating_sub(1);
                std::mem::forget(entry);
                evidence.sessions_remaining = evidence.sessions_remaining.saturating_add(1);
                evidence.external_session_slot_references =
                    evidence.external_session_slot_references.saturating_add(external_references);
                evidence.resource_handles_remaining =
                    evidence.resource_handles_remaining.saturating_add(1);
                evidence.shutdown_worker_owner_abandonments =
                    evidence.shutdown_worker_owner_abandonments.saturating_add(1);
            }
            return evidence;
        }
        pending = busy;
        std::thread::sleep(CANCELLATION_POLL);
    }
}

fn shutdown_entry_if_ready(
    entry: DecoderEntry,
    busy: &mut VecDeque<DecoderEntry>,
) -> Option<AudioWindowDecoderShutdownEvidence> {
    let slot_owner = Arc::clone(&entry.slot);
    let Some(mut slot) = slot_owner.try_lock() else {
        busy.push_back(entry);
        return None;
    };
    let external_references = Arc::strong_count(&entry.slot).saturating_sub(2);
    let entry = ManuallyDrop::new(entry);
    let terminated =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match slot.as_mut() {
            Some(session) => session.terminate(),
            None => AudioWindowDecoderShutdownEvidence::default(),
        }));
    match terminated {
        Ok(mut terminated) => {
            slot.take();
            drop(slot);
            drop(slot_owner);
            drop(ManuallyDrop::into_inner(entry));
            terminated.external_session_slot_references =
                terminated.external_session_slot_references.saturating_add(external_references);
            Some(terminated)
        }
        Err(payload) => {
            drop(slot);
            drop(slot_owner);
            Some(AudioWindowDecoderShutdownEvidence {
                sessions_remaining: 1,
                external_session_slot_references: external_references,
                resource_handles_remaining: 1,
                shutdown_worker_panics: 1,
                shutdown_worker_owner_abandonments: u32::from(
                    dispose_canonical_or_abandon_opaque_panic_payload(payload),
                )
                .saturating_add(1),
                ..AudioWindowDecoderShutdownEvidence::default()
            })
        }
    }
}

fn abandon_teardown_owners(
    owners: VecDeque<DecoderTeardownOwner>,
    evidence: &mut AudioWindowDecoderShutdownEvidence,
) {
    let owner_count = owners.len();
    for owner in owners {
        std::mem::forget(owner);
    }
    evidence.sessions_remaining = evidence.sessions_remaining.saturating_add(owner_count);
    evidence.resource_handles_remaining =
        evidence.resource_handles_remaining.saturating_add(owner_count);
    evidence.shutdown_worker_owner_abandonments = evidence
        .shutdown_worker_owner_abandonments
        .saturating_add(owner_count.min(u32::MAX as usize) as u32);
}

fn abandon_decoder_entries(
    entries: VecDeque<DecoderEntry>,
    evidence: &mut AudioWindowDecoderShutdownEvidence,
) {
    let owners = entries.into_iter().map(DecoderTeardownOwner::Entry).collect();
    abandon_teardown_owners(owners, evidence);
}

enum StdoutMessage {
    Data(Vec<u8>),
    Eof,
    Error(String),
}

struct DecodeSession {
    source_path: std::path::PathBuf,
    sample_rate: u32,
    channel_layout: AudioChannelLayout,
    next_frame: i64,
    child: Option<Child>,
    terminal_status: Option<ExitStatus>,
    stdout_rx: Option<Receiver<StdoutMessage>>,
    stderr_rx: Option<Receiver<Vec<u8>>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    pending: Vec<u8>,
    pending_offset: usize,
    ended: bool,
    awaiting_terminal_status: bool,
    permit: Option<DecoderSessionPermit>,
    shutdown_evidence: AudioWindowDecoderShutdownEvidence,
}

struct DecodeSessionSpawnFailure {
    error: Box<MondrianError>,
    owner: Option<PartialDecodeSession>,
}

struct PartialDecodeSession {
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdout_rx: Option<Receiver<StdoutMessage>>,
    stderr_rx: Option<Receiver<Vec<u8>>>,
    stdout_thread: Option<JoinHandle<()>>,
    stderr_thread: Option<JoinHandle<()>>,
    permit: Option<DecoderSessionPermit>,
    shutdown_evidence: AudioWindowDecoderShutdownEvidence,
}

impl PartialDecodeSession {
    fn new(mut child: Child, permit: DecoderSessionPermit) -> Self {
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        Self {
            child: Some(child),
            stdout,
            stderr,
            stdout_rx: None,
            stderr_rx: None,
            stdout_thread: None,
            stderr_thread: None,
            permit: Some(permit),
            shutdown_evidence: AudioWindowDecoderShutdownEvidence::default(),
        }
    }

    fn terminate(&mut self) -> AudioWindowDecoderShutdownEvidence {
        self.stdout_rx.take();
        self.stderr_rx.take();
        self.stdout.take();
        self.stderr.take();
        if let Some(child) = self.child.take() {
            terminate_child_with_evidence(child, &mut self.shutdown_evidence);
        }
        join_pump_thread(
            self.stdout_thread.take(),
            PumpKind::Stdout,
            &mut self.shutdown_evidence,
        );
        join_pump_thread(
            self.stderr_thread.take(),
            PumpKind::Stderr,
            &mut self.shutdown_evidence,
        );
        self.permit.take();
        std::mem::take(&mut self.shutdown_evidence)
    }
}

impl Drop for PartialDecodeSession {
    fn drop(&mut self) {
        // An unexpected caller-side disposal must remain non-blocking. The
        // intended path transfers this owner to the prebuilt teardown worker;
        // a missed transfer deliberately leaks rather than killing/waiting or
        // joining foreign owners on the decode caller.
        if let Some(child) = self.child.take() {
            std::mem::forget(child);
        }
        if let Some(stdout) = self.stdout.take() {
            std::mem::forget(stdout);
        }
        if let Some(stderr) = self.stderr.take() {
            std::mem::forget(stderr);
        }
        if let Some(receiver) = self.stdout_rx.take() {
            std::mem::forget(receiver);
        }
        if let Some(receiver) = self.stderr_rx.take() {
            std::mem::forget(receiver);
        }
        if let Some(worker) = self.stdout_thread.take() {
            std::mem::forget(worker);
        }
        if let Some(worker) = self.stderr_thread.take() {
            std::mem::forget(worker);
        }
        if let Some(permit) = self.permit.take() {
            std::mem::forget(permit);
        }
    }
}

impl DecodeSession {
    #[cfg(all(test, feature = "validation"))]
    fn spawn_with_command(
        key: &SessionKey,
        start_frame: i64,
        permit: DecoderSessionPermit,
        command: impl FnOnce() -> std::result::Result<Command, crate::FfmpegCommandError>,
    ) -> std::result::Result<Self, Box<DecodeSessionSpawnFailure>> {
        Self::spawn_with_factories(key, start_frame, permit, command, Command::spawn, || false)
    }

    fn spawn_with_factories(
        key: &SessionKey,
        start_frame: i64,
        permit: DecoderSessionPermit,
        command: impl FnOnce() -> std::result::Result<Command, crate::FfmpegCommandError>,
        native_spawn: impl FnOnce(&mut Command) -> std::io::Result<Child>,
        canceled: impl Fn() -> bool,
    ) -> std::result::Result<Self, Box<DecodeSessionSpawnFailure>> {
        let (input_start_frame, exact_trim_frames) =
            exact_seek_partition(start_frame, key.sample_rate);
        let pan_filter = identity_pan_filter(key.channel_layout);
        let mut command = command().map_err(|error| {
            Box::new(DecodeSessionSpawnFailure { error: Box::new(error.into()), owner: None })
        })?;
        command.arg("-v").arg("error").arg("-nostdin");
        if input_start_frame > 0 {
            command
                .arg("-ss")
                .arg(audio_frame_timestamp(input_start_frame, key.sample_rate));
        }
        command.arg("-i").arg(&key.source.path);
        if exact_trim_frames > 0 {
            command
                .arg("-ss")
                .arg(audio_frame_timestamp(exact_trim_frames, key.sample_rate));
        }
        command
            .arg("-map")
            .arg(ffmpeg_stream_map(key.source.selection.stream_index()))
            .arg("-vn")
            .arg("-sn")
            .arg("-dn")
            .arg("-f")
            .arg("f32le")
            .arg("-acodec")
            .arg("pcm_f32le")
            .arg("-filter:a")
            .arg(pan_filter)
            .arg("-ac")
            .arg(key.channel_layout.channel_count().to_string())
            .arg("-ar")
            .arg(key.sample_rate.to_string())
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_child_window(&mut command);
        if canceled() {
            return Err(Box::new(DecodeSessionSpawnFailure {
                error: Box::new(canceled_audio_decode(&key.source.path)),
                owner: None,
            }));
        }
        let child = native_spawn(&mut command).map_err(|error| {
            Box::new(DecodeSessionSpawnFailure {
                error: Box::new(MondrianError::DecodeFailed {
                    asset_id: key.source.path.display().to_string(),
                    reason: format!("启动持久音频解码 Session 失败: {error}"),
                }),
                owner: None,
            })
        })?;
        let mut partial = PartialDecodeSession::new(child, permit);
        if canceled() {
            return Err(Box::new(DecodeSessionSpawnFailure {
                error: Box::new(canceled_audio_decode(&key.source.path)),
                owner: Some(partial),
            }));
        }
        let stdout = match partial.stdout.take() {
            Some(stdout) => stdout,
            None => {
                return Err(Box::new(DecodeSessionSpawnFailure {
                    error: Box::new(MondrianError::DecodeFailed {
                        asset_id: key.source.path.display().to_string(),
                        reason: "ffmpeg persistent session did not expose stdout".to_owned(),
                    }),
                    owner: Some(partial),
                }));
            }
        };
        let stderr = match partial.stderr.take() {
            Some(stderr) => stderr,
            None => {
                partial.stdout = Some(stdout);
                return Err(Box::new(DecodeSessionSpawnFailure {
                    error: Box::new(MondrianError::DecodeFailed {
                        asset_id: key.source.path.display().to_string(),
                        reason: "ffmpeg persistent session did not expose stderr".to_owned(),
                    }),
                    owner: Some(partial),
                }));
            }
        };
        let (stdout_tx, stdout_rx) = mpsc::sync_channel(PUMP_LOOKAHEAD_CHUNKS);
        let (stderr_tx, stderr_rx) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("mondrian-audio-stdout".to_owned())
            .spawn(move || pump_stdout(stdout, stdout_tx))
        {
            Ok(thread) => {
                partial.stdout_rx = Some(stdout_rx);
                partial.stdout_thread = Some(thread);
            }
            Err(error) => {
                partial.stderr = Some(stderr);
                return Err(Box::new(DecodeSessionSpawnFailure {
                    error: Box::new(MondrianError::DecodeFailed {
                        asset_id: key.source.path.display().to_string(),
                        reason: format!("启动音频 stdout pump 失败: {error}"),
                    }),
                    owner: Some(partial),
                }));
            }
        };
        if canceled() {
            partial.stderr = Some(stderr);
            return Err(Box::new(DecodeSessionSpawnFailure {
                error: Box::new(canceled_audio_decode(&key.source.path)),
                owner: Some(partial),
            }));
        }
        match std::thread::Builder::new()
            .name("mondrian-audio-stderr".to_owned())
            .spawn(move || pump_stderr(stderr, stderr_tx))
        {
            Ok(thread) => {
                partial.stderr_rx = Some(stderr_rx);
                partial.stderr_thread = Some(thread);
            }
            Err(error) => {
                return Err(Box::new(DecodeSessionSpawnFailure {
                    error: Box::new(MondrianError::DecodeFailed {
                        asset_id: key.source.path.display().to_string(),
                        reason: format!("启动音频 stderr pump 失败: {error}"),
                    }),
                    owner: Some(partial),
                }));
            }
        };

        Ok(Self {
            source_path: key.source.path.clone(),
            sample_rate: key.sample_rate,
            channel_layout: key.channel_layout,
            next_frame: start_frame,
            child: partial.child.take(),
            terminal_status: None,
            stdout_rx: partial.stdout_rx.take(),
            stderr_rx: partial.stderr_rx.take(),
            stdout_thread: partial.stdout_thread.take(),
            stderr_thread: partial.stderr_thread.take(),
            pending: Vec::new(),
            pending_offset: 0,
            ended: false,
            awaiting_terminal_status: false,
            permit: partial.permit.take(),
            shutdown_evidence: std::mem::take(&mut partial.shutdown_evidence),
        })
    }

    fn decode(
        &mut self,
        frame_count: usize,
        cancellation: &ExecutionCancellationToken,
        shutdown_signal: &AudioWindowDecoderShutdownSignal,
    ) -> Result<AudioBuffer> {
        let frame_bytes =
            self.channel_layout.channel_count().saturating_mul(std::mem::size_of::<f32>());
        let target_bytes = frame_count.checked_mul(frame_bytes).ok_or_else(|| {
            MondrianError::Other(anyhow::anyhow!("audio decode window extent overflow"))
        })?;
        let mut bytes = Vec::with_capacity(target_bytes);

        while bytes.len() < target_bytes {
            if cancellation.is_canceled() || shutdown_signal.is_requested() {
                return Err(canceled_audio_decode(&self.source_path));
            }
            self.consume_pending(&mut bytes, target_bytes);
            if bytes.len() >= target_bytes || self.ended {
                break;
            }
            let message = match self.stdout_rx.as_ref() {
                Some(receiver) => receiver.recv_timeout(CANCELLATION_POLL),
                None => break,
            };
            match message {
                Ok(StdoutMessage::Data(chunk)) => {
                    self.pending = chunk;
                    self.pending_offset = 0;
                }
                Ok(StdoutMessage::Eof) => {
                    self.finish_after_stdout()?;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: self.source_path.display().to_string(),
                        reason: "persistent audio stdout pump disconnected without an EOF contract"
                            .to_owned(),
                    });
                }
                Ok(StdoutMessage::Error(reason)) => {
                    return Err(MondrianError::DecodeFailed {
                        asset_id: self.source_path.display().to_string(),
                        reason,
                    });
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        self.capture_exit_status()?;

        if bytes.len() % frame_bytes != 0 {
            return Err(MondrianError::DecodeFailed {
                asset_id: self.source_path.display().to_string(),
                reason: "ffmpeg returned a truncated interleaved f32le audio frame".to_owned(),
            });
        }
        let mut samples = Vec::with_capacity(bytes.len() / std::mem::size_of::<f32>());
        for chunk in bytes.chunks_exact(std::mem::size_of::<f32>()) {
            samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        let decoded_frames = samples.len() / self.channel_layout.channel_count();
        self.next_frame = self.next_frame.saturating_add(decoded_frames as i64);
        Ok(AudioBuffer {
            samples,
            sample_rate: self.sample_rate,
            channel_layout: self.channel_layout,
        })
    }

    fn consume_pending(&mut self, destination: &mut Vec<u8>, target_bytes: usize) {
        let available = self.pending.len().saturating_sub(self.pending_offset);
        let count = available.min(target_bytes.saturating_sub(destination.len()));
        if count > 0 {
            destination.extend_from_slice(
                &self.pending[self.pending_offset..self.pending_offset.saturating_add(count)],
            );
            self.pending_offset = self.pending_offset.saturating_add(count);
        }
        if self.pending_offset >= self.pending.len() {
            self.pending.clear();
            self.pending_offset = 0;
        }
    }

    fn finish_after_stdout(&mut self) -> Result<()> {
        self.ended = true;
        self.awaiting_terminal_status = true;
        self.stdout_rx.take();
        Ok(())
    }

    fn finalize_after_stdout_on_owner_worker(&mut self) -> std::result::Result<(), String> {
        self.ended = true;
        self.awaiting_terminal_status = false;
        self.stdout_rx.take();
        let deadline = Instant::now() + DECODER_TERMINAL_STATUS_WAIT;
        let status = if let Some(status) = self.terminal_status.take() {
            status
        } else {
            loop {
                let status = match self.child.as_mut() {
                    Some(child) => child.try_wait().map_err(|error| {
                        self.shutdown_evidence.child_process_termination_failures = self
                            .shutdown_evidence
                            .child_process_termination_failures
                            .saturating_add(1);
                        format!("检查持久音频解码 Session 终止状态失败: {error}")
                    })?,
                    None => return Ok(()),
                };
                if let Some(status) = status {
                    self.child.take();
                    self.shutdown_evidence.child_processes_observed =
                        self.shutdown_evidence.child_processes_observed.saturating_add(1);
                    self.shutdown_evidence.child_processes_terminated =
                        self.shutdown_evidence.child_processes_terminated.saturating_add(1);
                    break status;
                }
                if Instant::now() >= deadline {
                    return Err(
                        "persistent audio session did not publish terminal status after stdout EOF"
                            .to_owned(),
                    );
                }
                std::thread::sleep(CANCELLATION_POLL);
            }
        };
        let stderr = self.take_stderr_tail();
        if !status.success() {
            return Err(format!(
                "ffmpeg 持久音频解码 Session 失败: {}",
                String::from_utf8_lossy(&stderr)
            ));
        }
        Ok(())
    }

    fn capture_exit_status(&mut self) -> Result<()> {
        if self.terminal_status.is_some() {
            return Ok(());
        }
        let status = match self.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    self.shutdown_evidence.child_process_termination_failures =
                        self.shutdown_evidence.child_process_termination_failures.saturating_add(1);
                    return Err(MondrianError::DecodeFailed {
                        asset_id: self.source_path.display().to_string(),
                        reason: format!("检查持久音频解码 Session 状态失败: {error}"),
                    });
                }
            },
            None => None,
        };
        if let Some(status) = status {
            self.child.take();
            self.shutdown_evidence.child_processes_observed =
                self.shutdown_evidence.child_processes_observed.saturating_add(1);
            self.shutdown_evidence.child_processes_terminated =
                self.shutdown_evidence.child_processes_terminated.saturating_add(1);
            let failed = !status.success();
            self.terminal_status = Some(status);
            if failed {
                return self.finish_after_stdout();
            }
        }
        Ok(())
    }

    fn take_stderr_tail(&mut self) -> Vec<u8> {
        self.stderr_rx
            .take()
            .and_then(|receiver| receiver.try_recv().ok())
            .unwrap_or_default()
    }

    fn terminate_resources(&mut self) {
        self.ended = true;
        self.stdout_rx.take();
        self.stderr_rx.take();
        if let Some(child) = self.child.take() {
            terminate_child_with_evidence(child, &mut self.shutdown_evidence);
        }
        self.terminal_status.take();
        join_pump_thread(
            self.stdout_thread.take(),
            PumpKind::Stdout,
            &mut self.shutdown_evidence,
        );
        join_pump_thread(
            self.stderr_thread.take(),
            PumpKind::Stderr,
            &mut self.shutdown_evidence,
        );
        self.pending.clear();
        self.pending_offset = 0;
    }

    fn terminate(&mut self) -> AudioWindowDecoderShutdownEvidence {
        self.terminate_resources();
        self.permit.take();
        std::mem::take(&mut self.shutdown_evidence)
    }
}

fn ffmpeg_stream_map(stream_index: u32) -> String {
    format!("0:{stream_index}")
}

impl Drop for DecodeSession {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

fn pump_stdout(mut stdout: ChildStdout, sender: SyncSender<StdoutMessage>) {
    loop {
        let mut chunk = vec![0_u8; PUMP_CHUNK_BYTES];
        match stdout.read(&mut chunk) {
            Ok(0) => {
                let _ = sender.send(StdoutMessage::Eof);
                break;
            }
            Ok(count) => {
                chunk.truncate(count);
                if sender.send(StdoutMessage::Data(chunk)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(StdoutMessage::Error(format!(
                    "读取 ffmpeg 持久音频输出失败: {error}"
                )));
                break;
            }
        }
    }
}

fn pump_stderr(mut stderr: ChildStderr, sender: SyncSender<Vec<u8>>) {
    let mut tail = VecDeque::with_capacity(STDERR_TAIL_BYTES);
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match stderr.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(count) => append_bounded_tail(&mut tail, &chunk[..count], STDERR_TAIL_BYTES),
        }
    }
    let _ = sender.send(tail.into_iter().collect());
}

fn append_bounded_tail(tail: &mut VecDeque<u8>, bytes: &[u8], capacity: usize) {
    if capacity == 0 {
        tail.clear();
        return;
    }
    let skip = bytes.len().saturating_sub(capacity);
    tail.extend(bytes[skip..].iter().copied());
    while tail.len() > capacity {
        tail.pop_front();
    }
}

fn exact_seek_partition(start_frame: i64, sample_rate: u32) -> (i64, i64) {
    let start_frame = start_frame.max(0);
    let preroll_frames = i64::from(sample_rate.max(1)).saturating_mul(EXACT_SEEK_PREROLL_SECONDS);
    let input_start_frame = start_frame.saturating_sub(preroll_frames).max(0);
    (
        input_start_frame,
        start_frame.saturating_sub(input_start_frame),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PumpKind {
    Stdout,
    Stderr,
}

fn join_pump_thread(
    thread: Option<JoinHandle<()>>,
    kind: PumpKind,
    evidence: &mut AudioWindowDecoderShutdownEvidence,
) {
    let Some(thread) = thread else {
        return;
    };
    let (panicked, owner_abandoned) = match thread.join() {
        Ok(()) => (false, false),
        Err(payload) => (
            true,
            dispose_canonical_or_abandon_opaque_panic_payload(payload),
        ),
    };
    match kind {
        PumpKind::Stdout => {
            evidence.stdout_pump_threads_observed =
                evidence.stdout_pump_threads_observed.saturating_add(1);
            if panicked {
                evidence.stdout_pump_threads_panicked =
                    evidence.stdout_pump_threads_panicked.saturating_add(1);
                if owner_abandoned {
                    evidence.stdout_pump_thread_owner_abandonments =
                        evidence.stdout_pump_thread_owner_abandonments.saturating_add(1);
                }
            } else {
                evidence.stdout_pump_threads_joined =
                    evidence.stdout_pump_threads_joined.saturating_add(1);
            }
        }
        PumpKind::Stderr => {
            evidence.stderr_pump_threads_observed =
                evidence.stderr_pump_threads_observed.saturating_add(1);
            if panicked {
                evidence.stderr_pump_threads_panicked =
                    evidence.stderr_pump_threads_panicked.saturating_add(1);
                if owner_abandoned {
                    evidence.stderr_pump_thread_owner_abandonments =
                        evidence.stderr_pump_thread_owner_abandonments.saturating_add(1);
                }
            } else {
                evidence.stderr_pump_threads_joined =
                    evidence.stderr_pump_threads_joined.saturating_add(1);
            }
        }
    }
}

fn terminate_child_with_evidence(
    mut child: Child,
    evidence: &mut AudioWindowDecoderShutdownEvidence,
) {
    evidence.child_processes_observed = evidence.child_processes_observed.saturating_add(1);
    let mut failed = false;
    let already_exited = match child.try_wait() {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(_) => {
            failed = true;
            false
        }
    };
    if already_exited {
        evidence.child_processes_terminated = evidence.child_processes_terminated.saturating_add(1);
    } else {
        if child.kill().is_err() {
            failed = true;
        }
        match child.wait() {
            Ok(_) => {
                evidence.child_processes_terminated =
                    evidence.child_processes_terminated.saturating_add(1);
            }
            Err(_) => {
                failed = true;
                evidence.resource_handles_remaining =
                    evidence.resource_handles_remaining.saturating_add(1);
            }
        }
    }
    if failed {
        evidence.child_process_termination_failures =
            evidence.child_process_termination_failures.saturating_add(1);
    }
}

#[cfg(windows)]
fn hide_child_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_child_window(_command: &mut Command) {}

#[cfg(test)]
#[path = "session/startup_tests.rs"]
mod startup_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "validation")]
    #[test]
    fn rejected_command_releases_permit_without_creating_a_partial_child_owner() {
        let pool = Arc::new(DecoderSessionPermitPool::new(1));
        let cancellation = ExecutionCancellationToken::new();
        let shutdown = AudioWindowDecoderShutdownSignal::new();
        let faulted = AtomicBool::new(false);
        let key = test_session_key(0);
        let permit = pool
            .acquire(&cancellation, &shutdown, &faulted, &key.source.path)
            .expect("one physical permit");
        assert_eq!(pool.diagnostics(), (1, 1, 1));
        let failure = DecodeSession::spawn_with_command(&key, 0, permit, || {
            Err(crate::QualifiedFfmpegToolchainError::CapsuleNamespaceChanged.into())
        })
        .err()
        .expect("admission rejected before spawn");
        assert!(
            failure.owner.is_none(),
            "no child or pump owner was created"
        );
        assert!(crate::FfmpegCommandError::is_cause_of(&failure.error));
        assert_eq!(pool.diagnostics(), (0, 1, 1));
        let next = pool
            .acquire(&cancellation, &shutdown, &faulted, &key.source.path)
            .expect("permit can be reacquired without teardown");
        drop(next);
        assert_eq!(pool.diagnostics(), (0, 1, 1));
    }
    use crate::info::ChannelLayout;
    use mondrian_core::MediaFileFingerprint;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Debug)]
    struct DropTrackingPayload {
        dropped: Arc<AtomicBool>,
    }

    impl std::fmt::Display for DropTrackingPayload {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("drop-tracking payload")
        }
    }

    impl std::error::Error for DropTrackingPayload {}

    impl Drop for DropTrackingPayload {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }

    pub(super) fn test_session_key(index: u32) -> SessionKey {
        SessionKey {
            source: AudioSourceIdentity {
                path: PathBuf::from(format!("session-{index}.wav")),
                len: 1,
                modified_secs: Some(1),
                modified_nanos: Some(1),
                selection: super::super::AudioSourceSelection::new(
                    index,
                    ChannelLayout::Exact(AudioChannelLayout::Stereo),
                    MediaFileFingerprint::default(),
                ),
                channel_layout: AudioChannelLayout::Stereo,
            },
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
        }
    }

    fn decoder_with_empty_sessions(count: usize) -> PersistentFfmpegAudioWindowDecoder {
        let mut state = DecoderState::new(count.max(1));
        for index in 0..count {
            state.entries.push_back(DecoderEntry {
                key: test_session_key(index as u32),
                slot: Arc::new(Mutex::new(None)),
            });
        }
        state.peak_sessions = count;
        PersistentFfmpegAudioWindowDecoder::with_state_and_spawner(
            state,
            product_decoder_teardown_spawner(),
        )
    }

    fn decoder_with_session(session: DecodeSession) -> PersistentFfmpegAudioWindowDecoder {
        let mut state = DecoderState::new(1);
        state.entries.push_back(DecoderEntry {
            key: test_session_key(0),
            slot: Arc::new(Mutex::new(Some(session))),
        });
        state.peak_sessions = 1;
        PersistentFfmpegAudioWindowDecoder::with_state_and_spawner(
            state,
            product_decoder_teardown_spawner(),
        )
    }

    fn test_decode_session(
        child: Option<Child>,
        stdout_thread: Option<JoinHandle<()>>,
        stderr_thread: Option<JoinHandle<()>>,
    ) -> DecodeSession {
        DecodeSession {
            source_path: PathBuf::from("shutdown-session.wav"),
            sample_rate: 48_000,
            channel_layout: AudioChannelLayout::Stereo,
            next_frame: 0,
            child,
            terminal_status: None,
            stdout_rx: None,
            stderr_rx: None,
            stdout_thread,
            stderr_thread,
            pending: Vec::new(),
            pending_offset: 0,
            ended: false,
            awaiting_terminal_status: false,
            permit: None,
            shutdown_evidence: AudioWindowDecoderShutdownEvidence::default(),
        }
    }

    #[test]
    fn stderr_tail_is_strictly_bounded_and_keeps_the_latest_bytes() {
        let mut tail = VecDeque::new();
        append_bounded_tail(&mut tail, b"abcdef", 4);
        append_bounded_tail(&mut tail, b"gh", 4);
        assert_eq!(tail.into_iter().collect::<Vec<_>>(), b"efgh");
    }

    #[test]
    fn exact_seek_partition_bounds_decode_preroll_without_shifting_target() {
        let sample_rate = 48_000;
        assert_eq!(exact_seek_partition(0, sample_rate), (0, 0));
        assert_eq!(
            exact_seek_partition(sample_rate.into(), sample_rate),
            (0, 48_000)
        );
        assert_eq!(exact_seek_partition(480_000, sample_rate), (0, 480_000));
        assert_eq!(
            exact_seek_partition(528_000, sample_rate),
            (48_000, 480_000)
        );
        let (input, trim) = exact_seek_partition(i64::MAX, sample_rate);
        assert_eq!(input.saturating_add(trim), i64::MAX);
        assert!(trim <= 480_000);
    }

    #[test]
    fn ffmpeg_stream_map_uses_the_absolute_container_index() {
        assert_eq!(ffmpeg_stream_map(0), "0:0");
        assert_eq!(ffmpeg_stream_map(7), "0:7");
    }

    #[test]
    fn online_session_capacity_reduction_terminates_idle_lru_immediately() {
        let decoder = decoder_with_empty_sessions(4);

        decoder.reconfigure_session_capacity(2);

        let state = decoder.state.lock();
        assert_eq!(state.session_capacity, 2);
        assert_eq!(state.entries.len(), 2);
        assert_eq!(state.entries[0].key, test_session_key(0));
        assert_eq!(state.entries[1].key, test_session_key(1));
        assert_eq!(state.capacity_reconfigurations, 1);
        assert_eq!(state.capacity_trim_evictions, 2);
    }

    #[test]
    fn busy_sessions_converge_after_capacity_reduction_without_forced_termination() {
        let decoder = decoder_with_empty_sessions(3);
        let (first_busy, second_busy, third_busy) = {
            let state = decoder.state.lock();
            (
                Arc::clone(&state.entries[0].slot),
                Arc::clone(&state.entries[1].slot),
                Arc::clone(&state.entries[2].slot),
            )
        };

        decoder.reconfigure_session_capacity(1);
        let busy = decoder.diagnostics();
        assert_eq!(busy.sessions, 0);
        assert_eq!(busy.session_capacity, 1);
        assert_eq!(busy.sessions_above_capacity, 0);
        assert_eq!(busy.capacity_trim_evictions, 0);
        assert_eq!(decoder.state.lock().entries.len(), 3);

        drop(second_busy);
        drop(third_busy);
        decoder.converge_capacity();

        let converged = decoder.diagnostics();
        assert_eq!(converged.sessions, 0);
        assert_eq!(converged.sessions_above_capacity, 0);
        assert_eq!(converged.capacity_trim_evictions, 2);
        assert_eq!(decoder.state.lock().entries.len(), 1);
        assert_eq!(decoder.state.lock().entries[0].key, test_session_key(0));
        drop(first_busy);
    }

    #[test]
    fn physical_session_permit_blocks_replacement_until_retirement() {
        let pool = Arc::new(DecoderSessionPermitPool::new(1));
        let cancellation = ExecutionCancellationToken::new();
        let shutdown = Arc::new(AudioWindowDecoderShutdownSignal::new());
        let faulted = Arc::new(AtomicBool::new(false));
        let first = pool
            .acquire(
                &cancellation,
                &shutdown,
                &faulted,
                std::path::Path::new("first"),
            )
            .expect("first physical session permit");
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let waiting_pool = Arc::clone(&pool);
        let waiting_shutdown = Arc::clone(&shutdown);
        let waiting_faulted = Arc::clone(&faulted);
        let waiter = std::thread::spawn(move || {
            let permit = waiting_pool
                .acquire(
                    &ExecutionCancellationToken::new(),
                    &waiting_shutdown,
                    &waiting_faulted,
                    std::path::Path::new("replacement"),
                )
                .expect("replacement permit");
            sender.send(permit).expect("publish replacement permit");
        });

        std::thread::sleep(Duration::from_millis(20));
        assert!(receiver.try_recv().is_err());
        assert_eq!(pool.diagnostics(), (1, 1, 1));

        drop(first);
        let replacement = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("retirement releases replacement admission");
        waiter.join().expect("permit waiter");
        assert_eq!(pool.diagnostics(), (1, 1, 1));
        drop(replacement);
        assert_eq!(pool.diagnostics(), (0, 1, 1));
    }

    #[test]
    fn ordinary_last_decoder_drop_hands_blocking_teardown_to_background() {
        let release = Arc::new(AtomicBool::new(false));
        let pump_exited = Arc::new(AtomicBool::new(false));
        let pump_release = Arc::clone(&release);
        let pump_exit = Arc::clone(&pump_exited);
        let pump = std::thread::spawn(move || {
            while !pump_release.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            pump_exit.store(true, Ordering::Release);
        });
        let decoder = decoder_with_session(test_decode_session(None, Some(pump), None));
        let (drop_finished_tx, drop_finished_rx) = mpsc::sync_channel(1);
        let dropper = std::thread::spawn(move || {
            drop(decoder);
            let _ = drop_finished_tx.send(());
        });

        if drop_finished_rx.recv_timeout(Duration::from_secs(2)).is_err() {
            release.store(true, Ordering::Release);
            let _ = dropper.join();
            panic!("ordinary persistent decoder Drop blocked on pipe-pump teardown");
        }
        dropper.join().expect("ordinary decoder dropper returns");
        assert!(!pump_exited.load(Ordering::Acquire));

        release.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !pump_exited.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(pump_exited.load(Ordering::Acquire));
    }

    #[test]
    fn construction_time_teardown_worker_spawn_failure_fails_closed() {
        let spawn_attempted = Arc::new(AtomicBool::new(false));
        let observed_spawn = Arc::clone(&spawn_attempted);
        let error_payload_dropped = Arc::new(AtomicBool::new(false));
        let spawn_error_payload_dropped = Arc::clone(&error_payload_dropped);
        let failing_spawner: DecoderTeardownSpawner = Arc::new(move |_work| {
            observed_spawn.store(true, Ordering::Release);
            Err(std::io::Error::other(DropTrackingPayload {
                dropped: Arc::clone(&spawn_error_payload_dropped),
            }))
        });
        let decoder = PersistentFfmpegAudioWindowDecoder::with_state_and_spawner(
            DecoderState::new(1),
            failing_spawner,
        );
        let evidence = decoder.shutdown_sessions();

        assert!(spawn_attempted.load(Ordering::Acquire));
        assert!(!error_payload_dropped.load(Ordering::Acquire));
        assert_eq!(evidence.shutdown_worker_start_failures, 1);
        assert_eq!(evidence.shutdown_workers_started, 0);
        assert_eq!(evidence.shutdown_workers_terminated, 0);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn construction_time_opaque_spawner_panic_is_contained() {
        let panic_payload_dropped = Arc::new(AtomicBool::new(false));
        let spawner_payload_dropped = Arc::clone(&panic_payload_dropped);
        let panicking_spawner: DecoderTeardownSpawner =
            Arc::new(move |_work| -> std::io::Result<JoinHandle<()>> {
                std::panic::panic_any(DropTrackingPayload {
                    dropped: Arc::clone(&spawner_payload_dropped),
                })
            });
        let decoder = PersistentFfmpegAudioWindowDecoder::with_state_and_spawner(
            DecoderState::new(1),
            panicking_spawner,
        );
        let evidence = decoder.shutdown_sessions();

        assert!(!panic_payload_dropped.load(Ordering::Acquire));
        assert_eq!(evidence.shutdown_worker_start_failures, 1);
        assert_eq!(evidence.shutdown_worker_panics, 1);
        assert_eq!(evidence.shutdown_worker_owner_abandonments, 1);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn consuming_decoder_shutdown_reclaims_all_idle_session_slots() {
        let decoder = decoder_with_empty_sessions(3);

        let evidence = decoder.shutdown_sessions();

        assert_eq!(evidence.sessions_before, 3);
        assert_eq!(evidence.sessions_remaining, 0);
        assert!(evidence.all_resources_released());
        assert_eq!(decoder.diagnostics().sessions, 0);
    }

    #[test]
    fn concurrent_shutdown_followers_reuse_the_leader_terminal_receipt() {
        let decoder = Arc::new(PersistentFfmpegAudioWindowDecoder::with_capacity(1));
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut callers = Vec::new();
        for _ in 0..2 {
            let decoder = Arc::clone(&decoder);
            let barrier = Arc::clone(&barrier);
            callers.push(std::thread::spawn(move || {
                barrier.wait();
                decoder.shutdown_sessions()
            }));
        }
        barrier.wait();
        let first = callers.remove(0).join().expect("first shutdown caller");
        let second = callers.remove(0).join().expect("second shutdown caller");

        assert_eq!(first, second);
        assert_eq!(first.shutdown_workers_started, 1);
        assert_eq!(first.shutdown_workers_terminated, 1);
        assert!(first.all_resources_released());
    }

    #[test]
    fn consuming_decoder_shutdown_reaps_child_and_joins_both_pumps() {
        let child = std::process::Command::new(
            std::env::current_exe().expect("current test executable path"),
        )
        .arg("shutdown_child_fixture")
        .env("MONDRIAN_AUDIO_SHUTDOWN_CHILD_FIXTURE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn disposable child process");
        let decoder = decoder_with_session(test_decode_session(
            Some(child),
            Some(std::thread::spawn(|| {})),
            Some(std::thread::spawn(|| {})),
        ));

        let evidence = decoder.shutdown_sessions();

        assert_eq!(evidence.sessions_before, 1);
        assert_eq!(evidence.child_processes_observed, 1);
        assert_eq!(evidence.child_processes_terminated, 1);
        assert_eq!(evidence.stdout_pump_threads_observed, 1);
        assert_eq!(evidence.stdout_pump_threads_joined, 1);
        assert_eq!(evidence.stderr_pump_threads_observed, 1);
        assert_eq!(evidence.stderr_pump_threads_joined, 1);
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn eof_terminal_handoff_waits_for_delayed_normal_process_exit() {
        let child = std::process::Command::new(
            std::env::current_exe().expect("current test executable path"),
        )
        .arg("shutdown_child_fixture")
        .env("MONDRIAN_AUDIO_SHUTDOWN_CHILD_EXIT_DELAY_MS", "20")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn delayed terminal child");
        let decoder = PersistentFfmpegAudioWindowDecoder::with_capacity(1);
        let mut session = test_decode_session(Some(child), None, None);
        session.ended = true;
        session.awaiting_terminal_status = true;
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);

        assert!(decoder.enqueue_teardown(VecDeque::from([
            DecoderTeardownOwner::FinalizeSession { session, completion: completion_tx },
        ])));
        assert_eq!(
            completion_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("terminal publication"),
            Ok(())
        );

        let evidence = decoder.shutdown_sessions();
        assert_eq!(evidence.child_processes_observed, 1);
        assert_eq!(evidence.child_processes_terminated, 1);
        assert!(evidence.all_resources_released());
    }

    #[test]
    fn shutdown_child_fixture() {
        if let Some(delay_ms) = std::env::var_os("MONDRIAN_AUDIO_SHUTDOWN_CHILD_EXIT_DELAY_MS") {
            let delay_ms = delay_ms.to_string_lossy().parse::<u64>().unwrap_or(0);
            std::thread::sleep(Duration::from_millis(delay_ms));
            return;
        }
        if std::env::var_os("MONDRIAN_AUDIO_SHUTDOWN_CHILD_FIXTURE").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    #[test]
    fn consuming_decoder_shutdown_preserves_pump_panics() {
        let decoder = decoder_with_session(test_decode_session(
            None,
            Some(std::thread::spawn(|| panic!("stdout pump panic"))),
            Some(std::thread::spawn(|| {})),
        ));

        let evidence = decoder.shutdown_sessions();

        assert_eq!(evidence.stdout_pump_threads_observed, 1);
        assert_eq!(evidence.stdout_pump_threads_joined, 0);
        assert_eq!(evidence.stdout_pump_threads_panicked, 1);
        assert_eq!(evidence.stdout_pump_thread_owner_abandonments, 0);
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn consuming_decoder_shutdown_abandons_opaque_pump_panic_payload() {
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let pump_payload_dropped = Arc::clone(&payload_dropped);
        let decoder = decoder_with_session(test_decode_session(
            None,
            Some(std::thread::spawn(move || {
                std::panic::panic_any(DropTrackingPayload { dropped: pump_payload_dropped })
            })),
            Some(std::thread::spawn(|| {})),
        ));

        let evidence = decoder.shutdown_sessions();

        assert_eq!(evidence.stdout_pump_threads_observed, 1);
        assert_eq!(evidence.stdout_pump_threads_joined, 0);
        assert_eq!(evidence.stdout_pump_threads_panicked, 1);
        assert_eq!(evidence.stdout_pump_thread_owner_abandonments, 1);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn consuming_decoder_shutdown_aggregates_clean_and_opaque_pump_outcomes() {
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let pump_payload_dropped = Arc::clone(&payload_dropped);
        let mut state = DecoderState::new(2);
        state.entries.push_back(DecoderEntry {
            key: test_session_key(0),
            slot: Arc::new(Mutex::new(Some(test_decode_session(
                None,
                Some(std::thread::spawn(|| {})),
                Some(std::thread::spawn(|| {})),
            )))),
        });
        state.entries.push_back(DecoderEntry {
            key: test_session_key(1),
            slot: Arc::new(Mutex::new(Some(test_decode_session(
                None,
                Some(std::thread::spawn(move || {
                    std::panic::panic_any(DropTrackingPayload { dropped: pump_payload_dropped })
                })),
                Some(std::thread::spawn(|| {})),
            )))),
        });
        state.peak_sessions = 2;
        let decoder = PersistentFfmpegAudioWindowDecoder::with_state_and_spawner(
            state,
            product_decoder_teardown_spawner(),
        );

        let evidence = decoder.shutdown_sessions();

        assert_eq!(evidence.sessions_before, 2);
        assert_eq!(evidence.stdout_pump_threads_observed, 2);
        assert_eq!(evidence.stdout_pump_threads_joined, 1);
        assert_eq!(evidence.stdout_pump_threads_panicked, 1);
        assert_eq!(evidence.stdout_pump_thread_owner_abandonments, 1);
        assert_eq!(evidence.stderr_pump_threads_observed, 2);
        assert_eq!(evidence.stderr_pump_threads_joined, 2);
        assert_eq!(evidence.stderr_pump_threads_panicked, 0);
        assert_eq!(evidence.stderr_pump_thread_owner_abandonments, 0);
        assert!(!payload_dropped.load(Ordering::Acquire));
        assert!(!evidence.all_resources_released());
    }

    #[test]
    fn consuming_decoder_shutdown_reports_external_session_slot_references() {
        let decoder = decoder_with_empty_sessions(1);
        let retained_slot = Arc::clone(&decoder.state.lock().entries[0].slot);
        let retained_guard = retained_slot.lock();

        let evidence = decoder.shutdown_sessions();

        assert_eq!(evidence.sessions_remaining, 1);
        assert_eq!(evidence.external_session_slot_references, 1);
        assert_eq!(evidence.resource_handles_remaining, 1);
        assert!(!evidence.all_resources_released());
        drop(retained_guard);
        drop(retained_slot);
    }
}
