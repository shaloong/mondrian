//! Product author edits admitted against the original pending native hold.
//! This host-model control exposes Media custody, not a native Runtime claim.

use super::super::super::product_action::{AudioProductAction, ProductAction};
use super::*;

#[derive(Debug, Clone, Copy)]
enum PendingEdit {
    ChangedFader,
    SameValueFader,
    Rename,
}

fn assert_pending_edit_admission(edit: PendingEdit) {
    let (mut state, output, required, handled_at) = native_hold_actual_required(true, false, false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state
            .handle_audio_playback_event(required, handled_at)
            .expect("original pending native control event");
        state.reanchor_playback_observation_projection(Instant::now());
        assert!(
            state.parameter_gesture_command.is_none(),
            "original nongesture author seam"
        );
        let engine = state.playback_engine.snapshot();
        let ticket = state.playback_engine.native_audio_control_hold_ticket();
        let playback = state.audio_playback_snapshot();
        let hold_generation = state.audio_playback.native_control_hold_generation();
        let reprepare_generation = state.audio_playback.native_reprepare_pending_generation();
        let binding =
            state.current_audio_playback_author_binding().expect("original audible closure");
        let session = state.authoring_session_id().unwrap();
        let sequence = state.active_sequence_id().unwrap();
        let author_generation = state.authoring.as_ref().unwrap().author_generation();
        let history = state.authoring.as_ref().unwrap().history().diagnostics();
        let document = serde_json::to_value(state.authoring.as_ref().unwrap().document()).unwrap();
        let output_id = state.active_sequence().unwrap().audio_program.outputs[0].id;
        let output_facts = {
            let owned = output.lock();
            (
                owned.snapshot,
                owned.revision,
                owned.acknowledged,
                owned.queued,
                owned.auto_acknowledge,
            )
        };
        assert_eq!(engine.state, TransportState::HoldingForNativeAudioControl);
        assert!(ticket.is_some());
        assert_eq!(hold_generation, Some(playback.generation));
        assert_eq!(reprepare_generation, Some(playback.generation));
        assert_eq!(
            playback.state,
            mondrian_media::AudioPlaybackState::RepreparePending
        );
        assert!(state.audio_playback.native_editor_source_available());
        assert!(state.native_audio_renderer.is_none());
        assert_eq!(
            state.audio_playback_author_binding.as_ref(),
            binding.as_ref()
        );
        assert_ne!(output_facts.1, output_facts.2);
        assert_eq!(output_facts.3, 0);
        assert!(!output_facts.4);
        assert!(state
            .playback_engine
            .next_wake(state.playback_engine.monotonic_high_water())
            .expect("original output retirement wake")
            .is_some());
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0].strip.fader_db,
            0.0
        );

        let outcome = match edit {
            PendingEdit::ChangedFader | PendingEdit::SameValueFader => {
                let value = if matches!(edit, PendingEdit::ChangedFader) {
                    -6.0
                } else {
                    0.0
                };
                state.dispatch_action(
                    ProductAction::Audio(AudioProductAction::EditChannelStrip(
                        mondrian_timeline::AudioChannelStripEditRequest {
                            owner: mondrian_timeline::AudioChannelStripOwner::ProgramOutput {
                                output_id,
                            },
                            edit: mondrian_timeline::AudioChannelStripEdit::SetFaderDb { value },
                        },
                    ))
                    .into_external_action(),
                )
            }
            PendingEdit::Rename => {
                state.commit_active_sequence_edit("metadata during native hold", |sequence| {
                    sequence.name = "renamed without replacing native Source".to_owned();
                    Ok(())
                })
            }
        };
        if matches!(edit, PendingEdit::ChangedFader) {
            let error =
                outcome.expect_err("audible product edit must refuse before author publication");
            assert!(
                matches!(&error,
                    mondrian_core::MondrianError::ActionNotExecuted { action, reason }
                    if action == "authoring_commit"
                        && reason == "native author or editor work must settle before audio Source replacement"),
                "unexpected precommit refusal: {error:?}"
            );
        } else {
            outcome.expect("equivalent audio execution remains editable during the hold");
        }

        let author = state.authoring.as_ref().unwrap();
        let committed_history = author.history().diagnostics();
        assert_eq!(state.authoring_session_id(), Some(session));
        assert_eq!(state.active_sequence_id(), Some(sequence));
        if matches!(edit, PendingEdit::Rename) {
            assert_eq!(
                author.author_generation().get(),
                author_generation.get() + 1
            );
            assert_eq!(committed_history.undo_entries, history.undo_entries + 1);
            assert_eq!(committed_history.redo_entries, 0);
            assert_eq!(committed_history.pending_entries, 0);
            assert_ne!(serde_json::to_value(author.document()).unwrap(), document);
            assert_eq!(
                state.active_sequence().unwrap().name,
                "renamed without replacing native Source"
            );
        } else {
            assert_eq!(author.author_generation(), author_generation);
            assert_eq!(committed_history, history);
            assert_eq!(serde_json::to_value(author.document()).unwrap(), document);
        }
        assert_eq!(
            state.active_sequence().unwrap().audio_program.outputs[0].strip.fader_db,
            0.0
        );
        assert_eq!(
            state.current_audio_playback_author_binding().unwrap(),
            binding
        );
        assert_eq!(
            state.audio_playback_author_binding.as_ref(),
            binding.as_ref()
        );
        assert_eq!(state.playback_engine.snapshot(), engine);
        assert_eq!(
            state.playback_engine.native_audio_control_hold_ticket(),
            ticket
        );
        assert_eq!(state.audio_playback_snapshot(), playback);
        assert_eq!(
            state.audio_playback.native_control_hold_generation(),
            hold_generation
        );
        assert_eq!(
            state.audio_playback.native_reprepare_pending_generation(),
            reprepare_generation
        );
        assert!(state.audio_playback.native_editor_source_available());
        assert!(state.native_audio_renderer.is_none());
        let current_output_facts = {
            let owned = output.lock();
            (
                owned.snapshot,
                owned.revision,
                owned.acknowledged,
                owned.queued,
                owned.auto_acknowledge,
            )
        };
        assert_eq!(current_output_facts, output_facts);
        assert!(state
            .playback_engine
            .next_wake(state.playback_engine.monotonic_high_water())
            .expect("the same unacknowledged output retirement remains pending")
            .is_some());
    }));
    // End the fixture only after target facts have been checked. No output
    // acknowledgement or native callback is manufactured to make admission pass.
    let deadline = Instant::now() + Duration::from_secs(2);
    let pause = state.pause();
    let audio = state.shutdown_audio_playback_until(deadline);
    let cleanup = state.shutdown_for_endurance(deadline);
    assert!(
        pause.is_ok() && audio.all_workers_terminated() && cleanup.all_resources_released(),
        "AUTHOR_AUDIO_ADMISSION_CLEANUP_HOLD: edit={edit:?}; target_passed={}; pause={pause:?}; audio={audio:?}; app={cleanup:?}",
        result.is_ok()
    );
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn pending_native_hold_refuses_changed_product_fader_before_author_publication() {
    assert_pending_edit_admission(PendingEdit::ChangedFader);
}

#[test]
fn pending_native_hold_allows_same_value_product_fader_without_author_publication() {
    assert_pending_edit_admission(PendingEdit::SameValueFader);
}

#[test]
fn pending_native_hold_allows_metadata_commit_without_changing_audible_execution() {
    assert_pending_edit_admission(PendingEdit::Rename);
}
