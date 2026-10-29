//! Durable Host Some over older opaque state, followed by a real fresh native owner.
//! This owned Windows gate uses software Timeline PCM; it is not DSP-history recovery.
use super::project_state_roundtrip::{initialize_nonzero_audio, unentered_owner};
use super::*;
use crate::app::product_action::{AudioNativeEditorPayload, AudioProductAction, ProductAction};
use mondrian_core::{
    AudioOwnerInstallationError, ExecutionCancellationToken,
    NativeAudioEditorControlIntent as Intent, NativeAudioEditorControlOutcome as Outcome,
    NativeAudioEditorControlRefusal as Refusal, NativeAudioParameterObservationOrigin as Origin,
};
use mondrian_media::{
    AudioPcmContinuity, AudioPcmRenderGeneration, AudioPcmRenderOutcome, AudioPcmRenderRequest,
    AudioPcmRenderer,
};

fn control_event_owner(
    event: &AudioPlaybackEvent,
) -> (mondrian_core::AudioPcmRendererMaterializationId, u64) {
    match event {
        AudioPlaybackEvent::NativeOwnerBindingsObserved { renderer_id, generation, .. }
        | AudioPlaybackEvent::NativeEditorObserved { renderer_id, generation, .. }
        | AudioPlaybackEvent::NativeParameterObservationsObserved {
            renderer_id, generation, ..
        }
        | AudioPlaybackEvent::NativeDiagnosticsObserved { renderer_id, generation, .. } => {
            (*renderer_id, *generation)
        }
        other => panic!("unexpected author, Ack, hold, attachment or PCM work: {other:?}"),
    }
}
fn assert_consumed(evidence: mondrian_media::AudioPlaybackShutdownEvidence) {
    assert_eq!(evidence.render_workers_started, 1);
    assert_eq!(evidence.render_workers_terminated, 1);
    assert_eq!(evidence.render_retirement_workers_started, 1);
    assert_eq!(evidence.render_retirement_workers_terminated, 1);
    assert!(evidence.all_workers_terminated(), "{evidence:?}");
    assert!(evidence.shutdown_resource_facts_complete_at_deadline);
    assert!(!evidence.shutdown_owner_lifetime_unresolved_at_deadline);
    assert_eq!(evidence.foreign_owner_panics, 0);
    assert_eq!(evidence.foreign_owner_abandonments, 0);
}

#[test]
#[ignore = "current normal Product and owned VST3 Editor DLL; public Project reload, fresh Host Some/opaque overlay, actual GUI and eight software Timeline PCM frames"]
fn app_owned_vst3_saved_host_some_overlays_opaque_in_fresh_native_owner() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let files = tempfile::tempdir().expect("bounded fresh-overlay archive");
    let archive = files.path().join("宿主覆盖后重开🙂.mdp");
    let gain = mondrian_core::ParameterId::new("vst3.param.0").unwrap();
    let ColdNativeControl { mut state, _directory, output, renderer: old_weak } =
        prepare_app_native_control_with_initializer(
            "MONDRIAN_VST3_EDITOR_TEST_PLUGIN",
            "C48B1B724CDE4A91A2C9C4EBB7DD1E83",
            "Mondrian Owned Native Editor Fixture",
            3,
            92,
            |sequence, library, root| {
                initialize_nonzero_audio(sequence, library, root);
                let instance = &mut sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
                assert!(instance.parameters.values().all(|row| row.automation.is_none()));
                assert_editor_configuration(instance.opaque_state.as_ref().unwrap(), 0.5, 0.5, 0);
                // Assemble legal Project data before the original native Source exists.
                // The old opaque streams remain unchanged; no native edit is invented.
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
    let expected_sequence = state.active_sequence().unwrap().clone();
    let instance = expected_sequence.audio_program.outputs[0].strip.pre_fader.processors[0].clone();
    let curve = instance.parameters[&gain].automation.as_ref().unwrap();
    assert_eq!(curve.default_value.to_bits(), 0.25_f64.to_bits());
    assert!(curve.keyframes.is_empty());
    assert_eq!(instance.parameters.len(), 3);
    assert!(instance
        .parameters
        .iter()
        .all(|(id, row)| id == &gain || row.automation.is_none()));
    assert_editor_configuration(instance.opaque_state.as_ref().unwrap(), 0.5, 0.5, 0);
    assert!(expected_sequence.audio_program.note_lanes.is_empty());
    let old_verifier = old_weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let old_claim = old_verifier.current_claim().unwrap();
    let old_unentered = unentered_owner(&old_weak.upgrade().unwrap(), instance.id);
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let old_document =
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let old_history = state.authoring.as_ref().unwrap().history().diagnostics();
        let old_generation = state.audio_playback_snapshot().generation;
        let mut old_ready = false;
        let mut old_renderer_id = None;
        let mut old_target = None;
        let mut old_object = None;
        // Obtain a genuine old installed capability. The old owner never opens
        // a GUI or enters signal processing in this test.
        poll_idle_until(&mut state, old_generation, |event| {
            let (actual_renderer, actual_generation) = control_event_owner(event);
            assert_eq!(actual_generation, old_generation);
            if let Some(previous) = old_renderer_id {
                assert_eq!(actual_renderer, previous);
            }
            old_renderer_id = Some(actual_renderer);
            assert_eq!(
                unentered_owner(&old_weak.upgrade().unwrap(), instance.id),
                old_unentered
            );
            assert!(old_verifier.validate(&old_claim).is_ok());
            match event {
                AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } => {
                    assert_eq!(
                        *status,
                        mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                    );
                    assert!(!old_ready);
                    old_ready = true;
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                    let target = observation.target();
                    assert_eq!(target.instance, instance.id);
                    assert_eq!(target.installed_generation, old_claim.generation());
                    assert!(target.continuity_epoch.is_none());
                    if let Some(previous) = old_target {
                        assert_eq!(target, previous);
                    }
                    old_target = Some(target);
                    let object = (observation.process_id(), observation.object_serial());
                    if let Some(previous) = old_object {
                        assert_eq!(object, previous);
                    }
                    old_object = Some(object);
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                other => panic!("only actual old binding/query is required: {other:?}"),
            }
            old_ready && old_target.is_some()
        });
        let old_target = old_target.unwrap();
        assert!(old_object.is_some(), "the old target was actually observed");
        assert_eq!(
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
            old_document
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            old_history
        );
        old_weak
            .upgrade()
            .unwrap()
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        let project = state.authoring.as_ref().unwrap().project_id();
        let old_session = state.authoring_session_id().unwrap();
        let clap = state.clap_catalog.as_ref().unwrap().clone();
        let vst3 = state.vst3_catalog.as_ref().unwrap().clone();
        state
            .save_project_file_as(archive.clone())
            .expect("public Host-overlay Save As");
        assert!(archive.is_file());
        state
            .close_project()
            .expect("public ProjectClose retires the actual old Source");
        assert_consumed(state.shutdown_audio_playback_and_wait());
        assert!(old_weak.upgrade().is_none());
        assert!(matches!(
            old_verifier.current_claim(),
            Err(AudioOwnerInstallationError::NotInstalled)
        ));
        assert!(state.authoring.is_none() && state.native_audio_renderer.is_none());
        assert!(!state.audio_playback.native_editor_source_available());
        assert_eq!(output.lock().queued, 0);
        assert!(
            Instant::now() < deadline,
            "old save/close exceeded the original 30s window"
        );

        let mut fresh = AppState::with_native_audio_catalogs(clap, vst3);
        assert!(fresh.shutdown_audio_playback_and_wait().all_workers_terminated());
        let mut snapshot = audio_snapshot();
        snapshot.active = false;
        snapshot.buffered_frames = 0;
        let fresh_output = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
            snapshot,
            revision: 0,
            acknowledged: 0,
            auto_acknowledge: true,
            queued: 0,
        }));
        let config = mondrian_media::AudioPlaybackConfig {
            chunk_frames: 4,
            preroll_frames: 4,
            high_watermark_frames: 8,
            max_in_flight: 1,
            ..mondrian_media::AudioPlaybackConfig::product_default()
        };
        fresh.audio_playback = AppAudioPlayback::Available(Box::new(
            AudioPlayback::with_output(
                config,
                Box::new(NativeHoldPlatformOutput(fresh_output.clone())),
            )
            .unwrap(),
        ));
        reopened = Some(fresh);
        let fresh = reopened.as_mut().unwrap();
        fresh.open_project_file(archive.clone()).expect("public new-App Project reopen");
        assert_eq!(fresh.authoring.as_ref().unwrap().project_id(), project);
        assert_ne!(fresh.authoring_session_id(), Some(old_session));
        assert!(fresh
            .active_sequence()
            .unwrap()
            .author_state_eq_ignoring_revision(&expected_sequence));
        assert!(!fresh.can_undo_action() && !fresh.can_redo_action());
        assert!(fresh.native_audio_editor.is_none() && fresh.native_audio_renderer.is_none());
        assert_eq!(fresh.native_parameter_query_serial, 0);
        fresh
            .synchronize_playback_authoring_binding()
            .expect("bind the reopened stopped Engine");
        let source = fresh.audio_playback_renderer("fresh VST3 Host overlay").unwrap().unwrap();
        let unentered = unentered_owner(&source.renderer, instance.id);
        let weak = Arc::downgrade(&source.renderer);
        assert!(!std::sync::Weak::ptr_eq(&weak, &old_weak));
        fresh
            .commit_audio_playback("fresh VST3 Host overlay", audio_anchor(0), Some(source))
            .unwrap();
        let erased: std::sync::Weak<dyn AudioPcmRenderer> = weak.clone();
        assert!(fresh.audio_playback.matches_source_weak(&erased));
        let renderer = weak.upgrade().expect("the actual installed fresh Timeline Source");
        let verifier = renderer.owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().unwrap();
        assert!(!verifier.same_source(&old_verifier));
        assert!(!verifier.same_registration(&old_verifier));
        let document = serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap();
        let history = fresh.authoring.as_ref().unwrap().history().diagnostics();
        let author = fresh.authoring.as_ref().unwrap().execution_snapshot();
        let engine = fresh.playback_engine.snapshot();
        let generation = fresh.audio_playback_snapshot().generation;
        let payload = AudioNativeEditorPayload {
            session_id: fresh.authoring_session_id().unwrap(),
            sequence_id: expected_sequence.id,
            address: mondrian_timeline::AudioProcessorRackAddress::ChannelStrip {
                owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                    output_id: expected_sequence.audio_program.outputs[0].id,
                },
                rack: mondrian_timeline::AudioChannelStripRack::PreFader,
            },
            occurrence: None,
            processor_id: instance.id,
            definition: instance.definition.clone(),
            sequence_time: mondrian_core::DomainTime::new(
                mondrian_core::AuthoringTimeDomain::Sequence(expected_sequence.id),
                TimelineTime::ZERO,
            ),
            open: true,
        };
        let assert_unentered = || {
            assert_eq!(unentered_owner(&renderer, instance.id), unentered);
            assert!(verifier.validate(&claim).is_ok());
            assert!(old_weak.upgrade().is_none());
            assert_eq!(fresh_output.lock().queued, 0);
            assert!(
                Instant::now() < deadline,
                "fresh overlay exceeded the original 30s window"
            );
        };
        let mut ready = false;
        let mut initial_target = None;
        let mut native_object = None;
        let mut renderer_id = None;
        poll_idle_until(fresh, generation, |event| {
            let (actual_renderer, actual_generation) = control_event_owner(event);
            assert_eq!(actual_generation, generation);
            if let Some(previous) = renderer_id {
                assert_eq!(actual_renderer, previous);
            }
            renderer_id = Some(actual_renderer);
            assert_unentered();
            match event {
                AudioPlaybackEvent::NativeOwnerBindingsObserved {
                    renderer_id: actual,
                    status,
                    ..
                } => {
                    assert_eq!(
                        *status,
                        mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                    );
                    assert!(!ready);
                    ready = true;
                    renderer_id = Some(*actual);
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved {
                    renderer_id: actual,
                    observation,
                    ..
                } => {
                    if let Some(previous) = renderer_id {
                        assert_eq!(*actual, previous);
                    }
                    renderer_id = Some(*actual);
                    let target = observation.target();
                    assert_eq!(target.instance, instance.id);
                    assert_eq!(target.installed_generation, claim.generation());
                    assert!(target.continuity_epoch.is_none());
                    assert_ne!(target.root_session, old_target.root_session);
                    assert_ne!(target.local_session, old_target.local_session);
                    assert_ne!(target.binding, old_target.binding);
                    if let Some(previous) = initial_target {
                        assert_eq!(target, previous);
                    }
                    initial_target = Some(target);
                    let object = (observation.process_id(), observation.object_serial());
                    if let Some(previous) = native_object {
                        assert_eq!(object, previous);
                    }
                    native_object = Some(object);
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                other => panic!("no editor, author, Ack or PCM before fresh Open: {other:?}"),
            }
            ready && initial_target.is_some()
        });
        let initial_target = initial_target.unwrap();
        let native_object = native_object.unwrap();
        let renderer_id = renderer_id.unwrap();
        renderer
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        fresh
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                    .into_external_action(),
            )
            .unwrap();
        let mut opened = None;
        let mut facts: Option<Vec<mondrian_core::NativeAudioParameterFact>> = None;
        poll_gui_until(fresh, generation, |event| {
            assert_eq!(control_event_owner(event), (renderer_id, generation));
            assert_unentered();
            match event {
                AudioPlaybackEvent::NativeEditorObserved {
                    renderer_id: actual,
                    intent: Intent::ResolveAndOpen { locator, .. },
                    outcome: Outcome::Opened { target, width, height },
                    ..
                } => {
                    assert_eq!(*actual, renderer_id);
                    assert!(locator.matches_target(*target));
                    assert_eq!(*target, initial_target);
                    assert_eq!((*width, *height), (320, 160));
                    assert!(opened.replace(*target).is_none());
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved {
                    renderer_id: actual,
                    observation,
                    ..
                } => {
                    assert_eq!(*actual, renderer_id);
                    assert_eq!(observation.target(), initial_target);
                    assert_eq!(
                        (observation.process_id(), observation.object_serial()),
                        native_object
                    );
                    // Pre-Opened polls do not qualify the overlay. A natural
                    // MainQuery after the actual Opened receipt must supply it.
                    if opened.is_some() {
                        assert_eq!(observation.facts().len(), 3);
                        for (fact, (id, value, name)) in observation.facts().iter().zip([
                            ("vst3.param.0", 0.25_f64, "Gain"),
                            ("vst3.param.1", 0.5_f64, "Balance"),
                            ("vst3.param.2", 0.0_f64, "Fixture failure scenario"),
                        ]) {
                            assert_eq!(fact.parameter.as_str(), id);
                            assert_eq!(fact.revision, observation.source_revision());
                            assert_eq!(
                                fact.value_bits,
                                Some(value.to_bits()),
                                "FRESH_VST3_HOST_OVERLAY_MAIN_QUERY_MISMATCH: {id}"
                            );
                            assert_eq!(fact.author_value_bits, Some(value.to_bits()));
                            assert_eq!(fact.name.as_deref(), Some(name));
                            assert_eq!(fact.origin, Origin::MainQuery { rescan_flags: 0 });
                            assert_eq!(fact.event_flags, 0);
                        }
                        if let Some(previous) = &facts {
                            assert_eq!(observation.facts(), previous.as_slice());
                        }
                        facts = Some(observation.facts().to_vec());
                    }
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                other => panic!("only fresh Open and natural MainQuery are admitted: {other:?}"),
            }
            opened.is_some() && facts.is_some()
        });
        let opened = opened.unwrap();
        let facts = facts.unwrap();
        assert_eq!(
            fresh.native_audio_editor_surface_state(&payload),
            (true, false)
        );
        for fact in &facts {
            assert_eq!(
                fresh.native_audio_parameter_presentation(&payload, &fact.parameter),
                Some(fact)
            );
        }
        let view = owned_editor_view(native_object.0);
        assert_eq!(
            renderer
                .control_native_editor(
                    Intent::Close { target: old_target },
                    &ExecutionCancellationToken::new()
                )
                .unwrap(),
            Outcome::Refused(Refusal::NotInstalled)
        );
        assert_ne!(
            unsafe { is_window(view) },
            0,
            "old real target cannot close the fresh view"
        );
        assert_eq!(
            fresh.native_audio_editor_surface_state(&payload),
            (true, false)
        );
        assert_unentered();
        let mut close = payload.clone();
        close.open = false;
        fresh
            .dispatch_action(
                ProductAction::Audio(AudioProductAction::NativeEditor(close))
                    .into_external_action(),
            )
            .unwrap();
        poll_gui_until(fresh, generation, |event| {
            assert_eq!(control_event_owner(event), (renderer_id, generation));
            assert_unentered();
            match event {
                AudioPlaybackEvent::NativeEditorObserved {
                    renderer_id: actual,
                    intent: Intent::Close { target: requested },
                    outcome: Outcome::Closed { target },
                    ..
                } => {
                    assert_eq!(*actual, renderer_id);
                    assert_eq!((*requested, *target), (opened, opened));
                    true
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                    assert_eq!(observation.target(), opened);
                    assert_eq!(
                        (observation.process_id(), observation.object_serial()),
                        native_object
                    );
                    assert_eq!(observation.facts(), facts.as_slice());
                    false
                }
                AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                other => {
                    panic!("no author, Ack or Source replacement during fresh Close: {other:?}")
                }
            }
        });
        assert_eq!(unsafe { is_window(view) }, 0);
        assert_eq!(
            fresh.native_audio_editor_surface_state(&payload),
            (false, false)
        );
        assert_unentered();
        assert!(fresh.audio_playback.matches_source_weak(&erased));
        assert_eq!(
            serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(
            fresh.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(
            fresh.authoring.as_ref().unwrap().execution_snapshot().generation(),
            author.generation()
        );
        assert!(author.enter_commit().is_ok());
        assert_eq!(fresh.playback_engine.snapshot(), engine);
        assert_eq!(fresh.native_parameter_query_serial, 0);
        renderer
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        assert!(fresh.playback_engine.native_audio_control_hold_ticket().is_none());
        assert!(fresh.audio_playback.native_control_hold_generation().is_none());
        assert!(fresh.audio_playback.native_reprepare_pending_generation().is_none());

        let pcm_generation = AudioPcmRenderGeneration::new(generation);
        let cancel = ExecutionCancellationToken::new();
        for (start_sample, continuity) in [
            (0, AudioPcmContinuity::Enter(pcm_generation)),
            (4, AudioPcmContinuity::Continue(pcm_generation)),
        ] {
            let AudioPcmRenderOutcome::Rendered(buffer) = renderer
                .render(
                    AudioPcmRenderRequest {
                        transport_activity: None,
                        start_sample,
                        frame_count: 4,
                        sample_rate: 48_000,
                        channel_layout: AudioChannelLayout::Stereo,
                        continuity,
                    },
                    &cancel,
                )
                .expect("the same fresh Timeline Source must execute its Host overlay")
            else {
                panic!("two complete four-frame fresh PCM windows are required");
            };
            // Independent fixture math: +/- .25 input * .25 Gain * 1 Balance.
            // Opaque Gain .5 instead produces +/- .125, including the first frame.
            assert_eq!(
                buffer.samples,
                [0.0625_f32, -0.0625].repeat(4),
                "FRESH_VST3_HOST_OVERLAY_FIRST_PCM_MISMATCH: start={start_sample}"
            );
            assert!(verifier.validate(&claim).is_ok());
            assert!(fresh.audio_playback.matches_source_weak(&erased));
        }
        assert_eq!(
            serde_json::to_value(fresh.authoring.as_ref().unwrap().document()).unwrap(),
            document
        );
        assert_eq!(
            fresh.authoring.as_ref().unwrap().history().diagnostics(),
            history
        );
        assert_eq!(fresh.playback_engine.snapshot(), engine);
        assert_eq!(fresh_output.lock().queued, 0);
        assert!(old_weak.upgrade().is_none());
        // No PID inequality is used: OS reuse cannot establish or disprove the
        // new author Session, Source registration and actual observed owner.
        drop(renderer);
        fresh.close_project().expect("public close of the fresh executed Source");
        assert_consumed(fresh.shutdown_audio_playback_and_wait());
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            verifier.current_claim(),
            Err(AudioOwnerInstallationError::NotInstalled)
        ));
        assert_eq!(fresh_output.lock().queued, 0);
        assert!(
            Instant::now() < deadline,
            "complete fresh-overlay workflow exceeded its original 30s window"
        );
    }));
    // Always consume both Apps before propagating the original target failure.
    let old_cleanup = state.shutdown_audio_playback_and_wait();
    let new_cleanup = reopened.as_mut().map(AppState::shutdown_audio_playback_and_wait);
    let complete = |e: &mondrian_media::AudioPlaybackShutdownEvidence| {
        e.all_workers_terminated()
            && e.shutdown_resource_facts_complete_at_deadline
            && !e.shutdown_owner_lifetime_unresolved_at_deadline
            && e.foreign_owner_panics == 0
            && e.foreign_owner_abandonments == 0
    };
    let clean = complete(&old_cleanup) && new_cleanup.as_ref().is_none_or(complete);
    if let Err(failure) = result {
        if !clean {
            eprintln!("fresh VST3 Host-overlay cleanup incomplete: old={old_cleanup:?}; new={new_cleanup:?}");
        }
        std::panic::resume_unwind(failure);
    }
    assert!(clean, "old={old_cleanup:?}; new={new_cleanup:?}");
    println!(
        "{}",
        serde_json::json!({
            "case":"app-owned-vst3-fresh-host-overlay", "project_reloaded":true,
            "fresh_native_owner_observed":true, "host_some":0.25, "opaque_gain":0.5,
            "remaining_none":2, "natural_main_query":[0.25,0.5,0.0], "old_target_refused":true,
            "unentered_before_signal":true, "pcm_frames":8, "pcm_literal":[0.0625,-0.0625],
            "software_timeline_pcm":true, "device_qualified":false,
            "dsp_history_recovery_qualified":false, "realtime_qualified":false,
            "graceful_100ms_qualified":false
        })
    );
}
