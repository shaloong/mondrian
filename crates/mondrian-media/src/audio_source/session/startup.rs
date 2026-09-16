//! Native process construction, isolated from cancellable read and teardown.

use super::*;
use crate::audio_source::AudioDecoderStartupShutdownEvidence;

type SpawnResult = std::result::Result<DecodeSession, Box<DecodeSessionSpawnFailure>>;

pub(super) fn product_startup_spawner() -> DecoderTeardownSpawner {
    Arc::new(|work| {
        std::thread::Builder::new()
            .name("mondrian-audio-source-startup".to_owned())
            .spawn(work)
    })
}

pub(super) struct StartupLane {
    state: Mutex<StartupState>,
    ready: Condvar,
    finished: AtomicBool,
    signal: Arc<AudioWindowDecoderShutdownSignal>,
    retirement: Arc<DecoderTeardownQueue>,
    decoder_state: Arc<Mutex<DecoderState>>,
    faulted: Arc<AtomicBool>,
}

struct StartupState {
    requests: VecDeque<StartupRequest>,
    evidence: AudioDecoderStartupShutdownEvidence,
}

struct StartupRequest {
    key: SessionKey,
    start_frame: i64,
    permit: DecoderSessionPermit,
    command: Command,
    cancellation: ExecutionCancellationToken,
    completion: StartupCompletion,
    sender: SyncSender<StartupCompletion>,
    #[cfg(test)]
    native_spawn: Option<NativeChildSpawner>,
}

/// A move-only producer lease; its Drop only transfers native owners to teardown.
/// In particular, a disconnected or buffered channel cannot synchronously drop a Session.
pub(super) struct StartupCompletion {
    lane: Arc<StartupLane>,
    outcome: Option<SpawnResult>,
    published: bool,
    claimed: bool,
}

impl StartupCompletion {
    pub(super) fn take(&mut self) -> SpawnResult {
        let Some(outcome) = self.outcome.take() else {
            return Err(Box::new(DecodeSessionSpawnFailure {
                error: Box::new(MondrianError::DecodeFailed {
                    asset_id: String::new(),
                    reason: "startup completion was already claimed".to_owned(),
                }),
                owner: None,
            }));
        };
        self.claimed = true;
        self.published = false;
        let mut state = self.lane.state.lock();
        state.evidence.requests_claimed += 1;
        state.evidence.unclaimed_results_remaining -= 1;
        outcome
    }
}

impl Drop for StartupCompletion {
    fn drop(&mut self) {
        // Retire first, release producer last. Queue closure cannot race this handoff.
        if let Some(outcome) = self.outcome.take() {
            let owner = match outcome {
                Ok(session) => Some(DecoderTeardownOwner::Session(session)),
                Err(failure) => failure.owner.map(DecoderTeardownOwner::PartialSession),
            };
            if let Some(owner) = owner {
                let overflow = self.lane.retirement.push(VecDeque::from([owner]));
                if !overflow.is_empty() {
                    self.lane.faulted.store(true, Ordering::Release);
                    let mut evidence = AudioWindowDecoderShutdownEvidence::default();
                    abandon_teardown_owners(overflow, &mut evidence);
                    self.lane.decoder_state.lock().retired_shutdown.merge(evidence);
                }
            }
        }
        let mut state = self.lane.state.lock();
        if !self.claimed {
            state.evidence.requests_retired += 1;
        }
        if self.published {
            state.evidence.unclaimed_results_remaining -= 1;
        }
        state.evidence.producers_remaining -= 1;
        drop(state);
        self.lane.ready.notify_all();
        self.lane.signal.notify_worker();
    }
}

impl StartupLane {
    pub(super) fn new(
        signal: Arc<AudioWindowDecoderShutdownSignal>,
        retirement: Arc<DecoderTeardownQueue>,
        decoder_state: Arc<Mutex<DecoderState>>,
        faulted: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(StartupState {
                requests: VecDeque::new(),
                evidence: AudioDecoderStartupShutdownEvidence {
                    required: true,
                    ..Default::default()
                },
            }),
            ready: Condvar::new(),
            finished: AtomicBool::new(false),
            signal,
            retirement,
            decoder_state,
            faulted,
        })
    }

    pub(super) fn start(
        self: &Arc<Self>,
        spawner: DecoderTeardownSpawner,
    ) -> Option<JoinHandle<()>> {
        self.state.lock().evidence.attempted = true;
        let lane = Arc::clone(self);
        let spawned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawner(Box::new(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lane.run()));
                if let Err(payload) = result {
                    lane.faulted.store(true, Ordering::Release);
                    let abandoned = dispose_canonical_or_abandon_opaque_panic_payload(payload);
                    let mut state = lane.state.lock();
                    state.evidence.panics += 1;
                    state.evidence.owner_abandonments += u32::from(abandoned);
                    state.evidence.unverified_native_owners += 1;
                }
                let pending = {
                    let mut state = lane.state.lock();
                    state.evidence.queued_remaining = 0;
                    std::mem::take(&mut state.requests)
                };
                // Request envelopes remain producer owners during this disposal.
                drop(pending);
                lane.finished.store(true, Ordering::Release);
                lane.ready.notify_all();
                lane.signal.notify_worker();
            }))
        }));
        match spawned {
            Ok(Ok(worker)) => {
                self.state.lock().evidence.workers_started = 1;
                Some(worker)
            }
            failure => {
                let (panics, abandoned) = match failure {
                    Ok(Err(error)) => (0, abandon_io_error(error).1),
                    Err(payload) => (
                        1,
                        dispose_canonical_or_abandon_opaque_panic_payload(payload),
                    ),
                    Ok(Ok(_)) => unreachable!(),
                };
                let mut state = self.state.lock();
                state.evidence.start_failures = 1;
                state.evidence.panics = panics;
                state.evidence.owner_abandonments = u32::from(abandoned);
                drop(state);
                self.faulted.store(true, Ordering::Release);
                self.finished.store(true, Ordering::Release);
                self.signal.request();
                None
            }
        }
    }

    pub(super) fn wait(
        self: &Arc<Self>,
        key: &SessionKey,
        start_frame: i64,
        permit: DecoderSessionPermit,
        cancellation: &ExecutionCancellationToken,
        #[cfg(test)] native_spawn: Option<NativeChildSpawner>,
    ) -> Result<StartupCompletion> {
        // Admission remains synchronous and typed. Cancellation cannot replace a rejection.
        let command = crate::ffmpeg_command()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut state = self.state.lock();
        loop {
            if cancellation.is_canceled() || self.signal.is_requested() {
                return Err(canceled_audio_decode(&key.source.path));
            }
            if self.faulted.load(Ordering::Acquire) || self.finished.load(Ordering::Acquire) {
                return Err(startup_unavailable(&key.source.path));
            }
            // Physical permits bound native owners. This structural bound additionally
            // covers unclaimed failed results whose native permit has already retired.
            if state.evidence.producers_remaining < AUDIO_SOURCE_DECODER_SESSION_CAPACITY_MAX {
                break;
            }
            self.ready.wait_for(&mut state, CANCELLATION_POLL);
        }
        state.evidence.requests_admitted += 1;
        state.evidence.producers_remaining += 1;
        state.evidence.queued_remaining += 1;
        state.requests.push_back(StartupRequest {
            key: key.clone(),
            start_frame,
            permit,
            command,
            cancellation: cancellation.clone(),
            sender,
            completion: StartupCompletion {
                lane: Arc::clone(self),
                outcome: None,
                published: false,
                claimed: false,
            },
            #[cfg(test)]
            native_spawn,
        });
        drop(state);
        self.signal.notify_startup_worker();
        loop {
            if cancellation.is_canceled() || self.signal.is_requested() {
                return Err(canceled_audio_decode(&key.source.path));
            }
            match receiver.recv_timeout(CANCELLATION_POLL) {
                Ok(completion) => return Ok(completion),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(startup_unavailable(&key.source.path))
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn run(&self) {
        self.signal.register_startup_worker();
        loop {
            let request = {
                let mut state = self.state.lock();
                let request = state.requests.pop_front();
                if request.is_some() {
                    state.evidence.queued_remaining -= 1;
                    state.evidence.in_flight_remaining += 1;
                } else if self.signal.is_requested() {
                    return;
                }
                request
            };
            let Some(request) = request else {
                std::thread::park();
                continue;
            };
            let StartupRequest {
                key,
                start_frame,
                permit,
                command,
                cancellation,
                mut completion,
                sender,
                #[cfg(test)]
                native_spawn,
            } = request;
            let canceled = || cancellation.is_canceled() || self.signal.is_requested();
            let outcome = if canceled() {
                self.state.lock().evidence.canceled_before_spawn += 1;
                drop(permit);
                Err(Box::new(DecodeSessionSpawnFailure {
                    error: Box::new(canceled_audio_decode(&key.source.path)),
                    owner: None,
                }))
            } else {
                let native_attempted = std::cell::Cell::new(false);
                let outcome = DecodeSession::spawn_with_factories(
                    &key,
                    start_frame,
                    permit,
                    || Ok(command),
                    |command| {
                        native_attempted.set(true);
                        #[cfg(test)]
                        if let Some(spawn) = native_spawn {
                            return spawn(command);
                        }
                        command.spawn()
                    },
                    canceled,
                );
                if !native_attempted.get() {
                    // Includes the last pre-native checkpoint during argument
                    // preparation, not only cancellation observed on dequeue.
                    self.state.lock().evidence.canceled_before_spawn += 1;
                }
                outcome
            };
            completion.outcome = Some(outcome);
            completion.published = true;
            {
                let mut state = self.state.lock();
                state.evidence.in_flight_remaining -= 1;
                state.evidence.unclaimed_results_remaining += 1;
            }
            // Capacity one, exactly one send. SendError owns the same safe envelope.
            let _ = sender.send(completion);
        }
    }

    pub(super) fn producers_closed(&self) -> bool {
        self.finished.load(Ordering::Acquire) && self.state.lock().evidence.producers_remaining == 0
    }

    pub(super) fn join(
        &self,
        worker: Option<JoinHandle<()>>,
    ) -> AudioDecoderStartupShutdownEvidence {
        if let Some(worker) = worker {
            if worker.thread().id() == std::thread::current().id() {
                self.state.lock().evidence.owner_abandonments += 1;
                drop(worker);
            } else {
                let result = worker.join();
                let abandoned = result.err().map(dispose_canonical_or_abandon_opaque_panic_payload);
                let mut state = self.state.lock();
                state.evidence.workers_joined += 1;
                if let Some(abandoned) = abandoned {
                    state.evidence.panics += 1;
                    state.evidence.owner_abandonments += u32::from(abandoned);
                }
                if !self.finished.load(Ordering::Acquire) {
                    state.evidence.publication_missing += 1;
                }
            }
        }
        self.state.lock().evidence
    }
}

fn startup_unavailable(path: &std::path::Path) -> MondrianError {
    MondrianError::DecodeFailed {
        asset_id: path.display().to_string(),
        reason: "persistent audio startup owner is unavailable".to_owned(),
    }
}
