use super::*;
use std::sync::atomic::AtomicUsize;

struct Latch {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

struct Release(mpsc::Sender<()>);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

fn latch() -> (Latch, mpsc::Receiver<()>, Release) {
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    (
        Latch { entered, release: released },
        observed,
        Release(release),
    )
}

/// Ordinary renderer model with an actual Media registration and serial worker.
/// These controls qualify queue/claim lifetime, not native Source validation.
struct CloseRenderer {
    installed: Arc<NativeInstallationProbe>,
    runtime: Mutex<()>,
    poll_latch: Mutex<Option<Latch>>,
    close_latch: Mutex<Option<Latch>>,
    timed_out: AtomicBool,
    pending_prefix: AtomicBool,
    query_error: Mutex<Option<mondrian_core::MondrianError>>,
    getters: AtomicUsize,
    polls: AtomicUsize,
    queries: Mutex<Vec<thread::ThreadId>>,
    renders: AtomicUsize,
}

impl CloseRenderer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            installed: NativeInstallationProbe::new(false, false),
            runtime: Mutex::new(()),
            poll_latch: Mutex::new(None),
            close_latch: Mutex::new(None),
            timed_out: AtomicBool::new(false),
            pending_prefix: AtomicBool::new(false),
            query_error: Mutex::new(None),
            getters: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            queries: Mutex::new(Vec::new()),
            renders: AtomicUsize::new(0),
        })
    }

    fn wait(&self, slot: &Mutex<Option<Latch>>) {
        if let Some(latch) = slot.lock().take() {
            latch.entered.send(()).unwrap();
            if matches!(
                latch.release.recv_timeout(Duration::from_secs(2)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                self.timed_out.store(true, Ordering::Release);
            }
        }
    }
}

impl AudioPcmRenderer for CloseRenderer {
    fn bind_native_child_settlement(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.installed.bind_native_child_settlement(registrar)
    }

    fn native_observations_supported(&self) -> bool {
        true
    }

    fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
        // Like Timeline, the actual getter shares the execution lock. Producer
        // request/finish must not invoke this while a preceding job owns it.
        let _runtime = self.runtime.lock();
        self.getters.fetch_add(1, Ordering::AcqRel);
        self.installed.owner_installation_verifier()
    }

    fn bind_owner_installation(
        &self,
        verifier: AudioOwnerInstallationVerifier,
    ) -> Result<bool, AudioOwnerInstallationError> {
        self.installed.bind_owner_installation(verifier)
    }

    fn install_native_owner_bindings(
        &self,
        claim: &AudioOwnerInstallationClaim,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmNativeOwnerBindingStatus> {
        self.installed.install_native_owner_bindings(claim, cancellation)
    }

    fn poll_native_observations(
        &self,
        _cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<Option<mondrian_core::NativeAudioObservationBatch>> {
        let _runtime = self.runtime.lock();
        self.polls.fetch_add(1, Ordering::AcqRel);
        self.wait(&self.poll_latch);
        Ok(None)
    }

    fn validate_native_author_source_closed(
        &self,
        _cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<()> {
        let _runtime = self.runtime.lock();
        self.queries.lock().push(thread::current().id());
        self.wait(&self.close_latch);
        if let Some(error) = self.query_error.lock().take() {
            return Err(error);
        }
        if self.pending_prefix.load(Ordering::Acquire) {
            Err(source_close::refusal("preceding author prefix is retained"))
        } else {
            Ok(())
        }
    }

    fn render(
        &self,
        request: AudioPcmRenderRequest,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
        self.renders.fetch_add(1, Ordering::AcqRel);
        self.installed.render(request, cancellation)
    }
}

fn prepare_close_renderer() -> (AudioPlayback, Arc<CloseRenderer>, Arc<dyn AudioPcmRenderer>) {
    install_close_renderer(CloseRenderer::new())
}

fn install_close_renderer(
    renderer: Arc<CloseRenderer>,
) -> (AudioPlayback, Arc<CloseRenderer>, Arc<dyn AudioPcmRenderer>) {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
    playback
        .prepare(
            sample_position(0),
            erased.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    (playback, renderer, erased)
}

fn wait_close(playback: &mut AudioPlayback, ticket: &AudioPcmSourceCloseTicket) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap();
        if !matches!(
            playback.source_close_status(ticket).unwrap(),
            AudioPcmSourceCloseStatus::Pending
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "original close query never completed"
        );
        thread::yield_now();
    }
}

#[test]
fn source_close_waits_behind_actual_worker_without_a_caller_runtime_getter() {
    // Arm before actual prepare, so the ordinary first diagnostic is the
    // preceding work. No private in-flight marker is fabricated or cleared.
    let renderer = CloseRenderer::new();
    let (gate, entered, release) = latch();
    *renderer.poll_latch.lock() = Some(gate);
    let (mut playback, renderer, erased) = install_close_renderer(renderer);
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let getter_count = renderer.getters.load(Ordering::Acquire);
    let claim = renderer.installed.verifier().current_claim().unwrap();
    let control = playback.native_control_cancellation.clone();
    let generation = playback.generation;
    let ticket = playback.request_source_close(&erased, sample_position(0)).unwrap();
    assert_eq!(playback.generation, generation + 1, "one original Suspend");
    assert_eq!(renderer.getters.load(Ordering::Acquire), getter_count);
    assert!(
        !control.is_canceled(),
        "original native control token survives"
    );
    assert!(matches!(
        playback.source_close_status(&ticket).unwrap(),
        AudioPcmSourceCloseStatus::Pending
    ));
    assert_eq!(
        playback.reprime(sample_position(0)),
        Err(AudioPlaybackError::SourceClosePending)
    );
    assert_eq!(
        playback.suspend(sample_position(0)),
        Err(AudioPlaybackError::SourceClosePending)
    );
    assert_eq!(
        playback.clear_source(sample_position(0)),
        Err(AudioPlaybackError::SourceClosePending)
    );
    let queued = playback
        .render_queue
        .state
        .lock()
        .pending
        .iter()
        .filter(|work| matches!(work, RenderWork::NativeObservations(_)))
        .count();
    assert_eq!(
        queued, 1,
        "SourceClosed occupies the original observation slot"
    );
    let extra = RenderWork::NativeObservations(NativeObservationWork {
        generation: playback.generation,
        renderer_id: playback.renderer.as_ref().unwrap().materialization_id,
        renderer: erased.clone(),
        cancellation: playback.generation_cancellation.clone(),
        installation_generation: Some(claim.generation()),
        operation: NativeObservationOperation::Poll,
    });
    match playback.render_queue.push(extra) {
        Ok(()) => panic!("SourceClosed must not add another observation slot"),
        Err(work) => playback.handoff_retired_render_owners(VecDeque::from([work]), None),
    }
    drop(release);
    wait_close(&mut playback, &ticket);
    assert!(!renderer.timed_out.load(Ordering::Acquire));
    assert!(matches!(
        playback.source_close_status(&ticket).unwrap(),
        AudioPcmSourceCloseStatus::Closed
    ));
    assert_eq!(
        renderer.queries.lock().as_slice(),
        &[renderer.installed.calls.lock()[0].0]
    );
    assert!(renderer.installed.verifier().validate(&claim).is_ok());
    assert!(playback.matches_source(&erased));
    let queried = renderer.getters.load(Ordering::Acquire);
    // A retained alias can hold Runtime again. Finish needs no foreign getter.
    let held = renderer.runtime.lock();
    playback.finish_source_close(&ticket, sample_position(0)).unwrap();
    drop(held);
    assert_eq!(renderer.getters.load(Ordering::Acquire), queried);
    assert_eq!(renderer.renders.load(Ordering::Acquire), 0);
    assert!(matches!(
        renderer.installed.verifier().validate(&claim),
        Err(AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(!playback.matches_source(&erased));
    assert_eq!(
        playback.finish_source_close(&ticket, sample_position(0)),
        Err(AudioPlaybackError::SourceCloseSuperseded)
    );
    drop(erased);
    drop(renderer);
    assert!(playback.shutdown_and_wait().all_workers_terminated());
}

#[test]
fn source_close_refused_prefix_keeps_claim_and_cancel_never_resumes_pcm() {
    let (mut playback, renderer, erased) = prepare_close_renderer();
    renderer.pending_prefix.store(true, Ordering::Release);
    let claim = renderer.installed.verifier().current_claim().unwrap();
    let ticket = playback.request_source_close(&erased, sample_position(0)).unwrap();
    wait_close(&mut playback, &ticket);
    for _ in 0..2 {
        assert!(
            matches!(playback.source_close_status(&ticket).unwrap(), AudioPcmSourceCloseStatus::Refused(mondrian_core::MondrianError::ActionNotExecuted { action, reason }) if action == "native_author_source_closed" && reason == "preceding author prefix is retained")
        );
    }
    assert_eq!(
        playback.finish_source_close(&ticket, sample_position(0)),
        Err(AudioPlaybackError::SourceCloseNotClosed)
    );
    assert!(renderer.installed.verifier().validate(&claim).is_ok());
    playback.cancel_source_close(&ticket).unwrap();
    assert!(playback.matches_source(&erased));
    assert!(renderer.installed.verifier().validate(&claim).is_ok());
    assert!(playback.render_blocked);
    playback.poll(AudioPlaybackMode::Preroll, sample_position(0)).unwrap();
    assert_eq!(renderer.renders.load(Ordering::Acquire), 0);
    assert_eq!(renderer.queries.lock().len(), 1);
    assert!(renderer.pending_prefix.load(Ordering::Acquire));
    playback.clear_source(sample_position(0)).unwrap();
    drop(erased);
    drop(renderer);
    assert!(playback.shutdown_and_wait().all_workers_terminated());
}

#[test]
fn source_close_default_is_an_actual_refusal_not_closed() {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let renderer = NativeInstallationProbe::new(false, false);
    let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
    playback
        .prepare(
            sample_position(0),
            erased.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    let ticket = playback.request_source_close(&erased, sample_position(0)).unwrap();
    wait_close(&mut playback, &ticket);
    assert!(
        matches!(playback.source_close_status(&ticket).unwrap(), AudioPcmSourceCloseStatus::Refused(mondrian_core::MondrianError::ActionNotExecuted { reason, .. }) if reason == "renderer does not support native author source-closed validation")
    );
    assert!(playback.matches_source(&erased));
    assert_eq!(
        playback.finish_source_close(&ticket, sample_position(0)),
        Err(AudioPlaybackError::SourceCloseNotClosed)
    );
    playback.cancel_source_close(&ticket).unwrap();
    playback.clear_source(sample_position(0)).unwrap();
    drop(erased);
    drop(renderer);
    assert!(playback.shutdown_and_wait().all_workers_terminated());
}

#[test]
fn source_close_cancelled_late_completion_cannot_complete_the_next_ticket() {
    let (mut playback, renderer, erased) = prepare_close_renderer();
    let (gate, entered, release) = latch();
    *renderer.close_latch.lock() = Some(gate);
    let first = playback.request_source_close(&erased, sample_position(0)).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    playback.cancel_source_close(&first).unwrap();
    assert_eq!(
        playback.source_close_status(&first).unwrap_err(),
        AudioPlaybackError::SourceCloseSuperseded
    );
    let second = playback.request_source_close(&erased, sample_position(0)).unwrap();
    assert!(matches!(
        playback.source_close_status(&second).unwrap(),
        AudioPcmSourceCloseStatus::Pending
    ));
    drop(release);
    wait_close(&mut playback, &second);
    assert!(!renderer.timed_out.load(Ordering::Acquire));
    assert_eq!(renderer.queries.lock().len(), 2);
    assert!(playback.stale_completion_count >= 1);
    assert!(matches!(
        playback.source_close_status(&second).unwrap(),
        AudioPcmSourceCloseStatus::Closed
    ));
    assert_eq!(
        playback.cancel_source_close(&first),
        Err(AudioPlaybackError::SourceCloseSuperseded)
    );
    playback.finish_source_close(&second, sample_position(0)).unwrap();
    drop(erased);
    drop(renderer);
    assert!(playback.shutdown_and_wait().all_workers_terminated());
}

#[test]
fn source_close_wrong_source_and_wrong_playback_ticket_preserve_installed_owners() {
    let (mut first, renderer, erased) = prepare_close_renderer();
    let (mut second, other, other_erased) = prepare_close_renderer();
    let generation = first.generation;
    assert!(matches!(
        first.request_source_close(&other_erased, sample_position(0)),
        Err(AudioPlaybackError::OwnerInstallation(
            AudioOwnerInstallationError::WrongRegistration
        ))
    ));
    assert_eq!(first.generation, generation);
    assert!(first.matches_source(&erased));
    let ticket = first.request_source_close(&erased, sample_position(0)).unwrap();
    assert_eq!(
        second.cancel_source_close(&ticket),
        Err(AudioPlaybackError::SourceCloseSuperseded)
    );
    assert!(second.matches_source(&other_erased));
    wait_close(&mut first, &ticket);
    first.finish_source_close(&ticket, sample_position(0)).unwrap();
    second.clear_source(sample_position(0)).unwrap();
    drop((renderer, erased, other, other_erased));
    assert!(first.shutdown_and_wait().all_workers_terminated());
    assert!(second.shutdown_and_wait().all_workers_terminated());
}

#[test]
fn source_close_failed_queue_submission_keeps_source_and_retires_original_work() {
    let (mut playback, renderer, erased) = prepare_close_renderer();
    let claim = renderer.installed.verifier().current_claim().unwrap();
    let stopped = playback.render_queue.stop();
    playback.handoff_retired_render_owners(stopped, None);
    assert!(matches!(
        playback.request_source_close(&erased, sample_position(0)),
        Err(AudioPlaybackError::ExecutionUnavailable)
    ));
    assert!(playback.source_close_admission.is_none());
    assert!(playback.matches_source(&erased));
    assert!(renderer.installed.verifier().validate(&claim).is_ok());
    assert_eq!(renderer.queries.lock().len(), 0);
    drop(erased);
    drop(renderer);
    assert!(playback.shutdown_and_wait().all_workers_terminated());
}

#[test]
fn source_close_uses_original_observation_capacity_and_reports_actual_inline_layout() {
    assert_eq!(MAX_PENDING_NATIVE_WORK, MAX_PENDING_NATIVE_EDITOR_WORK + 3);
    let config = AudioPlaybackConfig {
        max_in_flight: MAX_RETIRED_RENDER_OWNERS - MAX_PENDING_NATIVE_WORK,
        ..test_config()
    };
    assert!(
        validate_config(config).is_err(),
        "original combined 256 bound is unchanged"
    );
    assert!(
        std::mem::size_of::<NativeObservationOperation>()
            >= std::mem::size_of::<AudioPcmSourceCloseTicket>()
    );
    assert!(
        std::mem::size_of::<NativeObservationWork>()
            >= std::mem::size_of::<NativeObservationOperation>()
    );
    assert!(
        std::mem::size_of::<Option<source_close::SourceCloseAdmission>>()
            >= std::mem::size_of::<AudioPcmSourceCloseTicket>()
    );
}

#[derive(Debug)]
struct ErrorDropThread(Arc<Mutex<Option<thread::ThreadId>>>);
impl fmt::Display for ErrorDropThread {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("source-close foreign error")
    }
}
impl std::error::Error for ErrorDropThread {}
impl Drop for ErrorDropThread {
    fn drop(&mut self) {
        *self.0.lock() = Some(thread::current().id());
    }
}

#[test]
fn source_close_stopped_publication_retires_the_original_foreign_error() {
    let (mut playback, renderer, erased) = prepare_close_renderer();
    let error_drop = Arc::new(Mutex::new(None));
    *renderer.query_error.lock() = Some(mondrian_core::MondrianError::Other(anyhow::Error::new(
        ErrorDropThread(error_drop.clone()),
    )));
    let (gate, entered, release) = latch();
    *renderer.close_latch.lock() = Some(gate);
    let _ticket = playback.request_source_close(&erased, sample_position(0)).unwrap();
    entered.recv_timeout(Duration::from_secs(2)).unwrap();
    playback.begin_shutdown();
    assert!(
        error_drop.lock().is_none(),
        "query still owns its original error"
    );
    drop(release);
    let worker = renderer.installed.calls.lock()[0].0;
    drop((erased, renderer));
    let evidence = playback.shutdown_and_wait();
    assert!(evidence.all_workers_terminated());
    let dropped_on = error_drop.lock().expect("original error actually retired");
    assert_ne!(dropped_on, thread::current().id());
    assert_ne!(
        dropped_on, worker,
        "foreign error uses the original retirement worker"
    );
}

#[test]
fn source_close_cancel_closed_preserves_source_without_reenter_or_query() {
    let (mut playback, renderer, erased) = prepare_close_renderer();
    let ticket = playback.request_source_close(&erased, sample_position(0)).unwrap();
    wait_close(&mut playback, &ticket);
    assert!(matches!(
        playback.source_close_status(&ticket).unwrap(),
        AudioPcmSourceCloseStatus::Closed
    ));
    let claim = renderer.installed.verifier().current_claim().unwrap();
    playback.cancel_source_close(&ticket).unwrap();
    assert!(playback.matches_source(&erased));
    assert!(renderer.installed.verifier().validate(&claim).is_ok());
    assert!(playback.render_blocked);
    assert_eq!(renderer.queries.lock().len(), 1);
    assert_eq!(renderer.renders.load(Ordering::Acquire), 0);
    playback.clear_source(sample_position(0)).unwrap();
    drop((renderer, erased));
    assert!(playback.shutdown_and_wait().all_workers_terminated());
}
