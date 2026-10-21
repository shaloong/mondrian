//! Portable serial queue and Source custody controls, not native COM evidence.
use super::*;
use mondrian_core::{
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as EditorOut,
    NativeAudioEditorTarget as Target, NativeAudioParameterQuery as Query,
    NativeAudioParameterQueryOutcome as Out, NativeAudioParameterQueryRefusal as Refusal,
};
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::atomic::AtomicUsize;

struct QueryRenderer {
    installed: Arc<NativeInstallationProbe>,
    calls: AtomicUsize,
    threads: Mutex<Vec<thread::ThreadId>>,
    cancel: AtomicBool,
    gated: AtomicBool,
    entered: AtomicBool,
    release: AtomicBool,
    wait_after_cancel: AtomicBool,
}
impl QueryRenderer {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            installed: NativeInstallationProbe::new(false, false),
            calls: AtomicUsize::new(0),
            threads: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
            gated: AtomicBool::new(false),
            entered: AtomicBool::new(false),
            release: AtomicBool::new(false),
            wait_after_cancel: AtomicBool::new(false),
        })
    }
    fn query(&self) -> Query {
        let root_plan = mondrian_core::AudioPreparedPlanId::new();
        let root_session = mondrian_core::AudioRenderSessionId::new();
        Query {
            target: Target {
                root_plan,
                root_session,
                local_plan: root_plan,
                local_session: root_session,
                instance: mondrian_core::AudioProcessorInstanceId::new(),
                owner: mondrian_core::NativeAudioEditorOccurrenceOwner::Output(
                    mondrian_core::ProgramOutputId::new(),
                ),
                insertion: mondrian_core::NativeAudioEditorInsertion::PreFader,
                continuity_epoch: None,
                binding: mondrian_core::AudioProcessorBindingId::new(),
                installed_generation: self
                    .installed
                    .verifier()
                    .current_claim()
                    .unwrap()
                    .generation(),
            },
            parameter: mondrian_core::ParameterId::new("vst3.param.0").unwrap(),
        }
    }
}
impl AudioPcmRenderer for QueryRenderer {
    fn bind_native_child_settlement(
        &self,
        registrar: &mondrian_core::NativeChildSettlementRegistrar,
    ) -> Result<(), mondrian_core::NativeChildSettlementError> {
        self.installed.bind_native_child_settlement(registrar)
    }
    fn owner_installation_verifier(&self) -> Option<AudioOwnerInstallationVerifier> {
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
    fn control_native_editor(
        &self,
        intent: Intent,
        _: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<EditorOut> {
        Ok(match intent {
            Intent::Open { target, .. } => EditorOut::Opened { target, width: 320, height: 160 },
            Intent::Close { target } => EditorOut::Closed { target },
            _ => panic!("explicit test target only"),
        })
    }
    fn query_native_parameter(
        &self,
        query: &Query,
        cancellation: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<Out> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.threads.lock().push(thread::current().id());
        self.entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.gated.load(Ordering::Acquire)
            && !self.release.load(Ordering::Acquire)
            && (self.wait_after_cancel.load(Ordering::Acquire) || !cancellation.is_canceled())
            && Instant::now() < deadline
        {
            thread::yield_now();
        }
        if self.cancel.load(Ordering::Acquire) || cancellation.is_canceled() {
            return Err(mondrian_core::MondrianError::Cancelled);
        }
        let fact = mondrian_core::NativeAudioParameterFact {
            revision: 3,
            parameter: query.parameter.clone(),
            event_flags: 0,
            value_bits: Some(0.25_f64.to_bits()),
            author_value_bits: Some(0.25_f64.to_bits()),
            origin: mondrian_core::NativeAudioParameterObservationOrigin::MainQuery {
                rescan_flags: 0,
            },
            hidden: Some(false),
            periodic: None,
            name: Some("Probe".into()),
            module: None,
            info_flags: Some(1),
            display: Some("0.25".into()),
        };
        Ok(Out::Observed(Box::new(
            mondrian_core::NativeAudioParameterObservation::project(
                query.target,
                NonZeroU32::new(std::process::id()).unwrap(),
                NonZeroU64::new(7).unwrap(),
                3,
                vec![fact],
            )
            .unwrap(),
        )))
    }
    fn render(
        &self,
        _: AudioPcmRenderRequest,
        _: &ExecutionCancellationToken,
    ) -> mondrian_core::Result<AudioPcmRenderOutcome> {
        panic!("Idle query cannot dispatch PCM")
    }
}
fn wait_event(
    playback: &mut AudioPlayback,
    accept: impl Fn(&AudioPlaybackEvent) -> bool,
) -> Vec<AudioPlaybackEvent> {
    let mut events = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        events.extend(playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap().events);
        if events.iter().any(&accept) {
            return events;
        }
        thread::yield_now();
    }
    panic!("original serial control completion did not arrive");
}
fn opened(playback: &mut AudioPlayback, renderer: &Arc<QueryRenderer>) -> Query {
    let query = renderer.query();
    assert_eq!(
        playback
            .request_native_parameter_query(NonZeroU64::new(1).unwrap(), query.clone())
            .unwrap(),
        Err(Refusal::OwnerChanged)
    );
    assert_eq!(renderer.calls.load(Ordering::Acquire), 0);
    playback
        .request_native_audio_editor(Intent::Open {
            target: query.target,
            title: mondrian_core::NativeAudioEditorTitle::new("queue query control".into())
                .unwrap(),
        })
        .unwrap();
    wait_event(playback, |e| {
        matches!(
            e,
            AudioPlaybackEvent::NativeEditorObserved { outcome: EditorOut::Opened { .. }, .. }
        )
    });
    query
}
#[test]
fn native_parameter_query_media_queues_one_current_query_on_original_worker_without_pcm() {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let renderer = QueryRenderer::new();
    playback
        .prepare(
            sample_position(0),
            renderer.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    let query = opened(&mut playback, &renderer);
    let ticket = NonZeroU64::new(17).unwrap();
    assert_eq!(
        playback.request_native_parameter_query(ticket, query.clone()).unwrap(),
        Ok(())
    );
    assert_eq!(
        playback
            .request_native_parameter_query(NonZeroU64::new(18).unwrap(), query.clone())
            .unwrap(),
        Err(Refusal::Busy)
    );
    let events = wait_event(&mut playback, |e| {
        matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { .. })
    });
    let observed = events
        .iter()
        .find_map(|e| match e {
            AudioPlaybackEvent::NativeParameterQueryCompleted {
                ticket: actual,
                query: selected,
                completion:
                    AudioNativeParameterQueryCompletion::Completed(Out::Observed(observation)),
                ..
            } => {
                assert_eq!(*actual, ticket);
                assert_eq!(selected, &query);
                Some(observation)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(observed.facts().len(), 1);
    assert_eq!(observed.facts()[0].parameter, query.parameter);
    assert_eq!(renderer.calls.load(Ordering::Acquire), 1);
    assert_ne!(renderer.threads.lock()[0], thread::current().id());
    assert!(playback.native_parameter_query_pending.is_none());
    assert!(playback.matches_source(&(renderer.clone() as Arc<dyn AudioPcmRenderer>)));
}
#[test]
fn native_parameter_query_media_cancelled_completion_preserves_installed_source_and_next_query() {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let renderer = QueryRenderer::new();
    playback
        .prepare(
            sample_position(0),
            renderer.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    let query = opened(&mut playback, &renderer);
    let claim = renderer.installed.verifier().current_claim().unwrap();
    renderer.cancel.store(true, Ordering::Release);
    playback
        .request_native_parameter_query(NonZeroU64::new(19).unwrap(), query.clone())
        .unwrap()
        .unwrap();
    let events = wait_event(&mut playback, |e| {
        matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { .. })
    });
    assert!(events.iter().any(|e| matches!(
        e,
        AudioPlaybackEvent::NativeParameterQueryCompleted {
            completion: AudioNativeParameterQueryCompletion::Cancelled,
            ..
        }
    )));
    renderer.installed.verifier().validate(&claim).unwrap();
    assert!(playback.matches_source(&(renderer.clone() as Arc<dyn AudioPcmRenderer>)));
    assert!(playback.native_parameter_query_pending.is_none());
    renderer.cancel.store(false, Ordering::Release);
    playback
        .request_native_parameter_query(NonZeroU64::new(20).unwrap(), query)
        .unwrap()
        .unwrap();
    assert!(wait_event(&mut playback, |e| matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { .. })).iter().any(|e|
        matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Completed(Out::Observed(_)), .. } if ticket.get() == 20)));
}
#[test]
fn native_parameter_query_media_old_source_completion_cannot_clear_new_pending_or_publish_fact() {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let renderer = QueryRenderer::new();
    playback
        .prepare(
            sample_position(0),
            renderer.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    let query = opened(&mut playback, &renderer);
    renderer.gated.store(true, Ordering::Release);
    playback
        .request_native_parameter_query(NonZeroU64::new(21).unwrap(), query)
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(renderer.entered.load(Ordering::Acquire));
    let replacement = QueryRenderer::new();
    playback
        .prepare(
            sample_position(0),
            replacement.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    let events = settle_native_installation(&mut playback);
    assert!(events.iter().any(|e| matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Cancelled, .. } if ticket.get() == 21)));
    let query = opened(&mut playback, &replacement);
    playback
        .request_native_parameter_query(NonZeroU64::new(22).unwrap(), query)
        .unwrap()
        .unwrap();
    assert!(wait_event(&mut playback, |e| matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { .. })).iter().any(|e|
        matches!(e, AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Completed(Out::Observed(_)), .. } if ticket.get() == 22)));
}

fn pending_query_with_open_surface() -> (AudioPlayback, Arc<QueryRenderer>, Query) {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let renderer = QueryRenderer::new();
    playback
        .prepare(
            sample_position(0),
            renderer.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    let query = opened(&mut playback, &renderer);
    renderer.gated.store(true, Ordering::Release);
    renderer.wait_after_cancel.store(true, Ordering::Release);
    playback
        .request_native_parameter_query(NonZeroU64::new(31).unwrap(), query.clone())
        .unwrap()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !renderer.entered.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(renderer.entered.load(Ordering::Acquire));
    (playback, renderer, query)
}

#[test]
fn native_parameter_query_media_same_generation_detach_completes_cancelled() {
    let (mut playback, renderer, query) = pending_query_with_open_surface();
    let generation = playback.generation;
    // Model the already-admitted Close observation while native query work is
    // still outstanding. This does not qualify a native Close implementation.
    playback.native_editor_attachment = None;
    let events = wait_event(&mut playback, |e| {
        matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Cancelled, .. } if ticket.get() == 31)
    });
    assert_eq!(playback.generation, generation);
    assert!(events.iter().any(|e| matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { query: actual, .. } if actual == &query)));
    assert!(playback.native_parameter_query_pending.is_none());
    assert!(playback.matches_source(&(renderer.clone() as Arc<dyn AudioPcmRenderer>)));
    renderer.release.store(true, Ordering::Release);
}

#[test]
fn native_parameter_query_media_old_reply_cannot_release_new_pending() {
    let (mut playback, renderer, query) = pending_query_with_open_surface();
    playback.reprime(sample_position(0)).unwrap();
    wait_event(&mut playback, |e| {
        matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Cancelled, .. } if ticket.get() == 31)
    });
    playback
        .request_native_parameter_query(NonZeroU64::new(32).unwrap(), query)
        .unwrap()
        .unwrap();
    assert_eq!(
        playback.native_parameter_query_pending.as_ref().unwrap().ticket.get(),
        32
    );
    renderer.release.store(true, Ordering::Release);
    let events = wait_event(&mut playback, |e| {
        matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Completed(Out::Observed(_)), .. } if ticket.get() == 32)
    });
    assert!(!events.iter().any(|e| matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, .. } if ticket.get() == 31)));
    assert_eq!(renderer.calls.load(Ordering::Acquire), 2);
    assert!(playback.native_parameter_query_pending.is_none());
}

#[test]
fn native_parameter_query_media_queued_shutdown_cancels_without_executing_query() {
    let (mut playback, renderer, query) = pending_query_with_open_surface();
    playback.reprime(sample_position(0)).unwrap();
    wait_event(&mut playback, |e| {
        matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Cancelled, .. } if ticket.get() == 31)
    });
    playback
        .request_native_parameter_query(NonZeroU64::new(33).unwrap(), query)
        .unwrap()
        .unwrap();
    assert_eq!(
        renderer.calls.load(Ordering::Acquire),
        1,
        "second query is still queued behind the first"
    );
    playback.begin_shutdown();
    let events = playback.poll(AudioPlaybackMode::Idle, sample_position(0)).unwrap().events;
    assert!(events.iter().any(|e| matches!(e,
        AudioPlaybackEvent::NativeParameterQueryCompleted { ticket, completion: AudioNativeParameterQueryCompletion::Cancelled, .. } if ticket.get() == 33)));
    assert_eq!(
        renderer.calls.load(Ordering::Acquire),
        1,
        "retired queued query never reads native values"
    );
    assert!(playback.native_parameter_query_pending.is_none());
    renderer.release.store(true, Ordering::Release);
}

#[test]
fn native_parameter_query_media_control_hold_pre_refuses_without_getter() {
    let (mut playback, _) = playback_with_buffered_frames(0);
    let renderer = QueryRenderer::new();
    playback
        .prepare(
            sample_position(0),
            renderer.clone(),
            AudioPcmContinuityModel::GenerationState,
        )
        .unwrap();
    settle_native_installation(&mut playback);
    let query = opened(&mut playback, &renderer);
    playback.native_control_hold = Some(NativeControlHold::EditorCleanupPending(query.target));
    assert_eq!(
        playback
            .request_native_parameter_query(NonZeroU64::new(34).unwrap(), query)
            .unwrap(),
        Err(Refusal::Busy)
    );
    assert_eq!(renderer.calls.load(Ordering::Acquire), 0);
    assert!(playback.native_parameter_query_pending.is_none());
}
