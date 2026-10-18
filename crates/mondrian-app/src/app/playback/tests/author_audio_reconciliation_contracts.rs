//! Ordinary author commits over the actual installed Timeline/BuiltIn source.
//! Native presentation behavior belongs to the separate real-plugin history control.

use super::project_close_audio_contracts::{available_state, install_builtin_source};
use super::*;

#[test]
fn idle_audible_author_edit_retires_installed_source_without_moving_transport() {
    let (mut state, output) = available_state();
    let mut installed = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let weak = install_builtin_source(&mut state);
        installed = Some(weak.clone());
        let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().expect("actual installed Source claim");
        assert!(verifier.validate(&claim).is_ok());
        assert!(state.audio_playback.native_editor_source_available());
        assert!(state.active_audio_delivery_evidence().is_some());
        let binding = state
            .current_audio_playback_author_binding()
            .expect("actual compiled audible binding")
            .expect("active Program Output");
        let engine = state.playback_engine.snapshot();
        let playback = state.audio_playback_snapshot();
        let session = state.authoring_session_id().unwrap();
        let sequence = state.active_sequence_id().unwrap();
        let author_generation = state.authoring.as_ref().unwrap().author_generation();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let processor_count = state.active_sequence().unwrap().audio_program.outputs[0]
            .strip
            .pre_fader
            .processors
            .len();
        assert_eq!(engine.state, TransportState::Stopped);
        assert_eq!(state.current_frame(), 0);
        assert_eq!(playback.in_flight, 0);
        assert_eq!(playback.next_start_sample, 0);
        assert_eq!(output.lock().queued, 0);

        state
            .commit_active_sequence_edit("append audible BuiltIn gain", |sequence| {
                sequence.audio_program.outputs[0].strip.pre_fader.processors.push(
                    mondrian_timeline::audio::AudioProcessorInstance::built_in(
                        mondrian_timeline::audio::BUILTIN_GAIN_DEFINITION_ID,
                        1,
                    ),
                );
                Ok(())
            })
            .expect("one original author transaction");

        let author = state.authoring.as_ref().unwrap();
        let committed_history = author.history().diagnostics();
        assert_eq!(state.authoring_session_id(), Some(session));
        assert_eq!(state.active_sequence_id(), Some(sequence));
        assert_eq!(
            author.author_generation().get(),
            author_generation.get() + 1
        );
        assert_eq!(committed_history.undo_entries, history.undo_entries + 1);
        assert_eq!(committed_history.redo_entries, 0);
        assert_eq!(committed_history.pending_entries, 0);
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0]
                .strip
                .pre_fader
                .processors
                .len(),
            processor_count + 1
        );
        assert_ne!(
            state.current_audio_playback_author_binding().unwrap().as_ref(),
            Some(&binding),
            "the committed processor changes the actual compiled audible closure"
        );
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert_eq!(state.current_frame(), 0);
        assert!(!state.is_playing());
        assert!(
            !state.audio_playback.native_editor_source_available(),
            "Idle authoring must not retain the old audible Source"
        );
        assert!(state.native_audio_renderer.is_none());
        assert!(matches!(
            verifier.current_claim(),
            Err(mondrian_core::AudioOwnerInstallationError::NotInstalled)
        ));
        assert!(state.active_audio_delivery_evidence().is_none());
        assert!(state.audio_playback_snapshot().generation > playback.generation);
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
        assert_eq!(output.lock().queued, 0);

        let deadline = Instant::now() + Duration::from_secs(2);
        while weak.strong_count() != 0 && Instant::now() < deadline {
            let poll = state
                .audio_playback
                .poll(AudioPlaybackMode::Idle, audio_anchor(0))
                .expect("original retirement worker remains pollable");
            assert!(
                poll.events.is_empty(),
                "retirement cannot rebuild the Source"
            );
            assert_eq!(poll.snapshot.in_flight, 0);
            assert_eq!(poll.snapshot.next_start_sample, 0);
            std::thread::yield_now();
        }
        assert!(
            weak.upgrade().is_none(),
            "old Timeline retires while App remains alive"
        );
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert_eq!(output.lock().queued, 0);
    }));
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_resources_released(),
        "AUTHOR_AUDIO_RECONCILIATION_CLEANUP_HOLD: {cleanup:?}"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(installed.unwrap().upgrade().is_none());
}

#[test]
fn idle_metadata_author_edit_retains_installed_source_and_rebinds_author_snapshot() {
    let (mut state, output) = available_state();
    let mut installed = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let weak = install_builtin_source(&mut state);
        installed = Some(weak.clone());
        let verifier = weak.upgrade().unwrap().owner_installation_verifier().unwrap();
        let claim = verifier.current_claim().expect("actual installed Source claim");
        assert!(verifier.validate(&claim).is_ok());
        assert!(weak.upgrade().unwrap().native_author_snapshot_current());
        let delivery = state.active_audio_delivery_evidence().expect("actual delivery mapping");
        let binding = state
            .current_audio_playback_author_binding()
            .expect("actual compiled audible binding")
            .expect("active Program Output");
        let engine = state.playback_engine.snapshot();
        let playback = state.audio_playback_snapshot();
        let session = state.authoring_session_id().unwrap();
        let sequence = state.active_sequence_id().unwrap();
        let author_generation = state.authoring.as_ref().unwrap().author_generation();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        assert_ne!(
            state.active_sequence().unwrap().name,
            "renamed Idle Program"
        );
        assert_eq!(engine.state, TransportState::Stopped);
        assert_eq!(state.current_frame(), 0);
        assert_eq!(playback.in_flight, 0);
        assert_eq!(playback.next_start_sample, 0);
        assert_eq!(output.lock().queued, 0);

        state
            .commit_active_sequence_edit("rename Idle Sequence", |sequence| {
                sequence.name = "renamed Idle Program".to_owned();
                Ok(())
            })
            .expect("one original metadata transaction");

        let author = state.authoring.as_ref().unwrap();
        let committed_history = author.history().diagnostics();
        assert_eq!(state.authoring_session_id(), Some(session));
        assert_eq!(state.active_sequence_id(), Some(sequence));
        assert_eq!(
            author.author_generation().get(),
            author_generation.get() + 1
        );
        assert_eq!(committed_history.undo_entries, history.undo_entries + 1);
        assert_eq!(committed_history.redo_entries, 0);
        assert_eq!(committed_history.pending_entries, 0);
        assert_eq!(
            state.active_sequence().unwrap().name,
            "renamed Idle Program"
        );
        assert_eq!(
            state.current_audio_playback_author_binding().unwrap().as_ref(),
            Some(&binding),
            "metadata leaves the actual audible closure unchanged"
        );
        assert_eq!(state.audio_playback_author_binding.as_ref(), Some(&binding));
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert_eq!(state.current_frame(), 0);
        assert!(!state.is_playing());
        assert!(state.audio_playback.native_editor_source_available());
        assert!(state
            .native_audio_renderer
            .as_ref()
            .is_some_and(|current| { std::sync::Weak::ptr_eq(current, &weak) }));
        assert!(
            verifier.validate(&claim).is_ok(),
            "the original claim stays current"
        );
        assert!(
            weak.upgrade().unwrap().native_author_snapshot_current(),
            "the retained Runtime accepts the new author generation without replacing DSP"
        );
        assert_eq!(state.active_audio_delivery_evidence(), Some(delivery));
        assert_eq!(
            state.audio_playback_snapshot().generation,
            playback.generation
        );
        assert_eq!(state.audio_playback_snapshot().in_flight, 0);
        assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
        assert_eq!(output.lock().queued, 0);
    }));
    let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
    assert!(
        cleanup.all_resources_released(),
        "AUTHOR_AUDIO_RECONCILIATION_CLEANUP_HOLD: {cleanup:?}"
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    assert!(installed.unwrap().upgrade().is_none());
}

#[test]
fn pending_project_close_refuses_nonempty_history_before_transport_or_author_mutation() {
    for redo in [false, true] {
        let (mut state, output) = available_state();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state
                .commit_active_sequence_edit("seed history lifecycle control", |sequence| {
                    sequence.name = "history before pending Close".to_owned();
                    Ok(())
                })
                .expect("one real metadata history entry");
            if redo {
                assert!(state.undo_timeline().expect("ordinary public Undo seeds Redo"));
            }
            let history = state.authoring.as_ref().unwrap().history().diagnostics();
            assert_eq!(history.undo_entries, usize::from(!redo));
            assert_eq!(history.redo_entries, usize::from(redo));
            assert_eq!(history.pending_entries, 0);
            assert!(!state.audio_playback.native_editor_source_available());
            assert!(state.native_audio_renderer.is_none());
            assert!(state.active_audio_delivery_evidence().is_none());
            assert!(state.begin_project_close().expect("original NoSource Close admission"));
            assert!(state.pending_project_close.is_some());
            assert!(
                !state.project_close_awaiting_audio(),
                "NoSource enters persistence Quiescing without a Media close gate"
            );
            assert!(state.project_lifecycle_blocks_actions());
            let session = state.authoring_session_id().unwrap();
            let author_generation = state.authoring.as_ref().unwrap().author_generation();
            let document = serde_json::to_value(state.authoring.as_ref().unwrap().document())
                .expect("complete retained author document");
            let engine = state.playback_engine.snapshot();
            let playback = state.audio_playback_snapshot();
            assert_eq!(playback.in_flight, 0);
            assert_eq!(playback.next_start_sample, 0);
            assert_eq!(output.lock().queued, 0);

            let action = if redo {
                "redo_timeline"
            } else {
                "undo_timeline"
            };
            let error = if redo {
                state.redo_timeline()
            } else {
                state.undo_timeline()
            }
            .expect_err("nonempty public history must respect Project lifecycle admission");
            assert!(
                matches!(&error,
                    mondrian_core::MondrianError::ActionNotExecuted { action: actual, reason }
                    if actual == action
                        && reason == "Project lifecycle must settle before history restore"),
                "unexpected {action} refusal: {error:?}"
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
            assert_eq!(state.playback_engine.snapshot(), engine);
            assert_eq!(
                state.audio_playback_snapshot().generation,
                playback.generation
            );
            assert_eq!(state.audio_playback_snapshot().in_flight, 0);
            assert_eq!(state.audio_playback_snapshot().next_start_sample, 0);
            assert_eq!(output.lock().queued, 0);
            assert!(!state.audio_playback.native_editor_source_available());
            assert!(state.native_audio_renderer.is_none());
            assert!(state.active_audio_delivery_evidence().is_none());
            assert!(state.pending_project_close.is_some());
            assert!(!state.project_close_awaiting_audio());
            assert!(state.project_close_fault.is_none());
        }));
        let cleanup = state.shutdown_for_endurance(Instant::now() + Duration::from_secs(2));
        assert!(
            cleanup.all_resources_released(),
            "HISTORY_LIFECYCLE_GUARD_CLEANUP_HOLD: redo={redo}; {cleanup:?}"
        );
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
