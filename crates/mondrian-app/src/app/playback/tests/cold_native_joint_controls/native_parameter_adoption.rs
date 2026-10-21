//! Owned VST3 presentation adoption and public Project persistence only.
//! Host-origin Adopt has no native proposal/Ack and qualifies no DSP recovery.
use super::*;
use crate::app::parameter_gesture::ParameterGestureAction;
use crate::app::product_action::{AudioNativeEditorPayload, AudioProductAction, ProductAction};
use crate::app_ui::test_utils::{
    native_parameter_adoption_payload, native_parameter_query_payload,
};
use mondrian_core::{
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
};

fn passive(
    event: &AudioPlaybackEvent,
    renderer: mondrian_core::AudioPcmRendererMaterializationId,
    generation: u64,
) -> bool {
    match event {
        AudioPlaybackEvent::NativeDiagnosticsObserved {
            renderer_id, generation: observed, ..
        } => {
            assert_eq!((*renderer_id, *observed), (renderer, generation));
            false
        }
        AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id,
            generation: observed,
            observation,
        } => {
            assert_eq!((*renderer_id, *observed), (renderer, generation));
            assert!(
                observation.facts().is_empty(),
                "the explicit query alone supplies this presentation"
            );
            false
        }
        other => panic!("unexpected author, Ack, hold, binding or PCM work: {other:?}"),
    }
}

fn assert_one_host_value(
    state: &AppState,
    original: &mondrian_timeline::audio::AudioProcessorInstance,
    gain: &mondrian_core::ParameterId,
) {
    let processor = &state.active_sequence().unwrap().audio_program.outputs[0]
        .strip
        .pre_fader
        .processors[0];
    assert_eq!(processor.id, original.id);
    assert_eq!(processor.definition, original.definition);
    assert_eq!(processor.bypassed, original.bypassed);
    assert_eq!(
        processor.opaque_state, original.opaque_state,
        "Host value ownership does not rewrite the native configuration"
    );
    assert_eq!(processor.parameters.len(), 3);
    for (id, before) in &original.parameters {
        let after = &processor.parameters[id];
        assert_eq!(after.schema, before.schema);
        assert_eq!(after.ui, before.ui);
        if id == gain {
            let curve = after.automation.as_ref().expect("explicit one-row Host ownership");
            assert_eq!(&curve.parameter_id, gain);
            assert_eq!(curve.default_value.to_bits(), 0.5_f64.to_bits());
            assert!(curve.keyframes.is_empty());
        } else {
            assert!(
                after.automation.is_none(),
                "every other value stays native-owned"
            );
        }
    }
}

#[test]
#[ignore = "current normal Product and owned VST3 Editor DLL; explicit query/Open refusal/Close Adopt/public Project reload, no PCM/device"]
fn app_owned_vst3_query_adoption_after_close_persists_one_host_value() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let files = tempfile::tempdir().expect("bounded public Project archive");
    let archive = files.path().join("查询采用后保存🙂.mdp");
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control(
            "MONDRIAN_VST3_EDITOR_TEST_PLUGIN",
            "C48B1B724CDE4A91A2C9C4EBB7DD1E83",
            "Mondrian Owned Native Editor Fixture",
            3,
            92,
        );
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let sequence = state.active_sequence().unwrap();
        let native_output = &sequence.audio_program.outputs[0];
        let original = native_output.strip.pre_fader.processors[0].clone();
        assert!(original.parameters.values().all(|p| p.automation.is_none()));
        assert_editor_configuration(
            original.opaque_state.as_ref().unwrap().as_ref(),
            0.5,
            0.5,
            0,
        );
        let gain = mondrian_core::ParameterId::new("vst3.param.0").unwrap();
        let payload = AudioNativeEditorPayload {
            session_id: state.authoring_session_id().unwrap(),
            sequence_id: sequence.id,
            address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip {
                owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                    output_id: native_output.id,
                },
                rack: mondrian_timeline::AudioChannelStripRack::PreFader,
            },
            occurrence: None,
            processor_id: original.id,
            definition: original.definition.clone(),
            sequence_time: mondrian_core::DomainTime::new(
                mondrian_core::AuthoringTimeDomain::Sequence(sequence.id),
                TimelineTime::ZERO,
            ),
            open: true,
        };
        let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().unwrap();
        let generation = state.audio_playback_snapshot().generation;
        let engine = state.playback_engine.snapshot();
        let mut renderer = None;
        poll_idle_until(&mut state, generation, |event| match event {
            AudioPlaybackEvent::NativeOwnerBindingsObserved {
                renderer_id,
                generation: observed,
                status,
            } => {
                assert_eq!(*observed, generation);
                assert_eq!(
                    *status,
                    mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                );
                assert!(renderer.replace(*renderer_id).is_none());
                true
            }
            AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
            other => panic!("only actual binding Ready precedes Open: {other:?}"),
        });
        let renderer = renderer.unwrap();
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                    .into_external_action(),
            )
            .unwrap();
        let mut opened = None;
        poll_gui_until(&mut state, generation, |event| match event {
            AudioPlaybackEvent::NativeEditorObserved {
                renderer_id,
                generation: observed,
                intent: Intent::ResolveAndOpen { locator, .. },
                outcome: Outcome::Opened { target, width, height },
            } => {
                assert_eq!((*renderer_id, *observed), (renderer, generation));
                assert!(locator.matches_target(*target));
                assert_eq!(
                    (target.instance, target.installed_generation),
                    (original.id, claim.generation())
                );
                assert_eq!((*width, *height), (320, 160));
                assert!(opened.replace(*target).is_none());
                true
            }
            event => passive(event, renderer, generation),
        });
        let opened = opened.unwrap();
        assert_eq!(
            state.native_audio_editor_surface_state(&payload),
            (true, false)
        );
        assert!(
            state.active_parameter_gesture.is_none(),
            "the no-side-effect refusal case has no prior gesture to finish"
        );
        assert!(state.native_audio_parameter_presentation(&payload, &gain).is_none());
        let query = native_parameter_query_payload(&state, payload.address, original.id, &gain)
            .expect("actual visible noFact row action");
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::QueryNativeParameter(query))
                    .into_external_action(),
            )
            .unwrap();
        let ticket = std::num::NonZeroU64::new(state.native_parameter_query_serial).unwrap();
        let mut native_object = None;
        poll_gui_until(&mut state, generation, |event| match event {
            AudioPlaybackEvent::NativeParameterQueryCompleted {
                ticket: actual,
                renderer_id,
                generation: observed,
                query,
                completion,
            } => {
                assert_eq!(
                    (*actual, *renderer_id, *observed),
                    (ticket, renderer, generation)
                );
                assert_eq!((query.target, &query.parameter), (opened, &gain));
                let mondrian_media::AudioNativeParameterQueryCompletion::Completed(
                    mondrian_core::NativeAudioParameterQueryOutcome::Observed(observation),
                ) = completion
                else {
                    panic!("actual query must succeed: {completion:?}")
                };
                assert_eq!(observation.target(), opened);
                assert_eq!(observation.facts().len(), 1);
                let fact = &observation.facts()[0];
                assert_eq!(fact.parameter, gain);
                assert_eq!(fact.value_bits, Some(0.5_f64.to_bits()));
                assert_eq!(fact.author_value_bits, Some(0.5_f64.to_bits()));
                assert_eq!(fact.name.as_deref(), Some("Gain"));
                assert!(matches!(
                    fact.origin,
                    mondrian_core::NativeAudioParameterObservationOrigin::MainQuery { .. }
                ));
                native_object = Some((observation.process_id(), observation.object_serial()));
                true
            }
            event => passive(event, renderer, generation),
        });
        let native_object = native_object.unwrap();
        let fact = state.native_audio_parameter_presentation(&payload, &gain).unwrap().clone();
        let open_edit =
            native_parameter_adoption_payload(&state, payload.address, original.id, &gain)
                .expect("existing projected Adopt capability");
        let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let author_generation = state.project_author_generation();
        assert!(
            state.active_parameter_gesture.is_none(),
            "no existing gesture may be finished by this refusal control"
        );
        let error = state
            .dispatch_action(
                ParameterGestureAction::AdoptDisplayedNativeParameter { edit: open_edit.clone() }
                    .action(),
            )
            .expect_err("actual Opened attachment must refuse audio closure replacement");
        assert!(
            matches!(&error, mondrian_core::MondrianError::ActionNotExecuted { action, reason }
            if action == "authoring_commit" && reason == "native author or editor work must settle before audio Source replacement"),
            "{error:?}"
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(state.project_author_generation(), author_generation);
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert_eq!(state.audio_playback_snapshot().generation, generation);
        assert!(state.active_parameter_gesture.is_none());
        assert_eq!(
            state.native_audio_parameter_presentation(&payload, &gain),
            Some(&fact)
        );
        assert!(verifier.validate(&claim).is_ok());
        let erased: std::sync::Weak<dyn mondrian_media::AudioPcmRenderer> = weak.clone();
        assert!(state.audio_playback.matches_source_weak(&erased));
        assert!(weak.upgrade().unwrap().native_author_snapshot_current());
        assert_eq!(output.lock().queued, 0);

        let mut close = payload.clone();
        close.open = false;
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(close))
                    .into_external_action(),
            )
            .unwrap();
        poll_gui_until(&mut state, generation, |event| match event {
            AudioPlaybackEvent::NativeEditorObserved {
                renderer_id,
                generation: observed,
                intent: Intent::Close { target },
                outcome: Outcome::Closed { target: closed },
            } => {
                assert_eq!((*renderer_id, *observed), (renderer, generation));
                assert_eq!((*target, *closed), (opened, opened));
                true
            }
            event => passive(event, renderer, generation),
        });
        assert_eq!(
            state.native_audio_editor_surface_state(&payload),
            (false, false)
        );
        assert_eq!(
            state.native_audio_parameter_presentation(&payload, &gain),
            Some(&fact),
            "Close retains this exact current Fact and floor"
        );
        assert!(verifier.validate(&claim).is_ok());
        let edit = native_parameter_adoption_payload(&state, payload.address, original.id, &gain)
            .expect("actual closed row retains Adopt qualification");
        assert_eq!(
            edit, open_edit,
            "Close does not replace the target/revision capability"
        );
        state
            .dispatch_action(
                ParameterGestureAction::AdoptDisplayedNativeParameter { edit: edit.clone() }
                    .action(),
            )
            .expect("one ordinary Host-origin author transaction after Close");
        assert_one_host_value(&state, &original, &gain);
        let adopted_history = state.authoring.as_ref().unwrap().history().diagnostics();
        assert_eq!(adopted_history.undo_entries, history.undo_entries + 1);
        assert_eq!(adopted_history.pending_entries, 0);
        assert_eq!(adopted_history.redo_entries, 0);
        assert_eq!(state.project_author_generation(), author_generation + 1);
        assert!(state.native_audio_renderer.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert!(
            state.native_audio_parameter_presentation(&payload, &gain).is_none(),
            "Idle adoption retires the old Source and presentation cache"
        );
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
        let adopted_document =
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        assert!(
            state
                .dispatch_action(
                    ParameterGestureAction::AdoptDisplayedNativeParameter { edit: edit.clone() }
                        .action()
                )
                .is_err(),
            "the old snapshot cannot be adopted twice"
        );
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            adopted_document
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            adopted_history
        );
        assert_eq!(output.lock().queued, 0);
        let project = state.authoring.as_ref().unwrap().project_id();
        let old_session = state.authoring_session_id().unwrap();
        let clap = state.clap_catalog.as_ref().unwrap().clone();
        let vst3 = state.vst3_catalog.as_ref().unwrap().clone();
        state
            .save_project_file_as(archive.clone())
            .expect("public closed-author Save As");
        assert!(archive.is_file());
        state
            .close_project()
            .expect("public ProjectClose settles original Source aliases");
        let closed = state.shutdown_audio_playback_and_wait();
        assert!(
            closed.all_workers_terminated()
                && closed.shutdown_resource_facts_complete_at_deadline
                && !closed.shutdown_owner_lifetime_unresolved_at_deadline
                && closed.foreign_owner_panics == 0
                && closed.foreign_owner_abandonments == 0,
            "{closed:?}"
        );
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
        assert_eq!(output.lock().queued, 0);
        let mut fresh = AppState::with_native_audio_catalogs(clap, vst3);
        assert!(fresh.shutdown_audio_playback_and_wait().all_workers_terminated());
        // Public reload below inspects durable author state only. No new native
        // prepare, editor, signal processing or physical device is admitted.
        reopened = Some(fresh);
        let fresh = reopened.as_mut().unwrap();
        fresh.open_project_file(archive.clone()).expect("public new-App Project reopen");
        assert_eq!(fresh.authoring.as_ref().unwrap().project_id(), project);
        assert_ne!(fresh.authoring_session_id(), Some(old_session));
        assert_one_host_value(fresh, &original, &gain);
        assert!(!fresh.can_undo_action() && !fresh.can_redo_action());
        assert!(fresh.native_audio_editor.is_none());
        assert!(fresh.native_audio_renderer.is_none());
        assert_eq!(fresh.native_parameter_query_serial, 0);
        let reloaded = serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap();
        let reloaded_history = fresh.authoring.as_ref().unwrap().history().diagnostics();
        assert!(
            fresh
                .dispatch_action(
                    ParameterGestureAction::AdoptDisplayedNativeParameter { edit }.action()
                )
                .is_err(),
            "old Session/transient Fact is not reopened authority"
        );
        assert_eq!(
            serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap(),
            reloaded
        );
        assert_eq!(
            fresh.authoring.as_ref().unwrap().history().diagnostics(),
            reloaded_history
        );
        assert!(
            Instant::now() < deadline,
            "finite workflow exceeded 30s control budget"
        );
        println!(
            "{}",
            serde_json::json!({"case":"app-owned-vst3-query-adopt-project", "parameters":3, "some":1, "remaining_none":2, "adopted_value":0.5, "opened":true, "open_refused":true, "closed":true, "history_delta":1, "project_reloaded":true, "native_identity_observed":true, "process":native_object.0.get(), "object":native_object.1.get(), "native_overlay_execution_qualified":false, "pcm_qualified":false, "device_qualified":false, "dsp_history_recovery_qualified":false, "realtime_qualified":false, "graceful_100ms_qualified":false})
        );
    }));
    let old_cleanup = state.shutdown_audio_playback_and_wait();
    let new_cleanup = reopened.as_mut().map(AppState::shutdown_audio_playback_and_wait);
    let clean = old_cleanup.all_workers_terminated()
        && old_cleanup.shutdown_resource_facts_complete_at_deadline
        && !old_cleanup.shutdown_owner_lifetime_unresolved_at_deadline
        && old_cleanup.foreign_owner_panics == 0
        && old_cleanup.foreign_owner_abandonments == 0
        && new_cleanup.as_ref().is_none_or(|e| {
            e.all_workers_terminated()
                && e.shutdown_resource_facts_complete_at_deadline
                && !e.shutdown_owner_lifetime_unresolved_at_deadline
                && e.foreign_owner_panics == 0
                && e.foreign_owner_abandonments == 0
        });
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
    assert!(clean, "old={old_cleanup:?}; new={new_cleanup:?}");
}
