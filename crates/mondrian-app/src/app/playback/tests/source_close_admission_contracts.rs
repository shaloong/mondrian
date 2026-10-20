//! Actual Timeline/Delivery/Runtime and serial Media source-close admission.
//! This BuiltIn/NotApplicable model owns no native child or author source.

use super::project_close_audio_contracts::available_state;
use super::*;
use mondrian_audio::{
    AudioParameterEventBatch, AudioProcessor, AudioProcessorAudioIo,
    AudioProcessorExecutionContract, AudioProcessorFactory, AudioProcessorFactoryStorageBorrow,
    AudioProcessorFactoryStorageError, AudioProcessorHostError, AudioProcessorPrepareRequest,
    AudioProcessorProcessContext, AudioProcessorResolver,
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Mutex as ChannelMutex,
};

struct BindingProbe {
    started: mpsc::SyncSender<()>,
    release: ChannelMutex<mpsc::Receiver<()>>,
    dropped: mpsc::SyncSender<()>,
    created: AtomicUsize,
    binding_queries: AtomicUsize,
    completed_queries: AtomicUsize,
    entries: AtomicUsize,
    processed: AtomicUsize,
    drop_count: AtomicUsize,
    pending_reprepare: AtomicBool,
    drop_thread: parking_lot::Mutex<Option<std::thread::ThreadId>>,
    binding_thread: parking_lot::Mutex<Option<(std::thread::ThreadId, Option<String>)>>,
}

struct BindingResolver(Arc<BindingProbe>);
struct BindingFactory(Arc<BindingProbe>);
struct BindingProcessor(Arc<BindingProbe>);

impl AudioProcessorResolver for BindingResolver {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::NotApplicable
    }

    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        assert!(matches!(
            request.definition(),
            mondrian_timeline::audio::AudioProcessorDefinitionRef::BuiltIn { .. }
        ));
        Ok(Arc::new(BindingFactory(self.0.clone())))
    }
}

impl AudioProcessorFactory for BindingFactory {
    fn native_child_settlement_requirement(
        &self,
    ) -> mondrian_core::NativeChildSettlementRequirement {
        mondrian_core::NativeChildSettlementRequirement::NotApplicable
    }

    fn execution_contract(&self) -> AudioProcessorExecutionContract {
        AudioProcessorExecutionContract::stateless()
    }

    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.0))
    }

    fn create(
        &self,
        _: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.0.created.fetch_add(1, Ordering::AcqRel);
        Ok(Box::new(BindingProcessor(self.0.clone())))
    }
}

impl AudioProcessor for BindingProcessor {
    fn bind_native_child_settlement(
        &mut self,
        _: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<
        mondrian_core::NativeChildSettlementBinding,
        mondrian_core::NativeChildSettlementError,
    > {
        Ok(mondrian_core::NativeChildSettlementBinding::NotApplicable)
    }

    // native_observations_supported remains the ordinary BuiltIn default false.
    // The original binding job still queries this method while holding the
    // actual Timeline Runtime mutex. No test code acquires that mutex itself.
    fn native_audio_object_observation(
        &mut self,
    ) -> Result<Option<mondrian_audio::NativeAudioObjectObservation>, AudioProcessorHostError> {
        assert_eq!(self.0.binding_queries.fetch_add(1, Ordering::AcqRel), 0);
        let current = std::thread::current();
        *self.0.binding_thread.lock() = Some((current.id(), current.name().map(str::to_owned)));
        self.0.started.send(()).expect("original binding worker entered");
        self.0
            .release
            .lock()
            .expect("single controlled receiver")
            .recv_timeout(Duration::from_secs(2))
            .expect("release original binding work within the test deadline");
        self.0.completed_queries.fetch_add(1, Ordering::AcqRel);
        Ok(None) // A real BuiltIn instance has no native object, not fake Closed facts.
    }

    fn preflight_reprepare(
        &mut self,
    ) -> Result<Option<mondrian_audio::NativeObservedRequest>, AudioProcessorHostError> {
        Ok(self.0.pending_reprepare.load(Ordering::Acquire).then_some(
            mondrian_audio::NativeObservedRequest::RequestOnly(
                mondrian_core::NativeAudioReprepareRequest::ClapRestart,
            ),
        ))
    }

    fn enter_state(&mut self, _: i64) -> Result<(), AudioProcessorHostError> {
        self.0.entries.fetch_add(1, Ordering::AcqRel);
        panic!("source-close admission must not enter DSP")
    }

    fn process(
        &mut self,
        _: AudioProcessorProcessContext,
        _: &mut dyn AudioProcessorAudioIo,
        _: AudioParameterEventBatch<'_>,
    ) -> Result<(), AudioProcessorHostError> {
        self.0.processed.fetch_add(1, Ordering::AcqRel);
        panic!("source-close admission must not process PCM")
    }
}

impl Drop for BindingProcessor {
    fn drop(&mut self) {
        *self.0.drop_thread.lock() = Some(std::thread::current().id());
        self.0.drop_count.fetch_add(1, Ordering::AcqRel);
        let _ = self.0.dropped.try_send(());
    }
}

struct ReleaseBinding(Option<mpsc::SyncSender<()>>);
impl ReleaseBinding {
    fn release(&mut self) {
        self.0
            .take()
            .expect("release the one previous work")
            .send(())
            .expect("live worker");
    }
}
impl Drop for ReleaseBinding {
    fn drop(&mut self) {
        // Runs before AppState/Media cleanup, including assertion unwinding.
        if let Some(release) = self.0.take() {
            let _ = release.try_send(());
        }
    }
}

struct BindingControl {
    state: AppState,
    output: Arc<parking_lot::Mutex<NativeHoldPlatformState>>,
    probe: Arc<BindingProbe>,
    started: mpsc::Receiver<()>,
    release: ReleaseBinding,
    dropped: mpsc::Receiver<()>,
    weak: std::sync::Weak<TimelineAudioPcmRenderer>,
}

fn binding_control() -> BindingControl {
    binding_control_with_restart(false)
}

fn binding_control_with_restart(restart: bool) -> BindingControl {
    let (mut state, output) = available_state();
    let (started_tx, started) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let (dropped_tx, dropped) = mpsc::sync_channel(1);
    let probe = Arc::new(BindingProbe {
        started: started_tx,
        release: ChannelMutex::new(release_rx),
        dropped: dropped_tx,
        created: AtomicUsize::new(0),
        binding_queries: AtomicUsize::new(0),
        completed_queries: AtomicUsize::new(0),
        entries: AtomicUsize::new(0),
        processed: AtomicUsize::new(0),
        drop_count: AtomicUsize::new(0),
        pending_reprepare: AtomicBool::new(restart),
        drop_thread: parking_lot::Mutex::new(None),
        binding_thread: parking_lot::Mutex::new(None),
    });
    let release = ReleaseBinding(Some(release));
    state.audio_processor_resolver = Arc::new(BindingResolver(probe.clone()));
    let mut sequence = state.active_sequence().unwrap().clone();
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
        mondrian_timeline::audio::AudioProcessorInstance::built_in(
            mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID,
            1,
        ),
    );
    state.test_set_sequence(Some(sequence));
    state
        .synchronize_playback_authoring_binding()
        .expect("real Engine/author binding");
    let weak = if restart {
        let sequence = state.active_sequence_id().unwrap();
        state
            .rename_sequence(sequence, "ordinary refused restart")
            .expect("one real author transaction for the history guard");
        assert_eq!(probe.created.load(Ordering::Acquire), 0);
        state.play().expect("public play installs the actual Timeline Source");
        state.native_audio_renderer.as_ref().unwrap().clone()
    } else {
        let source = state.audio_playback_renderer("source-close serial control").unwrap().unwrap();
        let weak = Arc::downgrade(&source.renderer);
        state
            .commit_audio_playback("source-close serial control", audio_anchor(0), Some(source))
            .expect("original Media Source and binding work");
        weak
    };

    BindingControl {
        state,
        output,
        probe,
        started,
        release,
        dropped,
        weak,
    }
}

#[test]
fn project_close_waits_for_previous_serial_timeline_binding() {
    let BindingControl {
        mut state,
        output,
        probe,
        started,
        mut release,
        dropped,
        weak,
    } = binding_control();

    // Only this new controlled segment is fenced. Failure still releases the
    // channel before production cleanup joins the original serial workers.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        started
            .recv_timeout(Duration::from_secs(2))
            .expect("binding actually owns Timeline lock");
        assert_eq!(probe.created.load(Ordering::Acquire), 1);
        assert_eq!(probe.binding_queries.load(Ordering::Acquire), 1);
        assert_eq!(probe.completed_queries.load(Ordering::Acquire), 0);
        let (thread, name) = probe.binding_thread.lock().clone().unwrap();
        assert_ne!(thread, std::thread::current().id());
        assert_eq!(name.as_deref(), Some("mondrian-audio-render"));
        let session = state.authoring_session_id().unwrap();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        assert!(state.begin_project_close().expect("one original async close admission"));
        assert!(state.project_close_awaiting_audio());
        assert_eq!(
            state.poll_project_close(),
            crate::app::project_lifecycle::ProjectClosePoll::Pending
        );
        assert_eq!(
            probe.completed_queries.load(Ordering::Acquire),
            0,
            "admission/poll cannot wait for or bypass the occupied Runtime"
        );
        assert!(state.audio_playback.native_editor_source_available());
        assert!(weak.upgrade().is_some());
        assert_eq!(state.authoring_session_id(), Some(session));
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
        assert_eq!(output.lock().queued, 0);

        release.release();
        // This read is deliberately AFTER release. The request producer above
        // never calls a Timeline getter that could lock the occupied Runtime.
        let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().expect("still installed before terminal close poll");
        assert!(verifier.validate(&claim).is_ok());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match state.poll_project_close() {
                crate::app::project_lifecycle::ProjectClosePoll::Pending => {}
                crate::app::project_lifecycle::ProjectClosePoll::Closed => break,
                other => panic!("same admitted close must complete: {other:?}"),
            }
            assert!(
                Instant::now() < deadline,
                "same close request did not settle"
            );
            std::thread::yield_now();
        }
        assert!(!state.audio_playback.native_editor_source_available());
        assert!(state.native_audio_renderer.is_none());
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
        assert!(state.authoring.is_none());
        assert!(matches!(
            state.audio_playback,
            AppAudioPlayback::Available(_)
        ));
        dropped
            .recv_timeout(Duration::from_secs(2))
            .expect("original worker retires actual processor");
        assert!(weak.upgrade().is_none(), "retired before whole-App cleanup");
        assert_eq!(probe.completed_queries.load(Ordering::Acquire), 1);
        assert_eq!(probe.binding_queries.load(Ordering::Acquire), 1);
        assert_eq!(probe.created.load(Ordering::Acquire), 1);
        assert_eq!(probe.entries.load(Ordering::Acquire), 0);
        assert_eq!(probe.processed.load(Ordering::Acquire), 0);
        assert_eq!(probe.drop_count.load(Ordering::Acquire), 1);
        assert_eq!(output.lock().queued, 0);
    }));
    drop(release);
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_resources_released(),
        "SOURCE_CLOSE_CONTROL_CLEANUP_HOLD: {cleanup:?}"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn endurance_shutdown_waits_for_previous_serial_timeline_binding() {
    let BindingControl {
        mut state,
        output,
        probe,
        started,
        mut release,
        dropped,
        weak,
    } = binding_control();
    let deadline = Instant::now() + Duration::from_secs(2);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        started
            .recv_timeout(Duration::from_secs(2))
            .expect("binding actually owns Timeline lock");
        assert_eq!(probe.created.load(Ordering::Acquire), 1);
        assert_eq!(probe.binding_queries.load(Ordering::Acquire), 1);
        assert_eq!(probe.completed_queries.load(Ordering::Acquire), 0);
        let (thread, name) = probe.binding_thread.lock().clone().unwrap();
        assert_ne!(thread, std::thread::current().id());
        assert_eq!(name.as_deref(), Some("mondrian-audio-render"));
        let session = state.authoring_session_id().unwrap();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();

        // This starts the one Project-close operation. Its Audio admission must
        // run before the Media/persistence owners receive their shutdown signals.
        state.begin_endurance_shutdown();
        assert!(state.project_close_awaiting_audio());
        assert_eq!(
            state.poll_project_close(),
            crate::app::project_lifecycle::ProjectClosePoll::Pending
        );
        assert!(state.project_close_awaiting_audio());
        assert_eq!(probe.completed_queries.load(Ordering::Acquire), 0);
        assert_eq!(probe.entries.load(Ordering::Acquire), 0);
        assert_eq!(probe.processed.load(Ordering::Acquire), 0);
        assert!(state.audio_playback.native_editor_source_available());
        assert!(weak.upgrade().is_some());
        let snapshot = state.audio_playback_snapshot();
        assert_ne!(
            snapshot.state,
            mondrian_media::AudioPlaybackState::ExecutionUnavailable
        );
        assert_eq!(snapshot.in_flight, 0);
        assert_eq!(snapshot.next_start_sample, 0);
        assert_eq!(state.project_persistence.pending_requests(), 0);
        assert_eq!(state.authoring_session_id(), Some(session));
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(output.lock().queued, 0);

        release.release();
        // The original Runtime getter is legal only after releasing its lock.
        let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().expect("same Source before consuming shutdown");
        assert!(verifier.validate(&claim).is_ok());
        verifier
    }));
    // Any failed precondition releases the real worker before consuming cleanup.
    drop(release);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        cleanup.all_resources_released(),
        "SOURCE_CLOSE_ENDURANCE_CLEANUP_HOLD: {cleanup:?}"
    );
    let verifier = match result {
        Ok(verifier) => verifier,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    assert!(cleanup.project.session_was_open);
    assert!(cleanup.project.pending_close_was_active);
    assert!(cleanup.project.lifecycle_failure.is_none());
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    dropped.try_recv().expect("original consuming retirement dropped the processor");
    assert!(weak.upgrade().is_none());
    assert_eq!(probe.created.load(Ordering::Acquire), 1);
    assert_eq!(probe.binding_queries.load(Ordering::Acquire), 1);
    assert_eq!(probe.completed_queries.load(Ordering::Acquire), 1);
    assert_eq!(probe.entries.load(Ordering::Acquire), 0);
    assert_eq!(probe.processed.load(Ordering::Acquire), 0);
    assert_eq!(probe.drop_count.load(Ordering::Acquire), 1);
    assert_eq!(output.lock().queued, 0);
}

#[test]
fn project_close_retains_fault_when_timeline_alias_is_lost_after_admission() {
    let BindingControl {
        mut state,
        output,
        probe,
        started,
        mut release,
        dropped,
        weak,
    } = binding_control();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        started
            .recv_timeout(Duration::from_secs(2))
            .expect("actual previous binding entered");
        let session = state.authoring_session_id().unwrap();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        assert!(state.begin_project_close().expect("one close admission"));
        assert!(state.project_close_awaiting_audio());
        // Lose only the App's derived alias, not Media's actual Source/claim.
        // This proves the rejection guard without manufacturing a native fault.
        state.native_audio_renderer = None;
        release.release();
        let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let reason = loop {
            match state.poll_project_close() {
                crate::app::project_lifecycle::ProjectClosePoll::Pending => {}
                crate::app::project_lifecycle::ProjectClosePoll::Faulted(reason) => break reason,
                other => panic!("lost alias must retain a close fault: {other:?}"),
            }
            assert!(Instant::now() < deadline, "same request did not settle");
            std::thread::yield_now();
        };
        assert!(
            reason.contains("installed audio Source has no current Timeline Runtime"),
            "{reason}"
        );
        assert!(
            reason.contains(
                "original audio Source ownership could not be proven during close admission"
            ),
            "{reason}"
        );
        assert!(state.has_project_close_fault());
        assert!(state.project_lifecycle_blocks_actions());
        assert!(
            state.begin_project_close().is_err(),
            "a second close cannot bypass the retained fault"
        );
        assert!(state.audio_playback.native_editor_source_available());
        assert!(verifier.validate(&claim).is_ok());
        assert_eq!(state.authoring_session_id(), Some(session));
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(state.project_persistence.pending_requests(), 0);
        assert_eq!(probe.entries.load(Ordering::Acquire), 0);
        assert_eq!(probe.processed.load(Ordering::Acquire), 0);
        assert_eq!(output.lock().queued, 0);

        state.native_audio_renderer = Some(weak.clone());
        state
            .force_close_project_after_fault()
            .expect("explicit original fault-discard path");
        assert!(state.authoring.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
        dropped
            .recv_timeout(Duration::from_secs(2))
            .expect("original processor retired");
        assert!(weak.upgrade().is_none());
        assert_eq!(probe.binding_queries.load(Ordering::Acquire), 1);
        assert_eq!(probe.drop_count.load(Ordering::Acquire), 1);
    }));
    drop(release);
    // Repair the deliberately removed projection before failure cleanup. This
    // grants no native Closed result; the original fault path still validates.
    if state.audio_playback.native_editor_source_available()
        && state.native_audio_renderer.is_none()
    {
        state.native_audio_renderer = Some(weak);
    }
    let fault_cleanup = if state.has_project_close_fault() {
        state.force_close_project_after_fault().map_err(|error| error.to_string())
    } else {
        Ok(())
    };
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_resources_released(),
        "SOURCE_CLOSE_LOST_ALIAS_CLEANUP_HOLD: {cleanup:?}; fault_cleanup={fault_cleanup:?}"
    );
    assert!(fault_cleanup.is_ok(), "{fault_cleanup:?}");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[derive(Default)]
struct RefusedRestartProbe {
    installs: AtomicUsize,
    drops: AtomicUsize,
    drop_thread: parking_lot::Mutex<Option<std::thread::ThreadId>>,
}

struct RefusedRestartProvider {
    source: std::sync::Weak<dyn AudioPcmRenderer>,
    probe: Arc<RefusedRestartProbe>,
}

impl mondrian_media::AudioPcmNativeReprepareHandoff for RefusedRestartProvider {
    fn renderer(&self) -> &std::sync::Weak<dyn AudioPcmRenderer> {
        &self.source
    }

    fn install(
        &mut self,
        _: mondrian_core::NativeAudioReprepareNotice,
        destination: mondrian_core::AudioOwnerInstallationVerifier,
        _: &mut mondrian_core::AudioOwnerRuntimeInstallationAuthorization,
        _: &mondrian_core::ExecutionCancellationToken,
    ) -> Result<
        mondrian_core::AudioOwnerInstallationClaim,
        mondrian_media::AudioPcmNativeReprepareRefusal,
    > {
        assert_eq!(self.probe.installs.fetch_add(1, Ordering::AcqRel), 0);
        assert!(
            destination.current_claim().is_ok(),
            "actual original Source registration"
        );
        Err(mondrian_media::AudioPcmNativeReprepareRefusal::ProcessorRejected)
    }

    fn retire(&mut self) -> Result<(), mondrian_media::AudioPcmNativeReprepareRefusal> {
        panic!("a refused model install has no installed receipt to retire")
    }

    fn settle(&mut self) -> Result<(), mondrian_media::AudioPcmNativeReprepareRefusal> {
        panic!("a refused model install cannot settle or grant Fresh")
    }
}

impl Drop for RefusedRestartProvider {
    fn drop(&mut self) {
        *self.probe.drop_thread.lock() = Some(std::thread::current().id());
        self.probe.drops.fetch_add(1, Ordering::AcqRel);
    }
}

fn close_after_real_refused_restart(paused: bool) {
    let BindingControl {
        mut state,
        output,
        probe,
        started,
        mut release,
        dropped,
        weak,
    } = binding_control_with_restart(true);
    let refused = Arc::new(RefusedRestartProbe::default());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        started
            .recv_timeout(Duration::from_secs(2))
            .expect("actual original binding work");
        release.release();
        let renderer = weak.upgrade().unwrap();
        let verifier = renderer.owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().unwrap();
        let erased: Arc<dyn AudioPcmRenderer> = renderer.clone();
        let source = Arc::downgrade(&erased);
        drop(erased);
        drop(renderer);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut required = false;
        let mut submitted = false;
        let mut observed = false;
        while !observed {
            let now = state.playback_observation_instant_anchor;
            let poll =
                state.audio_playback.poll(AudioPlaybackMode::Preroll, audio_anchor(0)).unwrap();
            assert_eq!(poll.snapshot.output.unwrap().buffered_frames, 0);
            for event in poll.events {
                match &event {
                    AudioPlaybackEvent::NativeReprepareRequired { .. } => {
                        assert!(!required);
                        required = true;
                    }
                    AudioPlaybackEvent::NativeReprepareOutputQuiesced {
                        pending_generation,
                        ..
                    } => {
                        assert!(required && !submitted);
                        let AppAudioPlayback::Available(playback) = &mut state.audio_playback
                        else {
                            unreachable!()
                        };
                        assert!(playback
                            .finalize_native_control_anchor(*pending_generation, audio_anchor(0))
                            .unwrap());
                        // Replace only the ordinary model's handoff provider.
                        // The actual Media token/Quiesced DTO is consumed first;
                        // the same DTO still reaches the original App handler.
                        // This is not the production native provider or a DLL test.
                        assert!(playback
                            .request_native_reprepare(
                                *pending_generation,
                                Box::new(RefusedRestartProvider {
                                    source: source.clone(),
                                    probe: refused.clone(),
                                })
                            )
                            .unwrap());
                        submitted = true;
                    }
                    AudioPlaybackEvent::NativeReprepareObserved { status, .. } => {
                        assert!(submitted && !observed);
                        assert_eq!(
                            *status,
                            mondrian_media::AudioPcmNativeReprepareStatus::Refused(
                                mondrian_media::AudioPcmNativeReprepareRefusal::ProcessorRejected
                            )
                        );
                        observed = true;
                    }
                    AudioPlaybackEvent::NativeOwnerBindingsObserved { .. } => {}
                    other => panic!("unexpected ordinary restart event: {other:?}"),
                }
                state.handle_audio_playback_event(event, now).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "real refused completion did not arrive"
            );
            std::thread::yield_now();
        }
        assert!(required && submitted);
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(refused.installs.load(Ordering::Acquire), 1);
        assert_eq!(probe.entries.load(Ordering::Acquire), 0);
        assert_eq!(probe.processed.load(Ordering::Acquire), 0);
        assert_eq!(output.lock().queued, 0);
        assert!(verifier.validate(&claim).is_ok());
        let generation = state.audio_playback.native_reprepare_refused_generation().unwrap();
        let old_ticket = state.playback_engine.native_audio_control_hold_ticket().unwrap();
        assert_eq!(old_ticket.pending_generation(), generation);
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::HoldingForNativeAudioControl
        );
        assert!(state.can_undo_action());
        let before = state.playback_engine.snapshot();
        let media = state.audio_playback_snapshot();
        let history = state.authoring_history().unwrap().diagnostics();
        let document = state.authoring.as_ref().unwrap().document().clone();
        let output_id = state.active_sequence().unwrap().audio_program.outputs[0].id;
        let fader = crate::app::product_action::ProductAction::Audio(
            crate::app::product_action::AudioProductAction::EditChannelStrip(
                mondrian_timeline::AudioChannelStripEditRequest {
                    owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput { output_id },
                    edit: mondrian_timeline::AudioChannelStripEdit::SetFaderDb { value: -6.0 },
                },
            ),
        );
        assert!(state.dispatch_action(fader.into_external_action()).is_err());
        assert!(state.undo_timeline().is_err());
        assert_eq!(state.playback_engine.snapshot(), before);
        assert_eq!(state.audio_playback_snapshot(), media);
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
        assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
        if paused {
            state.pause().expect("public pause preserves the failed owner");
            assert_eq!(
                state.playback_engine.snapshot().state,
                TransportState::Paused
            );
            assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
        } else {
            // A genuine Media output rotation leaves the old Engine ticket stale.
            // Public ProjectClose must reject it before any clock, owner or
            // persistence mutation; the next real ack repairs it via the handler.
            state.audio_playback.suspend(audio_anchor(0)).unwrap();
            let engine = state.playback_engine.snapshot();
            let high_water = state.playback_engine.monotonic_high_water();
            let projection = (
                state.playback_observation_instant_anchor,
                state.playback_observation_time_anchor,
            );
            let media = state.audio_playback_snapshot();
            let output_before = {
                let o = output.lock();
                (o.revision, o.acknowledged, o.queued, o.snapshot)
            };
            assert_ne!(
                Some(old_ticket.pending_generation()),
                state.audio_playback.native_reprepare_refused_generation()
            );
            assert!(state.begin_project_close().is_err());
            assert_eq!(state.playback_engine.snapshot(), engine);
            assert_eq!(state.playback_engine.monotonic_high_water(), high_water);
            assert_eq!(
                (
                    state.playback_observation_instant_anchor,
                    state.playback_observation_time_anchor
                ),
                projection
            );
            assert_eq!(state.audio_playback_snapshot(), media);
            assert_eq!(
                {
                    let o = output.lock();
                    (o.revision, o.acknowledged, o.queued, o.snapshot)
                },
                output_before
            );
            assert!(state.pending_project_close.is_none() && state.project_close_fault.is_none());
            assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
            assert_eq!(state.authoring.as_ref().unwrap().document(), &document);
            assert!(verifier.validate(&claim).is_ok());
        }
        // Consume only the actual new output acknowledgement, without PCM or
        // another admitted install. Refused remains Refused in both play goals.
        let poll = state.audio_playback.poll(AudioPlaybackMode::Idle, audio_anchor(0)).unwrap();
        let mut acknowledged = 0;
        for event in poll.events {
            assert!(matches!(
                event,
                AudioPlaybackEvent::NativeReprepareOutputQuiesced { .. }
            ));
            acknowledged += 1;
            let now = state.playback_observation_instant_anchor;
            state.handle_audio_playback_event(event, now).unwrap();
        }
        assert_eq!(acknowledged, 1);
        assert!(state.audio_playback.native_reprepare_refused_generation().is_some());
        if !paused {
            assert_eq!(
                state
                    .playback_engine
                    .native_audio_control_hold_ticket()
                    .unwrap()
                    .pending_generation(),
                state.audio_playback.native_reprepare_refused_generation().unwrap()
            );
        }
        state
            .close_project()
            .expect("public close consumes actual Timeline SourceClosed");
        assert!(state.authoring.is_none() && state.native_audio_renderer.is_none());
        assert!(state.audio_playback_author_binding.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert!(state.audio_playback.native_reprepare_refused_generation().is_none());
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Stopped
        );
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
        dropped
            .recv_timeout(Duration::from_secs(2))
            .expect("original Timeline processor retired");
        let deadline = Instant::now() + Duration::from_secs(2);
        while refused.drops.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "original provider retirement");
            std::thread::yield_now();
        }
        assert!(weak.upgrade().is_none());
        assert_eq!(refused.installs.load(Ordering::Acquire), 1);
        assert_eq!(refused.drops.load(Ordering::Acquire), 1);
        assert_ne!(
            *refused.drop_thread.lock(),
            Some(std::thread::current().id())
        );
        assert_eq!(probe.created.load(Ordering::Acquire), 1);
        assert_eq!(probe.drop_count.load(Ordering::Acquire), 1);
        assert_ne!(
            probe.drop_thread.lock().expect("actual Timeline processor Drop thread"),
            std::thread::current().id()
        );
        assert_eq!(probe.entries.load(Ordering::Acquire), 0);
        assert_eq!(probe.processed.load(Ordering::Acquire), 0);
        assert_eq!(output.lock().queued, 0);
    }));
    drop(release);
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_resources_released(),
        "REFUSED_CLOSE_CLEANUP_HOLD: {cleanup:?}"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn project_close_retires_refused_timeline_restart_without_retry_or_pcm() {
    close_after_real_refused_restart(false);
}

#[test]
fn project_close_retires_paused_refused_timeline_restart_without_entering_pcm() {
    close_after_real_refused_restart(true);
}
