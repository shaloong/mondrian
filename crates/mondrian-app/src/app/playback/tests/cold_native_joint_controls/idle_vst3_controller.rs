//! Idle Open must show the explicit unkeyed Host value, independently of opaque state.
//! Actual Unentered cursors and zero Media work do not count plugin-internal Process calls.
use super::project_state_roundtrip::unentered_owner;
use super::*;
use crate::app::product_action::{AudioNativeEditorPayload, AudioProductAction, ProductAction};
use crate::app_ui::test_utils::native_parameter_query_payload;
use mondrian_core::{
    ExecutionCancellationToken, NativeAudioEditorControlIntent as Intent,
    NativeAudioEditorControlOutcome as Outcome, NativeAudioParameterObservationOrigin as Origin,
};
use mondrian_media::AudioPcmRenderer;

#[test]
#[ignore = "current normal Product and owned VST3 Editor DLL; Idle static Some/controller synchronization before signal entry, no manual query/PCM/device"]
fn app_owned_vst3_idle_open_synchronizes_static_some_without_signal_entry() {
    let gain = mondrian_core::ParameterId::new("vst3.param.0").unwrap();
    let ColdNativeControl { mut state, _directory, output, renderer: weak } =
        prepare_app_native_control_with_initializer(
            "MONDRIAN_VST3_EDITOR_TEST_PLUGIN",
            "C48B1B724CDE4A91A2C9C4EBB7DD1E83",
            "Mondrian Owned Native Editor Fixture",
            3,
            92,
            |sequence, _, _| {
                let instance = &mut sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
                assert!(instance.parameters.values().all(|p| p.automation.is_none()));
                assert_editor_configuration(
                    instance.opaque_state.as_ref().unwrap().as_ref(),
                    0.5,
                    0.5,
                    0,
                );
                // Install a legal Project value before the original App constructor.
                // Do not rewrite either native stream or manufacture a native edit.
                instance
                    .parameters
                    .get_mut(&gain)
                    .unwrap()
                    .set_automation(
                        mondrian_core::ExactAutomationCurve::new(gain.clone(), 0.25).unwrap(),
                    )
                    .unwrap();
            },
        );
    let original = state.active_sequence().unwrap().audio_program.outputs[0]
        .strip
        .pre_fader
        .processors[0]
        .clone();
    assert_eq!(original.parameters.len(), 3);
    let curve = original.parameters[&gain].automation.as_ref().unwrap();
    assert_eq!(curve.parameter_id, gain);
    assert_eq!(curve.default_value.to_bits(), 0.25_f64.to_bits());
    assert!(
        curve.keyframes.is_empty(),
        "this gate supplies only a static Some"
    );
    for (id, parameter) in &original.parameters {
        if id != &gain {
            assert!(parameter.automation.is_none());
        }
    }
    assert_editor_configuration(
        original.opaque_state.as_ref().unwrap().as_ref(),
        0.5,
        0.5,
        0,
    );
    let sequence = state.active_sequence().unwrap();
    assert!(sequence.audio_program.note_lanes.is_empty());
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
    assert_eq!(history.undo_entries, 0);
    assert_eq!(history.pending_entries, 0);
    let engine = state.playback_engine.snapshot();
    let generation = state.audio_playback_snapshot().generation;
    let query_serial = state.native_parameter_query_serial;
    let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let claim = verifier.current_claim().unwrap();
    let unentered = unentered_owner(&weak.upgrade().unwrap(), original.id);
    let mut native_object = None;
    let target_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut renderer = None;
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
                    true
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                | AudioPlaybackEvent::NativeParameterObservationsObserved { .. } => false,
                other => panic!("no author, Ack, editor or PCM operation before Open: {other:?}"),
            }
        });
        let renderer = renderer.unwrap();
        let mut initial_target = None;
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
                    let target = observation.target();
                    assert_eq!(
                        (target.instance, target.installed_generation),
                        (original.id, claim.generation())
                    );
                    initial_target = Some(target);
                    native_object = Some((observation.process_id(), observation.object_serial()));
                    true
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                other => panic!("only the original native query precedes Open: {other:?}"),
            }
        });
        let initial_target = initial_target.unwrap();
        weak.upgrade()
            .unwrap()
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .expect("original Source closed before the sole Open");
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                    .into_external_action(),
            )
            .expect("original App Open");
        let mut opened = None;
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
                    assert_eq!(
                        (target.instance, target.installed_generation),
                        (original.id, claim.generation())
                    );
                    assert_eq!((*width, *height), (320, 160));
                    assert!(opened.replace(*target).is_none());
                    true
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. }
                | AudioPlaybackEvent::NativeParameterObservationsObserved { .. } => false,
                other => panic!("no author, Ack or Source replacement during Open: {other:?}"),
            }
        });
        let opened = opened.unwrap();
        assert_eq!(
            opened, initial_target,
            "Open retains the actual initial native target"
        );
        assert_eq!(
            state.native_audio_editor_surface_state(&payload),
            (true, false)
        );
        // The same serial Media worker now polls the actual full owner again.
        // This is its natural MainQuery receipt, never a manual single-ID query.
        let mut observed_facts = None;
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
                    assert_eq!(observation.target(), opened);
                    assert_eq!(observation.facts().len(), 3);
                    let object = (observation.process_id(), observation.object_serial());
                    if let Some(previous) = native_object {
                        assert_eq!(object, previous);
                    }
                    native_object = Some(object);
                    for (fact, (id, value, name)) in observation.facts().iter().zip([
                        ("vst3.param.0", 0.25_f64, "Gain"),
                        ("vst3.param.1", 0.5_f64, "Balance"),
                        ("vst3.param.2", 0.0_f64, "Fixture failure scenario"),
                    ]) {
                        assert_eq!(fact.parameter.as_str(), id);
                        assert_eq!(fact.revision, observation.source_revision());
                        assert_eq!(fact.value_bits, Some(value.to_bits()),
                            "VST3_IDLE_STATIC_SOME_CONTROLLER_MISMATCH: {id} must expose the independent Host static value after Open");
                        assert_eq!(fact.author_value_bits, Some(value.to_bits()));
                        assert_eq!(fact.name.as_deref(), Some(name));
                        assert_eq!(fact.origin, Origin::MainQuery { rescan_flags: 0 });
                        assert_eq!(fact.event_flags, 0);
                    }
                    observed_facts = Some(observation.facts().to_vec());
                    true
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                other => panic!("only full-owner MainQuery is admitted after Open: {other:?}"),
            }
        });
        let facts = observed_facts.unwrap();
        for fact in &facts {
            assert_eq!(
                state.native_audio_parameter_presentation(&payload, &fact.parameter),
                Some(fact)
            );
        }
        assert!(
            native_parameter_query_payload(&state, payload.address, original.id, &gain).is_none(),
            "Some retains its original scalar control, not a manual query action"
        );
        assert_eq!(state.native_parameter_query_serial, query_serial);
        weak.upgrade()
            .unwrap()
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .expect("controller synchronization creates no native proposal or Ack obligation");
        let view = owned_editor_view(native_object.unwrap().0);
        let mut close = payload.clone();
        close.open = false;
        state
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(close))
                    .into_external_action(),
            )
            .expect("original App Close");
        poll_gui_until(&mut state, generation, |event| {
            assert_eq!(
                unentered_owner(&weak.upgrade().unwrap(), original.id),
                unentered
            );
            match event {
                AudioPlaybackEvent::NativeEditorObserved {
                    renderer_id,
                    generation: observed,
                    intent: Intent::Close { target: requested },
                    outcome: Outcome::Closed { target },
                } => {
                    assert_eq!((*renderer_id, *observed), (renderer, generation));
                    assert_eq!((*requested, *target), (opened, opened));
                    true
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                    assert_eq!(observation.target(), opened);
                    assert_eq!(
                        (observation.process_id(), observation.object_serial()),
                        native_object.unwrap()
                    );
                    assert_eq!(observation.facts(), facts.as_slice());
                    false
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                other => panic!("no author, Ack or Source replacement during Close: {other:?}"),
            }
        });
        assert_eq!(unsafe { is_window(view) }, 0);
        assert_eq!(
            state.native_audio_editor_surface_state(&payload),
            (false, false)
        );
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
            .expect("original Source remains closed after Close");
        assert!(state.playback_engine.native_audio_control_hold_ticket().is_none());
        assert!(state.audio_playback.native_control_hold_generation().is_none());
        assert!(state.audio_playback.native_reprepare_pending_generation().is_none());
        assert_eq!(state.native_parameter_query_serial, query_serial);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(output.lock().queued, 0);
    }));
    // Preserve the target failure while consuming the sole original owner.
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
    let object = native_object.unwrap();
    println!(
        "{}",
        serde_json::json!({
            "case":"app-owned-vst3-idle-static-some", "opened":true, "closed":true,
            "parameters":3, "host_static":0.25, "opaque_gain":0.5, "remaining_none":2,
            "main_query_value":0.25, "process_id":object.0.get(), "object_serial":object.1.get(),
            "same_source_claim":true, "unentered":true, "native_process_counter_observed":false,
            "author_unchanged":true, "history_unchanged":true,
            "native_proposals":0, "acknowledgements":0, "manual_query":false,
            "pcm_qualified":false, "device_qualified":false, "realtime_qualified":false,
            "graceful_100ms_qualified":false
        })
    );
}
