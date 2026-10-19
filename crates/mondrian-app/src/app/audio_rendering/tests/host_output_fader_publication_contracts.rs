//! Ordinary real Source/author refusal and BuiltIn compatibility controls.
//! No native object, Closed observation, Ready cache or SDK result is fabricated.
use super::*;
use crate::app::parameter_gesture::{
    AudioScalarAuthorLease, AudioScalarGestureTarget, AudioScalarOwner, AudioScalarValueEdit,
    ParameterGestureAction, ParameterGestureEdit,
};
use crate::app::playback::tests::play_ready;
use crate::app::playback::tests::project_close_audio_contracts::{
    available_state, install_builtin_source, settle_builtin_binding,
};
use crate::app::playback::AppAudioPlayback;
use crate::app::product_action::{AudioProductAction, ProductAction};
use mondrian_audio::{
    AudioProcessor, AudioProcessorFactory, AudioProcessorFactoryStorageBorrow,
    AudioProcessorFactoryStorageError, AudioProcessorHostError, AudioProcessorPrepareRequest,
};
use mondrian_core::{AuthoringGestureId, NativeChildSettlementRequirement as Requirement};
use std::sync::atomic::{AtomicBool, Ordering};

// The wrapped processor remains the actual BuiltIn gain. Only its cold factory
// declaration can become unavailable; this model owns no physical native child.
struct DeclarationResolver(Arc<AtomicBool>);
struct DeclarationFactory {
    inner: Arc<dyn AudioProcessorFactory>,
    unavailable: Arc<AtomicBool>,
}
impl AudioProcessorResolver for DeclarationResolver {
    fn native_child_settlement_requirement(&self) -> Requirement {
        Requirement::NotApplicable
    }
    fn prepare(
        &self,
        request: AudioProcessorPrepareRequest<'_>,
    ) -> Result<Arc<dyn AudioProcessorFactory>, AudioProcessorHostError> {
        Ok(Arc::new(DeclarationFactory {
            inner: BuiltInAudioProcessorResolver.prepare(request)?,
            unavailable: Arc::clone(&self.0),
        }))
    }
}
impl AudioProcessorFactory for DeclarationFactory {
    fn native_child_settlement_requirement(&self) -> Requirement {
        if self.unavailable.load(Ordering::Acquire) {
            Requirement::Unavailable
        } else {
            Requirement::NotApplicable
        }
    }
    fn execution_contract(&self) -> mondrian_audio::AudioProcessorExecutionContract {
        self.inner.execution_contract()
    }
    fn visit_host_owned_storage<'a>(
        &'a self,
        visitor: &mut dyn FnMut(
            AudioProcessorFactoryStorageBorrow<'a>,
        ) -> Result<(), AudioProcessorFactoryStorageError>,
    ) -> Result<(), AudioProcessorFactoryStorageError> {
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(&self.inner))?;
        visitor(AudioProcessorFactoryStorageBorrow::arc_payload(
            &self.unavailable,
        ))?;
        self.inner.visit_host_owned_storage(visitor)
    }
    fn create(
        &self,
        registrar: Option<&mondrian_core::NativeChildSettlementRegistrar>,
    ) -> Result<Box<dyn AudioProcessor>, AudioProcessorHostError> {
        self.inner.create(registrar)
    }
}

fn with_playing_builtin(
    target: impl FnOnce(&mut AppState, &Arc<TimelineAudioPcmRenderer>, &AtomicBool),
) {
    let (mut state, output) = available_state();
    let unavailable = Arc::new(AtomicBool::new(false));
    state.audio_processor_resolver = Arc::new(DeclarationResolver(Arc::clone(&unavailable)));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        install_builtin_source(&mut state);
        play_ready(&mut state); // Original headless Ready/preroll model, not physical Viewer evidence.
        settle_builtin_binding(&mut state); // Idle serial-control poll admits no PCM.
        let renderer = state.native_audio_renderer.as_ref().unwrap().upgrade().unwrap();
        let concrete = Arc::downgrade(&renderer);
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = concrete;
        assert!(state.audio_playback.matches_source_weak(&erased));
        assert_eq!(
            renderer.host_output_fader_native_requirement().unwrap(),
            (Requirement::NotApplicable, false)
        );
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
        assert_eq!(output.lock().queued, 0);
        target(&mut state, &renderer, &unavailable);
        assert_eq!(output.lock().queued, 0);
        // This strong test alias ends before the original consuming cleanup.
        drop(renderer);
    }));
    // Restore the true no-child declaration even after a target assertion panic.
    // This is only the controlled getter, not a cleanup acknowledgement or Source fact.
    unavailable.store(false, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "complete ordinary cleanup: pause={pause:?}, audio={audio:?}, app={cleanup:?}"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

// The original setup helper intentionally proves sample zero. Live edits can
// legally advance Engine authority; settle their real current Source coordinate
// without changing that existing helper's oracle or scheduling any PCM.
fn settle_current_builtin_binding(state: &mut AppState) {
    let before = state.audio_playback_snapshot();
    let anchor = state
        .authoritative_audio_anchor(
            &state.playback_engine,
            state.playback_engine.monotonic_high_water(),
            "ordinary binding settle",
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let AppAudioPlayback::Available(playback) = &state.audio_playback {
            if playback.native_owner_binding_status().is_some() {
                break;
            }
        }
        let poll = state.audio_playback.poll(AudioPlaybackMode::Idle, anchor).unwrap();
        assert_eq!(poll.snapshot.in_flight, 0);
        assert_eq!(poll.snapshot.next_start_sample, before.next_start_sample);
        for event in poll.events {
            assert!(
                matches!(
                    &event,
                    mondrian_media::AudioPlaybackEvent::NativeOwnerBindingsObserved {
                        status: mondrian_media::AudioPcmNativeOwnerBindingStatus::Unavailable(
                            mondrian_media::AudioPcmNativeOwnerBindingRefusal::UnsupportedAdapter
                        ),
                        ..
                    }
                ),
                "only the actual BuiltIn binding completion is expected: {event:?}"
            );
            state.handle_audio_playback_event(event, Instant::now()).unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "current BuiltIn binding did not settle"
        );
        std::thread::yield_now();
    }
}

fn fader_action(state: &AppState, value: f64) -> mondrian_editor_state::Action {
    ProductAction::Audio(AudioProductAction::EditChannelStrip(
        mondrian_timeline::AudioChannelStripEditRequest {
            owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                output_id: state.active_sequence().unwrap().audio_program.outputs[0].id,
            },
            edit: mondrian_timeline::AudioChannelStripEdit::SetFaderDb { value },
        },
    ))
    .into_external_action()
}

fn fader_gesture(state: &AppState, value: f64) -> ParameterGestureEdit {
    ParameterGestureEdit::AudioScalar(AudioScalarValueEdit {
        target: Box::new(AudioScalarGestureTarget {
            lease: AudioScalarAuthorLease::from_state(state, state.active_sequence_id().unwrap())
                .unwrap(),
            owner: AudioScalarOwner::Fader {
                owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                    output_id: state.active_sequence().unwrap().audio_program.outputs[0].id,
                },
            },
        }),
        value,
    })
}

#[test]
fn playing_builtin_output_fader_keeps_generic_source_replacement() {
    with_playing_builtin(|state, renderer, _| {
        let before = state.audio_playback_snapshot();
        let generation = state.authoring.as_ref().unwrap().author_generation();
        let history = state.authoring_history().unwrap().diagnostics();
        let engine = state.playback_engine.snapshot();
        let verifier = renderer.owner_installation_verifier().unwrap();
        let old_claim = verifier.current_claim().unwrap();
        let action = fader_action(state, -6.0);
        state
            .dispatch_action(action)
            .expect("known BuiltIn retains the original generic edit path");
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0].strip.fader_db,
            -6.0
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().author_generation().get(),
            generation.get() + 1
        );
        assert_eq!(
            state.authoring_history().unwrap().diagnostics().undo_entries,
            history.undo_entries + 1
        );
        assert!(state.audio_playback_snapshot().generation > before.generation);
        assert!(verifier.validate(&old_claim).is_err());
        let after = state.playback_engine.snapshot();
        assert_eq!(after.epoch, engine.epoch);
        assert_eq!(after.rate, engine.rate);
        assert_eq!(after.state, mondrian_playback::TransportState::Playing);
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
    });
}

#[test]
fn playing_builtin_output_fader_keeps_begin_update_finish_gesture_policy() {
    with_playing_builtin(|state, _, _| {
        let entries = state.authoring_history().unwrap().diagnostics().undo_entries;
        let id = AuthoringGestureId::new();
        let begin = fader_gesture(state, -3.0);
        state
            .dispatch_action(ParameterGestureAction::Begin { gesture_id: id, edit: begin }.action())
            .expect("known BuiltIn permits the original playing gesture Begin");
        settle_current_builtin_binding(state);
        let update = fader_gesture(state, -6.0);
        state
            .dispatch_action(
                ParameterGestureAction::Update { gesture_id: id, edit: update }.action(),
            )
            .expect("known BuiltIn permits the original playing gesture Update");
        settle_current_builtin_binding(state);
        assert!(state.parameter_gesture_is_current());
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0].strip.fader_db,
            -6.0
        );
        assert_eq!(
            state.authoring_history().unwrap().diagnostics().undo_entries,
            entries + 1
        );
        state
            .dispatch_action(ParameterGestureAction::Finish { gesture_id: id }.action())
            .unwrap();
        assert!(state.active_parameter_gesture.is_none());
        assert_eq!(
            state.authoring_history().unwrap().diagnostics().undo_entries,
            entries + 1
        );
    });
}

#[test]
fn unknown_installed_factory_refuses_changed_faders_but_keeps_noop_and_stale_intents() {
    with_playing_builtin(|state, renderer, unavailable| {
        let before = state.authoring.as_ref().unwrap().document().clone();
        let generation = state.authoring.as_ref().unwrap().author_generation();
        let history = state.authoring_history().unwrap().diagnostics();
        let engine = state.playback_engine.snapshot();
        let pcm = state.audio_playback_snapshot();
        let verifier = renderer.owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().unwrap();
        let runtime_guard = renderer.state.lock();
        let busy = fader_action(state, -6.0);
        let busy_error = state
            .dispatch_action(busy)
            .expect_err("busy actual Runtime refuses before publication");
        drop(runtime_guard);
        assert!(busy_error.to_string().contains("the original audio Runtime is busy"));
        unavailable.store(true, Ordering::Release);
        assert_eq!(
            renderer.host_output_fader_native_requirement().unwrap(),
            (Requirement::Unavailable, false)
        );
        let changed = fader_action(state, -6.0);
        let error =
            state.dispatch_action(changed).expect_err("Unknown is never no-native evidence");
        assert!(
            matches!(error, mondrian_core::MondrianError::ActionNotExecuted { action, reason }
            if action == "authoring_commit" && reason == "the installed audio factory native ownership declaration is unavailable")
        );
        let valid = fader_gesture(state, -6.0);
        assert!(state
            .dispatch_action(
                ParameterGestureAction::Begin {
                    gesture_id: AuthoringGestureId::new(),
                    edit: valid
                }
                .action()
            )
            .is_err());
        assert!(state.active_parameter_gesture.is_none());
        let noop = fader_action(state, 0.0);
        state
            .dispatch_action(noop)
            .expect("same-value nongesture edit does not enter native classification");
        let mut stale = fader_gesture(state, -6.0);
        if let ParameterGestureEdit::AudioScalar(edit) = &mut stale {
            edit.target.lease.sequence_id = mondrian_core::SequenceId::new();
        }
        state
            .dispatch_action(
                ParameterGestureAction::Begin {
                    gesture_id: AuthoringGestureId::new(),
                    edit: stale,
                }
                .action(),
            )
            .expect("queued stale Begin stays ignored before Unknown classification");
        let update = fader_gesture(state, -6.0);
        state
            .dispatch_action(
                ParameterGestureAction::Update {
                    gesture_id: AuthoringGestureId::new(),
                    edit: update,
                }
                .action(),
            )
            .expect("foreign Update stays ignored");
        assert_eq!(state.authoring.as_ref().unwrap().document(), &before);
        assert_eq!(
            state.authoring.as_ref().unwrap().author_generation(),
            generation
        );
        assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert_eq!(state.audio_playback_snapshot().generation, pcm.generation);
        assert_eq!(
            state.audio_playback_snapshot().next_start_sample,
            pcm.next_start_sample
        );
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert!(verifier.validate(&claim).is_ok());
        assert!(state.active_parameter_gesture.is_none());
    });
}

#[derive(Clone, Copy)]
enum Refusal {
    RuntimeBusy,
    MetadataBusy,
    StaleSnapshot,
    UnreadyMedia,
}

#[test]
fn host_fader_dependency_refusals_do_not_install_prepared_author_tickets() {
    for refusal in [
        Refusal::RuntimeBusy,
        Refusal::MetadataBusy,
        Refusal::StaleSnapshot,
        Refusal::UnreadyMedia,
    ] {
        with_playing_builtin(|state, renderer, _| {
            let sequence_id = state.active_sequence_id().unwrap();
            if matches!(refusal, Refusal::StaleSnapshot) {
                state
                    .authoring
                    .as_mut()
                    .unwrap()
                    .edit_sequence(sequence_id, "advance canonical author only", |sequence| {
                        sequence.name = "stale renderer permission control".to_owned();
                        Ok(())
                    })
                    .unwrap();
            }
            let before = state.authoring.as_ref().unwrap().document().clone();
            let generation = state.authoring.as_ref().unwrap().author_generation();
            let history = state.authoring_history().unwrap().diagnostics();
            let engine = state.playback_engine.snapshot();
            let pcm = state.audio_playback_snapshot();
            let verifier = renderer.owner_installation_verifier().unwrap();
            let claim = verifier.current_claim().unwrap();
            let (_, prepared) = state
                .authoring
                .as_mut()
                .unwrap()
                .prepare_sequence_edit(sequence_id, "unpublished Host fader", |sequence| {
                    sequence.audio_program.outputs[0].strip.fader_db = -6.0;
                    Ok(())
                })
                .unwrap();
            let prepared = prepared.unwrap();
            let candidate = prepared
                .candidate_sequences()
                .iter()
                .find(|sequence| sequence.id == sequence_id)
                .unwrap();
            let program = Arc::new(
                mondrian_audio::compile_audio_program(
                    candidate,
                    AudioCompileRequest {
                        output_id: candidate.audio_program.outputs[0].id,
                        audition: AudioAuditionOverlay::default(),
                    },
                )
                .unwrap(),
            );
            let runtime_guard =
                matches!(refusal, Refusal::RuntimeBusy).then(|| renderer.state.lock());
            let metadata_guard =
                matches!(refusal, Refusal::MetadataBusy).then(|| renderer.author_snapshot.write());
            let installs = std::cell::Cell::new(0);
            let author = state.authoring.as_mut().unwrap();
            let AppAudioPlayback::Available(playback) = &mut state.audio_playback else {
                panic!("actual Media");
            };
            let error = renderer
                .publish_host_output_fader(program, playback, || {
                    installs.set(installs.get() + 1);
                    let commit = author.install_prepared_sequence_edit(prepared);
                    (commit, author.execution_snapshot())
                })
                .err()
                .expect("all modeled refusals precede Source CAS and author installation");
            drop(metadata_guard);
            drop(runtime_guard);
            let text = error.to_string();
            match refusal {
                Refusal::RuntimeBusy => assert!(
                    text.contains("the original audio Runtime is busy"),
                    "{text}"
                ),
                Refusal::MetadataBusy => assert!(
                    text.contains("audio author permission metadata is busy"),
                    "{text}"
                ),
                Refusal::StaleSnapshot => assert!(renderer
                    .author_snapshot
                    .read()
                    .as_ref()
                    .unwrap()
                    .enter_commit()
                    .is_err()),
                Refusal::UnreadyMedia => assert!(text.contains("Host output fader"), "{text}"),
            }
            assert_eq!(installs.get(), 0);
            assert_eq!(state.authoring.as_ref().unwrap().document(), &before);
            assert_eq!(
                state.authoring.as_ref().unwrap().author_generation(),
                generation
            );
            assert_eq!(state.authoring_history().unwrap().diagnostics(), history);
            assert_eq!(state.playback_engine.snapshot(), engine);
            assert_eq!(state.audio_playback_snapshot().generation, pcm.generation);
            assert_eq!(
                state.audio_playback_snapshot().next_start_sample,
                pcm.next_start_sample
            );
            assert_eq!(state.audio_playback_snapshot().in_flight, 0);
            assert!(verifier.validate(&claim).is_ok());
        });
    }
}

#[test]
#[ignore = "current normal Product and owned CLAP mode12 DLL; Source/author lock contract only"]
fn native_clap_host_fader_publication_preserves_author_exclusion() {
    super::playing_host_fader_continuity::run_native_host_fader_publication_exclusion();
}
