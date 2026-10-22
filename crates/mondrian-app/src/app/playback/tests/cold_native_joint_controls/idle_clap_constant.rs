//! First Idle Open must expose an explicit constant Some independently of opaque
//! state. Unentered/zero queued Media work do not count native Process calls.
use super::project_state_roundtrip::unentered_owner;
use super::*;
use crate::app::product_action::{AudioNativeEditorPayload, AudioProductAction, ProductAction};
use mondrian_core::{
    ExecutionCancellationToken, NativeAudioEditorControlIntent as Intent,
    NativeAudioEditorControlOutcome as Outcome, NativeAudioParameterObservationOrigin as Origin,
};
use mondrian_media::AudioPcmRenderer;

#[test]
#[ignore = "current normal Product and owned CLAP Gain editor DLL; static Some versus opaque at first Idle Open, no native edit/manual query/PCM/device"]
fn app_owned_clap_idle_open_exposes_static_some_independently_of_opaque_state() {
    let volume = mondrian_core::ParameterId::new("clap.param.1").unwrap();
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_clap_editor_control_with_initializer(|sequence, _, _| {
            let instance = &mut sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
            assert_eq!(instance.parameters.len(), 1);
            assert!(instance.parameters[&volume].automation.is_none());
            assert_eq!(
                instance.opaque_state.as_ref().unwrap().as_slice(),
                &1.0_f32.to_le_bytes()
            );
            instance
                .parameters
                .get_mut(&volume)
                .unwrap()
                .set_automation(
                    mondrian_core::ExactAutomationCurve::new(volume.clone(), 0.25).unwrap(),
                )
                .unwrap();
        });
    let sequence = state.active_sequence().unwrap();
    assert!(sequence.audio_program.note_lanes.is_empty());
    let original = sequence.audio_program.outputs[0].strip.pre_fader.processors[0].clone();
    let curve = original.parameters[&volume].automation.as_ref().unwrap();
    assert_eq!(curve.parameter_id, volume);
    assert!(curve.keyframes.is_empty());
    assert_eq!(curve.default_value.to_bits(), 0.25_f64.to_bits());
    assert_eq!(
        original.opaque_state.as_ref().unwrap().as_slice(),
        &1.0_f32.to_le_bytes()
    );
    let payload = AudioNativeEditorPayload {
        session_id: state.authoring_session_id().unwrap(),
        sequence_id: sequence.id,
        address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip {
            owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                output_id: sequence.audio_program.outputs[0].id,
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
    let author = state.authoring.as_ref().unwrap().execution_snapshot();
    let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    assert_eq!((history.undo_entries, history.pending_entries), (0, 0));
    let engine = state.playback_engine.snapshot();
    let generation = state.audio_playback_snapshot().generation;
    let query_serial = state.native_parameter_query_serial;
    let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let claim = verifier.current_claim().unwrap();
    let unentered = unentered_owner(&weak.upgrade().unwrap(), original.id);
    let mut renderer = None;
    let mut initial_target = None;
    let mut initial_renderer = None;
    let mut native_object = None;
    let mut opened = None;
    let target_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Consume the original Ready and first complete presentation before
        // Open. No value is prescribed at prepare time by this product gate.
        poll_idle_until(&mut state, generation, |event| {
            assert_eq!(
                unentered_owner(&weak.upgrade().unwrap(), original.id),
                unentered
            );
            match event {
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
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved {
                    renderer_id,
                    generation: observed,
                    observation,
                } => {
                    assert_eq!(*observed, generation);
                    if let Some(renderer) = renderer {
                        assert_eq!(*renderer_id, renderer);
                    }
                    assert_eq!(
                        (
                            observation.target().instance,
                            observation.target().installed_generation
                        ),
                        (original.id, claim.generation())
                    );
                    assert_eq!(observation.facts().len(), 1);
                    assert_eq!(observation.facts()[0].parameter, volume);
                    initial_renderer = Some(*renderer_id);
                    initial_target = Some(observation.target());
                    native_object = Some((observation.process_id(), observation.object_serial()));
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                other => panic!("no author, editor or PCM work before first Open: {other:?}"),
            }
            renderer.is_some() && initial_target.is_some()
        });
        let renderer = renderer.unwrap();
        assert_eq!(Some(renderer), initial_renderer);
        weak.upgrade()
            .unwrap()
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .expect("original Source is settled before Open");
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                    .into_external_action(),
            )
            .expect("original App CLAP Open action");
        let mut presentation = None;
        poll_gui_until(&mut state, generation, |event| {
            assert_eq!(
                unentered_owner(&weak.upgrade().unwrap(), original.id),
                unentered
            );
            match event {
                AudioPlaybackEvent::NativeEditorObserved {
                    renderer_id,
                    generation: observed,
                    intent: Intent::ResolveAndOpen { locator, .. },
                    outcome: Outcome::Opened { target, width, height },
                } => {
                    assert_eq!((*renderer_id, *observed), (renderer, generation));
                    assert!(locator.matches_target(*target));
                    assert_eq!(locator.sequence, payload.sequence_id);
                    assert_eq!(Some(*target), initial_target);
                    assert_eq!((*width, *height), (320, 100));
                    assert!(opened.replace(*target).is_none());
                    true
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved {
                    renderer_id,
                    generation: observed,
                    observation,
                } => {
                    assert_eq!((*renderer_id, *observed), (renderer, generation));
                    assert_eq!(Some(observation.target()), initial_target);
                    assert_eq!(
                        Some((observation.process_id(), observation.object_serial())),
                        native_object
                    );
                    presentation = Some(observation.as_ref().clone());
                    false
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                other => panic!("no author/Ack or Source replacement during Open: {other:?}"),
            }
        });
        assert_eq!(
            state.native_audio_editor_surface_state(&payload),
            (true, false)
        );
        // The initial revision's complete Fact was already consumed before
        // Open. CLAP Open invalidates that presentation once; its new natural
        // MainQuery may share the completion poll, in either event order.
        // Never discard it and require a manufactured second native query.
        if presentation.is_none() {
            poll_gui_until(&mut state, generation, |event| {
                assert_eq!(
                    unentered_owner(&weak.upgrade().unwrap(), original.id),
                    unentered
                );
                match event {
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        renderer_id,
                        generation: observed,
                        observation,
                    } => {
                        assert_eq!((*renderer_id, *observed), (renderer, generation));
                        assert_eq!(Some(observation.target()), opened);
                        assert_eq!(
                            Some((observation.process_id(), observation.object_serial())),
                            native_object
                        );
                        presentation = Some(observation.as_ref().clone());
                        true
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                    other => panic!("only actual MainQuery after Open: {other:?}"),
                }
            });
        }
        let observation = presentation.unwrap();
        assert_eq!(Some(observation.target()), opened);
        assert_eq!(observation.facts().len(), 1);
        let fact = &observation.facts()[0];
        assert_eq!(fact.parameter, volume);
        assert_eq!(fact.revision, observation.source_revision());
        assert_eq!(fact.origin, Origin::MainQuery { rescan_flags: 0 });
        assert_eq!(fact.event_flags, 0);
        assert_eq!(fact.value_bits, Some(0.25_f64.to_bits()),
            "CLAP_IDLE_STATIC_SOME_MISMATCH: actual first-Open native value must expose Host Some .25 independently of opaque/factory 1.0");
        assert_eq!(fact.author_value_bits, Some(0.25_f64.to_bits()));
        assert_eq!(fact.display.as_deref(), Some("25.00 %"));
        // The original event handler has run. An otherwise valid Fact arriving
        // before Opened may remain absent from its existing surface cache; this
        // gate qualifies the native query, without replay or cache manufacture.
    }));
    // Even a target RED closes the actual successfully opened view through the
    // original action/worker, then consumes the sole Source before resuming it.
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(target) = opened {
            let view = owned_editor_view_matching(
                native_object.unwrap().0,
                b"MondrianClapGainEditor_",
                false,
                Some(b"Mondrian CLAP Gain Editor"),
            );
            let mut close = payload.clone();
            close.open = false;
            state
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::NativeEditor(close))
                        .into_external_action(),
                )
                .expect("original App Close even after a target mismatch");
            poll_gui_until(&mut state, generation, |event| match event {
                AudioPlaybackEvent::NativeEditorObserved {
                    renderer_id,
                    generation: observed,
                    intent: Intent::Close { target: requested },
                    outcome: Outcome::Closed { target: actual },
                } => {
                    assert_eq!((*renderer_id, *observed), (renderer.unwrap(), generation));
                    assert_eq!((*requested, *actual), (target, target));
                    true
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                    assert_eq!(observation.target(), target);
                    assert_eq!(
                        Some((observation.process_id(), observation.object_serial())),
                        native_object
                    );
                    false
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                other => panic!("no author/Ack or Source replacement during Close: {other:?}"),
            });
            assert_eq!(unsafe { is_window(view) }, 0);
            assert_eq!(
                state.native_audio_editor_surface_state(&payload),
                (false, false)
            );
        }
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().execution_snapshot().generation(),
            author.generation()
        );
        assert!(author.enter_commit().is_ok());
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert!(verifier.validate(&claim).is_ok());
        assert_eq!(
            verifier.current_claim().unwrap().generation(),
            claim.generation()
        );
        assert!(state
            .native_audio_renderer
            .as_ref()
            .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
        assert_eq!(
            unentered_owner(&weak.upgrade().unwrap(), original.id),
            unentered
        );
        weak.upgrade()
            .unwrap()
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .expect("the original Source remains settled with no proposal/Ack");
        assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
        assert!(state.audio_playback.native_control_hold_generation().is_none());
        assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
        assert_eq!(state.native_parameter_query_serial, query_serial);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(output.lock().queued, 0);
    }));
    let cleanup = state.shutdown_audio_playback_and_wait();
    assert_eq!(cleanup.render_workers_started, 1);
    assert_eq!(cleanup.render_workers_terminated, 1);
    assert_eq!(cleanup.render_retirement_workers_started, 1);
    assert_eq!(cleanup.render_retirement_workers_terminated, 1);
    assert_eq!(cleanup.foreign_owner_panics, 0);
    assert_eq!(cleanup.foreign_owner_abandonments, 0);
    assert!(cleanup.all_workers_terminated(), "{cleanup:?}");
    assert!(cleanup.shutdown_resource_facts_complete_at_deadline);
    assert!(!cleanup.shutdown_owner_lifetime_unresolved_at_deadline);
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(weak.upgrade().is_none());
    assert_eq!(output.lock().queued, 0);
    if let Err(failure) = target_result {
        std::panic::resume_unwind(failure);
    }
    if let Err(failure) = close_result {
        std::panic::resume_unwind(failure);
    }
    let object = native_object.unwrap();
    println!(
        "{}",
        serde_json::json!({
            "case":"app-owned-clap-idle-static-some", "opened":true, "closed":true,
            "parameters":1, "host_static":0.25, "opaque_factory":1.0,
            "main_query_value":0.25, "process_id":object.0.get(), "object_serial":object.1.get(),
            "same_source_claim":true, "unentered":true, "native_process_counter_observed":false,
            "author_unchanged":true, "history_unchanged":true, "opaque_unchanged":true,
            "manual_query":false, "pcm_qualified":false, "device_qualified":false,
            "realtime_qualified":false, "graceful_100ms_qualified":false
        })
    );
}
