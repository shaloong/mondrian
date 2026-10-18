//! Ordinary Project audio lifecycle contracts over the actual Timeline/BuiltIn source.
//! These controls do not load a native DLL or manufacture native Closed facts.

use super::*;

fn available_state() -> (AppState, Arc<parking_lot::Mutex<NativeHoldPlatformState>>) {
    let mut state = state_with_sequence(20);
    assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
    let mut snapshot = audio_snapshot();
    snapshot.active = false;
    snapshot.buffered_frames = 0;
    let output = Arc::new(parking_lot::Mutex::new(NativeHoldPlatformState {
        snapshot,
        revision: 0,
        acknowledged: 0,
        auto_acknowledge: true,
        queued: 0,
    }));
    state.audio_playback = AppAudioPlayback::Available(Box::new(
        AudioPlayback::with_output(
            mondrian_media::AudioPlaybackConfig {
                chunk_frames: 4,
                preroll_frames: 4,
                high_watermark_frames: 8,
                max_in_flight: 1,
                ..mondrian_media::AudioPlaybackConfig::product_default()
            },
            Box::new(NativeHoldPlatformOutput(output.clone())),
        )
        .expect("original no-device output and Media workers"),
    ));
    state.audio_processor_resolver = Arc::new(mondrian_audio::BuiltInAudioProcessorResolver);
    state
        .synchronize_playback_authoring_binding()
        .expect("real Video Engine binding");
    (state, output)
}

fn install_builtin_source(state: &mut AppState) -> std::sync::Weak<TimelineAudioPcmRenderer> {
    let mut sequence = state.active_sequence().expect("real Authoring Session").clone();
    sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
        mondrian_timeline::audio::AudioProcessorInstance::built_in(
            mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID,
            1,
        ),
    );
    state.test_set_sequence(Some(sequence));
    state
        .synchronize_playback_authoring_binding()
        .expect("current author/Engine binding");
    let source = state
        .audio_playback_renderer("project-close ordinary source")
        .expect("actual Timeline/BuiltIn constructor")
        .expect("selected built-in gain requires execution");
    let weak = Arc::downgrade(&source.renderer);
    state
        .commit_audio_playback(
            "project-close ordinary source",
            audio_anchor(0),
            Some(source),
        )
        .expect("original Media installation");
    settle_builtin_binding(state);
    weak
}

fn settle_builtin_binding(state: &mut AppState) {
    // Settle the original binding job before the pure try_lock close guard.
    // A real BuiltIn owner has no native object; this is not a fake Closed DTO.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let poll = state
            .audio_playback
            .poll(AudioPlaybackMode::Idle, audio_anchor(0))
            .expect("original Idle binding completion");
        assert_eq!(poll.snapshot.in_flight, 0);
        assert_eq!(poll.snapshot.next_start_sample, 0);
        let mut completed = false;
        for event in poll.events {
            let AudioPlaybackEvent::NativeOwnerBindingsObserved { status, .. } = &event else {
                panic!("BuiltIn setup has no native observations or PCM: {event:?}");
            };
            assert_eq!(
                *status,
                mondrian_media::AudioPcmNativeOwnerBindingStatus::Unavailable(
                    mondrian_media::AudioPcmNativeOwnerBindingRefusal::UnsupportedAdapter,
                )
            );
            completed = true;
            state
                .handle_audio_playback_event(event, Instant::now())
                .expect("original App binding observation");
        }
        if completed {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "BuiltIn binding job did not settle"
        );
        std::thread::yield_now();
    }
}

#[test]
fn project_close_retires_timeline_source_and_keeps_playback_reusable() {
    let (mut state, output) = available_state();
    let weak = install_builtin_source(&mut state);
    let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let claim = verifier.current_claim().expect("actual Media Source claim");
    assert!(verifier.validate(&claim).is_ok());
    assert!(state.active_audio_delivery_evidence().is_some());

    state
        .close_project()
        .expect("public ProjectClose with a genuinely Closed source");
    assert!(state.authoring.is_none());
    assert!(state.active_sequence().is_none());
    assert_eq!(state.current_frame(), 0);
    assert_eq!(
        state.playback_engine.snapshot().state,
        TransportState::Stopped
    );
    assert!(matches!(
        state.audio_playback,
        AppAudioPlayback::Available(_)
    ));
    assert!(!state.audio_playback.native_editor_source_available());
    assert!(state.native_audio_renderer.is_none());
    assert!(state.audio_playback_author_binding.is_none());
    assert!(state.active_audio_delivery_evidence().is_none());
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    let deadline = Instant::now() + Duration::from_secs(2);
    while weak.upgrade().is_some() && Instant::now() < deadline {
        let poll = state
            .audio_playback
            .poll(AudioPlaybackMode::Idle, audio_anchor(0))
            .expect("original retirement worker remains pollable");
        assert!(poll.events.is_empty(), "retirement cannot rebuild a source");
        std::thread::yield_now();
    }
    assert!(
        weak.upgrade().is_none(),
        "old actual Timeline owner retires while App lives"
    );
    assert_eq!(output.lock().queued, 0);

    // ProjectClose retires the Project source, not the reusable Playback owner.
    state.test_set_sequence(Some(Sequence::new("second Project source")));
    let second = install_builtin_source(&mut state);
    let second_verifier = second.upgrade().unwrap().owner_installation_verifier().unwrap();
    assert!(
        verifier.same_source(&second_verifier),
        "one retained Media installation issuer"
    );
    assert!(!verifier.same_registration(&second_verifier));
    assert!(second_verifier.current_claim().is_ok());
    assert!(state.audio_playback.native_editor_source_available());
    state.close_project().expect("second ProjectClose uses the same owner path");
    assert!(matches!(
        second_verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(cleanup.all_resources_released(), "{cleanup:?}");
    assert!(second.upgrade().is_none());
}

#[test]
fn project_close_without_source_preserves_video_stop_for_available_and_unavailable_audio() {
    for unavailable in [false, true] {
        let (mut state, output) = available_state();
        if unavailable {
            assert!(state.shutdown_audio_playback_and_wait().all_workers_terminated());
            assert!(matches!(
                state.audio_playback,
                AppAudioPlayback::Unavailable { .. }
            ));
        }
        let session = state.authoring_session_id().expect("actual author Session");
        let sequence = state.active_sequence_id().expect("actual Sequence");
        state
            .seek_with_source(9, TimelineSeekSource::PointerDrag)
            .expect("original Video seek");
        assert_eq!(state.current_frame(), 9);
        assert_eq!(state.playback_engine.sequence_id(), Some(sequence));
        assert_eq!(state.authoring_session_id(), Some(session));
        assert!(!state.audio_playback.native_editor_source_available());
        state.close_project().expect("NoSource still executes the original Video stop");
        assert!(state.authoring.is_none());
        assert_eq!(state.current_frame(), 0);
        assert_eq!(
            state.playback_engine.snapshot().state,
            TransportState::Stopped
        );
        assert_eq!(
            matches!(state.audio_playback, AppAudioPlayback::Unavailable { .. }),
            unavailable
        );
        assert!(!state.audio_playback.native_editor_source_available());
        assert!(state.native_audio_renderer.is_none());
        assert_eq!(output.lock().queued, 0);
        let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
        assert!(cleanup.all_resources_released(), "{cleanup:?}");
    }
}

#[test]
fn project_close_missing_timeline_alias_refuses_before_stop_or_persistence() {
    let (mut state, output) = available_state();
    let weak = install_builtin_source(&mut state);
    let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let claim = verifier.current_claim().unwrap();
    let engine = state.playback_engine.snapshot();
    let playback = state.audio_playback_snapshot();
    let session = state.authoring_session_id().unwrap();
    let author_generation = state.authoring.as_ref().unwrap().author_generation();
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    state.native_audio_renderer = None; // Deliberately break only derived forwarding metadata.

    let error = state
        .close_project()
        .expect_err("SourceSome requires its actual Timeline alias");
    assert!(
        matches!(error.downcast_ref::<mondrian_core::MondrianError>(),
        Some(mondrian_core::MondrianError::ActionNotExecuted { action, reason })
        if action == "project_audio_handoff"
            && reason == "installed audio Source has no current Timeline Runtime"),
        "unexpected ProjectClose refusal: {error:?}"
    );
    assert_eq!(
        state.playback_engine.snapshot(),
        engine,
        "guard precedes stop"
    );
    assert_eq!(
        state.audio_playback_snapshot().generation,
        playback.generation
    );
    assert_eq!(state.authoring_session_id(), Some(session));
    assert_eq!(
        state.authoring.as_ref().unwrap().author_generation(),
        author_generation
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history
    );
    assert_eq!(
        serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
        document
    );
    assert!(state.pending_project_close.is_none());
    assert!(state.project_close_fault.is_none());
    assert!(verifier.validate(&claim).is_ok());
    assert!(state.audio_playback.native_editor_source_available());
    assert!(weak.upgrade().is_some());
    assert_eq!(output.lock().queued, 0);
    assert!(
        !state.project_lifecycle_blocks_actions(),
        "pure rejection does not enter Project persistence handoff"
    );

    // Restore only the deliberately removed alias, then use public close.
    state.native_audio_renderer = Some(weak.clone());
    state.close_project().expect("same real Source closes after metadata repair");
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(cleanup.all_resources_released(), "{cleanup:?}");
    assert!(weak.upgrade().is_none());
}

#[test]
fn project_close_stale_timeline_alias_refuses_current_source_without_mutation() {
    let (mut state, output) = available_state();
    let previous_weak = install_builtin_source(&mut state);
    let previous = previous_weak.upgrade().expect("retain the actual previous Timeline");
    let previous_verifier = previous.owner_installation_verifier().unwrap();
    let previous_claim = previous_verifier.current_claim().unwrap();

    // Build and install a real second Timeline on the same Media owner. An
    // extant old Arc is not proof that its Runtime still occupies that slot.
    let current_weak = install_builtin_source(&mut state);
    let current = current_weak.upgrade().expect("actual current Timeline");
    assert!(!Arc::ptr_eq(&previous, &current));
    let verifier = current.owner_installation_verifier().unwrap();
    let claim = verifier.current_claim().expect("actual second installation");
    assert!(previous_verifier.same_source(&verifier));
    assert!(!previous_verifier.same_registration(&verifier));
    assert!(matches!(
        previous_verifier.validate(&previous_claim),
        Err(mondrian_core::AudioOwnerInstallationError::Superseded)
    ));
    assert!(verifier.validate(&claim).is_ok());
    drop(current); // The current owner is held by Media, not this test.

    let engine = state.playback_engine.snapshot();
    let playback = state.audio_playback_snapshot();
    let session = state.authoring_session_id().unwrap();
    let author_generation = state.authoring.as_ref().unwrap().author_generation();
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    state.native_audio_renderer = Some(previous_weak.clone());

    let error = state
        .close_project()
        .expect_err("an old live Timeline is not the current Source");
    assert!(
        matches!(error.downcast_ref::<mondrian_core::MondrianError>(),
        Some(mondrian_core::MondrianError::ActionNotExecuted { action, reason })
        if action == "project_audio_handoff"
            && reason == "Timeline Runtime is not the installed audio Source"),
        "unexpected ProjectClose refusal: {error:?}"
    );
    assert_eq!(
        state.playback_engine.snapshot(),
        engine,
        "identity guard precedes stop"
    );
    assert_eq!(
        state.audio_playback_snapshot().generation,
        playback.generation
    );
    assert_eq!(state.authoring_session_id(), Some(session));
    assert_eq!(
        state.authoring.as_ref().unwrap().author_generation(),
        author_generation
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history
    );
    assert_eq!(
        serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
        document
    );
    assert!(state.pending_project_close.is_none());
    assert!(state.project_close_fault.is_none());
    assert!(!state.project_lifecycle_blocks_actions());
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|actual| std::sync::Weak::ptr_eq(actual, &previous_weak)));
    assert!(state.audio_playback.native_editor_source_available());
    assert!(
        verifier.validate(&claim).is_ok(),
        "the current claim was not revoked"
    );
    assert!(current_weak.upgrade().is_some());
    assert!(
        previous_weak.upgrade().is_some(),
        "the supplied alias was live, not missing"
    );
    assert_eq!(output.lock().queued, 0);

    state.native_audio_renderer = Some(current_weak.clone());
    state
        .close_project()
        .expect("the same current Source closes after forwarding repair");
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    // The old alias deliberately retained a scope. Release it BEFORE the
    // consuming cleanup so that intentional test ownership is not a false HOLD.
    drop(previous);
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(cleanup.all_resources_released(), "{cleanup:?}");
    assert!(previous_weak.upgrade().is_none());
    assert!(current_weak.upgrade().is_none());
}

#[test]
fn cancelled_project_open_finishes_after_audio_shutdown_without_resubmitting_stop() {
    let directory = tempfile::tempdir().expect("owned persistence input");
    let project_file = directory.path().join("project.mdp");
    let (mut state, output) = available_state();
    let weak = install_builtin_source(&mut state);
    let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    state.save_project_file_as(project_file.clone()).expect("durable baseline");
    let gate = state.project_persistence.gate_next_request();
    state.request_project_save().expect("save before Open barrier");
    gate.wait_until_running();

    state.request_project_open(project_file).expect("original Open admission");
    assert!(matches!(
        verifier.current_claim(),
        Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
    ));
    assert!(!state.audio_playback.native_editor_source_available());
    assert!(state.native_audio_renderer.is_none());
    assert_eq!(
        state.playback_engine.snapshot().state,
        TransportState::Stopped
    );
    assert_eq!(state.current_frame(), 0);
    assert!(state.begin_project_close().expect("cancel pending Open"));
    assert_eq!(
        state.poll_project_close(),
        crate::app::project_lifecycle::ProjectClosePoll::Pending
    );
    state.begin_endurance_shutdown();
    assert!(matches!(
        state.audio_playback,
        AppAudioPlayback::Available(_)
    ));
    gate.release();

    // The original lifecycle control uses this same persistence deadline.
    // Media remains the same sealed owner until the consuming phase joins it.
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(10));
    assert!(cleanup.all_resources_released(), "{cleanup:?}");
    assert!(cleanup.project.pending_open_was_active);
    assert!(weak.upgrade().is_none());
    assert_eq!(output.lock().queued, 0);
}

#[test]
fn cancelled_project_open_refuses_a_source_reinstalled_through_public_play() {
    let directory = tempfile::tempdir().expect("owned persistence input");
    let project_file = directory.path().join("project.mdp");
    let (mut state, output) = available_state();
    install_builtin_source(&mut state);
    state.save_project_file_as(project_file.clone()).expect("durable baseline");
    let gate = state.project_persistence.gate_next_request();
    let save_request = state.request_project_save().expect("save before Open barrier");
    gate.wait_until_running();
    state.request_project_open(project_file).expect("original Open admission");
    assert!(!state.audio_playback.native_editor_source_available());

    // Product dispatch refuses playback during this barrier. Direct public
    // callers still exist: the cancel terminal must not silently discard an
    // owner installed through that path or assume the dispatch guard ran.
    state.play().expect("direct public playback re-installs the actual source");
    state.pause().expect("pause retains that source");
    settle_builtin_binding(&mut state);
    let weak = state.native_audio_renderer.clone().expect("new actual Timeline");
    let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
    let claim = verifier.current_claim().expect("new installed permission");
    let session = state.authoring_session_id().unwrap();
    let author_generation = state.authoring.as_ref().unwrap().author_generation();
    let history = state.authoring.as_ref().unwrap().history().diagnostics();
    let engine = state.playback_engine.snapshot();
    let audio_generation = state.audio_playback_snapshot().generation;
    assert!(state.begin_project_close().expect("cancel pending Open"));
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(2);
    // Apply the admitted save through its original authority before freezing
    // the complete document. Publication legitimately advances its revision
    // and timestamp; neither belongs to the cancel terminal under test.
    loop {
        state.poll_project_persistence();
        if state
            .manual_project_file_applied_request
            .as_ref()
            .is_some_and(|(_, applied)| *applied == save_request)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the exact admitted save was not applied"
        );
        std::thread::yield_now();
    }
    let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
    let result = loop {
        let result = state.poll_project_close();
        if result != crate::app::project_lifecycle::ProjectClosePoll::Pending {
            break result;
        }
        assert!(
            Instant::now() < deadline,
            "cancel did not reach its original barrier"
        );
        std::thread::yield_now();
    };
    let reason = mondrian_core::MondrianError::ActionNotExecuted {
        action: "project_audio_handoff".to_owned(),
        reason: "requested Project Open regained an audio Source".to_owned(),
    }
    .to_string();
    assert_eq!(
        result,
        crate::app::project_lifecycle::ProjectClosePoll::SaveRejected(reason)
    );
    assert!(!state.project_lifecycle_blocks_actions());
    assert_eq!(state.authoring_session_id(), Some(session));
    assert_eq!(
        state.authoring.as_ref().unwrap().author_generation(),
        author_generation
    );
    assert_eq!(
        state.authoring.as_ref().unwrap().history().diagnostics(),
        history
    );
    assert_eq!(
        serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap(),
        document
    );
    assert_eq!(state.playback_engine.snapshot(), engine);
    assert_eq!(state.audio_playback_snapshot().generation, audio_generation);
    assert!(state.audio_playback.native_editor_source_available());
    assert!(verifier.validate(&claim).is_ok());
    assert!(state
        .native_audio_renderer
        .as_ref()
        .is_some_and(|actual| { std::sync::Weak::ptr_eq(actual, &weak) }));
    assert_eq!(output.lock().queued, 0);
    state.save_project_file().expect("original persistence admission resumed");
    state.close_project().expect("explicit later Close retires the retained source");
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(cleanup.all_resources_released(), "{cleanup:?}");
    assert!(weak.upgrade().is_none());
}
