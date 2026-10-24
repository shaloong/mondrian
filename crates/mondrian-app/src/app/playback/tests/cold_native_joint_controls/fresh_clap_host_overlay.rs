//! Persist Host Some over original opaque unity, then reopen through a fresh App.
//! CLAP's one natural Open query may precede Opened in the completion stream.
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

fn assert_open_query(
    observation: &mondrian_core::NativeAudioParameterObservation,
    volume: &mondrian_core::ParameterId,
) {
    assert_eq!(observation.facts().len(), 1);
    let fact = &observation.facts()[0];
    assert_eq!(&fact.parameter, volume);
    assert_eq!(fact.revision, observation.source_revision());
    assert_eq!(fact.origin, Origin::MainQuery { rescan_flags: 0 });
    assert_eq!(fact.event_flags, 0);
    assert_eq!(fact.value_bits, Some(0.25_f64.to_bits()),
        "FRESH_CLAP_HOST_OVERLAY_MAIN_QUERY_MISMATCH: persisted Host Some .25 must win over original opaque unity");
    assert_eq!(fact.author_value_bits, Some(0.25_f64.to_bits()));
    assert_eq!(fact.display.as_deref(), Some("25.00 %"));
}

#[test]
#[ignore = "current normal Product and owned CLAP Gain editor DLL; public Project reload, fresh Host Some/opaque overlay, actual GUI and eight software Timeline PCM frames"]
fn app_owned_clap_saved_host_some_overlays_opaque_in_fresh_native_owner() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let files = tempfile::tempdir().expect("bounded fresh-overlay archive");
    let archive = files.path().join("clap-host-overlay-reopened.mdp");
    let volume = mondrian_core::ParameterId::new("clap.param.1").unwrap();
    let ColdNativeControl { mut state, _directory, output, renderer: old_weak } =
        prepare_app_clap_editor_control_with_initializer(|sequence, library, root| {
            initialize_nonzero_audio(sequence, library, root);
            let instance = &mut sequence.audio_program.outputs[0].strip.pre_fader.processors[0];
            assert_eq!(instance.parameters.len(), 1);
            assert!(instance.parameters[&volume].automation.is_none());
            assert_eq!(
                instance.opaque_state.as_ref().unwrap().as_slice(),
                &1.0_f32.to_le_bytes()
            );
            // Legal author data precedes the original native Source. No native
            // edit or author-state capture is used to create this conflict.
            instance
                .parameters
                .get_mut(&volume)
                .unwrap()
                .set_automation(
                    mondrian_core::ExactAutomationCurve::new(volume.clone(), 0.25).unwrap(),
                )
                .unwrap();
        });
    let expected_sequence = state.active_sequence().unwrap().clone();
    let instance = expected_sequence.audio_program.outputs[0].strip.pre_fader.processors[0].clone();
    let curve = instance.parameters[&volume].automation.as_ref().unwrap();
    assert_eq!(curve.parameter_id, volume);
    assert_eq!(curve.default_value.to_bits(), 0.25_f64.to_bits());
    assert!(curve.keyframes.is_empty());
    assert_eq!(instance.parameters.len(), 1);
    assert_eq!(
        instance.opaque_state.as_ref().unwrap().as_slice(),
        &1.0_f32.to_le_bytes()
    );
    assert!(expected_sequence.audio_program.note_lanes.is_empty());
    let old_verifier = old_weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let old_claim = old_verifier.current_claim().unwrap();
    let old_unentered = unentered_owner(&old_weak.upgrade().unwrap(), instance.id);
    let mut reopened: Option<AppState> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let old_document =
            serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let old_history = state.authoring.as_ref().unwrap().history().diagnostics();
        let old_author = state.authoring.as_ref().unwrap().execution_snapshot();
        let old_engine = state.playback_engine.snapshot();
        let old_generation = state.audio_playback_snapshot().generation;
        let old_query_serial = state.native_parameter_query_serial;
        let mut old_ready = false;
        let mut old_renderer_id = None;
        let mut old_target = None;
        let mut old_object = None;
        // Obtain a real old installed capability, without opening its GUI or
        // entering signal processing. The complete initial query is consumed.
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
                    assert_eq!(observation.facts().len(), 1);
                    assert_eq!(observation.facts()[0].parameter, volume);
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
        assert_eq!(
            state.authoring.as_ref().unwrap().execution_snapshot().generation(),
            old_author.generation()
        );
        assert!(old_author.enter_commit().is_ok());
        assert_eq!(state.playback_engine.snapshot(), old_engine);
        assert_eq!(state.native_parameter_query_serial, old_query_serial);
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
        // Save must preserve the original author opaque bytes even though the
        // prepared native instance has already received the Host projection.
        assert!(state
            .active_sequence()
            .unwrap()
            .author_state_eq_ignoring_revision(&expected_sequence));
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0]
                .strip
                .pre_fader
                .processors[0]
                .opaque_state
                .as_ref()
                .unwrap()
                .as_slice(),
            &1.0_f32.to_le_bytes()
        );
        assert_eq!(
            state.authoring.as_ref().unwrap().history().diagnostics(),
            old_history
        );
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
        let source = fresh.audio_playback_renderer("fresh CLAP Host overlay").unwrap().unwrap();
        let unentered = unentered_owner(&source.renderer, instance.id);
        let weak = Arc::downgrade(&source.renderer);
        assert!(!std::sync::Weak::ptr_eq(&weak, &old_weak));
        fresh
            .commit_audio_playback("fresh CLAP Host overlay", audio_anchor(0), Some(source))
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
        let assert_author_unchanged = |fresh: &AppState| {
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
            assert!(fresh.audio_playback.matches_source_weak(&erased));
            assert!(fresh
                .native_audio_renderer
                .as_ref()
                .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &weak)));
            assert!(fresh.playback_engine.native_audio_control_hold_ticket().is_none());
            assert!(fresh.audio_playback.native_control_hold_generation().is_none());
            assert!(fresh.audio_playback.native_reprepare_pending_generation().is_none());
        };
        let mut ready = false;
        let mut initial_target = None;
        let mut native_object = None;
        let mut renderer_id = None;
        // Drain both Ready and the full initial presentation before the sole
        // Open. Its values are not the first-Open oracle.
        poll_idle_until(fresh, generation, |event| {
            let (actual_renderer, actual_generation) = control_event_owner(event);
            assert_eq!(actual_generation, generation);
            if let Some(previous) = renderer_id {
                assert_eq!(actual_renderer, previous);
            }
            renderer_id = Some(actual_renderer);
            assert_unentered();
            match event {
                AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } => {
                    assert_eq!(
                        *status,
                        mondrian_media::AudioPcmNativeOwnerBindingStatus::Ready
                    );
                    assert!(!ready);
                    ready = true;
                }
                AudioPlaybackEvent::NativeParameterObservationsObserved { observation, .. } => {
                    let target = observation.target();
                    assert_eq!(target.instance, instance.id);
                    assert_eq!(target.installed_generation, claim.generation());
                    assert!(target.continuity_epoch.is_none());
                    assert_ne!(target.root_session, old_target.root_session);
                    assert_ne!(target.local_session, old_target.local_session);
                    assert_ne!(target.binding, old_target.binding);
                    assert_eq!(observation.facts().len(), 1);
                    assert_eq!(observation.facts()[0].parameter, volume);
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
        assert_author_unchanged(fresh);
        renderer
            .validate_native_author_source_closed(&ExecutionCancellationToken::new())
            .unwrap();
        let mut opened = None;
        let target_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            fresh
                .dispatch_action(
                    ProductAction::Audio(AudioProductAction::NativeEditor(payload.clone()))
                        .into_external_action(),
                )
                .expect("sole original App CLAP Open action");
            let mut presentation: Option<mondrian_core::NativeAudioParameterObservation> = None;
            poll_gui_until(fresh, generation, |event| {
                assert_eq!(control_event_owner(event), (renderer_id, generation));
                assert_unentered();
                match event {
                    AudioPlaybackEvent::NativeEditorObserved {
                        intent: Intent::ResolveAndOpen { locator, .. },
                        outcome: Outcome::Opened { target, width, height },
                        ..
                    } => {
                        assert!(locator.matches_target(*target));
                        assert_eq!(locator.sequence, payload.sequence_id);
                        assert_eq!(*target, initial_target);
                        assert!(opened.replace(*target).is_none());
                        assert_eq!((*width, *height), (320, 100));
                    }
                    AudioPlaybackEvent::NativeParameterObservationsObserved {
                        observation, ..
                    } => {
                        assert_eq!(observation.target(), initial_target);
                        assert_eq!(
                            (observation.process_id(), observation.object_serial()),
                            native_object
                        );
                        if let Some(previous) = &presentation {
                            assert_eq!(observation.as_ref(), previous);
                        }
                        presentation = Some(observation.as_ref().clone());
                    }
                    AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => {}
                    other => {
                        panic!("only fresh Open and natural MainQuery are admitted: {other:?}")
                    }
                }
                opened.is_some() && presentation.is_some()
            });
            // Initial presentation was already consumed. Successful CLAP Open
            // invalidates dedup once, so this is the new natural Open query.
            // Either delivery order is valid; revision need not increase. The
            // original handlers run, without replay or surface-cache seeding.
            let observation = presentation.unwrap();
            assert_eq!(Some(observation.target()), opened);
            assert_open_query(&observation, &volume);
            assert_eq!(
                fresh.native_audio_editor_surface_state(&payload),
                (true, false)
            );
            let view = owned_editor_view_matching(
                native_object.0,
                b"MondrianClapGainEditor_",
                false,
                Some(b"Mondrian CLAP Gain Editor"),
            );
            assert_eq!(
                renderer
                    .control_native_editor(
                        Intent::Close { target: old_target },
                        &ExecutionCancellationToken::new(),
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
            assert_author_unchanged(fresh);
            assert_unentered();
            renderer
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
        }));
        // A target RED still closes a successfully observed actual view using
        // the public action before both Apps consume their owners.
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(target) = opened {
                let view = owned_editor_view_matching(
                    native_object.0,
                    b"MondrianClapGainEditor_",
                    false,
                    Some(b"Mondrian CLAP Gain Editor"),
                );
                let mut close = payload.clone();
                close.open = false;
                fresh
                    .dispatch_action(
                        ProductAction::Audio(AudioProductAction::NativeEditor(close))
                            .into_external_action(),
                    )
                    .expect("public fresh Close even after a target mismatch");
                poll_gui_until(fresh, generation, |event| {
                    assert_eq!(control_event_owner(event), (renderer_id, generation));
                    assert_unentered();
                    match event {
                        AudioPlaybackEvent::NativeEditorObserved {
                            intent: Intent::Close { target: requested },
                            outcome: Outcome::Closed { target: actual },
                            ..
                        } => {
                            assert_eq!((*requested, *actual), (target, target));
                            true
                        }
                        AudioPlaybackEvent::NativeParameterObservationsObserved {
                            observation,
                            ..
                        } => {
                            assert_eq!(observation.target(), target);
                            assert_eq!(
                                (observation.process_id(), observation.object_serial()),
                                native_object
                            );
                            assert_open_query(observation, &volume);
                            false
                        }
                        AudioPlaybackEvent::NativeDiagnosticsObserved { .. } => false,
                        other => panic!(
                            "no author/Ack or Source replacement during fresh Close: {other:?}"
                        ),
                    }
                });
                assert_eq!(unsafe { is_window(view) }, 0);
                assert_eq!(
                    fresh.native_audio_editor_surface_state(&payload),
                    (false, false)
                );
            }
            assert_author_unchanged(fresh);
            assert_unentered();
            renderer
                .validate_native_author_source_closed(&ExecutionCancellationToken::new())
                .unwrap();
        }));
        let pcm_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if target_result.is_err() || close_result.is_err() {
                return;
            }
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
                // Literal independent fixture math: +/- .25 WAV * Host .25.
                // Wrong opaque unity yields +/- .25, including the first frame.
                assert_eq!(
                    buffer.samples,
                    [0.0625_f32, -0.0625].repeat(4),
                    "FRESH_CLAP_HOST_OVERLAY_FIRST_PCM_MISMATCH: start={start_sample}"
                );
                assert!(verifier.validate(&claim).is_ok());
                assert_author_unchanged(fresh);
            }
            assert_eq!(fresh_output.lock().queued, 0);
            assert!(old_weak.upgrade().is_none());
        }));
        if let Err(failure) = target_result {
            std::panic::resume_unwind(failure);
        }
        if let Err(failure) = close_result {
            std::panic::resume_unwind(failure);
        }
        if let Err(failure) = pcm_result {
            std::panic::resume_unwind(failure);
        }
        // Freshness follows actual Source/author Session/registration and
        // observed target identity; OS-reusable PIDs are not compared.
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
    // Both success and failure consume both Apps; never replace a target RED
    // with a later cleanup assertion failure.
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
            eprintln!("fresh CLAP Host-overlay cleanup incomplete: old={old_cleanup:?}; new={new_cleanup:?}");
        }
        std::panic::resume_unwind(failure);
    }
    assert!(clean, "old={old_cleanup:?}; new={new_cleanup:?}");
    println!(
        "{}",
        serde_json::json!({
            "case":"app-owned-clap-fresh-host-overlay", "project_reloaded":true,
            "fresh_native_owner_observed":true, "host_some":0.25, "opaque_gain":1.0,
            "natural_open_main_query":0.25, "old_target_refused":true,
            "unentered_before_signal":true, "author_unchanged":true,
            "pcm_frames":8, "pcm_literal":[0.0625,-0.0625], "software_timeline_pcm":true,
            "device_qualified":false, "dsp_history_recovery_qualified":false,
            "realtime_qualified":false, "graceful_100ms_qualified":false
        })
    );
}
